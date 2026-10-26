use crate::cli::ExecutionCliConfig;
use crate::error::CliError;
use lingrep_core::analysis::{
    AnalyzerDescriptor, AnalyzerFamily, ExternalComponent, external_component_closure_digest,
};
use lingrep_core::execution::{
    ExternalAdapterCancellation, ExternalAdapterCommand, ExternalAnalyzerSession,
    VerifiedAdapterManifest,
};
use lingrep_core::token_match::{AttributeKind, SchemaAttribute, TokenSchema};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnalyzerFile {
    version: u32,
    kind: String,
    python: PathBuf,
    entrypoint: PathBuf,
    model: Option<PathBuf>,
    #[serde(default = "cpu")]
    backend: String,
    #[serde(default)]
    arguments: Vec<String>,
    language: Option<String>,
    #[serde(default)]
    schema: Vec<Attribute>,
    #[serde(default)]
    implementation: Vec<PathBuf>,
    #[serde(default)]
    resources: Vec<PathBuf>,
}
fn cpu() -> String {
    "cpu".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attribute {
    key: String,
    kind: String,
}

#[derive(Clone)]
pub(crate) struct ExternalPlan {
    pub descriptor: AnalyzerDescriptor,
    pub schema: TokenSchema,
    command: ExternalAdapterCommand,
    manifest: VerifiedAdapterManifest,
    limits: lingrep_core::execution::ExecutionLimits,
    cancellation: lingrep_core::execution::CancellationFlag,
    diagnostic_format: crate::cli::DiagnosticFormat,
}

fn invalid(error: impl std::fmt::Display) -> CliError {
    CliError::InvalidArgs(format!("analyzer configuration: {error}"))
}
fn digest_file(path: &Path) -> Result<[u8; 32], CliError> {
    let mut reader = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    std::io::copy(&mut reader, &mut hasher)?;
    Ok(*hasher.finalize().as_bytes())
}
fn entries(
    root: &Path,
    path: &Path,
    output: &mut Vec<(PathBuf, [u8; 32])>,
) -> Result<(), CliError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.is_symlink() {
        return Err(invalid(format!(
            "symlink in analyzer resources: {}",
            path.display()
        )));
    }
    if metadata.is_file() {
        output.push((
            path.strip_prefix(root).map_err(invalid)?.to_owned(),
            digest_file(path)?,
        ));
    } else if metadata.is_dir() {
        let mut paths = std::fs::read_dir(path)?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        for path in paths {
            entries(root, &path, output)?;
        }
    } else {
        return Err(invalid("analyzer resources must be regular files"));
    }
    Ok(())
}
fn hex(value: &str) -> Result<[u8; 32], CliError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("profile digest must contain 64 hexadecimal digits"));
    }
    let mut result = [0; 32];
    for (byte, pair) in result.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        *byte =
            u8::from_str_radix(std::str::from_utf8(pair).map_err(invalid)?, 16).map_err(invalid)?;
    }
    Ok(result)
}

impl ExternalPlan {
    pub(crate) fn load(
        path: &Path,
        config: &ExecutionCliConfig,
        cancellation: &lingrep_core::execution::CancellationFlag,
    ) -> Result<Self, CliError> {
        let path = path
            .canonicalize()
            .map_err(|error| invalid(format!("cannot open {}: {error}", path.display())))?;
        let file: AnalyzerFile = toml::from_str(&std::fs::read_to_string(&path)?)
            .map_err(|error| invalid(format!("{}: {error}", path.display())))?;
        if file.version != 1 {
            return Err(invalid("unsupported analyzer file version; expected 1"));
        }
        let base = path
            .parent()
            .ok_or_else(|| invalid("configuration has no parent"))?;
        let resolve = |path: &Path| -> Result<PathBuf, CliError> {
            let resolved = base.join(path);
            resolved
                .canonicalize()
                .map_err(|error| invalid(format!("cannot open {}: {error}", resolved.display())))
        };
        let entrypoint = resolve(&file.entrypoint)?;
        let python = base.join(&file.python);
        let python = python
            .parent()
            .ok_or_else(|| invalid("python path has no parent"))?
            .canonicalize()?
            .join(
                python
                    .file_name()
                    .ok_or_else(|| invalid("python path has no file name"))?,
            );
        if !python.is_file() {
            return Err(invalid(format!(
                "Python interpreter is not a file: {}",
                python.display()
            )));
        }
        let mut manifest_root = entrypoint
            .parent()
            .ok_or_else(|| invalid("entrypoint has no parent"))?
            .to_owned();
        let mut related = file
            .implementation
            .iter()
            .chain(&file.resources)
            .map(|path| resolve(path))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(model) = &file.model {
            related.push(resolve(model)?);
        }
        let protocol_project = matches!(file.kind.as_str(), "spacy" | "stanza")
            .then(|| {
                entrypoint
                    .parent()
                    .and_then(Path::parent)
                    .and_then(Path::parent)
                    .map(|parent| parent.join("adapter-protocol"))
                    .ok_or_else(|| invalid("adapter project has no protocol parent"))
            })
            .transpose()?;
        if let Some(protocol) = &protocol_project {
            let metadata = std::fs::symlink_metadata(protocol)?;
            if !metadata.is_dir() || metadata.is_symlink() {
                return Err(invalid(
                    "adapter protocol project must be a regular directory",
                ));
            }
            related.push(protocol.clone());
        }
        if matches!(file.kind.as_str(), "spacy" | "stanza") {
            related.push(
                entrypoint
                    .parent()
                    .and_then(Path::parent)
                    .ok_or_else(|| invalid("adapter project has no parent"))?
                    .to_owned(),
            );
        }
        for path in related {
            while !path.starts_with(&manifest_root) {
                if !manifest_root.pop() {
                    return Err(invalid("analyzer paths have no common root"));
                }
            }
        }
        let root = manifest_root.as_path();
        let relative = entrypoint.strip_prefix(root).map_err(invalid)?;
        let mut implementation = Vec::new();
        let mut resources = Vec::new();
        entries(root, &entrypoint, &mut implementation)?;
        for path in &file.implementation {
            entries(root, &resolve(path)?, &mut implementation)?;
        }
        for path in &file.resources {
            entries(root, &resolve(path)?, &mut resources)?;
        }
        let (command, schema, language, boundary, components, output_digest, family) = if file.kind
            == "spacy"
            || file.kind == "stanza"
        {
            if !file.arguments.is_empty()
                || file.language.is_some()
                || !file.schema.is_empty()
                || !file.implementation.is_empty()
                || !file.resources.is_empty()
            {
                return Err(invalid(
                    "spaCy/Stanza schema, language and file closure come from the sealed model and adapter; generic protocol fields are invalid",
                ));
            }
            let model = resolve(
                file.model
                    .as_deref()
                    .ok_or_else(|| invalid("model is required for spaCy/Stanza"))?,
            )?;
            let project = entrypoint.parent().and_then(Path::parent).ok_or_else(|| {
                invalid("adapter entrypoint must be in its project bin directory")
            })?;
            implementation.clear();
            entries(root, &project.join("bin"), &mut implementation)?;
            entries(root, &project.join("pyproject.toml"), &mut implementation)?;
            entries(root, &project.join("uv.lock"), &mut implementation)?;
            entries(root, &project.join("src"), &mut implementation)?;
            let protocol = protocol_project
                .as_ref()
                .expect("known adapters bind protocol code");
            entries(root, &protocol.join("pyproject.toml"), &mut implementation)?;
            entries(root, &protocol.join("src"), &mut implementation)?;
            entries(root, &model, &mut resources)?;
            let is_spacy = file.kind == "spacy";
            let profile_name = if is_spacy {
                "lingrep-spacy-model.json"
            } else {
                "lingrep-stanza-model.json"
            };
            let profile: serde_json::Value =
                serde_json::from_slice(&std::fs::read(model.join(profile_name))?)
                    .map_err(invalid)?;
            let profile_key = match config.profile {
                lingrep_core::analysis::ExtractionProfile::Morphology => "morphology",
                lingrep_core::analysis::ExtractionProfile::Tokenize => "tokenize",
                lingrep_core::analysis::ExtractionProfile::Syntax => {
                    return Err(invalid("syntax profile is not supported"));
                }
            };
            let segment_key = match config.segmentation {
                lingrep_core::analysis::SegmentationMode::Input => "input",
                lingrep_core::analysis::SegmentationMode::Analyzer => "analyzer",
            };
            let key = format!("{profile_key}/{segment_key}");
            let selection = &profile["selections"][&key];
            let schema: Vec<Attribute> = serde_json::from_value(selection["schema"].clone())
                .map_err(|_| invalid(format!("model does not provide {key}")))?;
            let language = profile["language"]
                .as_str()
                .ok_or_else(|| invalid("model profile has no language"))?
                .to_owned();
            let boundary = selection["boundary_policy"]
                .as_str()
                .ok_or_else(|| invalid("model profile has no boundary policy"))?
                .to_owned();
            let digest = hex(profile["selection_digests"][&key]
                .as_str()
                .ok_or_else(|| invalid("model profile has no selection digest"))?)?;
            let model_relative = model.strip_prefix(root).map_err(invalid)?;
            let command = if is_spacy {
                ExternalAdapterCommand::spacy(python, relative, model_relative, &file.backend, 32)
            } else {
                ExternalAdapterCommand::stanza(
                    python,
                    relative,
                    model_relative,
                    &language,
                    &file.backend,
                    1,
                )
            }
            .map_err(invalid)?;
            (
                command,
                schema,
                language,
                boundary,
                vec![
                    ExternalComponent::new(
                        if is_spacy {
                            "spacy-profile"
                        } else {
                            "stanza-profile"
                        },
                        digest,
                    )
                    .map_err(invalid)?,
                ],
                digest,
                if is_spacy {
                    AnalyzerFamily::External
                } else {
                    AnalyzerFamily::Stanza
                },
            )
        } else if file.kind == "protocol" {
            if file.backend != "cpu" {
                return Err(invalid(
                    "backend is configured by the generic protocol adapter's arguments",
                ));
            }
            if file.model.is_some() {
                return Err(invalid("model is supported only for spaCy/Stanza adapters"));
            }
            let command = ExternalAdapterCommand::interpreted(python, relative, file.arguments)
                .map_err(invalid)?;
            let boundary = match config.segmentation {
                lingrep_core::analysis::SegmentationMode::Input => "input-v1",
                lingrep_core::analysis::SegmentationMode::Analyzer => {
                    return Err(invalid(
                        "generic protocol adapters require input segmentation",
                    ));
                }
            };
            (
                command,
                file.schema,
                file.language
                    .ok_or_else(|| invalid("protocol adapter requires language"))?,
                boundary.to_owned(),
                vec![],
                [0; 32],
                AnalyzerFamily::External,
            )
        } else {
            return Err(invalid("kind must be spacy, stanza or protocol"));
        };
        implementation.sort_by(|a, b| a.0.cmp(&b.0));
        resources.sort_by(|a, b| a.0.cmp(&b.0));
        let manifest =
            VerifiedAdapterManifest::new(root, implementation, resources, &command, components)
                .map_err(invalid)?;
        let schema = TokenSchema::new(
            schema
                .into_iter()
                .map(|attribute| {
                    let kind = match attribute.kind.as_str() {
                        "scalar" => AttributeKind::Scalar,
                        "string_set" => AttributeKind::StringSet,
                        _ => return Err(invalid("schema kind must be scalar or string_set")),
                    };
                    SchemaAttribute::new(attribute.key, kind).map_err(invalid)
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
        .map_err(invalid)?;
        let descriptor = AnalyzerDescriptor::new(
            family,
            config.profile,
            config.segmentation,
            config.normalization,
            language,
            boundary,
            manifest.implementation_digest(),
            manifest.resource_manifest_digest(),
            [0; 32],
            [0; 32],
            external_component_closure_digest(manifest.components()),
            output_digest,
            schema.digest(),
        )
        .map_err(invalid)?;
        Ok(Self {
            descriptor,
            schema,
            command,
            manifest,
            limits: config.limits,
            cancellation: cancellation.clone(),
            diagnostic_format: config.diagnostic_format,
        })
    }
    pub(crate) fn session(&self) -> ExternalAnalyzerSession {
        let format = self.diagnostic_format;
        ExternalAnalyzerSession::new(
            self.command.clone(),
            self.manifest.clone(),
            self.descriptor.clone(),
            self.schema.clone(),
            self.limits,
            Arc::new(move |bytes| {
                use crate::protocol::diagnostic::*;
                use std::io::Write;
                let raw = String::from_utf8_lossy(bytes);
                let message = lingrep_cli_common::sanitize_terminal(&raw);
                let mut stderr = std::io::stderr().lock();
                if format == crate::cli::DiagnosticFormat::Json {
                    let diagnostic =
                        DiagnosticEnvelope::new(DiagnosticEvent::Diagnostic(DiagnosticData {
                            run_id: None,
                            observed_input_digest: None,
                            stage: DiagnosticStage::Input,
                            severity: DiagnosticSeverity::Warning,
                            code: "external_analyzer".into(),
                            message: message.into_owned(),
                            suggestions: vec![],
                            usage: None,
                            retry: None,
                            location: None,
                            related_locations: vec![],
                        }));
                    crate::protocol::write_jsonl(&mut stderr, &diagnostic)
                        .map_err(|e| e.to_string())
                } else {
                    writeln!(stderr, "warning[external_analyzer]: {message}")
                        .map_err(|e| e.to_string())
                }
            }),
            ExternalAdapterCancellation::from_flag(&self.cancellation),
        )
    }
}
