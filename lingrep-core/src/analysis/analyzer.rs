use std::fmt;

use crate::token_match::{BoundProjection, TokenSchema};

use super::{TextUnit, ValidatedAnalysisBatch};

pub(crate) mod sealed {
    pub trait Plan {}
    pub trait Session {}
}

/// Immutable validated configuration capable of creating lane-owned sessions.
pub trait AnalyzerPlan: sealed::Plan {
    type Session: AnalyzerSession;

    fn schema(&self) -> &TokenSchema;
    fn create_session(&self) -> Self::Session;

    /// The longest single input, in UTF-8 bytes, this analyzer can accept —
    /// `None` when it has no such bound.
    ///
    /// **This is a property of the tokenizer, not a configured limit.** It is
    /// not negotiable and not a resource guard the caller chose: exceeding it
    /// is a hard failure inside the analyzer, so a caller that bounds sentences
    /// only by its own `max_sentence_bytes` can still hand over text that
    /// cannot be analyzed at all.
    ///
    /// The two families genuinely differ, which is why this cannot be one
    /// constant. Vibrato has no such limit, as MeCab did not. Sudachi does, and
    /// it is *smaller than this workspace's default* `max_sentence_bytes`, so
    /// the gap between them is reachable rather than theoretical.
    ///
    /// It does not reach identity on its own: the analyzer's semantic identity
    /// already distinguishes the families, so two artifacts truncated at two
    /// analyzers' limits already carry two identities. What must be recorded is
    /// the *effective* limit a build truncated at, which is the minimum of this
    /// and the caller's.
    fn max_input_bytes(&self) -> Option<usize> {
        None
    }
}

/// Mutable analyzer state owned by exactly one tokenization lane.
pub trait AnalyzerSession: sealed::Session {
    fn schema(&self) -> &TokenSchema;

    fn analyze_bounded_batch(
        &mut self,
        inputs: Vec<TextUnit>,
        projection: &BoundProjection,
    ) -> Result<ValidatedAnalysisBatch, AnalysisError>;
}

/// Why a native analyzer could not return one complete canonical batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnalysisError {
    UnsupportedProjection,
    WrongInputKind,
    InvalidSourceRange,
    InvalidAlignment,
    ResourceAccounting,
    ProviderFailure(String),
}

impl fmt::Display for AnalysisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedProjection => {
                "analyzer cannot produce one or more requested scalar attributes"
            }
            Self::WrongInputKind => {
                "text-unit kind does not match the configured analyzer segmentation mode"
            }
            Self::InvalidSourceRange => "analyzer returned an invalid source range",
            Self::InvalidAlignment => "analyzer returned invalid surface/word alignment",
            Self::ResourceAccounting => "analyzer batch resource accounting overflow",
            Self::ProviderFailure(message) => {
                return write!(formatter, "analyzer provider failed: {message}");
            }
        })
    }
}

impl std::error::Error for AnalysisError {}
