//! Shared protocol and diagnostic mechanics for query-like commands.

use std::io::{self, Write};

use lingrep_core::execution::ExecutionError;
use rand::random;

use crate::cli::ExecutionCliConfig;
use crate::direct_source::encode_digest;
use crate::error::CliError;
use crate::output::sanitize_terminal;
use crate::protocol::OutputError;
use crate::protocol::query::{ExecutionLimits, InventoryStats, QueryEnvelope, TraversalLimits};
use crate::protocol::write_jsonl;

pub(crate) fn execution_output_error(error: OutputError) -> ExecutionError {
    let kind = match &error {
        OutputError::Io(error) => error.kind(),
        OutputError::Serialize(_) | OutputError::Protocol(_) => io::ErrorKind::InvalidData,
    };
    ExecutionError::Output {
        kind,
        message: error.to_string(),
    }
}

pub(crate) fn execution_output_io(error: io::Error) -> ExecutionError {
    ExecutionError::Output {
        kind: error.kind(),
        message: error.to_string(),
    }
}

pub(crate) fn publish_query_jsonl<W: Write>(
    writer: &mut W,
    event: &QueryEnvelope,
) -> Result<(), CliError> {
    write_jsonl(writer, event)
        .map_err(execution_output_error)
        .map_err(CliError::from)
}

pub(crate) fn protocol_inventory(
    inventory: lingrep_core::corpus::InventoryStats,
) -> InventoryStats {
    InventoryStats {
        documents: inventory.documents,
        distinct_targets: inventory.distinct_targets,
        alias_documents: inventory.alias_documents,
        max_target_multiplicity: inventory.max_target_multiplicity,
        external_targets: inventory.external_targets,
    }
}

pub(crate) fn protocol_execution_limits(config: &ExecutionCliConfig) -> ExecutionLimits {
    ExecutionLimits {
        max_sentence_bytes: config.limits.max_sentence_bytes as u64,
        max_stage_sentences: config.limits.max_stage_sentences as u64,
        max_stage_input_bytes: config.limits.max_stage_input_bytes as u64,
        max_tokenized_bytes: config.limits.max_tokenized_bytes as u64,
    }
}

pub(crate) fn protocol_traversal_limits(config: &ExecutionCliConfig) -> TraversalLimits {
    TraversalLimits {
        max_depth: config.traversal.max_depth as u64,
        max_source_entries: config.traversal.max_source_entries,
        max_directory_entries: config.traversal.max_directory_entries as u64,
        max_documents: config.traversal.max_documents,
        max_index_bytes: config.traversal.max_index_bytes as u64,
    }
}

pub(crate) fn protocol_traversal_limits_from_report(
    report: &lingrep_core::corpus::DiscoveryReport,
) -> TraversalLimits {
    TraversalLimits {
        max_depth: report.configured_limits.max_depth as u64,
        max_source_entries: report.configured_limits.max_source_entries,
        max_directory_entries: report.configured_limits.max_directory_entries as u64,
        max_documents: report.configured_limits.max_documents,
        max_index_bytes: report.configured_limits.max_index_bytes as u64,
    }
}

#[cfg(test)]
pub(crate) fn token_schema_mismatch(
    analyzer: &lingrep_corpus::AnalyzerRecord,
    recorded: &[u8; 32],
) -> Option<String> {
    let current = *analyzer.declared_token_schema()?.digest().as_bytes();
    token_schema_digest_mismatch(recorded, &current)
}

fn token_schema_digest_mismatch(recorded: &[u8; 32], current: &[u8; 32]) -> Option<String> {
    if recorded == current {
        return None;
    }
    Some(format!(
        "prepared artifact stores tokens built under token schema {}, and this build produces \
         {}. Its attribute set or value-kind mapping has changed, so results describe the \
         tokens the artifact was built with rather than the ones this build would compute. \
         Rebuild it with `corpus build` to compare like with like",
        encode_digest(recorded),
        encode_digest(current),
    ))
}

pub(crate) fn write_token_schema_notice(
    analyzer: &lingrep_corpus::AnalyzerRecord,
    inputs: &lingrep_corpus::AnalysisInputs,
) -> Result<(), CliError> {
    let mut expected = analyzer.declared_token_schema();
    let normalization = match analyzer.normalization.as_str() {
        "none" => Some(lingrep_core::analysis::NormalizationPolicy::None),
        "nfc" => Some(lingrep_core::analysis::NormalizationPolicy::Nfc),
        _ => None,
    };
    if let Some(normalization) = normalization {
        let is_tei = [
            lingrep_core::corpus::CorpusReadConfig::Tei,
            lingrep_core::corpus::CorpusReadConfig::SoranohaTei,
        ]
        .iter()
        .any(|reader| {
            lingrep_corpus::ReaderIdentity::derive(reader, normalization) == inputs.reader()
        });
        if is_tei {
            expected = expected
                .and_then(|schema| lingrep_core::corpus::tei::annotated_schema(&schema).ok());
        }
    }
    let Some(message) = expected.and_then(|schema| {
        token_schema_digest_mismatch(inputs.token_schema().as_bytes(), schema.digest().as_bytes())
    }) else {
        return Ok(());
    };
    writeln!(
        io::stderr().lock(),
        "warning[featurization_policy_mismatch]: {}",
        sanitize_terminal(&message)
    )
    .map_err(execution_output_io)
    .map_err(CliError::from)
}

pub(crate) fn write_binding_notice(
    matcher: &lingrep_core::fsm::CompiledCatalog,
    dictionary_path: &str,
) -> Result<(), CliError> {
    let Some(message) = crate::dictionary_binding::binding_mismatch(matcher, dictionary_path)
    else {
        return Ok(());
    };
    writeln!(
        io::stderr().lock(),
        "warning[dictionary_binding_mismatch]: {}",
        sanitize_terminal(&message)
    )
    .map_err(execution_output_io)
    .map_err(CliError::from)
}

pub(crate) fn random_id() -> String {
    format!("{:032x}", random::<u128>())
}

pub(crate) fn resolve_query_sample(
    sample_args: &crate::cli::SampleArgs,
    cache: bool,
    tsv: bool,
    output: crate::presentation::OutputMode,
    mode: crate::protocol::query::QueryMode,
) -> Result<Option<lingrep_core::execution::CorpusSample>, CliError> {
    use crate::presentation::OutputMode;
    use crate::protocol::query::QueryMode;
    let sample = sample_args.resolve().map_err(CliError::InvalidArgs)?;
    if sample_args.blocks && output != OutputMode::Jsonl {
        return Err(CliError::InvalidArgs(
            "--sample-blocks requires --json so the run declares its correlated sample unit".into(),
        ));
    }
    if sample.is_some() && cache {
        return Err(CliError::InvalidArgs(
            "--sample cannot be combined with --cache: the artifact would name a \
             fraction of the corpus as the whole of it. Sample a direct run, or \
             build the artifact first and sample the query against it."
                .to_owned(),
        ));
    }
    if sample.is_some() && tsv {
        let message = match mode {
            QueryMode::Matches => {
                "--sample cannot be combined with --tsv: `lingrep.matches-tsv/v1` \
             has no run record, so a sampled run would publish a fraction of \
             the matches as if it were all of them. Use JSONL, whose `run` \
             record declares the sample."
                    .to_owned()
            }
            QueryMode::Summarize => {
                "--sample cannot be combined with --tsv: `lingrep.summarize-tsv/v1` \
             has no run record, so a sampled count would be published as if it \
             were a census. Use JSONL, whose `run` record declares the sample."
                    .to_owned()
            }
        };
        return Err(CliError::InvalidArgs(message));
    }
    Ok(sample)
}
