use compact_str::CompactString;

/// A sentence exceeded a graph-enumeration budget; no matches from that sentence are returned.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error(
    "Pattern `{pattern}` exceeded the {resource} budget for one sentence (limit {limit}). {remedy}",
    remedy = resource_remedy(.resource)
)]
pub struct MatchResourceError {
    pub pattern: CompactString,
    pub resource: &'static str,
    pub limit: usize,
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
}

impl Default for MatchBudget {
    fn default() -> Self {
        Self {
            assignments: MAX_ASSIGNMENTS,
            steps: MAX_GRAPH_WORK,
            bytes: MAX_ALLOCATION_BYTES,
        }
    }
}

impl MatchBudget {
    fn charge(
        remaining: &mut usize,
        amount: usize,
        pattern: &str,
        resource: &'static str,
        limit: usize,
    ) -> Result<(), MatchResourceError> {
        *remaining = remaining
            .checked_sub(amount)
            .ok_or_else(|| MatchResourceError {
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
