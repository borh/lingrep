//! Publishes immutable artifact directories from private staging directories.

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{
    CorpusError,
    artifact::CompletedStagingArtifact,
    error::io_at,
    fs_capability::{Capability, rename_noreplace},
};

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, thiserror::Error)]
pub enum PublicationError {
    #[error("Could not save the output: {0}")]
    Io(#[from] io::Error),
    #[error("Cannot create '{path}': the destination already exists and was left unchanged")]
    DestinationExists { path: PathBuf },
    #[error(
        "Cannot save '{path}' safely: atomic rename without replacement is not supported (OS error {errno})"
    )]
    Unsupported { path: PathBuf, errno: i32 },
    #[error("Created '{path}', but could not confirm it will survive a system crash: {source}")]
    DurabilityUncertain {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
pub(crate) fn publish(completed: &mut CompletedStagingArtifact) -> Result<(), CorpusError> {
    let published = publish_guarded_with_ops(&RealOps, completed, || true, || true)?;
    debug_assert!(published, "unconditional publication guard must commit");
    Ok(())
}

pub(crate) trait PublicationOps {
    fn fsync(&self, path: &Path) -> io::Result<()>;
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()>;
}

pub(crate) struct RealOps;

impl PublicationOps for RealOps {
    fn fsync(&self, path: &Path) -> io::Result<()> {
        File::open(path)?.sync_all()
    }

    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        rename_noreplace(from, to)
    }
}

#[cfg(test)]
fn publish_with_ops(
    ops: &impl PublicationOps,
    completed: &mut CompletedStagingArtifact,
) -> Result<(), CorpusError> {
    let published = publish_guarded_with_ops(ops, completed, || true, || true)?;
    debug_assert!(published, "unconditional publication guard must commit");
    Ok(())
}

pub(crate) fn publish_guarded_with_ops(
    ops: &impl PublicationOps,
    completed: &mut CompletedStagingArtifact,
    should_continue: impl FnMut() -> bool,
    claim_commit: impl FnOnce() -> bool,
) -> Result<bool, CorpusError> {
    let components = completed.publication_components();
    let (staging, destination) = completed.staging_and_destination_mut();
    commit_directory_with_ops(
        ops,
        staging,
        destination,
        &components,
        should_continue,
        claim_commit,
    )
    .map_err(into_corpus_error)
}

fn commit_directory_with_ops(
    ops: &impl PublicationOps,
    staging: &mut StagingDirectory,
    destination: &Path,
    components: &[PathBuf],
    mut should_continue: impl FnMut() -> bool,
    claim_commit: impl FnOnce() -> bool,
) -> Result<bool, PublicationError> {
    for component in components {
        if !should_continue() {
            return Ok(false);
        }
        ops.fsync(component)
            .map_err(|error| sync_error(component, error))?;
    }
    if !should_continue() {
        return Ok(false);
    }
    ops.fsync(staging.path())
        .map_err(|error| sync_error(staging.path(), error))?;

    if !claim_commit() {
        return Ok(false);
    }
    ops.rename_noreplace(staging.path(), destination)
        .map_err(|error| map_rename_error(error, staging.path(), destination))?;
    staging.mark_published();

    let parent = destination_parent(destination);
    ops.fsync(parent)
        .map_err(|source| PublicationError::DurabilityUncertain {
            path: destination.to_path_buf(),
            source,
        })?;
    Ok(true)
}

fn into_corpus_error(error: PublicationError) -> CorpusError {
    match error {
        PublicationError::Io(source) => CorpusError::Io(source),
        PublicationError::DestinationExists { path } => CorpusError::DestinationExists { path },
        PublicationError::Unsupported { path, errno } => {
            CorpusError::UnsupportedPublication { path, errno }
        }
        PublicationError::DurabilityUncertain { path, .. } => {
            CorpusError::PublicationDurabilityUncertain { path }
        }
    }
}

fn sync_error(path: &Path, error: io::Error) -> io::Error {
    io_at("sync", path, error.kind(), error)
}

fn rename_error(staging: &Path, destination: &Path, error: io::Error) -> io::Error {
    let operation = format!("rename '{}' to", staging.display());
    io_at(operation, destination, error.kind(), error)
}

fn map_rename_error(error: io::Error, staging: &Path, destination: &Path) -> PublicationError {
    let errno = error.raw_os_error().unwrap_or(libc::EIO);
    if matches!(errno, libc::EEXIST | libc::ENOTEMPTY) {
        PublicationError::DestinationExists {
            path: destination.to_path_buf(),
        }
    } else if Capability::NoReplaceRename.unsupported_by(&error) {
        PublicationError::Unsupported {
            path: destination.to_path_buf(),
            errno,
        }
    } else {
        PublicationError::Io(rename_error(staging, destination, error))
    }
}

fn destination_parent(destination: &Path) -> &Path {
    destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub(crate) fn staging_paths(parent: &Path, name: &str, suffix: &str) -> (PathBuf, PathBuf) {
    (
        parent.join(format!(".{name}.staging-{suffix}")),
        parent.join(format!(".{name}.stagelock-{suffix}")),
    )
}

#[derive(Debug)]
pub(crate) struct StagingLock {
    path: PathBuf,
    _file: File,
}

impl StagingLock {
    /// Returns `None` when `path` already exists.
    pub(crate) fn acquire(path: PathBuf) -> io::Result<Option<Self>> {
        let mut file = match File::options().write(true).create_new(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(None),
            Err(error) => return Err(error),
        };
        if file.lock().is_ok()
            && let Some(host) = host_name()
        {
            let _ = file.write_all(&host);
        }
        Ok(Some(Self { path, _file: file }))
    }
}

impl Drop for StagingLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Removes staging directories for `name` only when their lock files record
/// this host and can be locked exclusively.
pub(crate) fn reclaim_orphaned_staging(parent: &Path, name: &str) {
    let Some(host) = host_name() else {
        return;
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    let lock_prefix = format!(".{name}.stagelock-");
    for entry in entries.flatten() {
        let lock_name = entry.file_name();
        let Some(suffix) = lock_name
            .to_str()
            .and_then(|lock_name| lock_name.strip_prefix(&lock_prefix))
        else {
            continue;
        };
        let (staging, lock_path) = staging_paths(parent, name, suffix);
        let Some(_lock) = lock_if_abandoned(&lock_path, &host) else {
            continue;
        };
        match fs::remove_dir_all(&staging) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {}
            _ => {
                let _ = fs::remove_file(&lock_path);
            }
        }
    }
}

/// Opens and exclusively locks the lock file at `lock_path` when it records
/// `host`, which is the evidence that the build that created it has stopped.
///
/// The returned file holds the lock until it is dropped.
fn lock_if_abandoned(lock_path: &Path, host: &[u8]) -> Option<File> {
    let mut file =
        crate::artifact::open_regular(File::options().read(true).write(true), lock_path).ok()?;
    let mut owner = Vec::new();
    if file.try_lock().is_err()
        || !is_file_at(&file, lock_path)
        || file.read_to_end(&mut owner).is_err()
        || owner != host
    {
        return None;
    }
    Some(file)
}

/// Whether the build that wrote a staging directory can still publish it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingStatus {
    /// A process holds the lock file's exclusive lock.
    Live,
    /// The lock file records this host and its lock is free, so the build
    /// that created it has stopped. The next build of the same name on this
    /// host removes it.
    Abandoned,
    /// Neither: the lock file is missing, or records another or no host name.
    Unknown,
}

/// A staging directory or lock file left beside a destination.
#[derive(Clone, Debug)]
pub struct StagingLeftover {
    /// The file name the staging directory would have been published under.
    pub published_name: String,
    pub staging: Option<PathBuf>,
    pub lock: Option<PathBuf>,
    pub status: StagingStatus,
}

/// Lists the staging directories and lock files immediately in `dir`, in
/// bytewise path order, without removing anything.
///
/// An entry with a staging name that is not a directory, or with a lock name
/// that is not a regular file, is skipped; a symbolic link is neither. A lock
/// file is locked and released while its status is probed.
pub fn staging_leftovers(dir: &Path) -> io::Result<Vec<StagingLeftover>> {
    let mut found = std::collections::BTreeMap::<(String, String), (bool, bool)>::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(rest) = name.strip_prefix('.') else {
            continue;
        };
        let kind = entry.file_type()?;
        for (marker, is_lock) in [(".staging-", false), (".stagelock-", true)] {
            if let Some((published, suffix)) = rest.rsplit_once(marker) {
                let expected = if is_lock {
                    kind.is_file()
                } else {
                    kind.is_dir()
                };
                if !expected {
                    break;
                }
                let slot = found
                    .entry((published.to_owned(), suffix.to_owned()))
                    .or_default();
                if is_lock {
                    slot.1 = true;
                } else {
                    slot.0 = true;
                }
                break;
            }
        }
    }
    let host = host_name();
    Ok(found
        .into_iter()
        .map(|((published_name, suffix), (has_staging, has_lock))| {
            let (staging, lock) = staging_paths(dir, &published_name, &suffix);
            let status = if !has_lock {
                StagingStatus::Unknown
            } else if crate::artifact::open_regular(File::options().read(true), &lock)
                .is_ok_and(|file| file.try_lock_shared().is_err())
            {
                StagingStatus::Live
            } else if host
                .as_deref()
                .is_some_and(|host| lock_if_abandoned(&lock, host).is_some())
            {
                StagingStatus::Abandoned
            } else {
                StagingStatus::Unknown
            };
            StagingLeftover {
                published_name,
                staging: has_staging.then_some(staging),
                lock: has_lock.then_some(lock),
                status,
            }
        })
        .collect())
}

#[cfg(unix)]
fn is_file_at(file: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), fs::symlink_metadata(path)) {
        (Ok(open), Ok(named)) => open.dev() == named.dev() && open.ino() == named.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn is_file_at(_file: &File, _path: &Path) -> bool {
    false
}

#[cfg(unix)]
fn host_name() -> Option<Vec<u8>> {
    let mut name = vec![0_u8; 256];
    // SAFETY: the pointer and length describe `name`, which outlives the call.
    if unsafe { libc::gethostname(name.as_mut_ptr().cast(), name.len()) } != 0 {
        return None;
    }
    let length = name.iter().position(|&byte| byte == 0)?;
    name.truncate(length);
    (!name.is_empty()).then_some(name)
}

#[cfg(not(unix))]
fn host_name() -> Option<Vec<u8>> {
    None
}

/// A directory beside a destination that is removed on drop unless it was
/// renamed into place.
#[derive(Debug)]
pub(crate) struct StagingDirectory {
    pub(crate) path: PathBuf,
    published: bool,
    _lock: StagingLock,
}

impl StagingDirectory {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn mark_published(&mut self) {
        self.published = true;
    }

    pub(crate) fn publish(&mut self, destination: &Path) -> Result<u64, PublicationError> {
        self.publish_with_ops(destination, &RealOps)
    }

    fn publish_with_ops(
        &mut self,
        destination: &Path,
        ops: &impl PublicationOps,
    ) -> Result<u64, PublicationError> {
        let bytes = fs::read_dir(&self.path)?
            .map(|entry| Ok(entry?.metadata()?.len()))
            .sum::<io::Result<u64>>()?;
        let published = commit_directory_with_ops(ops, self, destination, &[], || true, || true)?;
        debug_assert!(published, "unconditional publication guard must commit");
        Ok(bytes)
    }

    /// Creates a new, uniquely named staging directory under `parent` for `name`,
    /// retrying with a monotonic sequence counter if a candidate name already exists.
    pub(crate) fn create(parent: &Path, name: &str) -> io::Result<Self> {
        reclaim_orphaned_staging(parent, name);
        for _ in 0..1024 {
            let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let (path, lock) =
                staging_paths(parent, name, &format!("{}-{sequence}", std::process::id()));
            let Some(lock) = StagingLock::acquire(lock)? else {
                continue;
            };
            match fs::create_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        published: false,
                        _lock: lock,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a private staging name",
        ))
    }

    /// Creates a new, uniquely named directory beside `destination`, or
    /// returns `None` when `destination` has no file name.
    pub(crate) fn create_beside(destination: &Path) -> io::Result<Option<Self>> {
        let Some(name) = destination.file_name() else {
            return Ok(None);
        };
        let parent = destination_parent(destination);
        Self::create(parent, &name.to_string_lossy()).map(Some)
    }
}

impl std::ops::Deref for StagingDirectory {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.path()
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::{Arc, Barrier, Mutex},
    };

    use lingrep_core::{
        corpus::{
            AdmittedCorpusEvent, DiscoveryReport, DocumentId, InventoryStats, SourceDocument,
            TraversalHighWater, TraversalLimits,
        },
        spill::TempBudget,
    };

    use super::{PublicationOps, host_name, publish, publish_with_ops, staging_paths};
    use crate::fs_capability::{Faults, UNSUPPORTED_RENAME_ERRNOS};
    use crate::{
        AdmissionIdentity, AnalysisInputs, BuildConfig, CorpusArtifact, CorpusBuilder, CorpusError,
        DictionaryFingerprint, ReaderIdentity, artifact::CompletedStagingArtifact,
    };

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Event {
        Sync(PathBuf),
        Rename { from: PathBuf, to: PathBuf },
    }

    #[derive(Default)]
    struct RecordingOps {
        events: Mutex<Vec<Event>>,
        fail_sync: Option<PathBuf>,
        rename_errno: Option<i32>,
    }

    impl RecordingOps {
        fn failing_sync(path: PathBuf) -> Self {
            Self {
                fail_sync: Some(path),
                ..Self::default()
            }
        }

        fn failing_rename(errno: i32) -> Self {
            Self {
                rename_errno: Some(errno),
                ..Self::default()
            }
        }

        fn events(&self) -> Vec<Event> {
            self.events.lock().unwrap().clone()
        }
    }

    impl PublicationOps for RecordingOps {
        fn fsync(&self, path: &Path) -> std::io::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(Event::Sync(path.to_path_buf()));
            if self.fail_sync.as_deref() == Some(path) {
                return Err(std::io::Error::from_raw_os_error(libc::EIO));
            }
            std::fs::File::open(path)?.sync_all()
        }

        fn rename_noreplace(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            self.events.lock().unwrap().push(Event::Rename {
                from: from.to_path_buf(),
                to: to.to_path_buf(),
            });
            match self.rename_errno {
                Some(errno) => Err(std::io::Error::from_raw_os_error(errno)),
                None => crate::fs_capability::rename_noreplace(from, to),
            }
        }
    }

    #[test]
    fn staged_directory_publication_tracks_the_commit_point_on_sync_failure() {
        for after_commit in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("output");
            let mut staging = super::StagingDirectory::create_beside(&destination)
                .unwrap()
                .unwrap();
            std::fs::write(staging.path.join("component"), b"data").unwrap();
            let staging_path = staging.path.clone();
            let failure = if after_commit {
                directory.path().to_path_buf()
            } else {
                staging.path.clone()
            };
            let error = staging
                .publish_with_ops(&destination, &RecordingOps::failing_sync(failure))
                .unwrap_err();
            if after_commit {
                assert!(matches!(
                    error,
                    super::PublicationError::DurabilityUncertain { .. }
                ));
            } else {
                assert!(matches!(error, super::PublicationError::Io(_)));
            }
            drop(staging);
            assert!(!staging_path.exists());
            assert_eq!(destination.exists(), after_commit);
            if after_commit {
                assert_eq!(
                    std::fs::read(destination.join("component")).unwrap(),
                    b"data"
                );
            }
        }
    }

    struct BarrierOps {
        rename_barrier: Arc<Barrier>,
    }

    impl PublicationOps for BarrierOps {
        fn fsync(&self, path: &Path) -> std::io::Result<()> {
            std::fs::File::open(path)?.sync_all()
        }

        fn rename_noreplace(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            self.rename_barrier.wait();
            crate::fs_capability::rename_noreplace(from, to)
        }
    }

    fn build_config() -> BuildConfig {
        BuildConfig {
            // The fixture descriptor carries this build's own storage schema:
            // publication validates by opening what it wrote, and an artifact
            // recording a featurization no build produces cannot open.
            analysis: AnalysisInputs::new(
                ReaderIdentity::from_bytes([1; 32]),
                AdmissionIdentity::from_bytes([2; 32]),
                &crate::vibrato_fixture_descriptor(),
            ),
            analyzer: crate::AnalyzerRecord::vibrato_fixture(),
            dictionary: DictionaryFingerprint::from_bytes([3; 32]),
            trust: crate::TrustClass::LocallyExecuted,
            temp_budget: Arc::new(TempBudget::new(16 * 1024 * 1024)),
            sentence_metadata_keys: Vec::new(),
        }
    }

    fn discovery_report() -> DiscoveryReport {
        let mut digest = blake3::Hasher::new();
        digest.update(&("science.md".len() as u64).to_le_bytes());
        digest.update(b"science.md");
        DiscoveryReport {
            inventory: InventoryStats {
                documents: 1,
                distinct_targets: 1,
                alias_documents: 0,
                max_target_multiplicity: 1,
                external_targets: 0,
            },
            configured_limits: TraversalLimits::default(),
            observed: TraversalHighWater {
                max_depth: 0,
                examined_entries: 1,
                max_directory_entries: 1,
                index_bytes: 128,
            },
            ordered_inventory_digest: *digest.finalize().as_bytes(),
        }
    }

    async fn complete_staging(destination: &Path) -> CompletedStagingArtifact {
        let source = SourceDocument {
            ordinal: 0,
            logical_name: "science.md".into(),
            path: PathBuf::from("/unopened/science.md"),
            target_group_ordinal: 0,
            external_target: false,
        };
        let mut builder = CorpusBuilder::begin(build_config(), destination)
            .await
            .unwrap();
        builder.register_source_document(&source).unwrap();
        let observation = builder.source_observation();
        observation.begin_source(&source.logical_name).unwrap();
        observation.update_source_bytes(b"source bytes").unwrap();
        observation.end_source().unwrap();
        builder
            .push_corpus_event(&AdmittedCorpusEvent::DocumentStart {
                id: DocumentId::new(0),
                source_name: source.logical_name,
                metadata: Box::new([]),
            })
            .await
            .unwrap();
        builder
            .push_corpus_event(&AdmittedCorpusEvent::DocumentEnd {
                id: DocumentId::new(0),
            })
            .await
            .unwrap();
        builder.finalize_staging(discovery_report()).await.unwrap()
    }

    #[tokio::test]
    async fn publication_orders_component_staging_rename_and_parent_durability() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("science.lingcorpus");
        let mut completed = complete_staging(&destination).await;
        let staging = completed.path().to_path_buf();
        let ops = RecordingOps::default();

        publish_with_ops(&ops, &mut completed).unwrap();

        assert_eq!(
            ops.events(),
            vec![
                Event::Sync(staging.join("tokens.v1.zst")),
                Event::Sync(staging.join("locations.v1")),
                Event::Sync(staging.join("provenance.sqlite")),
                Event::Sync(staging.join("postings.v1")),
                Event::Sync(staging.join("manifest.json")),
                Event::Sync(staging.clone()),
                Event::Rename {
                    from: staging,
                    to: destination.clone(),
                },
                Event::Sync(directory.path().to_path_buf()),
            ]
        );
        CorpusArtifact::open(&destination)
            .unwrap()
            .verify_integrity()
            .await
            .unwrap();
    }

    /// Paths under `directory` that this process holds as a file descriptor or a mapping.
    #[cfg(target_os = "linux")]
    fn held_paths_under(directory: &Path) -> Vec<PathBuf> {
        let mut held = std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(|entry| std::fs::read_link(entry.unwrap().path()).ok())
            .chain(
                std::fs::read_to_string("/proc/self/maps")
                    .unwrap()
                    .lines()
                    .filter_map(|line| line.split_once('/').map(|(_, path)| format!("/{path}")))
                    .map(PathBuf::from),
            )
            .filter(|path| path.starts_with(directory))
            .collect::<Vec<_>>();
        held.sort();
        held.dedup();
        held
    }

    /// Fails the rename while the process holds anything under the staging directory, as
    /// Windows does for a directory renamed through WSL's drvfs.
    #[cfg(target_os = "linux")]
    struct RenameRequiresNothingHeld {
        held_at_rename: Mutex<Option<Vec<PathBuf>>>,
    }

    #[cfg(target_os = "linux")]
    impl PublicationOps for RenameRequiresNothingHeld {
        fn fsync(&self, path: &Path) -> std::io::Result<()> {
            std::fs::File::open(path)?.sync_all()
        }

        fn rename_noreplace(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            let held = held_paths_under(from);
            let refuse = !held.is_empty();
            *self.held_at_rename.lock().unwrap() = Some(held);
            if refuse {
                return Err(std::io::Error::from_raw_os_error(libc::EACCES));
            }
            crate::fs_capability::rename_noreplace(from, to)
        }
    }

    #[tokio::test]
    #[cfg(target_os = "linux")]
    async fn validated_publication_holds_nothing_under_staging_at_the_rename() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("science.lingcorpus");
        let mut completed = complete_staging(&destination).await;
        let ops = RenameRequiresNothingHeld {
            held_at_rename: Mutex::new(None),
        };

        let published =
            crate::artifact::publish_validated(&ops, &mut completed, None, || true, || true);

        assert_eq!(*ops.held_at_rename.lock().unwrap(), Some(Vec::new()));
        let artifact = published.unwrap().unwrap();
        assert_eq!(artifact.directory(), destination);
        artifact.verify_integrity().await.unwrap();
    }

    #[tokio::test]
    async fn pre_rename_fsync_failure_leaves_no_destination_and_cleans_staging() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("science.lingcorpus");
        let mut completed = complete_staging(&destination).await;
        let staging = completed.path().to_path_buf();
        let ops = RecordingOps::failing_sync(staging.clone());

        let error = publish_with_ops(&ops, &mut completed).unwrap_err();

        assert!(matches!(error, CorpusError::Io(_)));
        assert!(!destination.exists());
        assert!(staging.exists());
        assert!(
            ops.events()
                .iter()
                .all(|event| !matches!(event, Event::Rename { .. }))
        );
        drop(completed);
        assert!(!staging.exists());
    }

    #[tokio::test]
    async fn parent_fsync_failure_preserves_commit_for_inspect_retain_or_remove() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("science.lingcorpus");
        let mut completed = complete_staging(&destination).await;
        let staging = completed.path().to_path_buf();
        let ops = RecordingOps::failing_sync(directory.path().to_path_buf());

        let error = publish_with_ops(&ops, &mut completed).unwrap_err();

        assert!(matches!(
            error,
            CorpusError::PublicationDurabilityUncertain { ref path }
                if path == &destination
        ));
        assert_eq!(error.code(), "publication_durability_uncertain");
        assert!(!staging.exists());
        drop(completed);

        let artifact = CorpusArtifact::open(&destination).unwrap();
        artifact.verify_integrity().await.unwrap();
        drop(artifact);
        assert!(destination.is_dir(), "inspection must retain the artifact");

        let mut retry = complete_staging(&destination).await;
        let retry_staging = retry.path().to_path_buf();
        assert!(matches!(
            publish(&mut retry),
            Err(CorpusError::DestinationExists { ref path }) if path == &destination
        ));
        drop(retry);
        assert!(!retry_staging.exists());
        assert!(destination.is_dir());

        std::fs::remove_dir_all(&destination).unwrap();
        assert!(!destination.exists());
    }

    #[tokio::test]
    async fn an_unsupported_errno_from_the_rename_is_reported_as_unsupported_publication() {
        // Driven by the production set so a platform that adds an errno — macOS
        // adds ENOTSUP — is covered without restating the list.
        for &errno in UNSUPPORTED_RENAME_ERRNOS {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("science.lingcorpus");
            let mut completed = complete_staging(&destination).await;
            let staging = completed.path().to_path_buf();
            let ops = RecordingOps::failing_rename(errno);

            let error = publish_with_ops(&ops, &mut completed).unwrap_err();

            assert!(matches!(
                error,
                CorpusError::UnsupportedPublication {
                    ref path,
                    errno: found,
                } if path == &destination && found == errno
            ));
            assert!(!destination.exists());
            assert_eq!(
                ops.events()
                    .iter()
                    .filter(|event| matches!(event, Event::Rename { .. }))
                    .count(),
                1
            );
            drop(completed);
            assert!(!staging.exists());
        }
    }

    #[tokio::test]
    async fn a_failed_rename_names_the_staging_directory_and_the_destination() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("science.lingcorpus");
        let mut completed = complete_staging(&destination).await;
        let staging = completed.path().to_path_buf();

        let error = publish_with_ops(&RecordingOps::failing_rename(libc::EACCES), &mut completed)
            .unwrap_err();

        assert_eq!(error.code(), "io");
        let message = error.to_string();
        let expected = format!(
            "cannot rename '{}' to '{}': {}",
            staging.display(),
            destination.display(),
            std::io::Error::from_raw_os_error(libc::EACCES)
        );
        assert!(message.contains(&expected), "{message}");
        assert!(!destination.exists());
    }

    /// The errno of `error` or of the I/O error it names a path for.
    fn os_error(error: &std::io::Error) -> Option<i32> {
        error.raw_os_error().or_else(|| {
            std::error::Error::source(error)?
                .downcast_ref::<std::io::Error>()?
                .raw_os_error()
        })
    }

    /// Publication operations whose no-replace rename runs through the seam with `faults`.
    struct FaultedOps(Faults);

    impl PublicationOps for FaultedOps {
        fn fsync(&self, path: &Path) -> std::io::Result<()> {
            std::fs::File::open(path)?.sync_all()
        }

        fn rename_noreplace(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            crate::fs_capability::rename_noreplace_with(&self.0, from, to)
        }
    }

    fn no_replace_rename_failing(errno: i32) -> FaultedOps {
        FaultedOps(Faults {
            rename_noreplace: Some(errno),
            ..Faults::default()
        })
    }

    #[tokio::test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    async fn corpus_publication_falls_back_when_no_replace_rename_is_unsupported() {
        for &errno in UNSUPPORTED_RENAME_ERRNOS {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("science.lingcorpus");
            let mut completed = complete_staging(&destination).await;
            let staging = completed.path().to_path_buf();

            publish_with_ops(&no_replace_rename_failing(errno), &mut completed).unwrap();
            drop(completed);

            assert!(!staging.exists(), "errno {errno}");
            CorpusArtifact::open(&destination).unwrap();
        }
    }

    #[tokio::test]
    async fn corpus_publication_reports_other_rename_errors_without_a_fallback() {
        for errno in [libc::EPERM, libc::EACCES, libc::EXDEV] {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("science.lingcorpus");
            let mut completed = complete_staging(&destination).await;
            let staging = completed.path().to_path_buf();

            let error =
                publish_with_ops(&no_replace_rename_failing(errno), &mut completed).unwrap_err();
            drop(completed);

            assert!(
                matches!(error, CorpusError::Io(ref source) if os_error(source) == Some(errno)),
                "errno {errno}: {error:?}"
            );
            assert!(!destination.exists());
            assert!(!staging.exists());
        }
    }

    #[tokio::test]
    async fn corpus_publication_reports_a_destination_taken_during_rename() {
        for errno in [libc::EEXIST, libc::ENOTEMPTY] {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("science.lingcorpus");
            let mut completed = complete_staging(&destination).await;

            let error =
                publish_with_ops(&no_replace_rename_failing(errno), &mut completed).unwrap_err();

            assert!(matches!(
                error,
                CorpusError::DestinationExists { ref path } if path == &destination
            ));
        }
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn staged_directory_publication_falls_back_or_reports_why_it_could_not() {
        for (errno, fallback_rename, expected) in [
            (libc::EINVAL, None, Ok(())),
            (libc::ENOSYS, None, Ok(())),
            (libc::EOPNOTSUPP, None, Ok(())),
            (libc::EINVAL, Some(libc::EXDEV), Err("io")),
            (libc::EINVAL, Some(libc::EACCES), Err("io")),
            (libc::EINVAL, Some(libc::EINVAL), Err("unsupported")),
            (libc::EEXIST, None, Err("exists")),
            (libc::EPERM, None, Err("io")),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("output");
            let mut staging = super::StagingDirectory::create_beside(&destination)
                .unwrap()
                .unwrap();
            std::fs::write(staging.path.join("component"), b"data").unwrap();
            let staging_path = staging.path.clone();
            let ops = FaultedOps(Faults {
                rename_noreplace: Some(errno),
                rename: fallback_rename,
                ..Faults::default()
            });

            let result = staging.publish_with_ops(&destination, &ops);
            drop(staging);

            let case = (errno, fallback_rename);
            match (result, expected) {
                (Ok(bytes), Ok(())) => {
                    assert_eq!(bytes, 4, "{case:?}");
                    assert_eq!(
                        std::fs::read(destination.join("component")).unwrap(),
                        b"data"
                    );
                }
                (Err(super::PublicationError::Io(source)), Err("io")) => {
                    assert_eq!(os_error(&source), fallback_rename.or(Some(errno)));
                    assert!(!destination.exists(), "{case:?}");
                }
                (
                    Err(super::PublicationError::Unsupported { errno: found, .. }),
                    Err("unsupported"),
                ) => {
                    assert_eq!(found, libc::EINVAL);
                    assert!(!destination.exists(), "{case:?}");
                }
                (Err(super::PublicationError::DestinationExists { path }), Err("exists")) => {
                    assert_eq!(path, destination);
                }
                (result, expected) => panic!("{case:?}: {result:?}, expected {expected:?}"),
            }
            assert!(!staging_path.exists(), "{case:?}");
        }
    }

    fn killed_builder_leftovers(parent: &Path, suffix: &str, host: &[u8]) -> (PathBuf, PathBuf) {
        let (staging, lock) = staging_paths(parent, "science.lingcorpus", suffix);
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("tokens.v1.zst"), b"partial").unwrap();
        std::fs::write(&lock, host).unwrap();
        (staging, lock)
    }

    #[tokio::test]
    async fn a_build_reclaims_only_the_staging_of_a_dead_builder_on_this_host() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("science.lingcorpus");
        let (dead, dead_lock) =
            killed_builder_leftovers(directory.path(), "1-0", &host_name().unwrap());
        let (other_host, other_host_lock) =
            killed_builder_leftovers(directory.path(), "1-1", b"another-host.invalid");
        let (other_artifact, _) = staging_paths(directory.path(), "history.lingcorpus", "1-0");
        std::fs::create_dir(&other_artifact).unwrap();
        let unlocked = directory.path().join(".science.lingcorpus.staging-1-2");
        std::fs::create_dir(&unlocked).unwrap();
        let mut live = complete_staging(&destination).await;
        let live_staging = live.path().to_path_buf();

        drop(complete_staging(&destination).await);

        assert!(!dead.exists() && !dead_lock.exists());
        assert!(other_host.exists() && other_host_lock.exists());
        assert!(other_artifact.exists());
        assert!(unlocked.exists());
        assert!(live_staging.exists());
        publish(&mut live).unwrap();
        CorpusArtifact::open(&destination)
            .unwrap()
            .verify_integrity()
            .await
            .unwrap();
    }

    /// The leftover report classifies with the reclaim's own criteria and
    /// removes nothing.
    #[tokio::test]
    async fn staging_leftovers_classify_each_lock_without_removing_anything() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("science.lingcorpus");
        let live = complete_staging(&destination).await;
        let (dead, dead_lock) =
            killed_builder_leftovers(directory.path(), "1-0", &host_name().unwrap());
        let (other_host, _) =
            killed_builder_leftovers(directory.path(), "1-1", b"another-host.invalid");
        let unlocked = directory.path().join(".science.lingcorpus.staging-1-2");
        std::fs::create_dir(&unlocked).unwrap();

        let leftovers = super::staging_leftovers(directory.path()).unwrap();
        let status = |staging: &Path| {
            leftovers
                .iter()
                .find(|leftover| leftover.staging.as_deref() == Some(staging))
                .map(|leftover| leftover.status)
        };
        assert_eq!(leftovers.len(), 4, "{leftovers:?}");
        assert!(
            leftovers
                .iter()
                .all(|leftover| leftover.published_name == "science.lingcorpus")
        );
        assert_eq!(status(&dead), Some(super::StagingStatus::Abandoned));
        assert_eq!(status(&other_host), Some(super::StagingStatus::Unknown));
        assert_eq!(status(&unlocked), Some(super::StagingStatus::Unknown));
        assert_eq!(status(live.path()), Some(super::StagingStatus::Live));
        assert!(dead.exists() && dead_lock.exists() && other_host.exists());
    }

    #[tokio::test]
    async fn concurrent_rename_contenders_publish_exactly_one_complete_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("science.lingcorpus");
        let first = complete_staging(&destination).await;
        let second = complete_staging(&destination).await;
        let first_staging = first.path().to_path_buf();
        let second_staging = second.path().to_path_buf();
        let barrier = Arc::new(Barrier::new(2));

        let ((first_result, first), (second_result, second)) = std::thread::scope(|scope| {
            let first_barrier = Arc::clone(&barrier);
            let first = scope.spawn(move || {
                let mut completed = first;
                let result = publish_with_ops(
                    &BarrierOps {
                        rename_barrier: first_barrier,
                    },
                    &mut completed,
                );
                (result, completed)
            });
            let second_barrier = Arc::clone(&barrier);
            let second = scope.spawn(move || {
                let mut completed = second;
                let result = publish_with_ops(
                    &BarrierOps {
                        rename_barrier: second_barrier,
                    },
                    &mut completed,
                );
                (result, completed)
            });
            (first.join().unwrap(), second.join().unwrap())
        });

        assert_eq!(
            usize::from(first_result.is_ok()) + usize::from(second_result.is_ok()),
            1
        );
        let loser = first_result
            .err()
            .or_else(|| second_result.err())
            .expect("one contender must lose");
        assert!(matches!(
            loser,
            CorpusError::DestinationExists { ref path } if path == &destination
        ));
        drop((first, second));
        assert!(!first_staging.exists());
        assert!(!second_staging.exists());
        CorpusArtifact::open(&destination)
            .unwrap()
            .verify_integrity()
            .await
            .unwrap();
    }

    #[test]
    fn create_beside_allocates_distinct_staging_directories_without_colliding() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("output");
        let staged = (0..16)
            .map(|_| {
                super::StagingDirectory::create_beside(&destination)
                    .unwrap()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let paths = staged
            .iter()
            .map(|dir| dir.path().to_path_buf())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(paths.len(), 16);
        assert!(paths.iter().all(|path| path.is_dir()));
        drop(staged);
        assert!(paths.iter().all(|path| !path.exists()));
    }
}
