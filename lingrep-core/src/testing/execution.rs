//! Deterministic token fixtures run through the schema-bound executor.

use crate::analysis::{
    AnalysisError, AnalyzedInput, AnalyzedSentence, AnalyzerSession, SegmentationMode,
    SurfaceToken, TextUnit, ValidatedAnalysisBatch,
};
use crate::execution::*;
use crate::fsm::CompiledCatalog;
use crate::token_match::{BoundProjection, BoundTokenSentence, TokenInfo, TokenSchema};

pub(crate) struct FixtureSession<F> {
    tokenize: F,
    schema: TokenSchema,
}

impl<F> FixtureSession<F> {
    pub(crate) fn new(tokenize: F) -> Self {
        Self {
            tokenize,
            schema: TokenSchema::unidic_morphology(),
        }
    }
}

impl<F> crate::analysis::analyzer::sealed::Session for FixtureSession<F> {}
impl<F: Fn(&str) -> Vec<TokenInfo>> AnalyzerSession for FixtureSession<F> {
    fn schema(&self) -> &TokenSchema {
        &self.schema
    }

    fn analyze_bounded_batch(
        &mut self,
        inputs: Vec<TextUnit>,
        projection: &BoundProjection,
    ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
        let inputs = inputs
            .into_iter()
            .map(|input| {
                let tokens = (self.tokenize)(input.text());
                let range = input
                    .source_range(0, input.len())
                    .map_err(|_| AnalysisError::InvalidSourceRange)?;
                let words = BoundTokenSentence::project(projection, &tokens)
                    .map_err(|_| AnalysisError::UnsupportedProjection)?;
                let surfaces = if tokens.is_empty() {
                    Vec::new()
                } else {
                    vec![SurfaceToken::new(
                        range,
                        0,
                        u32::try_from(tokens.len())
                            .map_err(|_| AnalysisError::ResourceAccounting)?,
                    )]
                };
                let sentence = AnalyzedSentence::new(0, range, surfaces.into_boxed_slice(), words)?;
                AnalyzedInput::new(input, vec![sentence], SegmentationMode::Input)
            })
            .collect::<Result<Vec<_>, _>>()?;
        ValidatedAnalysisBatch::new(inputs)
    }
}

/// Each fixture word receives the full input's source span; all UniDic attributes
/// remain available to test sinks.
#[allow(clippy::too_many_arguments)]
pub fn execute_fixture<S, F, K, P, O>(
    source: S,
    tokenize: F,
    catalog: &CompiledCatalog,
    pool: &rayon::ThreadPool,
    limits: &ExecutionLimits,
    logical_batch_sentences: usize,
    cancel: &CancellationFlag,
    sink: &mut K,
    progress: &mut P,
    observer: &mut O,
    policy: impl Into<Publication>,
    scope: &SentenceAdmission,
) -> Result<RunOutcome, ExecutionError>
where
    S: SentenceBatchSource + Send,
    F: Fn(&str) -> Vec<TokenInfo>,
    K: MatchSink,
    P: ProgressSink<K>,
    O: TokenizedBatchObserver,
{
    let session = FixtureSession::new(tokenize);
    let schema = session.schema();
    let bound = catalog
        .bind_with_projection(schema, schema.keys().iter().cloned())
        .map_err(|error| ExecutionError::Source {
            ordinal: 0,
            message: error.to_string(),
        })?;
    execute_analyzed(
        source,
        AnalysisBinding::Session(session),
        &bound,
        pool,
        limits,
        logical_batch_sentences,
        cancel,
        sink,
        progress,
        observer,
        policy,
        scope,
    )
}
