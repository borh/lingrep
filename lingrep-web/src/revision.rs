use std::sync::Arc;

pub use crate::snapshot::CompletedSnapshot;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkbenchId(String);

impl WorkbenchId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn random() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RevisionId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheReadinessView {
    Ready,
    Building,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptPhase {
    WaitingForCorpus,
    Queued,
    Running,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryAttempt {
    pub revision: RevisionId,
    pub source: Arc<str>,
    pub phase: AttemptPhase,
    pub sentences: u64,
    pub tokens: u64,
    pub failure: Option<AttemptFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    Compile,
    Cache,
    Execution,
    /// A completed scan whose result contradicts the corpus's own provenance
    /// (sentence-count mismatch) or a corrupt-cache decode failure. Distinct
    /// from a genuine matcher `Execution` error or a worker panic.
    Integrity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptFailure {
    pub category: ErrorCategory,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoordinatorEvent {
    Progress {
        workbench: WorkbenchId,
        revision: RevisionId,
        sentences: u64,
        tokens: u64,
    },
    Completed {
        workbench: WorkbenchId,
        revision: RevisionId,
        snapshot: Arc<CompletedSnapshot>,
    },
    Cancelled {
        workbench: WorkbenchId,
        revision: RevisionId,
    },
    Failed {
        workbench: WorkbenchId,
        revision: RevisionId,
        failure: AttemptFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionCoordinatorState {
    workbench: WorkbenchId,
    displayed_snapshot: Option<Arc<CompletedSnapshot>>,
    current_attempt: Option<QueryAttempt>,
}

impl RevisionCoordinatorState {
    pub fn new(workbench: WorkbenchId) -> Self {
        Self {
            workbench,
            displayed_snapshot: None,
            current_attempt: None,
        }
    }

    pub fn workbench(&self) -> &WorkbenchId {
        &self.workbench
    }

    pub fn displayed_snapshot(&self) -> Option<&CompletedSnapshot> {
        self.displayed_snapshot.as_deref()
    }

    pub(crate) fn displayed_snapshot_arc(&self) -> Option<&Arc<CompletedSnapshot>> {
        self.displayed_snapshot.as_ref()
    }

    pub fn current_attempt(&self) -> Option<&QueryAttempt> {
        self.current_attempt.as_ref()
    }

    pub fn is_terminal(&self) -> bool {
        self.current_attempt.as_ref().is_some_and(|attempt| {
            matches!(
                attempt.phase,
                AttemptPhase::Completed | AttemptPhase::Cancelled | AttemptPhase::Failed
            )
        })
    }

    pub fn accept(
        &mut self,
        revision: RevisionId,
        source: Arc<str>,
        readiness: CacheReadinessView,
    ) -> bool {
        if self
            .current_attempt
            .as_ref()
            .is_some_and(|attempt| revision <= attempt.revision)
        {
            return false;
        }
        self.current_attempt = Some(QueryAttempt {
            revision,
            source,
            phase: match readiness {
                CacheReadinessView::Ready => AttemptPhase::Queued,
                CacheReadinessView::Building | CacheReadinessView::Failed => {
                    AttemptPhase::WaitingForCorpus
                }
            },
            sentences: 0,
            tokens: 0,
            failure: None,
        });
        true
    }

    pub fn mark_running(&mut self, revision: RevisionId) -> bool {
        let Some(attempt) = self.current_attempt.as_mut() else {
            return false;
        };
        if attempt.revision != revision || attempt.phase != AttemptPhase::Queued {
            return false;
        }
        attempt.phase = AttemptPhase::Running;
        true
    }

    pub fn mark_queued(&mut self, revision: RevisionId) -> bool {
        let Some(attempt) = self.current_attempt.as_mut() else {
            return false;
        };
        if attempt.revision != revision {
            return false;
        }
        match attempt.phase {
            AttemptPhase::WaitingForCorpus => {
                attempt.phase = AttemptPhase::Queued;
                true
            }
            AttemptPhase::Queued => true,
            _ => false,
        }
    }

    pub fn apply(&mut self, event: CoordinatorEvent) -> bool {
        match event {
            CoordinatorEvent::Progress {
                workbench,
                revision,
                sentences,
                tokens,
            } => self.apply_progress(&workbench, revision, sentences, tokens),
            terminal => self.apply_terminal(terminal),
        }
    }

    fn apply_progress(
        &mut self,
        workbench: &WorkbenchId,
        revision: RevisionId,
        sentences: u64,
        tokens: u64,
    ) -> bool {
        if workbench != &self.workbench {
            return false;
        }
        let Some(attempt) = self.current_attempt.as_mut() else {
            return false;
        };
        if attempt.revision != revision
            || attempt.phase != AttemptPhase::Running
            || sentences < attempt.sentences
            || tokens < attempt.tokens
        {
            return false;
        }
        attempt.sentences = sentences;
        attempt.tokens = tokens;
        true
    }

    #[cfg(test)]
    pub fn apply_progress_for_test(
        &mut self,
        workbench: &WorkbenchId,
        revision: RevisionId,
        sentences: u64,
        tokens: u64,
    ) -> bool {
        self.apply_progress(workbench, revision, sentences, tokens)
    }

    pub fn apply_terminal(&mut self, event: CoordinatorEvent) -> bool {
        if let CoordinatorEvent::Completed {
            revision, snapshot, ..
        } = &event
            && snapshot.revision != *revision
        {
            return false;
        }
        let (workbench, revision, phase, failure) = match &event {
            CoordinatorEvent::Completed {
                workbench,
                revision,
                ..
            } => (workbench, *revision, AttemptPhase::Completed, None),
            CoordinatorEvent::Cancelled {
                workbench,
                revision,
            } => (workbench, *revision, AttemptPhase::Cancelled, None),
            CoordinatorEvent::Failed {
                workbench,
                revision,
                failure,
            } => (
                workbench,
                *revision,
                AttemptPhase::Failed,
                Some(failure.clone()),
            ),
            CoordinatorEvent::Progress { .. } => return false,
        };
        if workbench != &self.workbench {
            return false;
        }
        let Some(attempt) = self.current_attempt.as_mut() else {
            return false;
        };
        if attempt.revision != revision
            || matches!(
                attempt.phase,
                AttemptPhase::Completed | AttemptPhase::Cancelled | AttemptPhase::Failed
            )
        {
            return false;
        }
        attempt.phase = phase;
        attempt.failure = failure;
        if let CoordinatorEvent::Completed { snapshot, .. } = event {
            self.displayed_snapshot = Some(snapshot);
        }
        true
    }
}
