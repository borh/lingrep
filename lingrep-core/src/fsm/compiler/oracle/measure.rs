//! Reference reductions by walking each parent chain and scanning each boundary.

use super::*;
use crate::fsm::Measurement;
use crate::fsm::compiler::model::{
    ExecutionMeasure, MeasureSource, Reducer, TreeLayer, TreeSeries,
};
use crate::token_match::SentenceSpan;
use std::num::NonZeroU64;
use std::ops::Range;

pub(super) fn value<T: TokenInfoTrait>(
    m: &ExecutionMeasure,
    tokens: &[T],
    spans: &[SentenceSpan],
    path: &OraclePath,
    start: usize,
) -> Measurement {
    let values = observations(m, tokens, spans, path, start);
    let values = match values {
        Ok(v) => v,
        Err(status) => return status,
    };
    let (sum, count) = match m.reducer {
        Reducer::Count => (values.len() as u64, 1),
        Reducer::Mean => (values.iter().map(|&v| v as u64).sum(), values.len() as u64),
        Reducer::Max => match values.iter().max() {
            Some(&v) => (v as u64, 1),
            None => return Measurement::Empty,
        },
    };
    NonZeroU64::new(count).map_or(Measurement::Empty, |count| Measurement::Number {
        sum,
        count,
    })
}

fn observations<T: TokenInfoTrait>(
    m: &ExecutionMeasure,
    tokens: &[T],
    spans: &[SentenceSpan],
    path: &OraclePath,
    start: usize,
) -> Result<Vec<usize>, Measurement> {
    let selected = |i: usize, tests: &[super::super::model::TokenPredicate]| {
        if tests.iter().any(|p| predicate_matches(&tokens[i], p)) {
            return Ok(true);
        }
        if tests
            .iter()
            .flat_map(|p| crate::fsm::scalar_attributes(p))
            .any(|attr| tokens[i].value_key(attr).is_none())
        {
            return Err(Measurement::Unavailable);
        }
        Ok(false)
    };
    let head = AttributeKey::new("dep.head").unwrap();
    let rel = AttributeKey::new("dep.rel").unwrap();
    let word_parent = |i: usize| {
        tokens[i]
            .scalar_key(&head)
            .and_then(|s| s.parse::<usize>().ok())
    };
    let tree = |layer: TreeLayer| {
        let mut units = Vec::<Range<usize>>::new();
        let mut parents = Vec::new();
        match layer {
            TreeLayer::Word => {
                if tokens
                    .iter()
                    .all(|t| t.scalar_key(&head).is_none() && t.scalar_key(&rel).is_none())
                {
                    return Err(Measurement::Unavailable);
                }
                for (i, t) in tokens.iter().enumerate() {
                    let p = word_parent(i).ok_or(Measurement::InvalidTree)?;
                    let r = t
                        .scalar_key(&rel)
                        .filter(|s| !s.is_empty() && *s != "_")
                        .ok_or(Measurement::InvalidTree)?;
                    if p > tokens.len() || (p == 0) != (r == "root" || r.starts_with("root:")) {
                        return Err(Measurement::InvalidTree);
                    }
                    units.push(i..i + 1);
                    parents.push(p.checked_sub(1));
                }
            }
            TreeLayer::Bunsetsu => {
                let mut chunks = spans
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.kind == "bunsetsu")
                    .collect::<Vec<_>>();
                if chunks.is_empty() {
                    return Err(Measurement::Unavailable);
                }
                chunks.sort_by_key(|(_, s)| s.start);
                let mut next = 0;
                for (_, s) in &chunks {
                    if s.start != next || s.end <= next || s.end > tokens.len() {
                        return Err(Measurement::InvalidTree);
                    }
                    next = s.end;
                    let range = s.start..s.end;
                    parents.push(match s.governor {
                        Some(id) => Some(
                            chunks
                                .iter()
                                .position(|(other, _)| *other == id)
                                .ok_or(Measurement::InvalidTree)?,
                        ),
                        None if range.clone().any(|i| {
                            word_parent(i) == Some(0)
                                && tokens[i]
                                    .scalar_key(&rel)
                                    .is_some_and(|r| r == "root" || r.starts_with("root:"))
                        }) =>
                        {
                            None
                        }
                        None => return Err(Measurement::InvalidTree),
                    });
                    units.push(range);
                }
                if next != tokens.len() {
                    return Err(Measurement::InvalidTree);
                }
            }
        }
        if parents.iter().filter(|p| p.is_none()).count() != 1 {
            return Err(Measurement::InvalidTree);
        }
        let mut depths = Vec::new();
        for i in 0..units.len() {
            let mut ancestors = BTreeSet::new();
            let mut node = i;
            while let Some(parent) = parents[node] {
                if !ancestors.insert(node) {
                    return Err(Measurement::InvalidTree);
                }
                node = parent;
            }
            depths.push(ancestors.len());
        }
        Ok((units, parents, depths))
    };
    let links = |parents: Vec<Option<usize>>| {
        parents
            .into_iter()
            .enumerate()
            .filter_map(|(i, p)| p.map(|p| (i, p)))
            .collect::<Vec<_>>()
    };
    let captured = |id| {
        path.slots
            .get(&id)
            .map(|r| r.0)
            .ok_or(Measurement::Unavailable)
    };
    Ok(match &m.source {
        MeasureSource::Tree(layer, series) => {
            let (units, parents, depths) = tree(*layer)?;
            let links = links(parents);
            match series {
                TreeSeries::Depth => depths,
                TreeSeries::Length => links.iter().map(|(i, p)| i.abs_diff(*p)).collect(),
                TreeSeries::OpenLinks => (0..=units.len())
                    .map(|b| {
                        links
                            .iter()
                            .filter(|&&(i, p)| i.min(p) < b && b <= i.max(p))
                            .count()
                    })
                    .collect(),
            }
        }
        MeasureSource::Depth(layer, id) => {
            let (units, _, depths) = tree(*layer)?;
            let i = captured(*id)?;
            vec![depths[units.iter().position(|r| r.contains(&i)).unwrap()]]
        }
        MeasureSource::Links { head, labels, test } => {
            let (_, parents, _) = tree(TreeLayer::Word)?;
            let h = captured(*head)?;
            let mut values = Vec::new();
            for (i, p) in links(parents) {
                let r = tokens[i].scalar_key(&rel).unwrap();
                let eligible = match labels {
                    RelationLabels::Any => true,
                    RelationLabels::Listed(ls) => ls.iter().any(|l| l.matches(r)),
                };
                if p == h && eligible && selected(i, test)? {
                    values.push(i.abs_diff(h))
                }
            }
            values
        }
        MeasureSource::Words(test) => {
            let mut values = Vec::new();
            for i in start..path.position {
                if selected(i, test)? {
                    values.push(1)
                }
            }
            values
        }
    })
}
