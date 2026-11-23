use super::*;
use crate::fsm::Measurement;
use crate::fsm::compiler::model::{
    ExecutionMeasure, MeasureSource, NamedMeasure, Reducer, TreeLayer, TreeSeries,
};
use std::cell::OnceCell;
use std::num::NonZeroU64;
use std::ops::Range;

#[derive(Clone)]
pub(super) struct Test {
    /// Each alternative with the scalar attributes it tests.
    alternatives: Vec<(PredicateId, Box<[AttributeKey]>)>,
}
pub(super) type RuntimeMeasure = NamedMeasure<CaptureId, Test, RelationLabel>;

pub(super) fn compile(m: &ExecutionMeasure, predicates: &mut PredicateCatalog) -> RuntimeMeasure {
    let mut compile_test = |tests: &[TokenPredicate]| Test {
        alternatives: tests
            .iter()
            .map(|t| {
                (
                    predicates.intern(t),
                    crate::fsm::scalar_attributes(t).cloned().collect(),
                )
            })
            .collect(),
    };
    let source = match &m.source {
        MeasureSource::Tree(layer, series) => MeasureSource::Tree(layer.clone(), *series),
        MeasureSource::Depth(layer, capture) => MeasureSource::Depth(layer.clone(), *capture),
        MeasureSource::Words(test) => MeasureSource::Words(compile_test(test)),
        MeasureSource::Links { head, labels, test } => MeasureSource::Links {
            head: *head,
            labels: labels.clone(),
            test: compile_test(test),
        },
    };
    NamedMeasure {
        name: m.name.clone(),
        span: m.span,
        reducer: m.reducer,
        source,
    }
}

struct Tree {
    // One-based parent positions, with zero denoting the root attachment.
    parents: Vec<usize>,
    depths: Vec<usize>,
    open: Vec<usize>,
    word_units: Vec<Range<usize>>,
}

impl Tree {
    fn read<S: TokenSequence + ?Sized>(tokens: &S, layer: &TreeLayer) -> Result<Self, Measurement> {
        let head_key = dependency_head_key();
        let rel_key = dependency_relation_key();
        let n = tokens.len();
        let (parents, word_units) = match layer {
            TreeLayer::Word => {
                if !(0..n).any(|i| {
                    tokens.scalar_key(i, &head_key).is_some()
                        || tokens.scalar_key(i, &rel_key).is_some()
                }) {
                    return Err(Measurement::Unavailable);
                }
                let mut parents = Vec::with_capacity(n);
                for i in 0..n {
                    let parent = tokens
                        .scalar_key(i, &head_key)
                        .and_then(|s| s.parse::<usize>().ok())
                        .ok_or(Measurement::InvalidTree)?;
                    let rel = tokens
                        .scalar_key(i, &rel_key)
                        .filter(|s| !s.is_empty() && *s != "_")
                        .ok_or(Measurement::InvalidTree)?;
                    let root = rel == "root" || rel.starts_with("root:");
                    if parent > n || (parent == 0) != root {
                        return Err(Measurement::InvalidTree);
                    }
                    parents.push(parent);
                }
                (parents, Vec::new())
            }
            TreeLayer::Bunsetsu | TreeLayer::Span(_) => {
                let kind = match layer {
                    TreeLayer::Span(kind) => kind.as_str(),
                    _ => "bunsetsu",
                };
                let requires_word_root = matches!(layer, TreeLayer::Bunsetsu);
                let mut spans = tokens
                    .spans()
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.kind == kind)
                    .collect::<Vec<_>>();
                if spans.is_empty() {
                    return Err(Measurement::Unavailable);
                }
                if !requires_word_root && spans.len() == 1 {
                    return Err(Measurement::InvalidTree);
                }
                spans.sort_unstable_by_key(|(_, s)| s.start);
                let ranks: BTreeMap<_, _> = spans
                    .iter()
                    .enumerate()
                    .map(|(rank, (id, _))| (*id, rank + 1))
                    .collect();
                let mut end = 0;
                let mut parents = Vec::with_capacity(spans.len());
                let mut ranges = Vec::with_capacity(spans.len());
                for (_, s) in spans {
                    if s.start != end || s.end <= s.start || s.end > n {
                        return Err(Measurement::InvalidTree);
                    }
                    end = s.end;
                    let range = s.start..s.end;
                    let parent = match s.governor {
                        Some(id) => *ranks.get(&id).ok_or(Measurement::InvalidTree)?,
                        None if !requires_word_root
                            || range.clone().any(|i| {
                                tokens
                                    .scalar_key(i, &head_key)
                                    .and_then(|s| s.parse::<usize>().ok())
                                    == Some(0)
                                    && tokens
                                        .scalar_key(i, &rel_key)
                                        .is_some_and(|r| r == "root" || r.starts_with("root:"))
                            }) =>
                        {
                            0
                        }
                        None => return Err(Measurement::InvalidTree),
                    };
                    ranges.push(range);
                    parents.push(parent);
                }
                if end != n {
                    return Err(Measurement::InvalidTree);
                }
                (parents, ranges)
            }
        };
        let size = parents.len();
        let mut children = vec![Vec::new(); size + 1];
        let mut delta = vec![0isize; size + 1];
        for (i, &parent) in parents.iter().enumerate() {
            children[parent].push(i);
            if parent != 0 {
                delta[(i + 1).min(parent)] += 1;
                delta[(i + 1).max(parent)] -= 1;
            }
        }
        if children[0].len() != 1 {
            return Err(Measurement::InvalidTree);
        }
        let mut depths = vec![0; size];
        let mut stack = children[0].clone();
        let mut visited = 0;
        while let Some(i) = stack.pop() {
            visited += 1;
            for &child in &children[i + 1] {
                depths[child] = depths[i] + 1;
                stack.push(child);
            }
        }
        if visited != size {
            return Err(Measurement::InvalidTree);
        }
        let mut current = 0;
        let open = delta
            .into_iter()
            .map(|d| {
                current += d;
                current as usize
            })
            .collect();
        Ok(Self {
            parents,
            depths,
            open,
            word_units,
        })
    }
}

#[derive(Default)]
struct LayerObservations {
    tree: OnceCell<Result<Tree, Measurement>>,
    series: [OnceCell<Tally>; 3],
}

impl LayerObservations {
    fn tree<S: TokenSequence + ?Sized>(
        &self,
        layer: &TreeLayer,
        tokens: &S,
    ) -> Result<&Tree, Measurement> {
        self.tree
            .get_or_init(|| Tree::read(tokens, layer))
            .as_ref()
            .map_err(|e| *e)
    }
}

#[derive(Default)]
pub(super) struct Observations<'a> {
    words: LayerObservations,
    bunsetsu: LayerObservations,
    named: BTreeMap<&'a str, LayerObservations>,
}

impl<'a> Observations<'a> {
    fn cached(&self, layer: &TreeLayer) -> Option<&LayerObservations> {
        match layer {
            TreeLayer::Word => Some(&self.words),
            TreeLayer::Bunsetsu => Some(&self.bunsetsu),
            TreeLayer::Span(kind) => self.named.get(kind.as_str()),
        }
    }

    fn layer(&mut self, layer: &'a TreeLayer) -> &LayerObservations {
        match layer {
            TreeLayer::Word => &self.words,
            TreeLayer::Bunsetsu => &self.bunsetsu,
            TreeLayer::Span(kind) => self.named.entry(kind.as_str()).or_default(),
        }
    }

    pub(super) fn charge<S: TokenSequence + ?Sized>(
        &self,
        m: &RuntimeMeasure,
        words: usize,
        tokens: &S,
        budget: &mut MatchBudget<'_>,
        pattern: &str,
    ) -> Result<(), MatchResourceError> {
        let word_layer = TreeLayer::Word;
        let (layer, scan) = match &m.source {
            MeasureSource::Words(_) => (None, words),
            MeasureSource::Depth(layer, _) => (Some(layer), 1),
            MeasureSource::Tree(layer, series) => {
                let cached = self
                    .cached(layer)
                    .is_some_and(|entry| entry.series[*series as usize].get().is_some());
                let scan = if cached {
                    1
                } else {
                    tokens.len().saturating_add(1)
                };
                (Some(layer), scan)
            }
            MeasureSource::Links { .. } => (Some(&word_layer), tokens.len()),
        };
        budget.work(pattern, scan)?;
        let Some(layer) = layer else {
            return Ok(());
        };
        let cached = self.cached(layer);
        if cached.is_some_and(|entry| entry.tree.get().is_some()) {
            return Ok(());
        }
        if cached.is_none() {
            let levels = self.named.len().saturating_add(1).ilog2() as usize + 1;
            budget.allocate(
                pattern,
                levels * 16 * size_of::<(&str, LayerObservations)>(),
            )?;
        }
        let units = if matches!(layer, TreeLayer::Word) {
            tokens.len()
        } else {
            let kind = match layer {
                TreeLayer::Span(kind) => kind.as_str(),
                _ => "bunsetsu",
            };
            budget.work(pattern, tokens.spans().len().saturating_add(tokens.len()))?;
            tokens.spans().iter().filter(|s| s.kind == kind).count()
        };
        budget.work(pattern, units)?;
        // Parent and child tables, DFS stack, boundary deltas, depths and span ranks.
        budget.allocate(
            pattern,
            units
                .saturating_add(1)
                .saturating_mul(24 * size_of::<usize>()),
        )
    }

    pub(super) fn observations<S: TokenSequence + ?Sized>(
        &mut self,
        m: &'a RuntimeMeasure,
        range: Range<usize>,
        captured_word: impl Fn(CaptureId) -> Option<usize>,
        predicates: &PredicateSession<'_>,
        tokens: &S,
    ) -> Result<Tally, Measurement> {
        let capture = |id| captured_word(id).ok_or(Measurement::Unavailable);
        let mut tally = Tally::default();
        let matches = |i: usize, test: &Test| -> Result<bool, Measurement> {
            let mut undecided = false;
            for (predicate, scalars) in &test.alternatives {
                if predicates
                    .matches_at_site::<PREDICATE_SITE_EVALUATED_TOKEN, _>(*predicate, tokens, i)
                {
                    return Ok(true);
                }
                undecided |= scalars.iter().any(|key| tokens.value_key(i, key).is_none());
            }
            if undecided {
                Err(Measurement::Unavailable)
            } else {
                Ok(false)
            }
        };
        match &m.source {
            MeasureSource::Tree(layer, series) => {
                let observations = self.layer(layer);
                let t = observations.tree(layer, tokens)?;
                return Ok(*observations.series[*series as usize].get_or_init(|| {
                    let mut tally = Tally::default();
                    match series {
                        TreeSeries::Length => {
                            for (i, &p) in t.parents.iter().enumerate() {
                                if p != 0 {
                                    tally.add((i + 1).abs_diff(p))
                                }
                            }
                        }
                        TreeSeries::Depth => {
                            for &d in &t.depths {
                                tally.add(d)
                            }
                        }
                        TreeSeries::OpenLinks => {
                            for &v in &t.open {
                                tally.add(v)
                            }
                        }
                    }
                    tally
                }));
            }
            MeasureSource::Depth(layer, c) => {
                let t = self.layer(layer).tree(layer, tokens)?;
                let i = capture(*c)?;
                let rank = match layer {
                    TreeLayer::Word => i,
                    TreeLayer::Bunsetsu | TreeLayer::Span(_) => {
                        t.word_units.partition_point(|r| r.end <= i)
                    }
                };
                tally.add(t.depths[rank]);
            }
            MeasureSource::Words(test) => {
                for i in range {
                    if matches(i, test)? {
                        tally.add(1)
                    }
                }
            }
            MeasureSource::Links { head, labels, test } => {
                let t = self.words.tree(&TreeLayer::Word, tokens)?;
                let head = capture(*head)?;
                let rel_key = dependency_relation_key();
                for (i, &parent) in t.parents.iter().enumerate() {
                    if parent == head + 1
                        && tokens
                            .scalar_key(i, &rel_key)
                            .is_some_and(|r| relation_label_matches(labels, r))
                        && matches(i, test)?
                    {
                        tally.add(head.abs_diff(i))
                    }
                }
            }
        }
        Ok(tally)
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Tally {
    count: u64,
    sum: u64,
    max: u64,
}
impl Tally {
    fn add(&mut self, value: usize) {
        self.count += 1;
        self.sum += value as u64;
        self.max = self.max.max(value as u64);
    }
    pub(super) fn reduce(self, reducer: Reducer) -> Measurement {
        let (sum, count) = match reducer {
            Reducer::Count => (self.count, 1),
            Reducer::Mean => (self.sum, self.count),
            Reducer::Max if self.count > 0 => (self.max, 1),
            Reducer::Max => (0, 0),
        };
        NonZeroU64::new(count).map_or(Measurement::Empty, |count| Measurement::Number {
            sum,
            count,
        })
    }
}
