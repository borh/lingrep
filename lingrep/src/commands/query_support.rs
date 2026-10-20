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

pub(crate) fn token_schema_mismatch(
    analyzer: &lingrep_corpus::AnalyzerRecord,
    recorded: &[u8; 32],
) -> Option<String> {
    let current = *analyzer.declared_token_schema()?.digest().as_bytes();
    if *recorded == current {
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
    recorded: &[u8; 32],
) -> Result<(), CliError> {
    let Some(message) = token_schema_mismatch(analyzer, recorded) else {
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
