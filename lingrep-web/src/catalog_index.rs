//! Startup discovery of the tracked lingrep patterns catalogs.
//!
//! The set is fixed for the process, which is what lets request handling be a
//! pure lookup against known keys: no browser-supplied string ever reaches the
//! filesystem, because a request is answered by an exact lookup against the
//! key set this module fixed at startup, rather than by resolving any
//! caller-supplied path.
//!
//! That guarantee is about request handling, not about the discovery walk
//! itself. The walk reads whatever the operator placed under the pattern
//! root, including through file symlinks, so the root is trusted
//! operator-controlled input, not something this module sandboxes against.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use indexmap::IndexMap;

/// Catalog file extension. Discovery is by extension rather than by a listed
/// set of filenames, matching how the workspace's pin tests treat `patterns/`.
const CATALOG_EXTENSION: &str = "lx";

/// Bound on how deep the discovery walk descends, so a pathologically deep
/// tree cannot hang startup. This does not guard against a directory-symlink
/// cycle: `DirEntry::file_type()` does not follow symlinks, so a directory
/// symlink is never classified `is_dir()` and is never descended into.
const MAX_DEPTH: usize = 8;

/// The catalogs available to this process, keyed by path relative to the
/// discovery root (`sf-experimental.lx`, `sub/other.lx`).
///
/// Immutable once built. Sources are read eagerly at startup — the tracked
/// catalogs total ~111 KB — so serving one is an `IndexMap` lookup.
#[derive(Debug, Default, Clone)]
pub struct CatalogIndex {
    entries: IndexMap<String, Arc<str>>,
}

impl CatalogIndex {
    /// Walk `root` for `*.lx` files and read each into memory, ordered by
    /// relative path.
    ///
    /// Deliberately infallible: an unreadable or absent root yields an empty
    /// index and a warning, because lingrep-web must start regardless of the
    /// directory a relative default resolves against.
    #[must_use]
    pub fn discover(root: &Path) -> Self {
        let mut found: Vec<(String, Arc<str>)> = Vec::new();
        collect(root, root, 0, &mut found);
        found.sort_by(|(left, _), (right, _)| left.cmp(right));
        if found.is_empty() {
            tracing::warn!(
                root = %root.display(),
                "no lingrep patterns catalogs discovered; the catalog list will be empty"
            );
        }
        Self {
            entries: found.into_iter().collect(),
        }
    }

    /// Source for an exact key, or `None`. The only lookup handlers perform.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Arc<str>> {
        self.entries.get(key)
    }

    /// Keys in discovery order.
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn collect(root: &Path, current: &Path, depth: usize, found: &mut Vec<(String, Arc<str>)>) {
    if depth > MAX_DEPTH {
        tracing::warn!(
            path = %current.display(),
            "catalog discovery depth bound reached; not descending further"
        );
        return;
    }
    let Ok(listing) = std::fs::read_dir(current) else {
        // Absent or unreadable: the caller degrades to an empty index.
        return;
    };
    for entry in listing.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect(root, &path, depth + 1, found);
        } else if path.extension().is_some_and(|ext| ext == CATALOG_EXTENSION) {
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            // Keys are always slash-separated so they are usable verbatim as
            // URL path suffixes on every platform.
            let key = relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            match std::fs::read_to_string(&path) {
                Ok(source) => found.push((key, Arc::from(source.as_str()))),
                Err(error) => tracing::warn!(
                    path = %path.display(),
                    %error,
                    "catalog unreadable; omitting it from the list"
                ),
            }
        }
    }
}

/// Discovery root implied by a `--patterns` path: its parent directory, so the
/// stock default `patterns/sentence-final.lx` yields `patterns`.
///
/// A bare filename has an empty parent, which would never open; the current
/// directory is the intended meaning.
#[must_use]
pub fn default_root(patterns: &Path) -> PathBuf {
    match patterns.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

#[cfg(test)]
mod tests {
    use super::{CatalogIndex, default_root};

    #[test]
    fn discovers_nested_catalogs_sorted_by_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("b.lx"), "b := [].\n").unwrap();
        std::fs::write(dir.path().join("a.lx"), "a := [].\n").unwrap();
        std::fs::write(dir.path().join("sub/c.lx"), "c := [].\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "not pattern").unwrap();

        let index = CatalogIndex::discover(dir.path());

        assert_eq!(
            index.keys().collect::<Vec<_>>(),
            ["a.lx", "b.lx", "sub/c.lx"]
        );
        assert_eq!(index.get("sub/c.lx").map(|s| &**s), Some("c := [].\n"));
        assert!(index.get("ignored.txt").is_none());
    }

    /// Non-ASCII names stay admitted: a researcher-authored `パターン.lx` is
    /// ordinary, and nothing in it is significant to a route pattern.
    #[test]
    fn non_ascii_keys_are_admitted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("パターン.lx"), "p := [].\n").unwrap();
        let index = CatalogIndex::discover(dir.path());
        assert_eq!(index.keys().collect::<Vec<_>>(), ["パターン.lx"]);
    }

    #[test]
    fn traversal_and_absolute_paths_are_not_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.lx"), "a := [].\n").unwrap();

        let index = CatalogIndex::discover(dir.path());

        assert!(index.get("../a.lx").is_none());
        assert!(index.get("/etc/passwd").is_none());
        assert!(index.get("sub/../a.lx").is_none());
    }

    #[test]
    fn an_unreadable_root_degrades_to_an_empty_index() {
        let index = CatalogIndex::discover(std::path::Path::new("/lingrep-web/no/such/root"));
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn default_root_is_the_parent_of_the_patterns_path() {
        assert_eq!(
            default_root(std::path::Path::new("patterns/sentence-final.lx")),
            std::path::PathBuf::from("patterns")
        );
    }

    /// A bare filename has no parent directory component; the root is the
    /// current directory rather than an empty path, which would never open.
    #[test]
    fn default_root_of_a_bare_filename_is_the_current_directory() {
        assert_eq!(
            default_root(std::path::Path::new("only.lx")),
            std::path::PathBuf::from(".")
        );
    }
}
