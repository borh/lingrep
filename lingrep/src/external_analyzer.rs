use crate::cli::ExecutionCliConfig;
use crate::error::CliError;
use lingrep_cli_common::external_analyzer::{ExternalPlan as VerifiedPlan, ExternalPlanError};
use lingrep_core::execution::{ExternalAdapterCancellation, ExternalAnalyzerSession};
use std::{path::Path, sync::Arc};

#[derive(Clone)]
pub(crate) struct ExternalPlan {
    plan: VerifiedPlan,
    config: ExecutionCliConfig,
    cancellation: lingrep_core::execution::CancellationFlag,
}

impl std::ops::Deref for ExternalPlan {
    type Target = VerifiedPlan;

    fn deref(&self) -> &Self::Target {
        &self.plan
    }
}

impl ExternalPlan {
    pub(crate) fn load(
        path: &Path,
        config: &ExecutionCliConfig,
        cancellation: &lingrep_core::execution::CancellationFlag,
    ) -> Result<Self, CliError> {
        let plan = VerifiedPlan::load(
            path,
            config.profile,
            config.segmentation,
            config.normalization,
        )
        .map_err(|error| match error {
            ExternalPlanError::InvalidArgs(message) => CliError::InvalidArgs(message),
            ExternalPlanError::InputIo { path, source } => CliError::InputIo { path, source },
            ExternalPlanError::Io(error) => CliError::Io(error),
        })?;
        Ok(Self {
            plan,
            config: config.clone(),
            cancellation: cancellation.clone(),
        })
    }

    pub(crate) fn session(&self) -> ExternalAnalyzerSession {
        let format = self.config.diagnostic_format;
        let stderr = self.config.stderr.clone();
        self.plan.session(
            self.config.limits,
            Arc::new(move |bytes| {
                crate::commands::query_support::write_warning(
                    &stderr,
                    format,
                    crate::protocol::diagnostic::DiagnosticStage::Input,
                    "external_analyzer",
                    lingrep_cli_common::sanitize_terminal(&String::from_utf8_lossy(bytes))
                        .into_owned(),
                    None,
                    Vec::new(),
                )
                .map_err(|e| e.to_string())
            }),
            ExternalAdapterCancellation::from_flag(&self.cancellation),
        )
    }
}
