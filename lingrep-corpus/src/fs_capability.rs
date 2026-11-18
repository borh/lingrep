//! Filesystem operations whose support differs between filesystems, and their fallbacks.
//!
//! Product code performs no-replace renames, no-clobber file persistence, symbolic-link
//! probes and SQLite WAL setup through this module only; `scripts/check-fs-capability-seam.sh` rejects direct uses
//! elsewhere.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{ffi::CString, os::unix::ffi::OsStrExt};

/// A filesystem feature that some supported filesystems lack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capability {
    /// `renameat2(RENAME_NOREPLACE)` on Linux, `renamex_np(RENAME_EXCL)` on macOS.
    NoReplaceRename,
    SymbolicLinks,
    HardLinks,
}

impl Capability {
    /// Reports whether `error`, returned by an attempt to use this capability, means that the
    /// filesystem lacks it rather than that the operation failed for another reason.
    #[must_use]
    pub fn unsupported_by(self, error: &io::Error) -> bool {
        let Some(errno) = error.raw_os_error() else {
            return false;
        };
        match self {
            Self::NoReplaceRename => UNSUPPORTED_RENAME_ERRNOS.contains(&errno),
            Self::SymbolicLinks => UNSUPPORTED_SYMLINK_ERRNOS.contains(&errno),
            Self::HardLinks => UNSUPPORTED_HARD_LINK_ERRNOS.contains(&errno),
        }
    }
}

// Linux reports an unimplemented `renameat2` as ENOSYS and a filesystem without
// RENAME_NOREPLACE as EINVAL or EOPNOTSUPP. macOS reports a volume without
// VOL_CAP_INT_RENAME_EXCL as ENOTSUP, which differs from EOPNOTSUPP there and equals it on Linux.
#[cfg(target_os = "macos")]
pub(crate) const UNSUPPORTED_RENAME_ERRNOS: &[i32] =
    &[libc::ENOSYS, libc::EINVAL, libc::EOPNOTSUPP, libc::ENOTSUP];
#[cfg(not(target_os = "macos"))]
pub(crate) const UNSUPPORTED_RENAME_ERRNOS: &[i32] =
    &[libc::ENOSYS, libc::EINVAL, libc::EOPNOTSUPP];

// macOS reports a vnode operation a filesystem lacks as ENOTSUP (autofs returns it for symlink).
#[cfg(target_os = "macos")]
const UNSUPPORTED_SYMLINK_ERRNOS: &[i32] = &[
    libc::EPERM,
    libc::EACCES,
    libc::ENOSYS,
    libc::EOPNOTSUPP,
    libc::ENOTSUP,
];
#[cfg(not(target_os = "macos"))]
const UNSUPPORTED_SYMLINK_ERRNOS: &[i32] =
    &[libc::EPERM, libc::EACCES, libc::ENOSYS, libc::EOPNOTSUPP];

// FAT and exFAT report a hard link as EPERM.
#[cfg(target_os = "macos")]
pub(crate) const UNSUPPORTED_HARD_LINK_ERRNOS: &[i32] =
    &[libc::EPERM, libc::ENOSYS, libc::EOPNOTSUPP, libc::ENOTSUP];
#[cfg(not(target_os = "macos"))]
pub(crate) const UNSUPPORTED_HARD_LINK_ERRNOS: &[i32] =
    &[libc::EPERM, libc::ENOSYS, libc::EOPNOTSUPP];

/// The system calls behind this module, replaceable so tests can inject errno results.
pub(crate) trait Primitives {
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()>;
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()>;
    fn create_new(&self, path: &Path) -> io::Result<fs::File>;
}

pub(crate) struct System;

impl Primitives for System {
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        rename_noreplace_syscall(from, to)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        fs::hard_link(original, link)
    }

    #[cfg(unix)]
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(not(unix))]
    fn symlink(&self, _target: &Path, _link: &Path) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    fn create_new(&self, path: &Path) -> io::Result<fs::File> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    }
}

/// Renames `from` to `to`, rejecting an existing destination when no-replace rename is supported.
///
/// Without filesystem support, directories use a checked rename that can replace an empty
/// destination created concurrently; non-directory sources return the unsupported-operation error.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    rename_noreplace_with(&System, from, to)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn rename_noreplace_with(
    primitives: &impl Primitives,
    from: &Path,
    to: &Path,
) -> io::Result<()> {
    let error = match primitives.rename_noreplace(from, to) {
        Err(error) if Capability::NoReplaceRename.unsupported_by(&error) => error,
        result => return result,
    };
    if !fs::symlink_metadata(from)?.is_dir() {
        return Err(error);
    }
    match fs::symlink_metadata(to) {
        Ok(_) => Err(io::Error::from_raw_os_error(libc::EEXIST)),
        Err(absent) if absent.kind() == io::ErrorKind::NotFound => primitives.rename(from, to),
        Err(other) => Err(other),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn rename_noreplace_with(
    primitives: &impl Primitives,
    from: &Path,
    to: &Path,
) -> io::Result<()> {
    primitives.rename_noreplace(from, to)
}

/// Moves a complete temporary file to `to`, failing with `AlreadyExists` instead of replacing
/// an existing `to`.
///
/// Without no-replace rename, the file is hard-linked to `to` and the temporary name removed.
/// Without hard links either, `to` is created exclusively and the content copied and synced,
/// so an interrupted write can leave a partial `to`. When that creation fails, the error keeps
/// the OS reason and suggests another output directory.
pub fn persist_noclobber(file: tempfile::NamedTempFile, to: &Path) -> io::Result<()> {
    persist_noclobber_with(&System, file, to)
}

pub(crate) fn persist_noclobber_with(
    primitives: &impl Primitives,
    file: tempfile::NamedTempFile,
    to: &Path,
) -> io::Result<()> {
    let (mut file, temporary) = file.into_parts();
    match primitives.rename_noreplace(&temporary, to) {
        Ok(()) => {
            temporary.keep().map_err(|error| error.error)?;
            return Ok(());
        }
        Err(error) if Capability::NoReplaceRename.unsupported_by(&error) => {}
        Err(error) => return Err(error),
    }
    match primitives.hard_link(&temporary, to) {
        Ok(()) => return Ok(()),
        Err(error) if Capability::HardLinks.unsupported_by(&error) => {}
        Err(error) => return Err(error),
    }
    let mut destination = match primitives.create_new(to) {
        Ok(destination) => destination,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Err(error),
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                NoClobberUnsupported {
                    path: to.to_owned(),
                    source: error,
                },
            ));
        }
    };
    let copied = copy_and_sync(&mut file, &mut destination, to);
    if copied.is_err() {
        drop(destination);
        let _ = fs::remove_file(to);
    }
    copied
}

fn copy_and_sync(source: &mut fs::File, destination: &mut fs::File, to: &Path) -> io::Result<()> {
    use std::io::Seek as _;
    source.seek(io::SeekFrom::Start(0))?;
    io::copy(source, destination)?;
    destination.sync_all()?;
    let parent = to
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::File::open(parent)?.sync_all()
}

#[derive(Debug, thiserror::Error)]
#[error(
    "cannot create {}: {source}. The filesystem supports neither no-replace rename nor hard \
     links; choose an output directory on a filesystem that supports one of them",
    path.display()
)]
struct NoClobberUnsupported {
    path: PathBuf,
    source: io::Error,
}

/// Creates and removes a symbolic link in `directory`.
///
/// When [`Capability::SymbolicLinks`] reports the error as unsupported, the directory's
/// filesystem cannot hold the links a caller is about to create there.
pub fn probe_symbolic_links(directory: &Path) -> io::Result<()> {
    probe_symbolic_links_with(&System, directory)
}

pub(crate) fn probe_symbolic_links_with(
    primitives: &impl Primitives,
    directory: &Path,
) -> io::Result<()> {
    let probe = directory.join(".symlink-probe");
    primitives.symlink(Path::new("."), &probe)?;
    fs::remove_file(&probe)
}

/// Enables WAL on a new SQLite database without a shared-memory index file.
///
/// Exclusive locking must precede the first WAL access for SQLite to keep the WAL index
/// in heap memory (<https://sqlite.org/wal.html#noshm>); 9p and some network filesystems
/// cannot map the `-shm` file.
pub(crate) async fn enable_wal_without_shared_memory(
    connection: &mut sqlx::SqliteConnection,
) -> sqlx::Result<()> {
    use sqlx::Executor as _;
    connection.execute("PRAGMA locking_mode=EXCLUSIVE").await?;
    connection.execute("PRAGMA journal_mode=WAL").await?;
    Ok(())
}

/// Checkpoints a database set up by [`enable_wal_without_shared_memory`] and returns it to
/// rollback-journal mode, closing `connection`; `options` reopen the same database.
///
/// The journal-mode change rewrites the header through a rollback journal, and SQLite fails
/// that write with `SQLITE_READONLY_DBMOVED` when the file's inode number differs from the
/// one recorded at open (<https://www.sqlite.org/rescode.html#readonly_dbmoved>). On macOS
/// 15.8's FSKit msdos driver a new file's inode number changes when the checkpoint writes it,
/// so the journal-mode change runs on a connection opened after the checkpoint.
pub(crate) async fn leave_wal(
    mut connection: sqlx::SqliteConnection,
    options: &sqlx::sqlite::SqliteConnectOptions,
) -> sqlx::Result<()> {
    use sqlx::{Connection as _, Executor as _, Row as _};
    let busy: i64 = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(&mut connection)
        .await?
        .try_get(0)?;
    if busy != 0 {
        return Err(sqlx::Error::Io(io::Error::other(
            "the WAL checkpoint could not complete",
        )));
    }
    connection.close().await?;
    let mut connection = sqlx::SqliteConnection::connect_with(options).await?;
    connection.execute("PRAGMA locking_mode=EXCLUSIVE").await?;
    // SQLite returns the unchanged journal mode, not an error, when a switch does not take
    // effect (<https://sqlite.org/pragma.html#pragma_journal_mode>).
    let mode: String = sqlx::query("PRAGMA journal_mode=DELETE")
        .fetch_one(&mut connection)
        .await?
        .try_get(0)?;
    if !mode.eq_ignore_ascii_case("delete") {
        return Err(sqlx::Error::Io(io::Error::other(format!(
            "the journal mode stayed {mode} instead of changing to delete"
        ))));
    }
    connection.close().await
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn path_argument(path: &Path, message: &'static str) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, message))
}

#[cfg(target_os = "linux")]
fn rename_noreplace_syscall(from: &Path, to: &Path) -> io::Result<()> {
    let from = path_argument(from, "staging path contains a NUL byte")?;
    let to = path_argument(to, "destination path contains a NUL byte")?;
    // SAFETY: both pointers are valid NUL-terminated path strings for the
    // duration of the call, and AT_FDCWD makes both paths process-relative.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn rename_noreplace_syscall(from: &Path, to: &Path) -> io::Result<()> {
    let from = path_argument(from, "staging path contains a NUL byte")?;
    let to = path_argument(to, "destination path contains a NUL byte")?;
    // SAFETY: both pointers are valid NUL-terminated path strings for the
    // duration of the call.
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_noreplace_syscall(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOSYS))
}

/// Errno results injected into chosen primitives; the others run against the real filesystem.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Faults {
    pub(crate) rename_noreplace: Option<i32>,
    pub(crate) rename: Option<i32>,
    pub(crate) hard_link: Option<i32>,
    pub(crate) symlink: Option<i32>,
    pub(crate) create_new: Option<i32>,
}

#[cfg(test)]
fn injected(errno: Option<i32>, real: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
    errno.map_or_else(real, |errno| Err(io::Error::from_raw_os_error(errno)))
}

#[cfg(test)]
impl Primitives for Faults {
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        injected(self.rename_noreplace, || System.rename_noreplace(from, to))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        injected(self.rename, || System.rename(from, to))
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        injected(self.hard_link, || System.hard_link(original, link))
    }

    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        injected(self.symlink, || System.symlink(target, link))
    }

    fn create_new(&self, path: &Path) -> io::Result<fs::File> {
        match self.create_new {
            Some(errno) => Err(io::Error::from_raw_os_error(errno)),
            None => System.create_new(path),
        }
    }
}

#[cfg(test)]
mod moved_after_growth;

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        Capability, Faults, UNSUPPORTED_HARD_LINK_ERRNOS, UNSUPPORTED_RENAME_ERRNOS,
        persist_noclobber, persist_noclobber_with, probe_symbolic_links, probe_symbolic_links_with,
        rename_noreplace, rename_noreplace_with,
    };

    fn staged_component(parent: &Path) -> PathBuf {
        let staging = parent.join(".output.staging");
        std::fs::create_dir(&staging).unwrap();
        std::fs::write(staging.join("component"), b"data").unwrap();
        staging
    }

    fn snapshot(path: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        if path.is_file() {
            return vec![(path.to_path_buf(), Some(std::fs::read(path).unwrap()))];
        }
        let mut entries = vec![(path.to_path_buf(), None)];
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap().path();
            entries.push((entry.clone(), Some(std::fs::read(&entry).unwrap())));
        }
        entries.sort();
        entries
    }

    fn no_replace_rename_failing(errno: i32) -> Faults {
        Faults {
            rename_noreplace: Some(errno),
            ..Faults::default()
        }
    }

    #[test]
    fn supported_no_replace_rename_publishes_and_rejects_an_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let staging = staged_component(directory.path());
        let destination = directory.path().join("output");
        rename_noreplace(&staging, &destination).unwrap();
        assert_eq!(
            std::fs::read(destination.join("component")).unwrap(),
            b"data"
        );

        let staging = staged_component(directory.path());
        let error = rename_noreplace(&staging, &destination).unwrap_err();
        assert!(matches!(
            error.raw_os_error(),
            Some(libc::EEXIST | libc::ENOTEMPTY)
        ));
        assert_eq!(std::fs::read(staging.join("component")).unwrap(), b"data");
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn without_no_replace_rename_a_directory_is_published_to_an_absent_destination() {
        for &errno in UNSUPPORTED_RENAME_ERRNOS {
            let directory = tempfile::tempdir().unwrap();
            let staging = staged_component(directory.path());
            let destination = directory.path().join("output");

            rename_noreplace_with(&no_replace_rename_failing(errno), &staging, &destination)
                .unwrap();

            assert!(!staging.exists());
            assert_eq!(
                std::fs::read(destination.join("component")).unwrap(),
                b"data"
            );
        }
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn without_no_replace_rename_an_existing_destination_is_left_unchanged() {
        let occupy: [fn(&Path); 3] = [
            |path| {
                std::fs::create_dir(path).unwrap();
                std::fs::write(path.join("existing"), b"kept").unwrap();
            },
            |path| std::fs::create_dir(path).unwrap(),
            |path| std::fs::write(path, b"kept").unwrap(),
        ];
        for &errno in UNSUPPORTED_RENAME_ERRNOS {
            for occupy in occupy {
                let directory = tempfile::tempdir().unwrap();
                let staging = staged_component(directory.path());
                let destination = directory.path().join("output");
                occupy(&destination);
                let before = snapshot(&destination);

                let error = rename_noreplace_with(
                    &no_replace_rename_failing(errno),
                    &staging,
                    &destination,
                )
                .unwrap_err();

                assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
                assert_eq!(snapshot(&destination), before);
                assert_eq!(std::fs::read(staging.join("component")).unwrap(), b"data");
            }
        }
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn without_no_replace_rename_a_file_is_not_renamed() {
        let directory = tempfile::tempdir().unwrap();
        let staging = directory.path().join(".output.staging");
        std::fs::write(&staging, b"data").unwrap();
        let destination = directory.path().join("output");

        let error = rename_noreplace_with(
            &no_replace_rename_failing(libc::EINVAL),
            &staging,
            &destination,
        )
        .unwrap_err();

        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
        assert!(!destination.exists());
        assert_eq!(std::fs::read(&staging).unwrap(), b"data");
    }

    #[test]
    fn errors_other_than_unsupported_skip_the_fallback() {
        for errno in [
            libc::EPERM,
            libc::EACCES,
            libc::EXDEV,
            libc::EEXIST,
            libc::ENOTEMPTY,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let staging = staged_component(directory.path());
            let destination = directory.path().join("output");

            let error =
                rename_noreplace_with(&no_replace_rename_failing(errno), &staging, &destination)
                    .unwrap_err();

            assert_eq!(error.raw_os_error(), Some(errno));
            assert!(!destination.exists(), "errno {errno}");
            assert_eq!(std::fs::read(staging.join("component")).unwrap(), b"data");
        }
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn a_failing_fallback_rename_keeps_its_os_error_and_the_staging_directory() {
        // EEXIST and ENOTEMPTY stand for a destination created between the absence check
        // and the rename.
        for errno in [
            libc::EXDEV,
            libc::EACCES,
            libc::EPERM,
            libc::EEXIST,
            libc::ENOTEMPTY,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let staging = staged_component(directory.path());
            let destination = directory.path().join("output");
            let faults = Faults {
                rename_noreplace: Some(libc::EINVAL),
                rename: Some(errno),
                ..Faults::default()
            };

            let error = rename_noreplace_with(&faults, &staging, &destination).unwrap_err();

            assert_eq!(error.raw_os_error(), Some(errno));
            assert!(!destination.exists(), "errno {errno}");
            assert_eq!(std::fs::read(staging.join("component")).unwrap(), b"data");
        }
    }

    #[test]
    fn no_replace_rename_classification_separates_missing_support_from_other_failures() {
        for &errno in UNSUPPORTED_RENAME_ERRNOS {
            let error = std::io::Error::from_raw_os_error(errno);
            assert!(
                Capability::NoReplaceRename.unsupported_by(&error),
                "{error}"
            );
        }
        for errno in [
            libc::EPERM,
            libc::EACCES,
            libc::EXDEV,
            libc::EEXIST,
            libc::ENOTEMPTY,
        ] {
            let error = std::io::Error::from_raw_os_error(errno);
            assert!(
                !Capability::NoReplaceRename.unsupported_by(&error),
                "{error}"
            );
        }
        let synthetic = std::io::Error::new(std::io::ErrorKind::Unsupported, "no errno");
        assert!(!Capability::NoReplaceRename.unsupported_by(&synthetic));
    }

    #[test]
    #[cfg(unix)]
    fn a_symbolic_link_probe_leaves_the_directory_as_it_found_it() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(".gitignore"), "*\n").unwrap();
        let before = snapshot(directory.path());

        probe_symbolic_links(directory.path()).unwrap();

        assert_eq!(snapshot(directory.path()), before);
    }

    #[test]
    fn a_failed_symbolic_link_probe_keeps_its_os_error_and_classification() {
        for (errno, unsupported) in [
            (libc::EPERM, true),
            (libc::EACCES, true),
            (libc::ENOSYS, true),
            (libc::EOPNOTSUPP, true),
            (libc::ENOTSUP, true),
            (libc::EIO, false),
            (libc::EEXIST, false),
            (libc::EXDEV, false),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let faults = Faults {
                symlink: Some(errno),
                ..Faults::default()
            };

            let error = probe_symbolic_links_with(&faults, directory.path()).unwrap_err();

            assert_eq!(error.raw_os_error(), Some(errno));
            assert_eq!(
                Capability::SymbolicLinks.unsupported_by(&error),
                unsupported,
                "{error}"
            );
            assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
        }
    }

    fn temporary_report(directory: &Path) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new_in(directory).unwrap();
        std::io::Write::write_all(&mut file, b"report").unwrap();
        file
    }

    fn entries(directory: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut entries = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect::<Vec<_>>();
        entries.sort();
        entries
    }

    #[test]
    fn persisting_without_clobbering_creates_the_file_or_keeps_an_existing_one() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("report.json");
        persist_noclobber(temporary_report(directory.path()), &destination).unwrap();
        assert_eq!(
            entries(directory.path()),
            [(destination.clone(), b"report".to_vec())]
        );

        let error =
            persist_noclobber(temporary_report(directory.path()), &destination).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            entries(directory.path()),
            [(destination, b"report".to_vec())]
        );
    }

    #[test]
    fn without_no_replace_rename_a_file_is_persisted_through_a_hard_link() {
        for &errno in UNSUPPORTED_RENAME_ERRNOS {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("report.json");

            persist_noclobber_with(
                &no_replace_rename_failing(errno),
                temporary_report(directory.path()),
                &destination,
            )
            .unwrap();

            assert_eq!(
                entries(directory.path()),
                [(destination.clone(), b"report".to_vec())]
            );

            let error = persist_noclobber_with(
                &no_replace_rename_failing(errno),
                temporary_report(directory.path()),
                &destination,
            )
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
            assert_eq!(
                entries(directory.path()),
                [(destination, b"report".to_vec())]
            );
        }
    }

    fn without_rename_or_link(link_errno: i32) -> Faults {
        Faults {
            rename_noreplace: Some(libc::EINVAL),
            hard_link: Some(link_errno),
            ..Faults::default()
        }
    }

    #[test]
    fn without_no_replace_rename_or_hard_links_a_file_is_created_exclusively() {
        for &errno in UNSUPPORTED_HARD_LINK_ERRNOS {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("report.json");

            persist_noclobber_with(
                &without_rename_or_link(errno),
                temporary_report(directory.path()),
                &destination,
            )
            .unwrap();

            assert_eq!(
                entries(directory.path()),
                [(destination.clone(), b"report".to_vec())]
            );

            std::fs::write(&destination, b"kept").unwrap();
            let error = persist_noclobber_with(
                &without_rename_or_link(errno),
                temporary_report(directory.path()),
                &destination,
            )
            .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
            assert_eq!(entries(directory.path()), [(destination, b"kept".to_vec())]);
        }
    }

    #[test]
    fn when_exclusive_creation_also_fails_persisting_names_the_reason_and_a_remedy() {
        for errno in [libc::EACCES, libc::EIO, libc::EROFS] {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("report.json");
            let faults = Faults {
                create_new: Some(errno),
                ..without_rename_or_link(libc::EPERM)
            };

            let error =
                persist_noclobber_with(&faults, temporary_report(directory.path()), &destination)
                    .unwrap_err();

            let message = error.to_string();
            let reason = std::io::Error::from_raw_os_error(errno).to_string();
            assert!(
                message.contains(&destination.display().to_string()),
                "{message}"
            );
            assert!(message.contains(&reason), "{message}");
            assert!(message.contains("choose an output directory"), "{message}");
            let original = std::error::Error::source(&error)
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .and_then(std::io::Error::raw_os_error);
            assert_eq!(original, Some(errno), "{message}");
            assert!(entries(directory.path()).is_empty());
        }
    }

    #[test]
    fn persisting_keeps_other_os_errors_and_leaves_nothing_behind() {
        for faults in [
            no_replace_rename_failing(libc::EEXIST),
            no_replace_rename_failing(libc::EPERM),
            no_replace_rename_failing(libc::EACCES),
            no_replace_rename_failing(libc::EXDEV),
            Faults {
                rename_noreplace: Some(libc::EINVAL),
                hard_link: Some(libc::EACCES),
                ..Faults::default()
            },
            Faults {
                rename_noreplace: Some(libc::EINVAL),
                hard_link: Some(libc::EXDEV),
                ..Faults::default()
            },
            Faults {
                rename_noreplace: Some(libc::EINVAL),
                hard_link: Some(libc::EEXIST),
                ..Faults::default()
            },
        ] {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("report.json");

            let error =
                persist_noclobber_with(&faults, temporary_report(directory.path()), &destination)
                    .unwrap_err();

            let expected = faults.hard_link.or(faults.rename_noreplace);
            assert_eq!(error.raw_os_error(), expected, "{faults:?}");
            assert!(entries(directory.path()).is_empty(), "{faults:?}");
        }
    }

    #[test]
    fn hard_link_classification_separates_missing_support_from_other_failures() {
        for &errno in UNSUPPORTED_HARD_LINK_ERRNOS {
            let error = std::io::Error::from_raw_os_error(errno);
            assert!(Capability::HardLinks.unsupported_by(&error), "{error}");
        }
        for errno in [libc::EACCES, libc::EXDEV, libc::EEXIST, libc::EMLINK] {
            let error = std::io::Error::from_raw_os_error(errno);
            assert!(!Capability::HardLinks.unsupported_by(&error), "{error}");
        }
    }

    async fn wal_database_grown_after_open(
        path: &Path,
    ) -> (sqlx::SqliteConnection, sqlx::sqlite::SqliteConnectOptions) {
        use sqlx::{Connection as _, Executor as _};
        super::moved_after_growth::register();
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .vfs(super::moved_after_growth::name())
            .create_if_missing(true);
        let mut connection = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        super::enable_wal_without_shared_memory(&mut connection)
            .await
            .unwrap();
        connection.execute("BEGIN IMMEDIATE").await.unwrap();
        connection
            .execute("CREATE TABLE rows(value BLOB)")
            .await
            .unwrap();
        for _ in 0..64 {
            sqlx::query("INSERT INTO rows(value) VALUES(zeroblob(4096))")
                .execute(&mut connection)
                .await
                .unwrap();
        }
        connection.execute("COMMIT").await.unwrap();
        (connection, options)
    }

    #[tokio::test]
    async fn leaving_wal_succeeds_when_the_database_reports_moved_after_growing() {
        use sqlx::{Connection as _, Row as _};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provenance.sqlite");
        let (connection, options) = wal_database_grown_after_open(&path).await;

        super::leave_wal(connection, &options).await.unwrap();

        let mut reader = sqlx::SqliteConnection::connect_with(&options.clone().read_only(true))
            .await
            .unwrap();
        let mode: String = sqlx::query("PRAGMA journal_mode")
            .fetch_one(&mut reader)
            .await
            .unwrap()
            .get(0);
        let rows: i64 = sqlx::query("SELECT count(*) FROM rows")
            .fetch_one(&mut reader)
            .await
            .unwrap()
            .get(0);
        reader.close().await.unwrap();
        assert_eq!((mode.as_str(), rows), ("delete", 64));
        let names = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, ["provenance.sqlite"]);
    }
}
