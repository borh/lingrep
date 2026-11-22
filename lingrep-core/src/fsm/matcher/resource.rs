use compact_str::CompactString;

/// Matching exceeded a graph budget for one sentence or the shared memory budget for results.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MatchResourceError {
    #[error(
        "Could not reconstruct the analytical roles of an accepted span in pattern `{pattern}`."
    )]
    InterpretationReplay { pattern: CompactString },
    #[error("Matching was cancelled.")]
    Cancelled,
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
    } else if resource.starts_with("interpretation") {
        "Narrow the interpreted alternatives or reduce repeated role occurrences."
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

    pub(super) fn interpretation(
        &mut self,
        pattern: &str,
        steps: usize,
        bytes: usize,
    ) -> Result<(), MatchResourceError> {
        self.output.check()?;
        Self::charge(
            &mut self.steps,
            steps,
            pattern,
            "interpretation work steps",
            MAX_GRAPH_WORK,
        )?;
        Self::charge(
            &mut self.bytes,
            bytes,
            pattern,
            "interpretation allocation bytes",
            MAX_ALLOCATION_BYTES,
        )
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
    cancel: Option<crate::execution::CancellationFlag>,
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
            cancel: None,
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

    pub(crate) fn with_cancellation(
        mut self,
        cancel: Option<&crate::execution::CancellationFlag>,
    ) -> Self {
        self.cancel = cancel.cloned();
        self
    }

    pub(crate) fn check(&self) -> Result<(), MatchResourceError> {
        if self
            .cancel
            .as_ref()
            .is_some_and(crate::execution::CancellationFlag::is_cancelled)
        {
            Err(MatchResourceError::Cancelled)
        } else if self.exhausted.load(std::sync::atomic::Ordering::Relaxed) {
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

impl crate::fsm::Interpretation {
    /// Owned bytes charged to matcher output and retained result buffers, including the record itself.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        use super::program::btree_storage_bytes;
        use crate::fsm::AnalyticalValue;
        fn value_bytes(value: &AnalyticalValue) -> usize {
            match value {
                AnalyticalValue::String(text) => compact_heap_bytes(text),
                AnalyticalValue::Boolean(_) | AnalyticalValue::Integer(_) => 0,
            }
        }
        let mut bytes = std::mem::size_of::<Self>()
            .saturating_add(std::mem::size_of_val(self.roles.as_ref()))
            .saturating_add(
                btree_storage_bytes::<CompactString, Box<[AnalyticalValue]>>(
                    self.classification.len(),
                ),
            );
        for role in &self.roles {
            bytes = bytes
                .saturating_add(compact_heap_bytes(&role.role))
                .saturating_add(btree_storage_bytes::<CompactString, AnalyticalValue>(
                    role.labels.len(),
                ));
            for (key, value) in &role.labels {
                bytes = bytes
                    .saturating_add(compact_heap_bytes(key))
                    .saturating_add(value_bytes(value));
            }
        }
        for (key, values) in &self.classification {
            bytes = bytes
                .saturating_add(compact_heap_bytes(key))
                .saturating_add(std::mem::size_of_val(values.as_ref()));
            for value in values {
                bytes = bytes.saturating_add(value_bytes(value));
            }
        }
        bytes
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
    for interpretation in &value.interpretations {
        bytes = bytes.saturating_add(interpretation.retained_bytes());
    }
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
            GraphBinding::Count { name, .. } | GraphBinding::Measure { name, .. } => {
                bytes = bytes.saturating_add(compact_heap_bytes(name))
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

    #[test]
    fn output_budget_check_and_reserve_observe_cancellation() {
        let cancel = crate::execution::CancellationFlag::new();
        let budget = OutputBudget::new(1024).with_cancellation(Some(&cancel));
        assert_eq!(budget.check(), Ok(()));
        assert_eq!(budget.reserve(8), Ok(()));
        assert_eq!(budget.allocated_bytes(), 8);

        cancel.cancel();
        assert_eq!(budget.check(), Err(MatchResourceError::Cancelled));
        assert_eq!(budget.reserve(8), Err(MatchResourceError::Cancelled));
        assert_eq!(budget.allocated_bytes(), 8);
    }
}
