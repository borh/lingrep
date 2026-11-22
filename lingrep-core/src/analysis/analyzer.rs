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

/// An analyzer failure while producing a complete canonical batch.
#[derive(Debug)]
pub enum AnalysisError {
    UnsupportedProjection,
    WrongInputKind,
    InvalidSourceRange,
    InvalidAlignment,
    ResourceAccounting,
    ProviderFailure(String),
    ProviderSource(Box<dyn std::error::Error + Send + Sync>),
}

impl fmt::Display for AnalysisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedProjection => {
                "The text analyzer does not provide one or more requested token attributes"
            }
            Self::WrongInputKind => {
                "text-unit kind does not match the configured analyzer segmentation mode"
            }
            Self::InvalidSourceRange => {
                "The text analyzer returned a text position outside the input"
            }
            Self::InvalidAlignment => {
                "The text analyzer returned tokens that do not align with the input text"
            }
            Self::ResourceAccounting => "analyzer batch resource accounting overflow",
            Self::ProviderSource(source) => {
                return write!(formatter, "The text analyzer failed: {source}");
            }
            Self::ProviderFailure(message) => {
                return write!(formatter, "The text analyzer failed: {message}");
            }
        })
    }
}

impl AnalysisError {
    pub(crate) fn provider(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::ProviderSource(Box::new(source))
    }
}

impl std::error::Error for AnalysisError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ProviderSource(source) => Some(source.as_ref()),
            _ => None,
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::AnalysisError;
    use crate::analysis::ExternalProtocolError;
    use crate::execution::ExternalProcessError;
    use std::error::Error;

    #[test]
    fn provider_error_chain_preserves_protocol_metadata() {
        let protocol = ExternalProtocolError::new_for_execution("alignment", "bad span", Some(42));
        let error = AnalysisError::provider(ExternalProcessError::Protocol(protocol));
        let process = error
            .source()
            .unwrap()
            .downcast_ref::<ExternalProcessError>()
            .unwrap();
        let protocol = process
            .source()
            .unwrap()
            .downcast_ref::<ExternalProtocolError>()
            .unwrap();
        assert_eq!(protocol.category(), "alignment");
        assert_eq!(protocol.batch_id(), Some(42));
    }
}
