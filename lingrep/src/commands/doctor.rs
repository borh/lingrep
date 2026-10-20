use std::io::{self, Write};
use std::path::{Path, PathBuf};

use lingrep_cli_common::SafeText;
use lingrep_core::dictionary::load_dictionary_data;
use lingrep_corpus::CorpusArtifact;

use crate::catalog_input::{CatalogInput, LoadedCatalog, load_and_compile};
use crate::cli::DoctorArgs;
use crate::direct_source::encode_digest;
use crate::error::CliError;
use crate::presentation::OutputMode;
use crate::protocol::doctor::{
    DoctorData, DoctorEnvelope, ProbeData, ProbeEvidence, ProbeKind, UnavailableBindingAttribute,
};
use crate::protocol::write_jsonl;

use super::CommandOutcome;

pub fn run(args: &DoctorArgs, output: OutputMode) -> Result<CommandOutcome, CliError> {
    let mut probes = Vec::with_capacity(3);
    let dictionary = args.dict_path.clone().or_else(|| {
        std::env::var_os("LINGREP_DICTIONARY_PATH")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    });
    probes.push(match dictionary {
        Some(path) => probe_dictionary(&path),
        None => ProbeData {
            name: "dictionary".into(),
            ok: false,
            message: "LINGREP_DICTIONARY_PATH is not set and --dict-path was not supplied".into(),
            kind: Some(ProbeKind::Dictionary),
            target: None,
            code: Some("dictionary".into()),
            evidence: None,
        },
    });
    let catalog = args.patterns.as_deref().and_then(|path| {
        let (probe, catalog) = probe_catalog(path);
        probes.push(probe);
        catalog
    });
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
    match load_dictionary_data(&target) {
        Ok(bytes) => ProbeData {
            name: "dictionary".into(),
            ok: true,
            message: "dictionary loaded".into(),
            kind: Some(ProbeKind::Dictionary),
            target: Some(target),
            code: None,
            evidence: Some(ProbeEvidence::Dictionary {
                blake3: blake3::hash(&bytes).to_hex().to_string(),
            }),
        },
        Err(error) => failed_probe(
            ProbeKind::Dictionary,
            target,
            CliError::Dictionary(error.to_string()),
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

/// Binds the catalog against the corpus's stored token schema, the same set a
/// `matches` run binds, so `doctor` predicts exactly what that run would warn
/// about or refuse.
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
    writeln!(
        writer,
        "ling readiness: {}",
        if data.ready { "ready" } else { "not ready" }
    )
    .map_err(CliError::Io)?;
    for probe in &data.probes {
        let target = probe
            .target
            .as_deref()
            .map(|value| format!(" [{}]", SafeText::from_untrusted(value)))
            .unwrap_or_default();
        writeln!(
            writer,
            "  {} {}{}: {}",
            if probe.ok { "ok" } else { "FAIL" },
            SafeText::from_untrusted(&probe.name),
            target,
            SafeText::from_untrusted(&probe.message)
        )
        .map_err(CliError::Io)?;
    }
    Ok(())
}
