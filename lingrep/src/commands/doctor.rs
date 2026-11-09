use std::io::{self, Write};
use std::path::Path;

use lingrep_cli_common::SafeText;
use lingrep_core::PipelineError;
use lingrep_core::dictionary::dictionary_identity;
use lingrep_corpus::CorpusArtifact;

use crate::catalog_input::{CatalogInput, LoadedCatalog, load_and_compile};
use crate::cli::{DoctorArgs, ExecutionCliConfig};
use crate::direct_source::encode_digest;
use crate::error::CliError;
use crate::presentation::OutputMode;
use crate::protocol::doctor::{
    DoctorData, DoctorEnvelope, ProbeData, ProbeEvidence, ProbeKind, UnavailableBindingAttribute,
};
use crate::protocol::write_jsonl;

use super::CommandOutcome;

pub fn run(
    args: &DoctorArgs,
    config: &ExecutionCliConfig,
    output: OutputMode,
) -> Result<CommandOutcome, CliError> {
    let mut probes = Vec::with_capacity(4);
    let mut readable_dictionary = None;
    if args.dict_path.is_some() || args.corpus.is_none() {
        let dictionary = args
            .dict_path
            .clone()
            .or_else(crate::packaged_dictionaries::fallback_dictionary_path);
        probes.push(match dictionary {
            Some(path) => {
                let probe = probe_dictionary(&path);
                if probe.ok {
                    readable_dictionary = Some(path);
                }
                probe
            }
            None => ProbeData {
                name: "dictionary".into(),
                ok: false,
                message: format!(
                    "no dictionary: {}",
                    crate::packaged_dictionaries::MISSING_DICTIONARY_HINT
                ),
                kind: Some(ProbeKind::Dictionary),
                target: None,
                code: Some("dictionary".into()),
                evidence: None,
            },
        });
    }
    let catalog = args.patterns.as_deref().and_then(|path| {
        let (probe, catalog) = probe_catalog(path);
        probes.push(probe);
        catalog
    });
    if let (Some(catalog), Some(dictionary)) = (&catalog, &readable_dictionary) {
        crate::commands::query_support::write_binding_notice(
            &catalog.output.matcher,
            &dictionary.display().to_string(),
            config,
        )?;
    }
    let corpus = args.corpus.as_deref().and_then(|path| {
        let (probe, artifact) = probe_corpus(path);
        probes.push(probe);
        artifact.map(|artifact| (path, artifact))
    });
    if let (Some(catalog), Some((corpus_path, artifact))) = (&catalog, &corpus) {
        probes.push(probe_binding(catalog, artifact, corpus_path));
    }

    let ready = probes.iter().all(|probe| probe.ok);
    let data = DoctorData { ready, probes };
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    match output {
        OutputMode::Jsonl => write_jsonl(&mut writer, &DoctorEnvelope::new(data))
            .map_err(|error| CliError::Io(io::Error::other(error)))?,
        OutputMode::Human => publish_human(&mut writer, &data)?,
        OutputMode::Tsv => {
            return Err(CliError::UnsupportedOutput {
                command: "doctor",
                output: "tsv",
            });
        }
    }
    Ok(if ready {
        CommandOutcome::Success
    } else {
        CommandOutcome::Failure
    })
}

fn probe_dictionary(path: &Path) -> ProbeData {
    let target = path.display().to_string();
    match dictionary_identity(&target) {
        Ok(digest) => ProbeData {
            name: "dictionary".into(),
            ok: true,
            message: "dictionary readable; identity available".into(),
            kind: Some(ProbeKind::Dictionary),
            target: Some(target),
            code: None,
            evidence: Some(ProbeEvidence::Dictionary {
                blake3: blake3::Hash::from(digest).to_hex().to_string(),
            }),
        },
        Err(error) => failed_probe(
            ProbeKind::Dictionary,
            target,
            CliError::Dictionary(match error {
                PipelineError::Dictionary(detail) => detail,
                other => other.to_string(),
            }),
        ),
    }
}

fn probe_catalog(path: &Path) -> (ProbeData, Option<LoadedCatalog>) {
    let target = path.display().to_string();
    match load_and_compile(&CatalogInput::File(path.to_path_buf())) {
        Ok(catalog) => (
            ProbeData {
                name: "catalog".into(),
                ok: true,
                message: "catalog compiled".into(),
                kind: Some(ProbeKind::Catalog),
                target: Some(target),
                code: None,
                evidence: Some(ProbeEvidence::Catalog {
                    fingerprint: catalog.fingerprint.clone(),
                    definition_count: (catalog.output.matcher.patterns().len()
                        + catalog.output.matcher.lexicons().len())
                        as u64,
                    warning_count: catalog.output.diagnostics.len() as u64,
                }),
            },
            Some(catalog),
        ),
        Err(error) => (failed_probe(ProbeKind::Catalog, target, error), None),
    }
}

fn probe_corpus(path: &Path) -> (ProbeData, Option<CorpusArtifact>) {
    let target = path.display().to_string();
    let admitted = CorpusArtifact::open(path).and_then(|artifact| {
        let inputs = artifact.analysis_inputs()?;
        let evidence = ProbeEvidence::Corpus {
            source_identity: encode_digest(artifact.source_identity().as_bytes()),
            analysis_identity: encode_digest(artifact.analysis_identity().as_bytes()),
            token_schema_identity: encode_digest(inputs.token_schema().as_bytes()),
            document_count: artifact.document_count(),
            sentence_count: artifact.sentence_count(),
            token_count: artifact.token_count(),
        };
        Ok((artifact, evidence))
    });
    match admitted {
        Ok((artifact, evidence)) => (
            ProbeData {
                name: "corpus".into(),
                ok: true,
                message: "artifact admitted without full content verification".into(),
                kind: Some(ProbeKind::Corpus),
                target: Some(target),
                code: None,
                evidence: Some(evidence),
            },
            Some(artifact),
        ),
        Err(error) => (
            failed_probe(ProbeKind::Corpus, target, CliError::Artifact(error)),
            None,
        ),
    }
}

fn probe_binding(
    catalog: &LoadedCatalog,
    artifact: &CorpusArtifact,
    corpus_path: &Path,
) -> ProbeData {
    let target = format!("{} against {}", catalog.source.name, corpus_path.display());
    let schema = match artifact.token_schema() {
        Ok(schema) => schema,
        Err(error) => return failed_probe(ProbeKind::Binding, target, CliError::Artifact(error)),
    };
    let projected = lingrep_core::execution::cached_run_attributes(&catalog.output.matcher);
    match catalog
        .output
        .matcher
        .bind_with_projection(&schema, projected)
    {
        Ok(bound) => {
            let unavailable: Vec<UnavailableBindingAttribute> = bound
                .unavailable_attributes()
                .iter()
                .map(|attribute| UnavailableBindingAttribute {
                    attribute: attribute.as_str().to_owned(),
                    definitions: catalog
                        .output
                        .analysis
                        .references_to(attribute)
                        .map(|site| site.definition.to_string())
                        .collect(),
                })
                .collect();
            let message = if unavailable.is_empty() {
                "catalog binds; every referenced attribute is supplied".to_owned()
            } else {
                format!(
                    "catalog binds; {} referenced attribute(s) are never supplied \
                             by this corpus",
                    unavailable.len()
                )
            };
            ProbeData {
                name: "binding".into(),
                ok: true,
                message,
                kind: Some(ProbeKind::Binding),
                target: Some(target),
                code: None,
                evidence: Some(ProbeEvidence::Binding {
                    unavailable,
                    required_missing: Vec::new(),
                }),
            }
        }
        Err(error) => ProbeData {
            name: "binding".into(),
            ok: false,
            message: error.to_string(),
            kind: Some(ProbeKind::Binding),
            target: Some(target),
            code: Some("binding".into()),
            evidence: Some(ProbeEvidence::Binding {
                unavailable: Vec::new(),
                required_missing: error
                    .missing_keys()
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
            }),
        },
    }
}

fn failed_probe(kind: ProbeKind, target: String, error: CliError) -> ProbeData {
    ProbeData {
        name: probe_name(kind).into(),
        ok: false,
        message: error.to_string(),
        kind: Some(kind),
        target: Some(target),
        code: Some(error.code().into()),
        evidence: None,
    }
}

fn probe_name(kind: ProbeKind) -> &'static str {
    match kind {
        ProbeKind::Dictionary => "dictionary",
        ProbeKind::Catalog => "catalog",
        ProbeKind::Corpus => "corpus",
        ProbeKind::Binding => "binding",
    }
}

fn publish_human(writer: &mut impl Write, data: &DoctorData) -> Result<(), CliError> {
    let total = data.probes.len();
    let failed = data.probes.iter().filter(|probe| !probe.ok).count();
    let checks = if total == 1 { "check" } else { "checks" };
    if failed == 0 {
        writeln!(writer, "ling doctor: {total} {checks} passed")
    } else {
        writeln!(writer, "ling doctor: {failed} of {total} {checks} failed")
    }
    .map_err(CliError::Io)?;
    for probe in &data.probes {
        let target = probe
            .target
            .as_deref()
            .map(|value| format!(" [{}]", SafeText::from_untrusted(value)))
            .unwrap_or_default();
        let warning = probe.ok.then(|| human_warning(probe)).flatten();
        let status = match (&warning, probe.ok) {
            (_, false) => "FAIL",
            (Some(_), true) => "warning",
            (None, true) => "ok",
        };
        writeln!(
            writer,
            "  {status} {}{}: {}",
            SafeText::from_untrusted(&probe.name),
            target,
            SafeText::from_untrusted(warning.as_deref().unwrap_or(&probe.message))
        )
        .map_err(CliError::Io)?;
    }
    Ok(())
}

const HUMAN_LIST_LIMIT: usize = 3;

fn human_warning(probe: &ProbeData) -> Option<String> {
    match probe.evidence.as_ref()? {
        ProbeEvidence::Catalog { warning_count, .. } if *warning_count > 0 => Some(format!(
            "catalog compiled with {warning_count} warning{}; run `ling check` for details",
            if *warning_count == 1 { "" } else { "s" }
        )),
        ProbeEvidence::Binding { unavailable, .. } if !unavailable.is_empty() => {
            let attributes = unavailable
                .iter()
                .take(HUMAN_LIST_LIMIT)
                .map(|attribute| {
                    let referenced = if attribute.definitions.is_empty() {
                        "referenced by the publication policy".to_owned()
                    } else {
                        format!(
                            "referenced by {}",
                            bounded_list(&attribute.definitions, HUMAN_LIST_LIMIT)
                        )
                    };
                    format!("`{}` ({referenced})", attribute.attribute)
                })
                .collect::<Vec<_>>()
                .join(", ");
            let more = unavailable.len().saturating_sub(HUMAN_LIST_LIMIT);
            let more = if more == 0 {
                String::new()
            } else {
                let noun = if more == 1 { "attribute" } else { "attributes" };
                format!(" and {more} more {noun}; see --json")
            };
            Some(format!("never supplied by this corpus: {attributes}{more}"))
        }
        _ => None,
    }
}

fn bounded_list(items: &[String], limit: usize) -> String {
    let shown = items
        .iter()
        .take(limit)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    match items.len().saturating_sub(limit) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_warning_bounds_attribute_and_definition_lists() {
        let names = |prefix: &str, count: usize| -> Vec<String> {
            (1..=count).map(|n| format!("{prefix}{n}")).collect()
        };
        let probe = ProbeData {
            name: "binding".into(),
            ok: true,
            message: String::new(),
            kind: Some(ProbeKind::Binding),
            target: None,
            code: None,
            evidence: Some(ProbeEvidence::Binding {
                unavailable: names("a", 4)
                    .into_iter()
                    .map(|attribute| UnavailableBindingAttribute {
                        definitions: if attribute == "a2" {
                            Vec::new()
                        } else {
                            names("d", 5)
                        },
                        attribute,
                    })
                    .collect(),
                required_missing: Vec::new(),
            }),
        };
        assert_eq!(
            human_warning(&probe).unwrap(),
            "never supplied by this corpus: \
             `a1` (referenced by d1, d2, d3 and 2 more), \
             `a2` (referenced by the publication policy), \
             `a3` (referenced by d1, d2, d3 and 2 more) and 1 more attribute; see --json"
        );
    }
}
