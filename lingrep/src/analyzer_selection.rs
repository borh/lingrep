use std::path::Path;

use lingrep_core::{
    analysis::{
        AnalyzerDescriptor, AnalyzerPlan, NativeAnalyzerPlan, SudachiConfig, SudachiPlan,
        SudachiResources, VibratoPlan,
    },
    corpus::{CorpusFormat, conllu},
    dictionary::create_tokenizer_with_identity,
    token_match::TokenSchema,
};

use crate::{
    cli::{AnalyzerCliConfig, ExecutionCliConfig},
    error::CliError,
};

#[derive(Clone)]
pub(crate) enum CliAnalyzerPlan {
    Annotated {
        inner: Box<Self>,
        schema: TokenSchema,
        descriptor: Box<AnalyzerDescriptor>,
        max_output_bytes: usize,
    },
    Native(Box<NativeAnalyzerPlan>),
    #[cfg(unix)]
    External(Box<crate::external_analyzer::ExternalPlan>),
}
impl CliAnalyzerPlan {
    pub(crate) fn for_format(
        self,
        format: CorpusFormat,
        max_output_bytes: usize,
    ) -> Result<Self, CliError> {
        if !matches!(format, CorpusFormat::Tei | CorpusFormat::SoranohaTei) {
            return Ok(self);
        }
        let schema = lingrep_core::corpus::tei::annotated_schema(self.schema())
            .map_err(|e| CliError::Corpus(e.to_string()))?;
        let descriptor = Box::new(lingrep_core::corpus::tei::annotated_descriptor(
            self.descriptor(),
            &schema,
        ));
        Ok(Self::Annotated {
            inner: Box::new(self),
            schema,
            descriptor,
            max_output_bytes,
        })
    }
    pub(crate) fn schema(&self) -> &TokenSchema {
        match self {
            Self::Annotated { schema, .. } => schema,
            Self::Native(plan) => plan.schema(),
            #[cfg(unix)]
            Self::External(plan) => &plan.schema,
        }
    }
    pub(crate) fn descriptor(&self) -> &AnalyzerDescriptor {
        match self {
            Self::Annotated { descriptor, .. } => descriptor,
            Self::Native(plan) => plan.descriptor(),
            #[cfg(unix)]
            Self::External(plan) => &plan.descriptor,
        }
    }
    pub(crate) fn create_session(&self) -> Box<dyn lingrep_core::analysis::AnalyzerSession> {
        match self {
            Self::Annotated {
                inner,
                schema,
                max_output_bytes,
                ..
            } => Box::new(lingrep_core::analysis::AnnotatedAnalysisSession::new(
                inner.create_session(),
                schema.clone(),
                *max_output_bytes,
            )),
            Self::Native(plan) => Box::new(plan.create_session()),
            #[cfg(unix)]
            Self::External(plan) => Box::new(plan.session()),
        }
    }
    pub(crate) fn create_lanes<'a>(
        &'a self,
        workers: usize,
        pool: &'a rayon::ThreadPool,
    ) -> Box<dyn lingrep_core::analysis::AnalyzerSession + 'a> {
        match self {
            Self::Annotated {
                inner,
                schema,
                max_output_bytes,
                ..
            } => Box::new(lingrep_core::analysis::AnnotatedAnalysisSession::new(
                inner.create_lanes(workers, pool),
                schema.clone(),
                *max_output_bytes,
            )),
            Self::Native(plan) => Box::new(lingrep_core::analysis::AnalyzerLanes::new(
                plan.as_ref(),
                workers,
                pool,
            )),
            #[cfg(unix)]
            Self::External(plan) => Box::new(plan.session()),
        }
    }
}

pub(crate) struct ResolvedNativeAnalyzer {
    pub plan: CliAnalyzerPlan,
    pub resource_identity: [u8; 32],
}

pub(crate) enum ResolvedInputAnalyzer {
    Native(ResolvedNativeAnalyzer),
    Pretokenized {
        schema: TokenSchema,
        descriptor: Box<AnalyzerDescriptor>,
    },
}

impl ResolvedInputAnalyzer {
    pub(crate) fn schema(&self) -> &TokenSchema {
        match self {
            Self::Native(resolved) => resolved.plan.schema(),
            Self::Pretokenized { schema, .. } => schema,
        }
    }

    pub(crate) fn descriptor(&self) -> &AnalyzerDescriptor {
        match self {
            Self::Native(resolved) => resolved.plan.descriptor(),
            Self::Pretokenized { descriptor, .. } => descriptor,
        }
    }

    pub(crate) fn resource_identity(&self) -> [u8; 32] {
        match self {
            Self::Native(resolved) => resolved.resource_identity,
            Self::Pretokenized { .. } => [0; 32],
        }
    }

    pub(crate) fn into_native_plan(self) -> Option<CliAnalyzerPlan> {
        match self {
            Self::Native(resolved) => Some(resolved.plan),
            Self::Pretokenized { .. } => None,
        }
    }
}

pub(crate) fn resolve_input(
    format: CorpusFormat,
    config: &ExecutionCliConfig,
    vibrato_dictionary: Option<&Path>,
    cancellation: &lingrep_core::execution::CancellationFlag,
) -> Result<ResolvedInputAnalyzer, CliError> {
    if format == CorpusFormat::Conllu {
        Ok(ResolvedInputAnalyzer::Pretokenized {
            schema: conllu::token_schema(),
            descriptor: Box::new(conllu::analyzer_descriptor()),
        })
    } else {
        let mut resolved = resolve_native(config, vibrato_dictionary, cancellation)?;
        resolved.plan = resolved
            .plan
            .for_format(format, config.limits.max_tokenized_bytes)?;
        Ok(ResolvedInputAnalyzer::Native(resolved))
    }
}

pub(crate) fn resolve_native(
    config: &ExecutionCliConfig,
    vibrato_dictionary: Option<&Path>,
    cancellation: &lingrep_core::execution::CancellationFlag,
) -> Result<ResolvedNativeAnalyzer, CliError> {
    match &config.analyzer {
        AnalyzerCliConfig::Vibrato => {
            let path = vibrato_dictionary.ok_or_else(|| {
                CliError::InvalidArgs(format!(
                    "--analyzer vibrato requires a dictionary: {}",
                    crate::packaged_dictionaries::MISSING_DICTIONARY_HINT
                ))
            })?;
            let path_text = path
                .to_str()
                .ok_or_else(|| CliError::Dictionary("dictionary path is not UTF-8".into()))?;
            let (tokenizer, digest) = create_tokenizer_with_identity(path_text)
                .map_err(|error| CliError::Dictionary(error.to_string()))?;
            Ok(ResolvedNativeAnalyzer {
                plan: CliAnalyzerPlan::Native(Box::new(NativeAnalyzerPlan::Vibrato(
                    VibratoPlan::configured(
                        tokenizer,
                        digest,
                        config.profile,
                        config.segmentation,
                        config.normalization,
                    ),
                ))),
                resource_identity: digest,
            })
        }
        AnalyzerCliConfig::Sudachi {
            dictionary,
            resources,
            split,
            edition,
        } => {
            let resources = resources
                .as_ref()
                .map_or(SudachiResources::BuiltIn, SudachiResources::directory);
            let (dictionary_digest, resource_digest) =
                SudachiConfig::content_digests(dictionary, &resources)
                    .map_err(|error| CliError::Dictionary(error.to_string()))?;
            let config = SudachiConfig::new(
                dictionary,
                resources,
                dictionary_digest,
                resource_digest,
                *edition,
                *split,
                config.profile,
                config.segmentation,
                config.normalization,
            )
            .map_err(|error| CliError::Dictionary(error.to_string()))?;
            let plan = SudachiPlan::load(config)
                .map_err(|error| CliError::Dictionary(error.to_string()))?;
            Ok(ResolvedNativeAnalyzer {
                resource_identity: *plan.descriptor().resource_manifest_digest(),
                plan: CliAnalyzerPlan::Native(Box::new(NativeAnalyzerPlan::Sudachi(plan))),
            })
        }
        AnalyzerCliConfig::ConfiguredExternal { path } => {
            #[cfg(unix)]
            {
                let plan =
                    crate::external_analyzer::ExternalPlan::load(path, config, cancellation)?;
                let resource_identity = *plan.descriptor.resource_manifest_digest();
                Ok(ResolvedNativeAnalyzer {
                    plan: CliAnalyzerPlan::External(Box::new(plan)),
                    resource_identity,
                })
            }
            #[cfg(not(unix))]
            {
                let _ = (path, cancellation);
                Err(CliError::InvalidArgs(
                    "external analyzers require a Unix platform".into(),
                ))
            }
        }
        AnalyzerCliConfig::External { .. } => Err(CliError::InvalidArgs(
            "use --analyzer-config FILE to select a verified external analyzer".into(),
        )),
    }
}
