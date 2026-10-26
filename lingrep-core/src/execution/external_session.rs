use super::{
    DiagnosticCallback, ExecutionLimits, ExternalAdapterCancellation, ExternalAdapterCommand,
    ExternalAdapterProcess, VerifiedAdapterManifest,
};
use crate::analysis::{
    AnalysisError, AnalyzerDescriptor, AnalyzerSession, ExternalProtocolLimits, ExternalRunConfig,
    TextUnit, ValidatedAnalysisBatch, analyzer::sealed,
};
use crate::token_match::{BoundProjection, TokenSchema};

/// Owns one verified process with a fixed projection until completion or drop.
pub struct ExternalAnalyzerSession {
    command: ExternalAdapterCommand,
    manifest: VerifiedAdapterManifest,
    descriptor: AnalyzerDescriptor,
    schema: TokenSchema,
    limits: ExecutionLimits,
    diagnostic: DiagnosticCallback,
    cancellation: ExternalAdapterCancellation,
    process: Option<ExternalAdapterProcess>,
    projection: Option<BoundProjection>,
    finished: bool,
}

impl ExternalAnalyzerSession {
    pub fn new(
        command: ExternalAdapterCommand,
        manifest: VerifiedAdapterManifest,
        descriptor: AnalyzerDescriptor,
        schema: TokenSchema,
        limits: ExecutionLimits,
        diagnostic: DiagnosticCallback,
        cancellation: ExternalAdapterCancellation,
    ) -> Self {
        Self {
            command,
            manifest,
            descriptor,
            schema,
            limits,
            diagnostic,
            cancellation,
            process: None,
            projection: None,
            finished: false,
        }
    }
}

impl sealed::Session for ExternalAnalyzerSession {}
impl AnalyzerSession for ExternalAnalyzerSession {
    fn schema(&self) -> &TokenSchema {
        &self.schema
    }
    fn analyze_bounded_batch(
        &mut self,
        inputs: Vec<TextUnit>,
        projection: &BoundProjection,
    ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
        let fail =
            |error: &dyn std::fmt::Display| AnalysisError::ProviderFailure(error.to_string());
        if self.finished {
            return Err(AnalysisError::ProviderFailure(
                "external analyzer session has finished".into(),
            ));
        }
        if self
            .projection
            .as_ref()
            .is_some_and(|bound| bound != projection)
        {
            return Err(AnalysisError::UnsupportedProjection);
        }
        if self.process.is_none() {
            let config = ExternalRunConfig::new(
                "ling-cli",
                self.descriptor.clone(),
                self.schema.clone(),
                projection.clone(),
                self.manifest.components().to_vec(),
                ExternalProtocolLimits::default(),
            )
            .map_err(|error| fail(&error))?;
            let process = ExternalAdapterProcess::launch_with_cancellation(
                self.command.clone(),
                self.manifest.clone(),
                config,
                self.diagnostic.clone(),
                self.cancellation.clone(),
            )
            .map_err(|error| fail(&error))?;
            self.process = Some(process);
            self.projection = Some(projection.clone());
        }
        let result = self
            .process
            .as_mut()
            .expect("launched process")
            .exchange_batch(inputs, &self.limits);
        match result {
            Ok(batch) => {
                let (analysis, warnings, _) = batch.into_parts();
                for warning in warnings {
                    (self.diagnostic)(format!("{warning:?}").as_bytes())
                        .map_err(|error| fail(&error))?;
                }
                Ok(analysis)
            }
            Err(error) => {
                self.finished = true;
                self.process.take();
                Err(fail(&error))
            }
        }
    }
    fn finish(&mut self) -> Result<(), AnalysisError> {
        self.finished = true;
        if let Some(process) = self.process.take() {
            process
                .finish()
                .map_err(|error| AnalysisError::ProviderFailure(error.to_string()))?;
        }
        Ok(())
    }
}
