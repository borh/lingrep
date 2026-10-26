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

    /// Longest admitted UTF-8 input accepted by this adapter, or `None` without a fixed ceiling.
    fn max_input_bytes(&self) -> Option<usize> {
        None
    }
}

/// Mutable analyzer state owned by exactly one tokenization lane.
pub trait AnalyzerSession: sealed::Session {
    /// Complete analyzer-owned I/O before the caller reports success.
    fn finish(&mut self) -> Result<(), AnalysisError> {
        Ok(())
    }

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

impl<T: AnalyzerSession + ?Sized> sealed::Session for Box<T> {}
impl<T: AnalyzerSession + ?Sized> AnalyzerSession for Box<T> {
    fn schema(&self) -> &TokenSchema {
        (**self).schema()
    }
    fn analyze_bounded_batch(
        &mut self,
        inputs: Vec<TextUnit>,
        projection: &BoundProjection,
    ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
        (**self).analyze_bounded_batch(inputs, projection)
    }
    fn finish(&mut self) -> Result<(), AnalysisError> {
        (**self).finish()
    }
}
