use serde::{Deserialize, Serialize};

/// A sentence-local annotation covering the half-open token interval `start..end`.
/// Its ID is its position in the sentence span table. A governor names another
/// span of the same kind, while the head indexes a token inside this interval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentenceSpan {
    pub kind: String,
    pub start: usize,
    pub end: usize,
    pub head: Option<usize>,
    pub label: Option<String>,
    pub governor: Option<usize>,
}

impl SentenceSpan {
    pub fn validate(spans: &[Self], tokens: usize) -> Result<(), String> {
        for span in spans {
            if span.kind.is_empty() || span.start >= span.end || span.end > tokens {
                return Err("empty span kind or invalid token interval".into());
            }
            if span
                .head
                .is_some_and(|i| !(span.start..span.end).contains(&i))
            {
                return Err("span head outside token interval".into());
            }
            if let Some(i) = span.governor
                && spans.get(i).is_none_or(|s| s.kind != span.kind)
            {
                return Err("span governor missing or of a different kind".into());
            }
        }
        if spans.iter().any(|s| s.governor.is_some()) {
            let mut visited = vec![None; spans.len()];
            for id in 0..spans.len() {
                let mut next = Some(id);
                while let Some(i) = next {
                    if visited[i] == Some(id) {
                        return Err("cycle in span dependencies".into());
                    }
                    if visited[i].is_some() {
                        break;
                    }
                    visited[i] = Some(id);
                    next = spans[i].governor;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(start: usize, end: usize, governor: Option<usize>) -> SentenceSpan {
        SentenceSpan {
            kind: "chunk".into(),
            start,
            end,
            head: Some(start),
            label: None,
            governor,
        }
    }

    #[test]
    fn dependencies_require_acyclic_same_kind_targets_and_internal_heads() {
        let spans = vec![chunk(0, 2, Some(1)), chunk(2, 3, None)];
        assert!(SentenceSpan::validate(&spans, 3).is_ok());
        for invalid in [
            SentenceSpan {
                governor: Some(0),
                ..spans[1].clone()
            },
            SentenceSpan {
                kind: "entity".into(),
                ..spans[1].clone()
            },
            SentenceSpan {
                head: Some(0),
                ..spans[1].clone()
            },
            chunk(3, 3, None),
            chunk(2, 4, None),
            chunk(2, 3, Some(2)),
        ] {
            assert!(SentenceSpan::validate(&[spans[0].clone(), invalid], 3).is_err());
        }
    }
}
