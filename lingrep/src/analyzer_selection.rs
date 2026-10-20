use std::path::Path;

use lingrep_core::{
    analysis::{
        AnalyzerDescriptor, AnalyzerPlan, NativeAnalyzerPlan, SudachiConfig, SudachiPlan,
        VibratoPlan,
    },
    corpus::{CorpusFormat, conllu},
    dictionary::create_tokenizer_with_identity,
    token_match::TokenSchema,
};

use crate::{
    cli::{AnalyzerCliConfig, ExecutionCliConfig},
    error::CliError,
};

pub(crate) struct ResolvedNativeAnalyzer {
    pub plan: NativeAnalyzerPlan,
    pub resource_identity: [u8; 32],
}

pub(crate) enum ResolvedInputAnalyzer {
    Native(ResolvedNativeAnalyzer),
    Pretokenized {
        schema: TokenSchema,
        descriptor: AnalyzerDescriptor,
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

    pub(crate) fn into_native_plan(self) -> Option<NativeAnalyzerPlan> {
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
) -> Result<ResolvedInputAnalyzer, CliError> {
    if format == CorpusFormat::Conllu {
        Ok(ResolvedInputAnalyzer::Pretokenized {
            schema: conllu::token_schema(),
            descriptor: conllu::analyzer_descriptor(),
        })
    } else {
        resolve_native(config, vibrato_dictionary).map(ResolvedInputAnalyzer::Native)
    }
}

pub(crate) fn resolve_native(
    config: &ExecutionCliConfig,
    vibrato_dictionary: Option<&Path>,
) -> Result<ResolvedNativeAnalyzer, CliError> {
    match &config.analyzer {
        AnalyzerCliConfig::Vibrato => {
            let path = vibrato_dictionary.ok_or_else(|| {
                CliError::InvalidArgs(
                    "--dict-path or LINGREP_DICTIONARY_PATH is required with --analyzer vibrato"
                        .into(),
                )
            })?;
            let path_text = path
                .to_str()
                .ok_or_else(|| CliError::Dictionary("dictionary path is not UTF-8".into()))?;
            let (tokenizer, digest) = create_tokenizer_with_identity(path_text)
                .map_err(|error| CliError::Dictionary(error.to_string()))?;
            Ok(ResolvedNativeAnalyzer {
                plan: NativeAnalyzerPlan::Vibrato(VibratoPlan::configured(
                    tokenizer,
                    digest,
                    config.profile,
                    config.segmentation,
                    config.normalization,
                )),
                resource_identity: digest,
            })
        }
        AnalyzerCliConfig::Sudachi {
            dictionary,
            resources,
            split,
            edition,
        } => {
            let (dictionary_digest, resource_digest) =
                SudachiConfig::content_digests(dictionary, resources)
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
                plan: NativeAnalyzerPlan::Sudachi(plan),
            })
        }
        AnalyzerCliConfig::External { .. } => Err(CliError::InvalidArgs(
            "external analyzers require the verified process-owner execution path".into(),
        )),
    }
}
