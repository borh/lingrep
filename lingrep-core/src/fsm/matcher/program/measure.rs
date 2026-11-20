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
        MeasureSource::Tree(layer, series) => MeasureSource::Tree(*layer, *series),
        MeasureSource::Depth(layer, capture) => MeasureSource::Depth(*layer, *capture),
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
    fn read<S: TokenSequence + ?Sized>(tokens: &S, layer: TreeLayer) -> Result<Self, Measurement> {
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
            TreeLayer::Bunsetsu => {
                let mut spans = tokens
                    .spans()
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.kind == "bunsetsu")
                    .collect::<Vec<_>>();
                if spans.is_empty() {
                    return Err(Measurement::Unavailable);
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
                        None if range.clone().any(|i| {
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
pub(super) struct Observations {
    words: OnceCell<Result<Tree, Measurement>>,
    bunsetsu: OnceCell<Result<Tree, Measurement>>,
    /// Whole-tree series reduced once per sentence, by layer and series.
    series: [[OnceCell<Tally>; 3]; 2],
}

impl Observations {
    fn series(&self, layer: TreeLayer, series: TreeSeries) -> &OnceCell<Tally> {
        &self.series[layer as usize][series as usize]
    }

    fn tree<S: TokenSequence + ?Sized>(
        &self,
        layer: TreeLayer,
        tokens: &S,
    ) -> Result<&Tree, Measurement> {
        let cell = match layer {
            TreeLayer::Word => &self.words,
            TreeLayer::Bunsetsu => &self.bunsetsu,
        };
        cell.get_or_init(|| Tree::read(tokens, layer))
            .as_ref()
            .map_err(|e| *e)
    }

    pub(super) fn charge<S: TokenSequence + ?Sized>(
        &self,
        m: &RuntimeMeasure,
        words: usize,
        tokens: &S,
        budget: &mut MatchBudget,
        pattern: &str,
    ) -> Result<(), MatchResourceError> {
        let (layer, scan) = match &m.source {
            MeasureSource::Words(_) => (None, words),
            MeasureSource::Depth(l, _) => (Some(*l), 1),
            MeasureSource::Tree(l, s) if self.series(*l, *s).get().is_some() => (Some(*l), 1),
            MeasureSource::Tree(l, _) => (Some(*l), tokens.len().saturating_add(1)),
            MeasureSource::Links { .. } => (Some(TreeLayer::Word), tokens.len()),
        };
        budget.work(pattern, scan)?;
        let units = match layer {
            Some(TreeLayer::Word) if self.words.get().is_none() => tokens.len(),
            Some(TreeLayer::Bunsetsu) if self.bunsetsu.get().is_none() => {
                budget.work(pattern, tokens.spans().len().saturating_add(tokens.len()))?;
                tokens
                    .spans()
                    .iter()
                    .filter(|s| s.kind == "bunsetsu")
                    .count()
            }
            _ => return Ok(()),
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
        &self,
        m: &RuntimeMeasure,
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
                let t = self.tree(*layer, tokens)?;
                return Ok(*self.series(*layer, *series).get_or_init(|| {
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
                let t = self.tree(*layer, tokens)?;
                let i = capture(*c)?;
                let rank = match layer {
                    TreeLayer::Word => i,
                    TreeLayer::Bunsetsu => t.word_units.partition_point(|r| r.end <= i),
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
                let t = self.tree(TreeLayer::Word, tokens)?;
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
