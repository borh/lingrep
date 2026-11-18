use compact_str::CompactString;

/// Matching exceeded a graph budget for one sentence or the shared memory budget for results.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MatchResourceError {
    #[error("Pattern `{pattern}` exceeded the {resource} budget for one sentence (limit {limit}). {remedy}", remedy = resource_remedy(.resource))]
    Sentence {
        pattern: CompactString,
        resource: &'static str,
        limit: usize,
    },
    #[error(
        "Matching this batch would exceed the estimated memory budget for results ({limit} bytes)."
    )]
    BatchOutput { limit: usize },
}

fn resource_remedy(resource: &str) -> &'static str {
    if resource == "graph relation depth" {
        "Reduce the number of graph relation clauses in this pattern."
    } else {
        "Narrow the token or graph predicates, or add an ordering constraint between graph binders."
    }
}

const MAX_ASSIGNMENTS: usize = 65_536;
const MAX_GRAPH_WORK: usize = 1_000_000;
const MAX_ALLOCATION_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_RELATION_DEPTH: usize = 128;

pub(crate) struct MatchBudget {
    assignments: usize,
    steps: usize,
    bytes: usize,
    pub(super) output: std::sync::Arc<OutputBudget>,
}

impl Default for MatchBudget {
    fn default() -> Self {
        Self::with_output(std::sync::Arc::new(OutputBudget::default()))
    }
}

impl MatchBudget {
    pub(crate) fn with_output(output: std::sync::Arc<OutputBudget>) -> Self {
        Self {
            assignments: MAX_ASSIGNMENTS,
            steps: MAX_GRAPH_WORK,
            bytes: MAX_ALLOCATION_BYTES,
            output,
        }
    }

    fn charge(
        remaining: &mut usize,
        amount: usize,
        pattern: &str,
        resource: &'static str,
        limit: usize,
    ) -> Result<(), MatchResourceError> {
        *remaining = remaining
            .checked_sub(amount)
            .ok_or_else(|| MatchResourceError::Sentence {
                pattern: pattern.into(),
                resource,
                limit,
            })?;
        Ok(())
    }

    pub(crate) fn step(&mut self, pattern: &str) -> Result<(), MatchResourceError> {
        self.work(pattern, 1)
    }

    pub(crate) fn work(&mut self, pattern: &str, steps: usize) -> Result<(), MatchResourceError> {
        Self::charge(
            &mut self.steps,
            steps,
            pattern,
            "graph work steps",
            MAX_GRAPH_WORK,
        )
    }

    pub(crate) fn allocate(
        &mut self,
        pattern: &str,
        bytes: usize,
    ) -> Result<(), MatchResourceError> {
        Self::charge(
            &mut self.bytes,
            bytes,
            pattern,
            "estimated graph assignment allocation bytes",
            MAX_ALLOCATION_BYTES,
        )
    }

    pub(crate) fn assignment(
        &mut self,
        pattern: &str,
        bytes: usize,
    ) -> Result<(), MatchResourceError> {
        Self::charge(
            &mut self.assignments,
            1,
            pattern,
            "graph assignments",
            MAX_ASSIGNMENTS,
        )?;
        self.allocate(pattern, bytes)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MatchPublicationError {
    #[error(transparent)]
    Resource(#[from] MatchResourceError),
    #[error(transparent)]
    Policy(#[from] crate::fsm::policy::AnchorResolutionError),
}

#[derive(Debug, thiserror::Error)]
pub enum EnhancedMatchError {
    #[error(transparent)]
    Resource(#[from] MatchResourceError),
    #[error(transparent)]
    Graph(#[from] crate::fsm::EnhancedGraphExecutionError),
}

/// Cumulative estimated result allocation for one batch; charges are not released during matching.
pub(crate) struct OutputBudget {
    remaining: std::sync::atomic::AtomicUsize,
    exhausted: std::sync::atomic::AtomicBool,
    limit: usize,
}

impl Default for OutputBudget {
    fn default() -> Self {
        Self::new(crate::test_scale::ScaleKnob::MatchOutputBytes.or(64 * 1024 * 1024))
    }
}

impl OutputBudget {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            remaining: std::sync::atomic::AtomicUsize::new(limit),
            exhausted: std::sync::atomic::AtomicBool::new(false),
            limit,
        }
    }

    fn fail(&self) -> MatchResourceError {
        self.exhausted
            .store(true, std::sync::atomic::Ordering::Relaxed);
        MatchResourceError::BatchOutput { limit: self.limit }
    }

    #[cfg(test)]
    pub(super) fn allocated_bytes(&self) -> usize {
        self.limit - self.remaining.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn check(&self) -> Result<(), MatchResourceError> {
        if self.exhausted.load(std::sync::atomic::Ordering::Relaxed) {
            Err(self.fail())
        } else {
            Ok(())
        }
    }

    pub(crate) fn reserve(&self, bytes: usize) -> Result<(), MatchResourceError> {
        self.check()?;
        if bytes == 0 {
            return Ok(());
        }
        self.remaining
            .try_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |remaining| remaining.checked_sub(bytes),
            )
            .map(|_| ())
            .map_err(|_| self.fail())
    }

    pub(crate) fn reserve_buffer<T>(&self, capacity: usize) -> Result<(), MatchResourceError> {
        self.reserve(
            capacity
                .checked_mul(std::mem::size_of::<T>())
                .ok_or_else(|| self.fail())?,
        )
    }

    pub(crate) fn reserve_vec<T>(
        &self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), MatchResourceError> {
        let needed = values
            .len()
            .checked_add(additional)
            .ok_or_else(|| self.fail())?;
        if needed > values.capacity() {
            let capacity = needed.max(values.capacity().saturating_mul(2)).max(4);
            self.reserve_buffer::<T>(capacity)?;
            values.reserve_exact(capacity - values.len());
        }
        Ok(())
    }
}

pub(crate) fn compact_heap_bytes(value: &CompactString) -> usize {
    if value.is_heap_allocated() {
        value.capacity()
    } else {
        0
    }
}

pub(crate) fn match_heap_bytes(value: &crate::fsm::Match) -> usize {
    use crate::fsm::{Capture, GraphBinding, GraphNodeAttribute, LexicalMatch};
    let mut bytes = compact_heap_bytes(&value.pattern)
        .saturating_add(
            value
                .captures
                .len()
                .saturating_mul(std::mem::size_of::<Capture>()),
        )
        .saturating_add(
            value
                .graph_bindings
                .len()
                .saturating_mul(std::mem::size_of::<GraphBinding>()),
        );
    for capture in &value.captures {
        bytes = bytes
            .saturating_add(compact_heap_bytes(&capture.name))
            .saturating_add(compact_heap_bytes(&capture.path));
    }
    for binding in &value.graph_bindings {
        match binding {
            GraphBinding::Node {
                name, attributes, ..
            } => {
                bytes = bytes
                    .saturating_add(compact_heap_bytes(name))
                    .saturating_add(
                        attributes
                            .len()
                            .saturating_mul(std::mem::size_of::<GraphNodeAttribute>()),
                    );
                for attribute in attributes {
                    bytes = bytes
                        .saturating_add(attribute.name.heap_bytes())
                        .saturating_add(compact_heap_bytes(&attribute.value));
                }
            }
            GraphBinding::Edge { name, relation, .. } => {
                bytes = bytes
                    .saturating_add(compact_heap_bytes(name))
                    .saturating_add(compact_heap_bytes(relation));
            }
        }
    }
    if let Some(lexical) = &value.lexical {
        bytes = bytes
            .saturating_add(std::mem::size_of::<LexicalMatch>())
            .saturating_add(compact_heap_bytes(&lexical.term))
            .saturating_add(
                lexical
                    .matched_by
                    .len()
                    .saturating_mul(std::mem::size_of::<crate::token_match::AttributeKey>()),
            );
        for key in &lexical.matched_by {
            bytes = bytes.saturating_add(key.heap_bytes());
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_limit_prevents_vector_allocation() {
        let budget = OutputBudget::new(1);
        let mut values = Vec::<crate::fsm::Match>::new();
        assert!(matches!(
            budget.reserve_vec(&mut values, 1),
            Err(MatchResourceError::BatchOutput { limit: 1 })
        ));
        assert_eq!(values.capacity(), 0);
        assert_eq!(budget.allocated_bytes(), 0);
    }
}
