//! `corpus variants` — what prepared realizations exist beside one source.
//!
//! This command reports; it never builds or selects a realization, and it
//! deletes only with `--prune`, and only artifacts of the target's source that
//! [`lingrep_corpus::is_superseded`] classifies. It reuses `SiblingScan`
//! verbatim rather than growing a second scanner, so what `corpus build`
//! decides on and what this command shows can never disagree.
//!
//! There is deliberately no `corpus compare`. Comparison is what this command
//! does when handed an artifact: differences are then expressed relative to
//! that artifact, and a second surface would be the same scan under a second
//! name.

use std::io::{self, Write};

use lingrep_corpus::{
    CorpusArtifact, SiblingRealization, SiblingScan, artifact_bytes, is_superseded,
    parse_artifact_name, remove_superseded,
};

use super::CommandOutcome;
use crate::cli::CorpusVariantsArgs;
use crate::direct_source::encode_digest;
use crate::error::CliError;
use crate::presentation::{ExplicitLimit, OutputMode};
use crate::protocol::variants::{
    VariantAnalyzer, VariantDiagnostic, VariantDifference, VariantRecord, VariantsComplete,
    VariantsEnvelope, VariantsEvent, VariantsReference, VariantsRun,
};
use crate::protocol::write_jsonl;

pub fn run(args: &CorpusVariantsArgs, output: OutputMode) -> Result<CommandOutcome, CliError> {
    let stdout = io::stdout();
    run_to(args, output, &mut stdout.lock())
}

pub(crate) fn run_to<W: Write>(
    args: &CorpusVariantsArgs,
    output: OutputMode,
    writer: &mut W,
) -> Result<CommandOutcome, CliError> {
    let reference = CorpusArtifact::open(&args.target).ok();
    // One rule for every target, because there is only one rule:
    // `canonical_artifact_path` names a realization with `with_file_name`, so a
    // realization *always* sits in the target's parent, and whether the target
    // is a file, a directory or an artifact never enters into it.
    //
    // This used to special-case a directory source root and scan the root
    // itself, on the reasoning that "a source's realizations sit beside it,
    // which is the same directory". That holds for a file — `a.jsonl` and
    // `a.jsonl.g6.….lingcorpus` really are siblings — and reads across to a
    // directory as "inside it", which is where no realization has ever been
    // written. Every directory root therefore reported zero, which is the one
    // answer indistinguishable from "none exist": `corpus variants /data/bccwj`
    // found nothing while `corpus variants /data` listed the artifact of
    // `/data/bccwj`. Directory roots are not a corner — a BCCWJ distribution is
    // one, and so is every Markdown corpus.
    let directory = lingrep_corpus::containing_directory(&args.target);

    let report = SiblingScan::scan(directory).map_err(|error| {
        CliError::Corpus(format!(
            "cannot scan {} for prepared realizations: {error}",
            directory.display()
        ))
    })?;

    let run_id = crate::commands::query_support::random_id();
    let reference_record = match &reference {
        Some(artifact) => VariantsReference::Artifact {
            path: args.target.display().to_string(),
            source_identity: encode_digest(artifact.source_identity().as_bytes()),
            analysis_identity: encode_digest(artifact.analysis_identity().as_bytes()),
        },
        None => VariantsReference::Source {
            root: args.target.display().to_string(),
        },
    };
    let has_reference = matches!(reference_record, VariantsReference::Artifact { .. });
    publish(
        writer,
        output,
        has_reference,
        &VariantsEnvelope::new(VariantsEvent::Run(VariantsRun {
            run_id,
            directory: directory.display().to_string(),
            reference: reference_record,
        })),
    )?;

    // The reference realization is the one at the target path, when the scan
    // found it. Comparing against the *scanned* record rather than the opened
    // artifact keeps both sides reading the same recorded facts.
    let anchor = reference.as_ref().and_then(|_| {
        report
            .realizations
            .iter()
            .find(|realization| realization.path == args.target)
    });

    let limit = args.limit.and_then(ExplicitLimit::resolve);
    let mut reported = 0_u64;
    for realization in &report.realizations {
        if limit.is_some_and(|limit| reported >= limit) {
            break;
        }
        let record = describe(realization, anchor);
        publish(
            writer,
            output,
            has_reference,
            &VariantsEnvelope::new(VariantsEvent::Variant(Box::new(record))),
        )?;
        reported += 1;
    }

    let source_name = source_name(&args.target);
    let mut diagnostics = Vec::with_capacity(report.diagnostics.len());
    let (mut pruned, mut freed_bytes) = (0_u64, 0_u64);
    for (path, error) in &report.diagnostics {
        let prunable = is_superseded(error)
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(parse_artifact_name)
                .is_some_and(|parsed| parsed.source_name == source_name);
        let mut diagnostic = VariantDiagnostic {
            path: path.display().to_string(),
            message: error.to_string(),
            bytes: artifact_bytes(path).ok(),
            prunable,
            pruned: false,
        };
        if prunable && args.prune {
            let bytes = remove_superseded(path, &source_name).map_err(|error| {
                CliError::Corpus(format!("cannot prune {}: {error}", path.display()))
            })?;
            diagnostic.bytes = Some(bytes);
            diagnostic.pruned = true;
            pruned += 1;
            freed_bytes += bytes;
        }
        diagnostics.push(diagnostic);
    }

    publish(
        writer,
        output,
        has_reference,
        &VariantsEnvelope::new(VariantsEvent::Complete(VariantsComplete {
            realizations: report.realizations.len() as u64,
            reported,
            unreadable: report.diagnostics.len() as u64,
            diagnostics,
            pruned,
            freed_bytes,
        })),
    )?;
    Ok(CommandOutcome::Success)
}

/// File name of the source whose realizations `target` names: the target's own
/// name, or the source part of a canonical artifact name.
fn source_name(target: &std::path::Path) -> String {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    parse_artifact_name(name).map_or_else(|| name.to_owned(), |parsed| parsed.source_name)
}

/// Names every dimension on which `realization` differs from `anchor`.
///
/// With no anchor — the argument was a source — a realization differs from
/// nothing and reports an empty list; the reader groups by `source_identity`
/// instead.
///
/// The identity scheme is its own dimension and is never folded into the
/// analyzer one. Two artifacts can be the same analysis addressed two ways,
/// and reporting that as an analyzer difference would tell an operator to
/// rebuild something already correct.
fn describe(
    realization: &SiblingRealization,
    anchor: Option<&SiblingRealization>,
) -> VariantRecord {
    let mut differences = Vec::new();
    let mut same_source = true;
    if let Some(anchor) = anchor
        && !std::ptr::eq(realization, anchor)
    {
        same_source = realization.source_identity == anchor.source_identity;
        if !same_source {
            differences.push(VariantDifference::Source);
        }
        if realization.reader != anchor.reader {
            differences.push(VariantDifference::Reader);
        }
        if realization.admission != anchor.admission {
            differences.push(VariantDifference::Admission);
        }
        if realization.analyzer.semantic_identity != anchor.analyzer.semantic_identity {
            differences.push(VariantDifference::Analyzer);
        }
        if realization.token_schema != anchor.token_schema {
            differences.push(VariantDifference::TokenSchema);
        }
        if realization.trust != anchor.trust {
            differences.push(VariantDifference::Trust);
        }
    }
    VariantRecord {
        path: realization.path.display().to_string(),
        identity_scheme: scheme_name(realization.scheme),
        format_generation: realization.format_generation,
        source_identity: encode_digest(realization.source_identity.as_bytes()),
        analysis_identity: encode_digest(realization.analysis_identity.as_bytes()),
        reader_identity: encode_digest(realization.reader.as_bytes()),
        admission_identity: encode_digest(realization.admission.as_bytes()),
        token_schema_identity: encode_digest(realization.token_schema.as_bytes()),
        dictionary_identity: encode_digest(realization.dictionary.as_bytes()),
        trust: realization.trust.address_component().to_owned(),
        analyzer: VariantAnalyzer::from_record(&realization.analyzer),
        document_count: realization.document_count,
        sentence_count: realization.sentence_count,
        token_count: realization.token_count,
        same_source,
        differences,
    }
}

fn scheme_name(scheme: lingrep_corpus::IdentityScheme) -> String {
    scheme.address_component().to_owned()
}

/// `has_reference` distinguishes the two human renderings. Without it every
/// realization in a source-argument run reads as "reference", because each one
/// correctly reports no differences — there is nothing to differ *from*. The
/// machine stream needs no such flag: it carries the reference in the run
/// record.
fn publish<W: Write>(
    writer: &mut W,
    output: OutputMode,
    has_reference: bool,
    event: &VariantsEnvelope,
) -> Result<(), CliError> {
    if output == OutputMode::Jsonl {
        return write_jsonl(writer, event).map_err(|error| CliError::Corpus(error.to_string()));
    }
    match &event.event {
        VariantsEvent::Run(run) => match &run.reference {
            VariantsReference::Artifact { path, .. } => writeln!(
                writer,
                "variants of {path}\n  (differences are relative to it)"
            ),
            VariantsReference::Source { root } => {
                writeln!(writer, "realizations beside {root}")
            }
        },
        VariantsEvent::Variant(record) => {
            let summary = if !has_reference {
                // No reference: group by recorded source content instead, so
                // realizations of one corpus are visibly a set.
                format!(
                    "source {}",
                    record.source_identity.chars().take(12).collect::<String>()
                )
            } else if record.differences.is_empty() {
                "reference".to_owned()
            } else {
                record
                    .differences
                    .iter()
                    .map(human_difference)
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            writeln!(
                writer,
                "  {}\n    scheme {}, {} document(s), {} sentence(s) — {summary}",
                record.path, record.identity_scheme, record.document_count, record.sentence_count
            )
        }
        // Human output caps detail, never the count: a run that showed three
        // of forty says forty.
        VariantsEvent::Complete(complete) => write_complete(writer, complete),
    }
    .map_err(CliError::Io)
}

fn write_complete<W: Write>(writer: &mut W, complete: &VariantsComplete) -> io::Result<()> {
    let mut prunable = (0_u64, 0_u64);
    for diagnostic in &complete.diagnostics {
        let size = diagnostic.bytes.map_or_else(
            || "size unknown".to_owned(),
            |bytes| format!("{bytes} bytes"),
        );
        if diagnostic.pruned {
            writeln!(writer, "  pruned {} ({size})", diagnostic.path)?;
            continue;
        }
        let mark = if diagnostic.prunable {
            prunable.0 += 1;
            prunable.1 += diagnostic.bytes.unwrap_or_default();
            "unreadable, prunable"
        } else {
            "unreadable"
        };
        writeln!(
            writer,
            "  {}\n    {mark} ({size}) — {}",
            diagnostic.path, diagnostic.message
        )?;
    }
    write!(
        writer,
        "complete: {} realization(s), {} reported, {} unreadable",
        complete.realizations, complete.reported, complete.unreadable
    )?;
    if complete.pruned > 0 {
        write!(
            writer,
            ", {} pruned, {} bytes freed",
            complete.pruned, complete.freed_bytes
        )?;
    }
    writeln!(writer)?;
    if prunable.0 > 0 {
        writeln!(
            writer,
            "--prune deletes the {} prunable artifact(s), {} bytes",
            prunable.0, prunable.1
        )?;
    }
    Ok(())
}

const fn human_difference(difference: &VariantDifference) -> &'static str {
    match difference {
        VariantDifference::Source => "source content",
        VariantDifference::Reader => "reader",
        VariantDifference::Admission => "admission policy",
        VariantDifference::Analyzer => "analyzer",
        VariantDifference::TokenSchema => "token schema",
        VariantDifference::Trust => "trust boundary",
    }
}
