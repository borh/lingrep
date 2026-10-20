//! Bounded, deterministic discovery of corpus documents.
//!
//! Discovery follows ordinary and symlinked directories in lexical depth-first
//! order. It deliberately retains only the active directory ancestry, one
//! directory's sorted relevant entries, and a bounded file-identity map.

use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use thiserror::Error;

type FileIdentity = (u64, u64);

const IDENTITY_BYTES: usize = std::mem::size_of::<FileIdentity>();
const HASHMAP_BUCKET_BYTES: usize = std::mem::size_of::<(FileIdentity, TargetGroup)>() + 1;

/// Explicit resource bounds for one corpus-tree traversal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraversalLimits {
    pub max_depth: usize,
    pub max_source_entries: u64,
    pub max_directory_entries: usize,
    pub max_documents: u64,
    pub max_index_bytes: usize,
}

impl Default for TraversalLimits {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_source_entries: 100_000_000,
            max_directory_entries: 1_000_000,
            max_documents: 10_000_000,
            max_index_bytes: 536_870_912,
        }
    }
}

/// One lexical document admitted from a corpus root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDocument {
    pub ordinal: u64,
    pub logical_name: String,
    pub path: PathBuf,
    pub target_group_ordinal: u64,
    pub external_target: bool,
}

/// Aggregate target-alias facts observed during traversal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InventoryStats {
    pub documents: u64,
    pub distinct_targets: u64,
    pub alias_documents: u64,
    pub max_target_multiplicity: u64,
    pub external_targets: u64,
}

/// Maximum traversal resources observed before successful completion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TraversalHighWater {
    pub max_depth: usize,
    pub examined_entries: u64,
    pub max_directory_entries: usize,
    pub index_bytes: usize,
}

/// The completed deterministic source inventory summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryReport {
    pub inventory: InventoryStats,
    pub configured_limits: TraversalLimits,
    pub observed: TraversalHighWater,
    pub ordered_inventory_digest: [u8; 32],
}

/// Typed failures from source discovery.
#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("directory cycle at {logical_path}")]
    Cycle { logical_path: PathBuf },
    #[error("dangling symbolic link at {logical_path}")]
    DanglingLink { logical_path: PathBuf },
    #[error("non-UTF-8 logical path component at {path}")]
    NonUtf8Name { path: PathBuf },
    #[error("traversal limit {field}={limit} exceeded at {logical_path} (observed {observed})")]
    Limit {
        field: &'static str,
        limit: u64,
        observed: u64,
        logical_path: PathBuf,
    },
    #[error("I/O error at {logical_path}: {source}")]
    IoAt {
        logical_path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl DiscoveryError {
    /// Convert a discovery failure for APIs that expose `io::Result`.
    pub(crate) fn into_io(self) -> std::io::Error {
        let kind = match &self {
            Self::IoAt { source, .. } => source.kind(),
            _ => std::io::ErrorKind::InvalidInput,
        };
        std::io::Error::new(kind, self)
    }
}

#[derive(Debug)]
struct TargetGroup {
    ordinal: u64,
    count: u64,
}

#[derive(Debug)]
struct DirectoryEntry {
    path: PathBuf,
    logical_name: String,
    target: Option<Metadata>,
    kind: EntryKind,
    retained_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Directory,
    DocumentFile,
    DeferredSymlink,
}

/// Which regular files a directory traversal admits as corpus documents.
///
/// A root that is already a regular file is admitted as-is; this selection is
/// used only while traversing a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusFileKind {
    Markdown,
    Conllu,
}

impl CorpusFileKind {
    fn matches(self, name: &std::ffi::OsStr) -> bool {
        let Some(extension) = Path::new(name).extension().and_then(|value| value.to_str()) else {
            return false;
        };
        match self {
            // Preserve the established directory contract: `.markdown` is a
            // single-file inference only; directory discovery selects `.md`.
            Self::Markdown => extension.eq_ignore_ascii_case("md"),
            Self::Conllu => extension.eq_ignore_ascii_case("conllu"),
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::Markdown => "Markdown",
            Self::Conllu => "CoNLL-U",
        }
    }
}

#[derive(Debug)]
struct DirectoryFrame {
    entries: std::vec::IntoIter<DirectoryEntry>,
    identity: FileIdentity,
    depth: usize,
    retained_bytes: usize,
}

/// A streaming, bounded recursive corpus document enumerator.
pub struct CorpusDiscovery {
    resolved_root: PathBuf,
    limits: TraversalLimits,
    frames: Vec<DirectoryFrame>,
    root_file: Option<DirectoryEntry>,
    targets: HashMap<FileIdentity, TargetGroup>,
    next_ordinal: u64,
    inventory: InventoryStats,
    observed: TraversalHighWater,
    index_bytes: usize,
    digest: blake3::Hasher,
    file_kind: CorpusFileKind,
}

/// A followed and bounded root classification that has not enumerated any
/// directory entries.
///
/// Keeping this ticket opaque lets commands validate the direct root before
/// publishing a run while deferring directory reads and recursive discovery
/// until execution starts.
#[derive(Debug)]
pub struct ValidatedCorpusRoot {
    root: PathBuf,
    resolved_root: PathBuf,
    limits: TraversalLimits,
    target: Metadata,
    kind: ValidatedRootKind,
}

#[derive(Debug)]
enum ValidatedRootKind {
    Directory,
    File {
        logical_name: String,
        retained_bytes: usize,
    },
}

impl ValidatedCorpusRoot {
    /// Whether the followed root target is a directory.
    #[must_use]
    pub fn is_directory(&self) -> bool {
        matches!(self.kind, ValidatedRootKind::Directory)
    }

    /// The root path as passed in (the format readers open files under it).
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl CorpusDiscovery {
    /// Open `root` for bounded lexical discovery. A regular file is one
    /// document; a directory admits Markdown files recursively.
    pub fn new(root: impl AsRef<Path>, limits: TraversalLimits) -> Result<Self, DiscoveryError> {
        Self::new_for_kind(root, limits, CorpusFileKind::Markdown)
    }

    /// Open `root` for bounded lexical discovery using one directory file
    /// selection. A regular-file root is admitted as-is.
    pub fn new_for_kind(
        root: impl AsRef<Path>,
        limits: TraversalLimits,
        file_kind: CorpusFileKind,
    ) -> Result<Self, DiscoveryError> {
        let validated = Self::validate_root(root, limits)?;
        Self::from_validated_root_for_kind(validated, file_kind)
    }

    /// Validate only the selected root without reading directory children.
    ///
    /// Followed metadata, canonicalization, root kind, root-file UTF-8 naming,
    /// and the minimum root traversal-index residency are checked here.
    pub fn validate_root(
        root: impl AsRef<Path>,
        limits: TraversalLimits,
    ) -> Result<ValidatedCorpusRoot, DiscoveryError> {
        let root = root.as_ref().to_path_buf();
        let root_logical = PathBuf::from(".");
        let target = followed_metadata(&root, &root_logical)?;
        let resolved_root = fs::canonicalize(&root).map_err(|error| io_at(&root_logical, error))?;
        let kind = if target.is_dir() {
            check_root_index_limit(&limits, IDENTITY_BYTES)?;
            ValidatedRootKind::Directory
        } else if target.is_file() {
            let logical_name = root
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| DiscoveryError::NonUtf8Name {
                    path: root_logical.clone(),
                })?
                .to_owned();
            let retained_bytes = path_and_name_capacity(&root, &logical_name);
            check_root_index_limit(&limits, retained_bytes)?;
            ValidatedRootKind::File {
                logical_name,
                retained_bytes,
            }
        } else {
            return Err(io_at(
                &root_logical,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "corpus path is neither a regular file nor a directory",
                ),
            ));
        };

        Ok(ValidatedCorpusRoot {
            root,
            resolved_root,
            limits,
            target,
            kind,
        })
    }

    /// Open a discovery cursor from a root-only validation ticket.
    ///
    /// For directory roots this is the first operation that reads and sorts the
    /// root directory; recursive child directories remain deferred to
    /// [`Self::next_document`].
    pub fn from_validated_root(validated: ValidatedCorpusRoot) -> Result<Self, DiscoveryError> {
        Self::from_validated_root_for_kind(validated, CorpusFileKind::Markdown)
    }

    /// Open a discovery cursor with an explicit directory file selection.
    pub fn from_validated_root_for_kind(
        validated: ValidatedCorpusRoot,
        file_kind: CorpusFileKind,
    ) -> Result<Self, DiscoveryError> {
        let ValidatedCorpusRoot {
            root,
            resolved_root,
            limits,
            target,
            kind,
        } = validated;
        let mut discovery = Self {
            resolved_root,
            limits,
            frames: Vec::new(),
            root_file: None,
            targets: HashMap::new(),
            next_ordinal: 0,
            inventory: InventoryStats::default(),
            observed: TraversalHighWater::default(),
            index_bytes: 0,
            digest: blake3::Hasher::new(),
            file_kind,
        };

        match kind {
            ValidatedRootKind::Directory => {
                discovery.push_directory(root, String::new(), target, 0)?;
            }
            ValidatedRootKind::File {
                logical_name,
                retained_bytes,
            } => {
                discovery.root_file = Some(DirectoryEntry {
                    path: root,
                    retained_bytes,
                    logical_name,
                    target: Some(target),
                    kind: EntryKind::DocumentFile,
                });
                discovery.charge(retained_bytes, "")?;
            }
        }
        Ok(discovery)
    }

    /// Return the next document in canonical per-directory depth-first order.
    pub fn next_document(&mut self) -> Result<Option<SourceDocument>, DiscoveryError> {
        if let Some(entry) = self.root_file.take() {
            self.release(entry.retained_bytes);
            return self.emit_document(entry).map(Some);
        }

        loop {
            if self.frames.is_empty() {
                return Ok(None);
            }
            let next = {
                let frame = self.frames.last_mut().expect("frame was present");
                frame.entries.next().map(|entry| {
                    frame.retained_bytes -= entry.retained_bytes;
                    (frame.depth, entry)
                })
            };
            let Some((parent_depth, entry)) = next else {
                let frame = self.frames.pop().expect("frame was present");
                self.release(frame.retained_bytes + IDENTITY_BYTES);
                continue;
            };
            self.release(entry.retained_bytes);
            let depth = parent_depth + 1;
            self.observed.max_depth = self.observed.max_depth.max(depth);
            self.check_usize_limit(
                "max_depth",
                self.limits.max_depth,
                depth,
                &entry.logical_name,
            )?;
            match entry.kind {
                EntryKind::DocumentFile => return self.emit_document(entry).map(Some),
                EntryKind::Directory => self.push_directory(
                    entry.path,
                    entry.logical_name,
                    entry
                        .target
                        .expect("ordinary directory has followed metadata"),
                    depth,
                )?,
                EntryKind::DeferredSymlink => {
                    let target = followed_metadata(&entry.path, Path::new(&entry.logical_name))?;
                    if target.is_dir() {
                        self.push_directory(entry.path, entry.logical_name, target, depth)?;
                    } else if target.is_file()
                        && self
                            .file_kind
                            .matches(entry.path.file_name().expect("read_dir entry has a name"))
                    {
                        return self
                            .emit_document(DirectoryEntry {
                                target: Some(target),
                                kind: EntryKind::DocumentFile,
                                ..entry
                            })
                            .map(Some);
                    }
                }
            }
        }
    }

    /// Finish discovery after the iterator has reached EOF and return its
    /// completed inventory report.
    pub fn finish(self) -> Result<DiscoveryReport, DiscoveryError> {
        if self.root_file.is_some() || !self.frames.is_empty() {
            return Err(io_at(
                Path::new("."),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "discovery must be exhausted before finish",
                ),
            ));
        }
        if self.inventory.documents == 0 {
            return Err(io_at(
                Path::new("."),
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "{} corpus directory contains no {} files",
                        self.file_kind.description(),
                        self.file_kind.description()
                    ),
                ),
            ));
        }
        let ordered_inventory_digest = *self.digest.finalize().as_bytes();
        Ok(DiscoveryReport {
            inventory: self.inventory,
            configured_limits: self.limits,
            observed: self.observed,
            ordered_inventory_digest,
        })
    }

    fn push_directory(
        &mut self,
        path: PathBuf,
        logical_directory: String,
        metadata: Metadata,
        depth: usize,
    ) -> Result<(), DiscoveryError> {
        let identity = identity(&metadata);
        if self.frames.iter().any(|frame| frame.identity == identity) {
            return Err(DiscoveryError::Cycle {
                logical_path: logical_path(&logical_directory),
            });
        }
        self.charge(IDENTITY_BYTES, &logical_directory)?;

        let mut entries = Vec::new();
        let mut directory_entries = 0usize;
        let directory_logical_path = logical_path(&logical_directory);
        let directory =
            fs::read_dir(&path).map_err(|error| io_at(&directory_logical_path, error))?;
        for entry in directory {
            let entry = entry.map_err(|error| io_at(&directory_logical_path, error))?;
            let entry_logical_path = join_logical_path(&logical_directory, &entry.file_name());
            directory_entries += 1;
            self.check_usize_limit(
                "max_directory_entries",
                self.limits.max_directory_entries,
                directory_entries,
                &entry_logical_path,
            )?;
            self.observed.examined_entries += 1;
            self.check_u64_limit(
                "max_source_entries",
                self.limits.max_source_entries,
                self.observed.examined_entries,
                &entry_logical_path,
            )?;

            if let Some(entry) = self.classify_entry(entry.path(), entry_logical_path)? {
                let old_capacity = entries.capacity();
                self.charge(entry.retained_bytes, &entry.logical_name)?;
                entries.push(entry);
                self.charge(
                    entries
                        .capacity()
                        .saturating_sub(old_capacity)
                        .saturating_mul(std::mem::size_of::<DirectoryEntry>()),
                    &logical_directory,
                )?;
            }
        }
        self.observed.max_directory_entries =
            self.observed.max_directory_entries.max(directory_entries);
        entries.sort_by(|left, right| {
            left.logical_name
                .as_bytes()
                .cmp(right.logical_name.as_bytes())
        });
        let retained_bytes = entries
            .capacity()
            .saturating_mul(std::mem::size_of::<DirectoryEntry>())
            + entries
                .iter()
                .map(|entry| entry.retained_bytes)
                .sum::<usize>();
        let old_capacity = self.frames.capacity();
        self.frames.push(DirectoryFrame {
            entries: entries.into_iter(),
            identity,
            depth,
            retained_bytes,
        });
        self.charge(
            self.frames
                .capacity()
                .saturating_sub(old_capacity)
                .saturating_mul(std::mem::size_of::<DirectoryFrame>()),
            &logical_directory,
        )?;
        Ok(())
    }

    fn classify_entry(
        &self,
        path: PathBuf,
        logical_path: PathBuf,
    ) -> Result<Option<DirectoryEntry>, DiscoveryError> {
        let link_metadata =
            fs::symlink_metadata(&path).map_err(|error| io_at(&logical_path, error))?;
        let name = path.file_name().expect("read_dir entry has a name");
        let is_symlink = link_metadata.file_type().is_symlink();
        let is_directory = link_metadata.is_dir();
        let is_document = link_metadata.is_file() && self.file_kind.matches(name);
        if is_symlink {
            if name.to_str().is_none() {
                if self
                    .file_kind
                    .matches(path.file_name().expect("read_dir entry has a name"))
                {
                    return Err(DiscoveryError::NonUtf8Name { path: logical_path });
                }
                return match fs::metadata(&path) {
                    Ok(target) if target.is_dir() => {
                        Err(DiscoveryError::NonUtf8Name { path: logical_path })
                    }
                    Ok(_) => Ok(None),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(io_at(&logical_path, error)),
                };
            }
            let logical_name = logical_path
                .to_str()
                .expect("a UTF-8 parent plus UTF-8 name is UTF-8")
                .to_owned();
            return Ok(Some(DirectoryEntry {
                retained_bytes: path_and_name_capacity(&path, &logical_name),
                path,
                logical_name,
                target: None,
                kind: EntryKind::DeferredSymlink,
            }));
        }
        if !is_directory && !is_document {
            return Ok(None);
        }
        if name.to_str().is_none() {
            return Err(DiscoveryError::NonUtf8Name {
                path: logical_path.clone(),
            });
        }
        let logical_name = logical_path
            .to_str()
            .expect("a UTF-8 parent plus UTF-8 name is UTF-8")
            .to_owned();
        Ok(Some(DirectoryEntry {
            retained_bytes: path_and_name_capacity(&path, &logical_name),
            path,
            logical_name,
            target: Some(link_metadata),
            kind: if is_directory {
                EntryKind::Directory
            } else {
                EntryKind::DocumentFile
            },
        }))
    }

    fn emit_document(&mut self, entry: DirectoryEntry) -> Result<SourceDocument, DiscoveryError> {
        let observed_documents = self.inventory.documents + 1;
        self.check_u64_limit(
            "max_documents",
            self.limits.max_documents,
            observed_documents,
            &entry.logical_name,
        )?;
        let identity = identity(
            entry
                .target
                .as_ref()
                .expect("emitted document has followed metadata"),
        );
        let target_group_ordinal = if let Some(group) = self.targets.get_mut(&identity) {
            group.count += 1;
            group.ordinal
        } else {
            let ordinal = self.inventory.distinct_targets;
            let old_capacity = self.targets.capacity();
            self.targets
                .insert(identity, TargetGroup { ordinal, count: 1 });
            self.charge(
                hashmap_capacity_bytes(self.targets.capacity())
                    .saturating_sub(hashmap_capacity_bytes(old_capacity)),
                &entry.logical_name,
            )?;
            self.inventory.distinct_targets += 1;
            ordinal
        };
        let target_count = self
            .targets
            .get(&identity)
            .expect("target group was inserted or found")
            .count;
        self.inventory.documents = observed_documents;
        self.inventory.alias_documents = self.inventory.documents - self.inventory.distinct_targets;
        self.inventory.max_target_multiplicity =
            self.inventory.max_target_multiplicity.max(target_count);

        let canonical_target = fs::canonicalize(&entry.path)
            .map_err(|error| io_at(Path::new(&entry.logical_name), error))?;
        let external_target = !canonical_target.starts_with(&self.resolved_root);
        if external_target {
            self.inventory.external_targets += 1;
        }
        self.digest
            .update(&(entry.logical_name.len() as u64).to_le_bytes());
        self.digest.update(entry.logical_name.as_bytes());
        let document = SourceDocument {
            ordinal: self.next_ordinal,
            logical_name: entry.logical_name,
            path: entry.path,
            target_group_ordinal,
            external_target,
        };
        self.next_ordinal += 1;
        Ok(document)
    }

    fn charge(&mut self, bytes: usize, logical_name: &str) -> Result<(), DiscoveryError> {
        let observed = self.index_bytes.saturating_add(bytes);
        self.observed.index_bytes = self.observed.index_bytes.max(observed);
        self.check_usize_limit(
            "max_index_bytes",
            self.limits.max_index_bytes,
            observed,
            logical_path(logical_name),
        )?;
        self.index_bytes = observed;
        Ok(())
    }

    fn release(&mut self, bytes: usize) {
        self.index_bytes = self.index_bytes.saturating_sub(bytes);
    }

    fn check_usize_limit(
        &self,
        field: &'static str,
        limit: usize,
        observed: usize,
        logical_path: impl AsRef<Path>,
    ) -> Result<(), DiscoveryError> {
        self.check_u64_limit(field, limit as u64, observed as u64, logical_path)
    }

    fn check_u64_limit(
        &self,
        field: &'static str,
        limit: u64,
        observed: u64,
        logical_path: impl AsRef<Path>,
    ) -> Result<(), DiscoveryError> {
        if observed > limit {
            return Err(DiscoveryError::Limit {
                field,
                limit,
                observed,
                logical_path: logical_path.as_ref().to_path_buf(),
            });
        }
        Ok(())
    }
}

fn followed_metadata(path: &Path, logical_path: &Path) -> Result<Metadata, DiscoveryError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let is_link =
                fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink());
            if is_link {
                Err(DiscoveryError::DanglingLink {
                    logical_path: logical_path.to_path_buf(),
                })
            } else {
                Err(io_at(logical_path, error))
            }
        }
        Err(error) => Err(io_at(logical_path, error)),
    }
}

fn identity(metadata: &Metadata) -> FileIdentity {
    (metadata.dev(), metadata.ino())
}

fn logical_path(logical_name: &str) -> PathBuf {
    if logical_name.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(logical_name)
    }
}

fn join_logical_path(parent: &str, name: &std::ffi::OsStr) -> PathBuf {
    let mut path = logical_path(parent);
    if path == Path::new(".") {
        path.clear();
    }
    path.push(name);
    path
}

fn io_at(logical_path: &Path, source: std::io::Error) -> DiscoveryError {
    DiscoveryError::IoAt {
        logical_path: logical_path.to_path_buf(),
        source,
    }
}

fn check_root_index_limit(limits: &TraversalLimits, observed: usize) -> Result<(), DiscoveryError> {
    if observed > limits.max_index_bytes {
        return Err(DiscoveryError::Limit {
            field: "max_index_bytes",
            limit: limits.max_index_bytes as u64,
            observed: observed as u64,
            logical_path: PathBuf::from("."),
        });
    }
    Ok(())
}

fn path_and_name_capacity(path: &PathBuf, logical_name: &String) -> usize {
    path.capacity() + logical_name.capacity()
}

fn hashmap_capacity_bytes(capacity: usize) -> usize {
    // hashbrown keeps at least one control byte per bucket and admits at most
    // seven eighths of its buckets. Round the public usable capacity up to that
    // bucket count so this remains conservative across growth steps.
    let buckets = capacity.saturating_mul(8).div_ceil(7);
    buckets.saturating_mul(HASHMAP_BUCKET_BYTES)
}
