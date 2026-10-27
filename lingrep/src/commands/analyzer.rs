use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cli::{AnalyzerPrepareArgs, AnalyzerProvider, ExecutionCliConfig};
use crate::error::{CliError, FailureCode};
use crate::presentation::OutputMode;

use super::CommandOutcome;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Request {
    release: String,
    provider: String,
    language: Option<String>,
    packages: Vec<String>,
    probe_text: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    request: Request,
    wheels: BTreeMap<String, String>,
    semantic_identity: [u8; 32],
}

fn failed(message: impl Into<String>) -> CliError {
    CliError::CommandFailure {
        code: FailureCode::Io,
        message: message.into(),
    }
}

fn checked(command: &mut Command) -> Result<(), CliError> {
    let status = command.stdout(Stdio::null()).status().map_err(|error| {
        failed(format!(
            "cannot run {}: {error}",
            command.get_program().to_string_lossy()
        ))
    })?;
    if !status.success() {
        return Err(failed(format!(
            "{} failed ({status})",
            command.get_program().to_string_lossy()
        )));
    }
    Ok(())
}

fn uv(root: &Path) -> Command {
    let mut command = Command::new("uv");
    command
        .arg("--no-config")
        .env("UV_PYTHON_INSTALL_DIR", root.join("python"));
    command
}

fn wheel_name(package: &str) -> String {
    format!("{package}-{}-py3-none-any.whl", env!("CARGO_PKG_VERSION"))
}

fn verify_wheel(path: &Path, expected: &str) -> Result<(), CliError> {
    let bytes = fs::read(path)?;
    let actual = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != expected {
        return Err(failed(format!("checksum mismatch for {}", path.display())));
    }
    Ok(())
}

fn read_asset(name: &str, directory: Option<&Path>) -> Result<Vec<u8>, CliError> {
    const LIMIT: u64 = 16 * 1024 * 1024;
    let mut reader: Box<dyn Read> = if let Some(directory) = directory {
        Box::new(fs::File::open(directory.join(name))?)
    } else {
        let url = format!(
            "https://github.com/borh/lingrep/releases/download/v{}/{name}",
            env!("CARGO_PKG_VERSION")
        );
        let response = reqwest::blocking::Client::builder()
            .https_only(true)
            .timeout(std::time::Duration::from_mins(2))
            .build()
            .and_then(|client| client.get(&url).send())
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| failed(format!("cannot download {url}: {error}")))?;
        Box::new(response)
    };
    let mut bytes = Vec::new();
    reader.by_ref().take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(failed(format!("release asset {name} exceeds 16 MiB")));
    }
    Ok(bytes)
}

fn acquire_wheels(
    root: &Path,
    args: &AnalyzerPrepareArgs,
) -> Result<BTreeMap<String, String>, CliError> {
    let sums = String::from_utf8(read_asset("SHA256SUMS", args.wheels.as_deref())?)
        .map_err(|error| failed(format!("invalid SHA256SUMS: {error}")))?;
    let directory = root.join("wheels");
    fs::create_dir(&directory)?;
    let adapter = match args.provider {
        AnalyzerProvider::Ginza => "lingrep_spacy_adapter",
        AnalyzerProvider::Stanza => "lingrep_stanza_adapter",
    };
    let mut result = BTreeMap::new();
    for package in ["lingrep_adapter_protocol", adapter] {
        let name = wheel_name(package);
        let records = sums
            .lines()
            .filter_map(|line| {
                let (digest, path) = line.split_once(char::is_whitespace)?;
                (path.trim().trim_start_matches("./") == name).then_some(digest)
            })
            .collect::<Vec<_>>();
        if records.len() != 1
            || records[0].len() != 64
            || !records[0].bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(failed(format!(
                "SHA256SUMS must contain one SHA-256 for {name}"
            )));
        }
        let path = directory.join(&name);
        fs::write(&path, read_asset(&name, args.wheels.as_deref())?)?;
        verify_wheel(&path, records[0])?;
        result.insert(name, records[0].to_owned());
    }
    Ok(result)
}

fn setup_command(root: &Path, args: &AnalyzerPrepareArgs, request: &Request) -> Command {
    let kind = match args.provider {
        AnalyzerProvider::Ginza => "spacy",
        AnalyzerProvider::Stanza => "stanza",
    };
    let mut command = Command::new(root.join("runtime/bin/python"));
    command
        .args([
            "-I",
            "-B",
            "-c",
            &format!("from lingrep_{kind}_adapter.setup import main; raise SystemExit(main())"),
        ])
        .arg("--output")
        .arg(root.join("bundle"))
        .args(["--probe-text", &request.probe_text]);
    match args.provider {
        AnalyzerProvider::Ginza => {
            command.args(["--model", "ja_ginza"]);
        }
        AnalyzerProvider::Stanza => {
            command.args(["--language", args.language.as_deref().unwrap_or_default()]);
            for package in &request.packages {
                command.args(["--package", package]);
            }
        }
    }
    command
}

fn provision(
    root: &Path,
    args: &AnalyzerPrepareArgs,
    request: Request,
    config: &ExecutionCliConfig,
) -> Result<(), CliError> {
    fs::write(root.join(".gitignore"), "*\n")?;
    let wheels = acquire_wheels(root, args)?;
    checked(uv(root).args(["python", "install", "--no-bin", "3.14"]))?;
    checked(
        uv(root)
            .args(["venv", "--managed-python", "--python", "3.14"])
            .arg(root.join("runtime")),
    )?;
    let mut install = uv(root);
    install
        .args([
            "pip",
            "install",
            "--link-mode",
            "copy",
            "--exclude-newer",
            "P3D",
            "--python",
        ])
        .arg(root.join("runtime/bin/python"))
        .arg(
            root.join("wheels")
                .join(wheel_name("lingrep_adapter_protocol")),
        );
    let (adapter, extras) = match args.provider {
        AnalyzerProvider::Ginza => ("lingrep_spacy_adapter", "spacy,ginza"),
        AnalyzerProvider::Stanza => ("lingrep_stanza_adapter", "cpu"),
    };
    install.arg(format!(
        "{}[{extras}]",
        root.join("wheels").join(wheel_name(adapter)).display()
    ));
    if args.provider == AnalyzerProvider::Stanza {
        install.args([
            "--index",
            "https://download.pytorch.org/whl/cpu",
            "--index-strategy",
            "unsafe-best-match",
        ]);
    }
    checked(&mut install)?;
    let python = root.join("runtime/bin/python").canonicalize()?;
    if !python.starts_with(root.join("python")) {
        return Err(failed("prepared Python is outside the analyzer directory"));
    }
    let downloads = tempfile::Builder::new()
        .prefix("downloads-")
        .tempdir_in(root)?;
    checked(
        setup_command(root, args, &request)
            .env("HF_HOME", downloads.path())
            .env("HF_HUB_CACHE", downloads.path().join("hub"))
            .env("HF_XET_CACHE", downloads.path().join("xet")),
    )?;
    downloads.close()?;
    let mut file: toml::Value = toml::from_str(&fs::read_to_string(
        root.join("bundle/analyzer.toml"),
    )?)
    .map_err(|error| failed(format!("invalid generated analyzer configuration: {error}")))?;
    for key in ["entrypoint", "model"] {
        let path = file[key]
            .as_str()
            .ok_or_else(|| failed(format!("generated analyzer has no {key}")))?;
        file[key] = toml::Value::String(format!("bundle/{path}"));
    }
    fs::write(
        root.join("analyzer.toml"),
        toml::to_string(&file).map_err(|error| failed(error.to_string()))?,
    )?;
    let semantic_identity = verify_config(root, config)?;
    fs::write(
        root.join("prepare.json"),
        serde_json::to_vec(&Prepared {
            request,
            wheels,
            semantic_identity,
        })
        .map_err(|error| failed(error.to_string()))?,
    )?;
    Ok(())
}

pub(crate) fn run(
    args: &AnalyzerPrepareArgs,
    config: &ExecutionCliConfig,
    mode: OutputMode,
) -> Result<CommandOutcome, CliError> {
    if mode != OutputMode::Human {
        return Err(CliError::UnsupportedOutput {
            command: "analyzer prepare",
            output: "json",
        });
    }
    let mut packages = args.packages.clone();
    packages.sort();
    let mut processors = BTreeSet::new();
    for value in &packages {
        let (processor, package) = value
            .split_once('=')
            .ok_or_else(|| CliError::InvalidArgs("--package requires PROCESSOR=PACKAGE".into()))?;
        if processor.is_empty() || package.is_empty() || !processors.insert(processor) {
            return Err(CliError::InvalidArgs(
                "--package requires unique processors and nonempty packages".into(),
            ));
        }
    }
    if args.provider == AnalyzerProvider::Ginza && (args.language.is_some() || !packages.is_empty())
    {
        return Err(CliError::InvalidArgs(
            "GiNZA has a fixed Japanese model; --language and --package apply to stanza".into(),
        ));
    }
    let request = Request {
        release: env!("CARGO_PKG_VERSION").to_owned(),
        provider: args
            .provider
            .to_possible_value()
            .expect("provider value")
            .get_name()
            .to_owned(),
        language: args.language.clone(),
        packages,
        probe_text: args
            .probe_text
            .clone()
            .unwrap_or_else(|| match args.provider {
                AnalyzerProvider::Ginza => "猫が走る。".into(),
                AnalyzerProvider::Stanza => "Test.".into(),
            }),
    };
    let output = std::path::absolute(&args.output)?;
    let parent = output
        .parent()
        .ok_or_else(|| CliError::InvalidArgs("analyzer output has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let output = parent.canonicalize()?.join(
        output
            .file_name()
            .ok_or_else(|| CliError::InvalidArgs("analyzer output has no name".into()))?,
    );
    match fs::create_dir(&output) {
        Ok(()) => {
            if let Err(error) = provision(&output, args, request, config) {
                if let Err(cleanup) = fs::remove_dir_all(&output) {
                    return Err(failed(format!(
                        "{error}; cannot remove incomplete analyzer {}: {cleanup}",
                        output.display()
                    )));
                }
                return Err(error);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&output)?;
            if !metadata.is_dir() || metadata.is_symlink() {
                return Err(CliError::InvalidArgs(
                    "analyzer output must be a regular directory".into(),
                ));
            }
            let ready = fs::read(output.join("prepare.json")).map_err(|_| {
                CliError::InvalidArgs(format!(
                    "incomplete analyzer directory {}; choose a new --output",
                    output.display()
                ))
            })?;
            let prepared: Prepared = serde_json::from_slice(&ready)
                .map_err(|error| failed(format!("invalid preparation receipt: {error}")))?;
            if prepared.request != request {
                return Err(CliError::InvalidArgs(
                    "output belongs to a different preparation; choose a new --output".into(),
                ));
            }
            let adapter = match args.provider {
                AnalyzerProvider::Ginza => "lingrep_spacy_adapter",
                AnalyzerProvider::Stanza => "lingrep_stanza_adapter",
            };
            let expected = [wheel_name("lingrep_adapter_protocol"), wheel_name(adapter)]
                .into_iter()
                .collect::<BTreeSet<_>>();
            if prepared.wheels.keys().cloned().collect::<BTreeSet<_>>() != expected {
                return Err(failed("preparation receipt has an invalid wheel set"));
            }
            for (name, digest) in prepared.wheels {
                verify_wheel(&output.join("wheels").join(name), &digest)?;
            }
            checked(&mut setup_command(&output, args, &request))?;
            if verify_config(&output, config)? != prepared.semantic_identity {
                return Err(failed(
                    "prepared analyzer files changed; choose a new --output",
                ));
            }
        }
        Err(error) => return Err(error.into()),
    }
    let mut stderr = std::io::stderr().lock();
    writeln!(stderr, "Analyzer files: {}", output.display())?;
    writeln!(stderr, "Delete that directory to remove the analyzer.")?;
    writeln!(
        stderr,
        "Shared downloads: `uv cache dir` locates them; `uv cache clean` clears them."
    )?;
    writeln!(
        std::io::stdout().lock(),
        "{}",
        output.join("analyzer.toml").display()
    )?;
    Ok(CommandOutcome::Success)
}

fn verify_config(root: &Path, config: &ExecutionCliConfig) -> Result<[u8; 32], CliError> {
    #[cfg(unix)]
    let verified = crate::external_analyzer::ExternalPlan::load(
        &root.join("analyzer.toml"),
        config,
        &lingrep_core::execution::CancellationFlag::default(),
    )?;
    #[cfg(unix)]
    return Ok(*verified.descriptor.semantic_identity().as_bytes());
    #[cfg(not(unix))]
    Err(failed("external analyzer preparation requires Unix"))
}
