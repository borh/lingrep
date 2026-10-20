use crate::token_match::{BoundProjection, TokenSchema};

use super::{
    AnalysisError, AnalyzerDescriptor, AnalyzerPlan, AnalyzerSession, SudachiPlan, SudachiSession,
    TextUnit, ValidatedAnalysisBatch, VibratoPlan, VibratoSession, analyzer::sealed,
};

/// One resolved in-process analyzer. Dispatch occurs once per bounded batch,
/// never in the token projection loop.
#[derive(Clone)]
pub enum NativeAnalyzerPlan {
    Vibrato(VibratoPlan),
    Sudachi(SudachiPlan),
}

impl NativeAnalyzerPlan {
    #[must_use]
    pub fn descriptor(&self) -> &AnalyzerDescriptor {
        match self {
            Self::Vibrato(plan) => plan.descriptor(),
            Self::Sudachi(plan) => plan.descriptor(),
        }
    }
}

impl sealed::Plan for NativeAnalyzerPlan {}

impl AnalyzerPlan for NativeAnalyzerPlan {
    type Session = NativeAnalyzerSession;

    fn schema(&self) -> &TokenSchema {
        match self {
            Self::Vibrato(plan) => plan.schema(),
            Self::Sudachi(plan) => plan.schema(),
        }
    }

    fn max_input_bytes(&self) -> Option<usize> {
        match self {
            Self::Vibrato(plan) => plan.max_input_bytes(),
            Self::Sudachi(plan) => plan.max_input_bytes(),
        }
    }

    fn create_session(&self) -> Self::Session {
        match self {
            Self::Vibrato(plan) => NativeAnalyzerSession::Vibrato(plan.create_session()),
            Self::Sudachi(plan) => NativeAnalyzerSession::Sudachi(plan.create_session()),
        }
    }
}

pub enum NativeAnalyzerSession {
    Vibrato(VibratoSession),
    Sudachi(SudachiSession),
}

impl sealed::Session for NativeAnalyzerSession {}

impl AnalyzerSession for NativeAnalyzerSession {
    fn schema(&self) -> &TokenSchema {
        match self {
            Self::Vibrato(session) => session.schema(),
            Self::Sudachi(session) => session.schema(),
        }
    }

    fn analyze_bounded_batch(
        &mut self,
        inputs: Vec<TextUnit>,
        projection: &BoundProjection,
    ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
        match self {
            Self::Vibrato(session) => session.analyze_bounded_batch(inputs, projection),
            Self::Sudachi(session) => session.analyze_bounded_batch(inputs, projection),
        }
    }
}
