use std::collections::{BTreeSet, VecDeque};
use std::ffi::OsString;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;

use super::ExecutionLimits;
use crate::analysis::{
    AnalyzerDescriptor, ExecutionProvenance, ExternalComponent, ExternalProtocolError,
    ExternalRunConfig, ExternalTranscript, ExternalValidatedBatch, ResponseRecordKind,
    SegmentationMode, TextUnit, TextUnitKind, response_record_kind,
};
use crate::token_match::TokenSchema;

const TERMINATION_GRACE: Duration = Duration::from_secs(5);

pub type DiagnosticCallback = Arc<dyn Fn(&[u8]) -> Result<(), String> + Send + Sync + 'static>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalAdapterCommand {
    interpreter: Option<PathBuf>,
    entrypoint: PathBuf,
    arguments: Box<[OsString]>,
    verified_bundle: Option<VerifiedBundle>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum VerifiedBundle {
    Spacy { model_root: PathBuf },
    Stanza { resource_root: PathBuf },
}

impl ExternalAdapterCommand {
    pub fn new<I, A>(entrypoint: impl Into<PathBuf>, arguments: I) -> Result<Self, &'static str>
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        Self::from_parts(None, entrypoint.into(), arguments)
    }

    pub fn interpreted<I, A>(
        interpreter: impl Into<PathBuf>,
        entrypoint: impl Into<PathBuf>,
        arguments: I,
    ) -> Result<Self, &'static str>
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        let interpreter = interpreter.into();
        if interpreter.as_os_str().is_empty() {
            return Err("external adapter interpreter must be nonempty");
        }
        Self::from_parts(Some(interpreter), entrypoint.into(), arguments)
    }

    pub fn spacy(
        interpreter: impl Into<PathBuf>,
        entrypoint: impl Into<PathBuf>,
        model_root: impl Into<PathBuf>,
        backend: &str,
        batch_size: u32,
    ) -> Result<Self, &'static str> {
        let interpreter = interpreter.into();
        if interpreter.as_os_str().is_empty() {
            return Err("spaCy adapter interpreter must be nonempty");
        }
        if batch_size == 0 {
            return Err("spaCy adapter batch size must be nonzero");
        }
        if !matches!(backend, "cpu" | "cuda" | "rocm") {
            return Err("spaCy adapter backend must be cpu, cuda, or rocm");
        }
        let model_root = model_root.into();
        normalized_relative_utf8(&model_root)
            .map_err(|_| "spaCy model root must be normalized relative UTF-8")?;
        let mut command = Self::from_parts(
            Some(interpreter),
            entrypoint.into(),
            [
                OsString::from("--model-root"),
                model_root.clone().into_os_string(),
                OsString::from("--backend"),
                OsString::from(backend),
                OsString::from("--batch-size"),
                OsString::from(batch_size.to_string()),
            ],
        )?;
        command.verified_bundle = Some(VerifiedBundle::Spacy { model_root });
        Ok(command)
    }

    pub fn stanza(
        interpreter: impl Into<PathBuf>,
        entrypoint: impl Into<PathBuf>,
        resource_root: impl Into<PathBuf>,
        language: &str,
        backend: &str,
        batch_size: u32,
    ) -> Result<Self, &'static str> {
        let interpreter = interpreter.into();
        if interpreter.as_os_str().is_empty() {
            return Err("Stanza adapter interpreter must be nonempty");
        }
        if batch_size != 1 {
            return Err("Stanza adapter provider batch size must be one");
        }
        if !matches!(backend, "cpu" | "cuda" | "rocm") {
            return Err("Stanza adapter backend must be cpu, cuda, or rocm");
        }
        if language.is_empty()
            || language.starts_with('-')
            || language.ends_with('-')
            || language.contains("--")
            || !language
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("Stanza adapter language must be a nonempty language tag");
        }
        let resource_root = resource_root.into();
        normalized_relative_utf8(&resource_root)
            .map_err(|_| "Stanza resource root must be normalized relative UTF-8")?;
        let mut command = Self::from_parts(
            Some(interpreter),
            entrypoint.into(),
            [
                OsString::from("--resource-root"),
                resource_root.clone().into_os_string(),
                OsString::from("--language"),
                OsString::from(language),
                OsString::from("--backend"),
                OsString::from(backend),
                OsString::from("--batch-size"),
                OsString::from(batch_size.to_string()),
            ],
        )?;
        command.verified_bundle = Some(VerifiedBundle::Stanza { resource_root });
        Ok(command)
    }

    fn from_parts<I, A>(
        interpreter: Option<PathBuf>,
        entrypoint: PathBuf,
        arguments: I,
    ) -> Result<Self, &'static str>
    where
        I: IntoIterator<Item = A>,
        A: Into<OsString>,
    {
        normalized_relative_utf8(&entrypoint)
            .map_err(|_| "external adapter entrypoint must be normalized relative UTF-8")?;
        Ok(Self {
            interpreter,
            entrypoint,
            arguments: arguments.into_iter().map(Into::into).collect(),
            verified_bundle: None,
        })
    }

    #[must_use]
    pub fn configuration_digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"lingrep.external-morphology/adapter-command/v1");
        match &self.interpreter {
            Some(interpreter) => {
                hasher.update(&[1]);
                update_command_frame(&mut hasher, interpreter.as_os_str().as_bytes());
            }
            None => {
                hasher.update(&[0]);
            }
        }
        update_command_frame(&mut hasher, self.entrypoint.as_os_str().as_bytes());
        hasher.update(
            &u64::try_from(self.arguments.len())
                .expect("in-memory argument count fits u64")
                .to_le_bytes(),
        );
        for argument in &self.arguments {
            update_command_frame(&mut hasher, argument.as_bytes());
        }
        *hasher.finalize().as_bytes()
    }

    #[must_use]
    fn semantic_configuration_digest(&self) -> [u8; 32] {
        let Some(bundle) = &self.verified_bundle else {
            return self.configuration_digest();
        };
        let mut hasher = blake3::Hasher::new();
        hasher.update(match bundle {
            VerifiedBundle::Spacy { .. } => b"lingrep.spacy-adapter/semantic-command/v1",
            VerifiedBundle::Stanza { .. } => b"lingrep.stanza-adapter/semantic-command/v1",
        });
        update_command_frame(&mut hasher, self.entrypoint.as_os_str().as_bytes());
        *hasher.finalize().as_bytes()
    }
}

fn update_command_frame(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(
        &u64::try_from(value.len())
            .expect("in-memory command field fits u64")
            .to_le_bytes(),
    );
    hasher.update(value);
}

fn normalized_relative_utf8(path: &Path) -> Result<String, String> {
    let value = path
        .to_str()
        .ok_or_else(|| "path must be UTF-8".to_owned())?;
    if path.is_absolute()
        || value.is_empty()
        || value.contains('\\')
        || value.contains('\0')
        || value.split('/').any(str::is_empty)
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("path must be a normalized root-relative path".to_owned());
    }
    Ok(value.to_owned())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManifestEntry {
    path: PathBuf,
    relative_path: String,
    length: u64,
    digest: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedAdapterManifest {
    root: PathBuf,
    implementation: Box<[ManifestEntry]>,
    resources: Box<[ManifestEntry]>,
    implementation_digest: [u8; 32],
    resource_manifest_digest: [u8; 32],
    components: Box<[ExternalComponent]>,
    command_digest: [u8; 32],
}

impl VerifiedAdapterManifest {
    pub fn new<I, R, P>(
        root: impl Into<PathBuf>,
        implementation: I,
        resources: R,
        command: &ExternalAdapterCommand,
        mut components: Vec<ExternalComponent>,
    ) -> Result<Self, ExternalProcessError>
    where
        I: IntoIterator<Item = (P, [u8; 32])>,
        R: IntoIterator<Item = (P, [u8; 32])>,
        P: Into<PathBuf>,
    {
        let root = root.into().canonicalize().map_err(|error| {
            ExternalProcessError::Manifest(format!("cannot resolve manifest root: {error}"))
        })?;
        let implementation = admit_manifest_entries(&root, implementation)?;
        let resources = admit_manifest_entries(&root, resources)?;
        if implementation.is_empty() {
            return Err(ExternalProcessError::Manifest(
                "implementation manifest must contain at least one file".to_owned(),
            ));
        }
        let entrypoint = normalized_relative_utf8(&command.entrypoint)
            .map_err(ExternalProcessError::Manifest)?;
        if !implementation
            .iter()
            .any(|entry| entry.relative_path == entrypoint)
        {
            return Err(ExternalProcessError::Manifest(
                "adapter command entrypoint is absent from the implementation manifest".to_owned(),
            ));
        }
        match &command.verified_bundle {
            Some(VerifiedBundle::Spacy { model_root }) => validate_spacy_bundle(
                &root,
                &implementation,
                &resources,
                &command.entrypoint,
                model_root,
            )?,
            Some(VerifiedBundle::Stanza { resource_root }) => validate_stanza_bundle(
                &root,
                &implementation,
                &resources,
                &command.entrypoint,
                resource_root,
            )?,
            None => {}
        }
        let mut paths = BTreeSet::new();
        if implementation
            .iter()
            .chain(resources.iter())
            .any(|entry| !paths.insert(entry.relative_path.clone()))
        {
            return Err(ExternalProcessError::Manifest(
                "manifest paths must be globally unique".to_owned(),
            ));
        }
        let command_digest = command.configuration_digest();
        components.push(
            ExternalComponent::new("adapter-command", command.semantic_configuration_digest())
                .expect("reserved command component name is valid"),
        );
        components.sort_by(|left, right| left.name().cmp(right.name()));
        if components.is_empty()
            || components
                .windows(2)
                .any(|pair| pair[0].name() == pair[1].name())
        {
            return Err(ExternalProcessError::Manifest(
                "manifest component closure must be nonempty with unique names".to_owned(),
            ));
        }
        let implementation_digest =
            manifest_digest(b"lingrep.adapter-implementation/v1", &implementation)?;
        let resource_manifest_digest =
            manifest_digest(b"lingrep.analyzer-resources/v1", &resources)?;
        Ok(Self {
            root,
            implementation: implementation.into_boxed_slice(),
            resources: resources.into_boxed_slice(),
            implementation_digest,
            resource_manifest_digest,
            components: components.into_boxed_slice(),
            command_digest,
        })
    }

    fn verify(&self) -> Result<(), ExternalProcessError> {
        for entry in self.implementation.iter().chain(self.resources.iter()) {
            verify_manifest_entry(&self.root, entry)?;
        }
        Ok(())
    }

    fn verify_command(&self, command: &ExternalAdapterCommand) -> Result<(), ExternalProcessError> {
        let entrypoint = normalized_relative_utf8(&command.entrypoint)
            .map_err(ExternalProcessError::Manifest)?;
        if command.configuration_digest() != self.command_digest
            || !self
                .implementation
                .iter()
                .any(|entry| entry.relative_path == entrypoint)
        {
            return Err(ExternalProcessError::Manifest(
                "adapter command does not match the verified implementation entrypoint/configuration"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub const fn implementation_digest(&self) -> [u8; 32] {
        self.implementation_digest
    }

    #[must_use]
    pub const fn resource_manifest_digest(&self) -> [u8; 32] {
        self.resource_manifest_digest
    }

    #[must_use]
    pub fn components(&self) -> &[ExternalComponent] {
        &self.components
    }
}

fn admit_manifest_entries<I, P>(
    root: &Path,
    entries: I,
) -> Result<Vec<ManifestEntry>, ExternalProcessError>
where
    I: IntoIterator<Item = (P, [u8; 32])>,
    P: Into<PathBuf>,
{
    let mut admitted = Vec::new();
    for (relative, digest) in entries {
        let relative = relative.into();
        let relative_path =
            normalized_relative_utf8(&relative).map_err(ExternalProcessError::Manifest)?;
        let path = root.join(&relative);
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            ExternalProcessError::Manifest(format!(
                "cannot stat manifest path {}: {error}",
                path.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ExternalProcessError::Manifest(format!(
                "manifest path {} is not a nonsymlink regular file",
                path.display()
            )));
        }
        let length = metadata.len();
        let entry = ManifestEntry {
            path,
            relative_path,
            length,
            digest,
        };
        verify_manifest_entry(root, &entry)?;
        admitted.push(entry);
    }
    admitted.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    if admitted
        .windows(2)
        .any(|pair| pair[0].relative_path == pair[1].relative_path)
    {
        return Err(ExternalProcessError::Manifest(
            "manifest paths must be unique".to_owned(),
        ));
    }
    Ok(admitted)
}

fn validate_spacy_bundle(
    root: &Path,
    implementation: &[ManifestEntry],
    resources: &[ManifestEntry],
    entrypoint: &Path,
    model_root: &Path,
) -> Result<(), ExternalProcessError> {
    validate_python_project_bundle(
        root,
        implementation,
        entrypoint,
        "lingrep-spacy-adapter",
        "spaCy",
    )?;

    let model = normalized_relative_utf8(model_root).map_err(ExternalProcessError::Manifest)?;
    let prefix = format!("{model}/");
    if resources.is_empty()
        || resources
            .iter()
            .any(|entry| !entry.relative_path.starts_with(&prefix))
    {
        return Err(ExternalProcessError::Manifest(
            "spaCy resources must be the nonempty exact model-root closure".to_owned(),
        ));
    }
    let profile = format!("{prefix}lingrep-spacy-model.json");
    if !resources.iter().any(|entry| entry.relative_path == profile) {
        return Err(ExternalProcessError::Manifest(
            "spaCy model profile is absent from the resource manifest".to_owned(),
        ));
    }

    let mut files = BTreeSet::new();
    collect_regular_files(root, &root.join(model_root), &mut files)?;
    let declared = resources
        .iter()
        .map(|entry| entry.relative_path.clone())
        .collect::<BTreeSet<_>>();
    if files != declared {
        return Err(ExternalProcessError::Manifest(
            "spaCy resource manifest is not the exact model-root file closure".to_owned(),
        ));
    }
    Ok(())
}

fn validate_stanza_bundle(
    root: &Path,
    implementation: &[ManifestEntry],
    resources: &[ManifestEntry],
    entrypoint: &Path,
    resource_root: &Path,
) -> Result<(), ExternalProcessError> {
    validate_python_project_bundle(
        root,
        implementation,
        entrypoint,
        "lingrep-stanza-adapter",
        "Stanza",
    )?;

    let resource =
        normalized_relative_utf8(resource_root).map_err(ExternalProcessError::Manifest)?;
    let prefix = format!("{resource}/");
    if resources.is_empty()
        || resources
            .iter()
            .any(|entry| !entry.relative_path.starts_with(&prefix))
    {
        return Err(ExternalProcessError::Manifest(
            "Stanza resources must be the nonempty exact resource-root closure".to_owned(),
        ));
    }
    for required in ["resources.json", "lingrep-stanza-model.json"] {
        let path = format!("{prefix}{required}");
        if !resources.iter().any(|entry| entry.relative_path == path) {
            return Err(ExternalProcessError::Manifest(format!(
                "Stanza {} is absent from the resource manifest",
                if required == "resources.json" {
                    "resource index"
                } else {
                    "model profile"
                }
            )));
        }
    }

    let mut files = BTreeSet::new();
    collect_regular_files(root, &root.join(resource_root), &mut files)?;
    let declared = resources
        .iter()
        .map(|entry| entry.relative_path.clone())
        .collect::<BTreeSet<_>>();
    if files != declared {
        return Err(ExternalProcessError::Manifest(
            "Stanza resource manifest is not the exact resource-root file closure".to_owned(),
        ));
    }
    Ok(())
}

fn validate_python_project_bundle(
    root: &Path,
    implementation: &[ManifestEntry],
    entrypoint: &Path,
    launcher: &str,
    provider: &str,
) -> Result<(), ExternalProcessError> {
    let project_root = entrypoint.parent().and_then(Path::parent).ok_or_else(|| {
        ExternalProcessError::Manifest(format!(
            "{provider} entrypoint must live below its project root"
        ))
    })?;
    if entrypoint != project_root.join("bin").join(launcher) {
        return Err(ExternalProcessError::Manifest(format!(
            "{provider} entrypoint must be the sealed project launcher"
        )));
    }
    let lock = normalized_relative_utf8(&project_root.join("uv.lock"))
        .map_err(ExternalProcessError::Manifest)?;
    let project = normalized_relative_utf8(&project_root.join("pyproject.toml"))
        .map_err(ExternalProcessError::Manifest)?;
    let mut project_files = BTreeSet::new();
    collect_regular_files(
        root,
        &root.join(project_root).join("bin"),
        &mut project_files,
    )?;
    collect_regular_files(
        root,
        &root.join(project_root).join("src"),
        &mut project_files,
    )?;
    collect_regular_files(root, &root.join(lock), &mut project_files)?;
    collect_regular_files(root, &root.join(project), &mut project_files)?;
    let declared = implementation
        .iter()
        .map(|entry| entry.relative_path.clone())
        .collect::<BTreeSet<_>>();
    if project_files != declared {
        return Err(ExternalProcessError::Manifest(format!(
            "{provider} implementation manifest is not the exact executable project closure"
        )));
    }
    Ok(())
}

fn collect_regular_files(
    root: &Path,
    current: &Path,
    files: &mut BTreeSet<String>,
) -> Result<(), ExternalProcessError> {
    let metadata = std::fs::symlink_metadata(current).map_err(|error| {
        ExternalProcessError::Manifest(format!(
            "cannot inspect spaCy model path {}: {error}",
            current.display()
        ))
    })?;
    if metadata.file_type().is_symlink() {
        return Err(ExternalProcessError::Manifest(format!(
            "spaCy model path {} contains a symlink",
            current.display()
        )));
    }
    if metadata.is_file() {
        let relative = current.strip_prefix(root).map_err(|_| {
            ExternalProcessError::Manifest("spaCy model escaped the manifest root".to_owned())
        })?;
        if relative
            .components()
            .any(|component| component.as_os_str() == "__pycache__")
            || relative
                .extension()
                .is_some_and(|extension| extension == "pyc")
        {
            return Err(ExternalProcessError::Manifest(
                "spaCy model root contains a generated Python cache".to_owned(),
            ));
        }
        files.insert(normalized_relative_utf8(relative).map_err(ExternalProcessError::Manifest)?);
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(ExternalProcessError::Manifest(format!(
            "spaCy model path {} is not regular",
            current.display()
        )));
    }
    for entry in std::fs::read_dir(current).map_err(|error| {
        ExternalProcessError::Manifest(format!(
            "cannot list spaCy model directory {}: {error}",
            current.display()
        ))
    })? {
        collect_regular_files(
            root,
            &entry
                .map_err(|error| {
                    ExternalProcessError::Manifest(format!(
                        "cannot read spaCy model entry: {error}"
                    ))
                })?
                .path(),
            files,
        )?;
    }
    Ok(())
}

fn verify_manifest_entry(root: &Path, entry: &ManifestEntry) -> Result<(), ExternalProcessError> {
    let expected = root.join(&entry.relative_path);
    if expected != entry.path {
        return Err(ExternalProcessError::Manifest(
            "manifest path changed after admission".to_owned(),
        ));
    }
    let mut current = root.to_path_buf();
    let parts = Path::new(&entry.relative_path)
        .components()
        .collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate() {
        let Component::Normal(part) = part else {
            return Err(ExternalProcessError::Manifest(
                "manifest path changed after normalization".to_owned(),
            ));
        };
        current.push(part);
        let metadata = std::fs::symlink_metadata(&current)
            .map_err(|error| ExternalProcessError::Manifest(error.to_string()))?;
        if metadata.file_type().is_symlink() {
            return Err(ExternalProcessError::Manifest(format!(
                "manifest path {} contains a symlink",
                current.display()
            )));
        }
        if index + 1 < parts.len() && !metadata.is_dir() {
            return Err(ExternalProcessError::Manifest(format!(
                "manifest path parent {} is not a directory",
                current.display()
            )));
        }
    }
    let metadata = std::fs::symlink_metadata(&entry.path)
        .map_err(|error| ExternalProcessError::Manifest(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != entry.length {
        return Err(ExternalProcessError::Manifest(format!(
            "manifest metadata mismatch for {}",
            entry.path.display()
        )));
    }
    let actual = hash_file_nofollow(&entry.path)?;
    if actual != entry.digest {
        return Err(ExternalProcessError::Manifest(format!(
            "manifest digest mismatch for {}",
            entry.path.display()
        )));
    }
    Ok(())
}

fn manifest_digest(
    domain: &[u8],
    entries: &[ManifestEntry],
) -> Result<[u8; 32], ExternalProcessError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(
        &u64::try_from(entries.len())
            .map_err(|_| {
                ExternalProcessError::Manifest("manifest entry count overflow".to_owned())
            })?
            .to_le_bytes(),
    );
    for entry in entries {
        hasher.update(
            &u64::try_from(entry.relative_path.len())
                .map_err(|_| {
                    ExternalProcessError::Manifest("manifest path length overflow".to_owned())
                })?
                .to_le_bytes(),
        );
        hasher.update(entry.relative_path.as_bytes());
        hasher.update(&entry.length.to_le_bytes());
        hasher.update(&entry.digest);
    }
    Ok(*hasher.finalize().as_bytes())
}

#[derive(Clone, Debug)]
pub struct ExternalAdapterCancellation {
    requested: Arc<AtomicBool>,
}

impl ExternalAdapterCancellation {
    #[must_use]
    pub fn new() -> Self {
        Self {
            requested: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.requested.store(true, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

impl Default for ExternalAdapterCancellation {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub enum ExternalProcessError {
    Manifest(String),
    Spawn(String),
    Io(String),
    Protocol(ExternalProtocolError),
    Exit {
        status: Option<i32>,
        stderr_tail: Vec<u8>,
    },
    Diagnostic(String),
    Worker(String),
    Cancelled,
}

impl fmt::Display for ExternalProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(message) => write!(formatter, "adapter manifest rejected: {message}"),
            Self::Spawn(message) => write!(formatter, "adapter spawn failed: {message}"),
            Self::Io(message) => write!(formatter, "adapter I/O failed: {message}"),
            Self::Protocol(error) => write!(formatter, "{error}"),
            Self::Exit {
                status,
                stderr_tail,
            } => write!(
                formatter,
                "adapter exited with status {:?}: {}",
                status,
                String::from_utf8_lossy(stderr_tail)
            ),
            Self::Diagnostic(message) => write!(formatter, "diagnostic callback failed: {message}"),
            Self::Worker(message) => write!(formatter, "adapter worker failed: {message}"),
            Self::Cancelled => formatter.write_str("adapter execution cancelled"),
        }
    }
}

impl std::error::Error for ExternalProcessError {}

impl From<ExternalProtocolError> for ExternalProcessError {
    fn from(value: ExternalProtocolError) -> Self {
        Self::Protocol(value)
    }
}

pub struct ExternalAdapterProcess {
    child: Option<Child>,
    process_group: i32,
    config: ExternalRunConfig,
    descriptor: Option<AnalyzerDescriptor>,
    execution: Option<ExecutionProvenance>,
    schema: Option<TokenSchema>,
    header_bytes: Vec<u8>,
    ready_bytes: Vec<u8>,
    next_batch_id: u64,
    last_input_id: Option<u64>,
    writer: Option<SyncSender<Vec<Vec<u8>>>>,
    stdout: Receiver<Result<Vec<u8>, String>>,
    writer_worker: Option<JoinHandle<Result<(), String>>>,
    stdout_worker: Option<JoinHandle<()>>,
    stderr_worker: Option<JoinHandle<()>>,
    stderr_tail: Arc<Mutex<VecDeque<u8>>>,
    diagnostic_failure: Arc<Mutex<Option<String>>>,
    cancellation: ExternalAdapterCancellation,
    unexpected_stdout: Option<String>,
    cleaned: bool,
}

impl fmt::Debug for ExternalAdapterProcess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExternalAdapterProcess")
            .field("process_group", &self.process_group)
            .field("next_batch_id", &self.next_batch_id)
            .field("cleaned", &self.cleaned)
            .finish_non_exhaustive()
    }
}

impl ExternalAdapterProcess {
    pub fn launch(
        command: ExternalAdapterCommand,
        manifest: VerifiedAdapterManifest,
        config: ExternalRunConfig,
        diagnostic: DiagnosticCallback,
    ) -> Result<Self, ExternalProcessError> {
        Self::launch_with_cancellation(
            command,
            manifest,
            config,
            diagnostic,
            ExternalAdapterCancellation::new(),
        )
    }

    pub fn launch_with_cancellation(
        command: ExternalAdapterCommand,
        manifest: VerifiedAdapterManifest,
        config: ExternalRunConfig,
        diagnostic: DiagnosticCallback,
        cancellation: ExternalAdapterCancellation,
    ) -> Result<Self, ExternalProcessError> {
        manifest.verify()?;
        manifest.verify_command(&command)?;
        if !config.admits_verified_manifest(
            manifest.implementation_digest(),
            manifest.resource_manifest_digest(),
            manifest.components(),
        ) {
            return Err(ExternalProcessError::Manifest(
                "verified manifests do not exactly match the run identity and component closure"
                    .to_owned(),
            ));
        }
        let header_bytes = config.header_bytes()?;
        let entrypoint = manifest.root.join(&command.entrypoint);
        let executable = command
            .interpreter
            .clone()
            .unwrap_or_else(|| entrypoint.clone());
        let mut child_command = Command::new(executable);
        if command.interpreter.is_some() {
            child_command.arg(&entrypoint);
        }
        child_command
            .args(&command.arguments)
            .current_dir(&manifest.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let child = child_command
            .spawn()
            .map_err(|error| ExternalProcessError::Spawn(error.to_string()))?;
        let process_group = i32::try_from(child.id())
            .map_err(|_| ExternalProcessError::Spawn("child PID exceeds i32".to_owned()))?;
        let mut spawn_guard = SpawnGuard::new(child, process_group);
        let stdin =
            spawn_guard.child_mut().stdin.take().ok_or_else(|| {
                ExternalProcessError::Spawn("child stdin was not piped".to_owned())
            })?;
        let stdout =
            spawn_guard.child_mut().stdout.take().ok_or_else(|| {
                ExternalProcessError::Spawn("child stdout was not piped".to_owned())
            })?;
        let stderr =
            spawn_guard.child_mut().stderr.take().ok_or_else(|| {
                ExternalProcessError::Spawn("child stderr was not piped".to_owned())
            })?;

        let (writer, writer_rx) = mpsc::sync_channel::<Vec<Vec<u8>>>(1);
        let writer_worker = thread::Builder::new()
            .name("lingrep-external-stdin".to_owned())
            .spawn(move || writer_loop(stdin, writer_rx))
            .map_err(|error| ExternalProcessError::Spawn(error.to_string()))?;
        let (stdout_tx, stdout_rx) = mpsc::sync_channel(1);
        let max_record_bytes = config.limits().max_record_bytes;
        let stdout_worker = thread::Builder::new()
            .name("lingrep-external-stdout".to_owned())
            .spawn(move || stdout_loop(stdout, stdout_tx, max_record_bytes))
            .map_err(|error| ExternalProcessError::Spawn(error.to_string()))?;
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let diagnostic_failure = Arc::new(Mutex::new(None));
        let stderr_worker = spawn_stderr_worker(
            stderr,
            diagnostic,
            Arc::clone(&stderr_tail),
            config.limits().max_stderr_tail_bytes,
            Arc::clone(&diagnostic_failure),
        )?;

        let mut process = Self {
            child: Some(spawn_guard.take()),
            process_group,
            config,
            descriptor: None,
            execution: None,
            schema: None,
            header_bytes: header_bytes.clone(),
            ready_bytes: Vec::new(),
            next_batch_id: 0,
            last_input_id: None,
            writer: Some(writer),
            stdout: stdout_rx,
            writer_worker: Some(writer_worker),
            stdout_worker: Some(stdout_worker),
            stderr_worker: Some(stderr_worker),
            stderr_tail,
            diagnostic_failure,
            cancellation,
            unexpected_stdout: None,
            cleaned: false,
        };
        let handshake = (|| {
            process.send(vec![header_bytes])?;
            let ready = process.receive()?;
            let mut transcript = process.header_bytes.clone();
            transcript.extend_from_slice(&ready);
            let transcript = ExternalTranscript::open(
                BufReader::new(std::io::Cursor::new(transcript)),
                process.config.limits(),
            )?;
            process.descriptor = Some(transcript.descriptor().clone());
            process.execution = Some(transcript.execution_provenance().clone());
            process.schema = Some(transcript.schema().clone());
            process.ready_bytes = ready;
            Ok(())
        })();
        if let Err(error) = handshake {
            process.abort();
            return Err(error);
        }
        Ok(process)
    }

    pub fn exchange_batch(
        &mut self,
        inputs: Vec<TextUnit>,
        limits: &ExecutionLimits,
    ) -> Result<ExternalValidatedBatch, ExternalProcessError> {
        let result = self.exchange_batch_inner(&inputs, limits);
        if result.is_err() {
            self.abort();
        }
        result
    }

    #[must_use]
    pub const fn process_group_id(&self) -> i32 {
        self.process_group
    }

    #[must_use]
    pub fn descriptor(&self) -> &AnalyzerDescriptor {
        self.descriptor
            .as_ref()
            .expect("successful process launch validates a descriptor")
    }

    #[must_use]
    pub fn execution_provenance(&self) -> &ExecutionProvenance {
        self.execution
            .as_ref()
            .expect("successful process launch validates execution provenance")
    }

    #[must_use]
    pub fn schema(&self) -> &TokenSchema {
        self.schema
            .as_ref()
            .expect("successful process launch validates a produced schema")
    }

    #[must_use]
    pub fn cancellation_handle(&self) -> ExternalAdapterCancellation {
        self.cancellation.clone()
    }

    #[must_use]
    pub fn retained_stderr_bytes(&self) -> usize {
        self.stderr_tail
            .lock()
            .expect("stderr tail mutex poisoned")
            .len()
    }

    fn exchange_batch_inner(
        &mut self,
        inputs: &[TextUnit],
        limits: &ExecutionLimits,
    ) -> Result<ExternalValidatedBatch, ExternalProcessError> {
        if self.cleaned {
            return Err(ExternalProcessError::Worker(
                "adapter process is already closed".to_owned(),
            ));
        }
        let batch_id = self.next_batch_id;
        limits.validate().map_err(|error| {
            ExternalProcessError::Protocol(ExternalProtocolError::new_for_execution(
                "limit",
                error.to_string(),
                Some(batch_id),
            ))
        })?;
        if inputs.is_empty() {
            return Err(ExternalProcessError::Protocol(
                crate::analysis::ExternalProtocolError::new_for_execution(
                    "state",
                    "live external batches must be nonempty",
                    Some(batch_id),
                ),
            ));
        }
        if inputs.len() > limits.max_stage_sentences {
            return Err(ExternalProtocolError::new_for_execution(
                "limit",
                "live document count exceeds max_stage_sentences",
                Some(batch_id),
            )
            .into());
        }
        let mut stage_input_bytes = 0_usize;
        for input in inputs {
            let expected_kind = match self.config.segmentation() {
                SegmentationMode::Input => TextUnitKind::Sentence,
                SegmentationMode::Analyzer => TextUnitKind::Segment,
            };
            if input.kind() != expected_kind {
                return Err(ExternalProtocolError::new_for_execution(
                    "document",
                    "live input kind differs from configured segmentation",
                    Some(batch_id),
                )
                .into());
            }
            if input.kind() == TextUnitKind::Sentence && input.len() > limits.max_sentence_bytes {
                return Err(ExternalProtocolError::new_for_execution(
                    "limit",
                    "live input exceeds max_sentence_bytes",
                    Some(batch_id),
                )
                .into());
            }
            stage_input_bytes = stage_input_bytes.checked_add(input.len()).ok_or_else(|| {
                ExternalProcessError::Protocol(ExternalProtocolError::new_for_execution(
                    "limit",
                    "live stage input byte count overflow",
                    Some(batch_id),
                ))
            })?;
        }
        if stage_input_bytes > limits.max_stage_input_bytes {
            return Err(ExternalProtocolError::new_for_execution(
                "limit",
                "live inputs exceed max_stage_input_bytes",
                Some(batch_id),
            )
            .into());
        }
        let mut prior = self.last_input_id;
        for input in inputs {
            if prior.is_some_and(|prior| input.id().get() <= prior) {
                return Err(ExternalProcessError::Protocol(
                    crate::analysis::ExternalProtocolError::new_for_execution(
                        "order",
                        "live input identities must be globally increasing",
                        Some(batch_id),
                    ),
                ));
            }
            prior = Some(input.id().get());
        }
        let request =
            self.config
                .batch_request_bytes(batch_id, inputs, limits.max_input_record_bytes)?;
        let mut logical = self.header_bytes.clone();
        logical.extend_from_slice(&self.ready_bytes);
        for record in &request {
            logical.extend_from_slice(record);
        }
        self.send(request)?;
        let mut response_sentence_counts = vec![0_u64; inputs.len()];
        let mut last_response_document = None;
        let mut total_response_sentences = 0_usize;
        let mut response_bytes = 0_usize;
        loop {
            let record = self.receive()?;
            response_bytes = response_bytes
                .checked_add(record.len())
                .ok_or_else(|| ExternalProcessError::Io("response byte overflow".to_owned()))?;
            if response_bytes > self.config.limits().max_response_batch_bytes {
                return Err(ExternalProtocolError::new_for_execution(
                    "limit",
                    "response batch exceeds max_response_batch_bytes",
                    Some(batch_id),
                )
                .into());
            }
            let kind = response_record_kind(&record, self.config.limits())?;
            let response_batch_id = match kind {
                ResponseRecordKind::Sentence { batch_id, .. }
                | ResponseRecordKind::BatchComplete { batch_id } => batch_id,
            };
            if response_batch_id != batch_id {
                return Err(ExternalProtocolError::new_for_execution(
                    "order",
                    "adapter response batch identity mismatch",
                    Some(batch_id),
                )
                .into());
            }
            if let ResponseRecordKind::Sentence {
                document_ordinal,
                input_id,
                sentence_ordinal,
                ..
            } = kind
            {
                let document_index = usize::try_from(document_ordinal).map_err(|_| {
                    ExternalProcessError::Protocol(ExternalProtocolError::new_for_execution(
                        "order",
                        "adapter response document ordinal exceeds usize",
                        Some(batch_id),
                    ))
                })?;
                let input = inputs.get(document_index).ok_or_else(|| {
                    ExternalProcessError::Protocol(ExternalProtocolError::new_for_execution(
                        "order",
                        "adapter response document ordinal is outside the request",
                        Some(batch_id),
                    ))
                })?;
                if last_response_document.is_some_and(|prior| document_index < prior) {
                    return Err(ExternalProtocolError::new_for_execution(
                        "order",
                        "adapter response document ordinals must be nondecreasing",
                        Some(batch_id),
                    )
                    .into());
                }
                if input.id().get() != input_id
                    || response_sentence_counts[document_index] != sentence_ordinal
                {
                    return Err(ExternalProtocolError::new_for_execution(
                        "order",
                        "adapter response input or sentence identity mismatch",
                        Some(batch_id),
                    )
                    .into());
                }
                response_sentence_counts[document_index] = response_sentence_counts[document_index]
                    .checked_add(1)
                    .ok_or_else(|| {
                        ExternalProcessError::Protocol(ExternalProtocolError::new_for_execution(
                            "limit",
                            "adapter sentence count overflow",
                            Some(batch_id),
                        ))
                    })?;
                last_response_document = Some(document_index);
                total_response_sentences =
                    total_response_sentences.checked_add(1).ok_or_else(|| {
                        ExternalProcessError::Protocol(ExternalProtocolError::new_for_execution(
                            "limit",
                            "adapter sentence count overflow",
                            Some(batch_id),
                        ))
                    })?;
                if total_response_sentences > limits.max_stage_sentences {
                    return Err(ExternalProtocolError::new_for_execution(
                        "limit",
                        "adapter response exceeds max_stage_sentences",
                        Some(batch_id),
                    )
                    .into());
                }
            }
            logical.extend_from_slice(&record);
            if matches!(kind, ResponseRecordKind::BatchComplete { .. }) {
                break;
            }
        }
        let mut transcript = ExternalTranscript::open_at(
            BufReader::new(std::io::Cursor::new(logical)),
            self.config.limits(),
            batch_id,
        )?;
        let bounds = crate::analysis::ExternalBatchBounds {
            max_input_record_bytes: limits.max_input_record_bytes,
            max_stage_input_bytes: limits.max_stage_input_bytes,
            max_stage_sentences: limits.max_stage_sentences,
            max_sentence_bytes: limits.max_sentence_bytes,
            max_tokenized_bytes: limits.max_tokenized_bytes,
        };
        let batch = transcript
            .next_validated_batch_with_bounds(bounds)?
            .ok_or_else(|| {
                ExternalProcessError::Worker("adapter returned no committed batch".to_owned())
            })?;
        if transcript
            .next_validated_batch_with_bounds(bounds)?
            .is_some()
        {
            return Err(ExternalProcessError::Worker(
                "adapter returned more than one batch".to_owned(),
            ));
        }
        self.next_batch_id = self
            .next_batch_id
            .checked_add(1)
            .ok_or_else(|| ExternalProcessError::Io("batch identity overflow".to_owned()))?;
        self.last_input_id = prior;
        Ok(batch)
    }

    pub fn finish(mut self) -> Result<(), ExternalProcessError> {
        self.finish_inner()
    }

    fn send(&self, records: Vec<Vec<u8>>) -> Result<(), ExternalProcessError> {
        if self.cancellation.is_cancelled() {
            return Err(ExternalProcessError::Cancelled);
        }
        let writer = self
            .writer
            .as_ref()
            .ok_or_else(|| ExternalProcessError::Worker("adapter stdin is closed".to_owned()))?;
        writer
            .send(records)
            .map_err(|_| ExternalProcessError::Worker("adapter stdin worker stopped".to_owned()))
    }

    fn receive(&mut self) -> Result<Vec<u8>, ExternalProcessError> {
        loop {
            if self.cancellation.is_cancelled() {
                return Err(ExternalProcessError::Cancelled);
            }
            match self.stdout.recv_timeout(Duration::from_millis(25)) {
                Ok(result) => return result.map_err(ExternalProcessError::Io),
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self
                        .child
                        .as_mut()
                        .and_then(|child| child.try_wait().ok())
                        .flatten();
                    return match status {
                        Some(status) if !status.success() => Err(self.exit_error(status.code())),
                        _ => Err(ExternalProcessError::Worker(
                            "adapter stdout ended before the required record".to_owned(),
                        )),
                    };
                }
            }
        }
    }

    fn finish_inner(&mut self) -> Result<(), ExternalProcessError> {
        if self.cleaned {
            return Ok(());
        }
        self.writer.take();
        let writer_result = join_writer(&mut self.writer_worker);
        writer_result?;
        let status = match self.wait_clean_exit() {
            Ok(status) => status,
            Err(error) => {
                self.abort();
                return Err(error);
            }
        };
        if self.group_exists() {
            self.terminate_group(libc::SIGTERM);
            let deadline = Instant::now() + TERMINATION_GRACE;
            while self.group_exists() && Instant::now() < deadline {
                self.capture_stdout();
                thread::sleep(Duration::from_millis(10));
            }
            if self.group_exists() {
                self.terminate_group(libc::SIGKILL);
            }
        }
        self.join_readers();
        self.cleaned = true;
        if !status.success() {
            return Err(self.exit_error(status.code()));
        }
        if let Some(message) = self.unexpected_stdout.take() {
            return Err(ExternalProcessError::Protocol(
                ExternalProtocolError::new_for_execution("state", message, None),
            ));
        }
        if let Some(message) = self
            .diagnostic_failure
            .lock()
            .expect("diagnostic failure mutex poisoned")
            .take()
        {
            return Err(ExternalProcessError::Diagnostic(message));
        }
        Ok(())
    }

    fn wait_clean_exit(&mut self) -> Result<ExitStatus, ExternalProcessError> {
        let deadline = Instant::now() + TERMINATION_GRACE;
        loop {
            if self.cancellation.is_cancelled() {
                return Err(ExternalProcessError::Cancelled);
            }
            let observed = self
                .child
                .as_mut()
                .ok_or_else(|| ExternalProcessError::Worker("child handle missing".to_owned()))?
                .try_wait()
                .map_err(|error| ExternalProcessError::Io(error.to_string()))?;
            match observed {
                Some(status) => {
                    self.child.take();
                    return Ok(status);
                }
                None if Instant::now() < deadline => {
                    self.capture_stdout();
                    thread::sleep(Duration::from_millis(10));
                }
                None => {
                    return Err(ExternalProcessError::Worker(
                        "adapter did not exit after stdin closed".to_owned(),
                    ));
                }
            }
        }
    }

    fn abort(&mut self) {
        if self.cleaned {
            return;
        }
        self.writer.take();
        self.terminate_group(libc::SIGTERM);
        let deadline = Instant::now() + TERMINATION_GRACE;
        while Instant::now() < deadline {
            self.capture_stdout();
            if let Some(child) = self.child.as_mut()
                && child.try_wait().ok().flatten().is_some()
            {
                self.child.take();
            }
            if !self.group_exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        if self.group_exists() {
            self.terminate_group(libc::SIGKILL);
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        let reaping_deadline = Instant::now() + Duration::from_secs(1);
        while self.group_exists() && Instant::now() < reaping_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let _ = join_writer(&mut self.writer_worker);
        self.join_readers();
        self.cleaned = true;
    }

    fn terminate_group(&self, signal: i32) {
        // Negative PID addresses the process group created at spawn. ESRCH is
        // benign: it means the group already exited between observation and signal.
        unsafe {
            libc::kill(-self.process_group, signal);
        }
    }

    fn group_exists(&self) -> bool {
        let result = unsafe { libc::kill(-self.process_group, 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    fn capture_stdout(&mut self) {
        while let Ok(record) = self.stdout.try_recv() {
            if self.unexpected_stdout.is_none() {
                self.unexpected_stdout = Some(match record {
                    Ok(_) => "adapter emitted a record after batch completion".to_owned(),
                    Err(error) => format!("adapter stdout failed after batch completion: {error}"),
                });
            }
        }
    }

    fn join_readers(&mut self) {
        loop {
            match self.stdout.recv_timeout(Duration::from_millis(10)) {
                Ok(record) => {
                    if self.unexpected_stdout.is_none() {
                        self.unexpected_stdout = Some(match record {
                            Ok(_) => "adapter emitted a record after batch completion".to_owned(),
                            Err(error) => {
                                format!("adapter stdout failed after batch completion: {error}")
                            }
                        });
                    }
                }
                Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {
                    if self
                        .stdout_worker
                        .as_ref()
                        .is_none_or(JoinHandle::is_finished)
                    {
                        break;
                    }
                }
            }
        }
        if let Some(worker) = self.stdout_worker.take() {
            let _ = worker.join();
        }
        if let Some(worker) = self.stderr_worker.take() {
            let _ = worker.join();
        }
    }

    fn exit_error(&self, status: Option<i32>) -> ExternalProcessError {
        let tail = self
            .stderr_tail
            .lock()
            .expect("stderr tail mutex poisoned")
            .iter()
            .copied()
            .collect();
        ExternalProcessError::Exit {
            status,
            stderr_tail: tail,
        }
    }
}

struct SpawnGuard {
    child: Option<Child>,
    process_group: i32,
}

impl SpawnGuard {
    fn new(child: Child, process_group: i32) -> Self {
        Self {
            child: Some(child),
            process_group,
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("spawn guard owns child")
    }

    fn take(mut self) -> Child {
        self.child.take().expect("spawn guard owns child")
    }
}

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            unsafe {
                libc::kill(-self.process_group, libc::SIGTERM);
            }
            let deadline = Instant::now() + TERMINATION_GRACE;
            let mut child_reaped = false;
            while Instant::now() < deadline {
                if !child_reaped && child.try_wait().ok().flatten().is_some() {
                    child_reaped = true;
                }
                let group_exists = {
                    let result = unsafe { libc::kill(-self.process_group, 0) };
                    result == 0
                        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
                };
                if !group_exists {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            unsafe {
                libc::kill(-self.process_group, libc::SIGKILL);
            }
            if !child_reaped {
                let _ = child.wait();
            }
        }
    }
}

impl Drop for ExternalAdapterProcess {
    fn drop(&mut self) {
        self.abort();
    }
}

fn hash_file_nofollow(path: &Path) -> Result<[u8; 32], ExternalProcessError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| {
            ExternalProcessError::Manifest(format!("cannot open {}: {error}", path.display()))
        })?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| ExternalProcessError::Manifest(error.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn writer_loop(
    mut stdin: std::process::ChildStdin,
    receiver: Receiver<Vec<Vec<u8>>>,
) -> Result<(), String> {
    for records in receiver {
        for bytes in records {
            stdin.write_all(&bytes).map_err(|error| error.to_string())?;
        }
        stdin.flush().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn stdout_loop(
    stdout: std::process::ChildStdout,
    sender: SyncSender<Result<Vec<u8>, String>>,
    max_record_bytes: usize,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        match read_bounded_line(&mut reader, max_record_bytes) {
            Ok(Some(bytes)) => {
                if sender.send(Ok(bytes)).is_err() {
                    break;
                }
            }
            Ok(None) => break,
            Err(error) => {
                let _ = sender.send(Err(error));
                break;
            }
        }
    }
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    max_record_bytes: usize,
) -> Result<Option<Vec<u8>>, String> {
    let mut bytes = Vec::new();
    let bound = max_record_bytes
        .checked_add(1)
        .ok_or_else(|| "record bound overflow".to_owned())?;
    let read = reader
        .by_ref()
        .take(bound as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| error.to_string())?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.len() > max_record_bytes {
        return Err("adapter stdout record exceeds max_record_bytes".to_owned());
    }
    if bytes.last() != Some(&b'\n') {
        return Err("adapter stdout record is missing its final LF".to_owned());
    }
    Ok(Some(bytes))
}

fn spawn_stderr_worker(
    mut stderr: std::process::ChildStderr,
    diagnostic: DiagnosticCallback,
    tail: Arc<Mutex<VecDeque<u8>>>,
    tail_limit: usize,
    callback_failure: Arc<Mutex<Option<String>>>,
) -> Result<JoinHandle<()>, ExternalProcessError> {
    thread::Builder::new()
        .name("lingrep-external-stderr".to_owned())
        .spawn(move || {
            let mut buffer = [0_u8; 8 * 1024];
            loop {
                let read = match stderr.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => read,
                };
                if let Err(error) = diagnostic(&buffer[..read]) {
                    let mut failure = callback_failure
                        .lock()
                        .expect("diagnostic failure mutex poisoned");
                    if failure.is_none() {
                        *failure = Some(error);
                    }
                }
                let mut retained = tail.lock().expect("stderr tail mutex poisoned");
                retained.extend(&buffer[..read]);
                while retained.len() > tail_limit {
                    retained.pop_front();
                }
            }
        })
        .map_err(|error| ExternalProcessError::Spawn(error.to_string()))
}

fn join_writer(
    worker: &mut Option<JoinHandle<Result<(), String>>>,
) -> Result<(), ExternalProcessError> {
    let Some(worker) = worker.take() else {
        return Ok(());
    };
    worker
        .join()
        .map_err(|_| ExternalProcessError::Worker("stdin worker panicked".to_owned()))?
        .map_err(ExternalProcessError::Io)
}
