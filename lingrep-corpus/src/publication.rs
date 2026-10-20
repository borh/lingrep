//! Crash-safe publication of one immutable artifact directory.
//!
//! The no-replace rename is the commit point. The staging owner remains armed
//! until that syscall succeeds, so every earlier error removes only the
//! builder's private staging directory. Every later error leaves the visible
//! artifact untouched.

use std::{fs::File, io, path::Path};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::{ffi::CString, os::unix::ffi::OsStrExt};

use crate::{CorpusError, artifact::CompletedStagingArtifact};

#[cfg(test)]
pub(crate) fn publish(completed: &mut CompletedStagingArtifact) -> Result<(), CorpusError> {
    let published = publish_guarded_with_ops(&RealOps, completed, || true, || true)?;
    debug_assert!(published, "unconditional publication guard must commit");
    Ok(())
}

pub(crate) fn publish_guarded(
    completed: &mut CompletedStagingArtifact,
    should_continue: impl FnMut() -> bool,
    claim_commit: impl FnOnce() -> bool,
) -> Result<bool, CorpusError> {
    publish_guarded_with_ops(&RealOps, completed, should_continue, claim_commit)
}

trait PublicationOps {
    fn fsync(&self, path: &Path) -> io::Result<()>;
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()>;
}

struct RealOps;

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

fn publish_guarded_with_ops(
    ops: &impl PublicationOps,
    completed: &mut CompletedStagingArtifact,
    mut should_continue: impl FnMut() -> bool,
    claim_commit: impl FnOnce() -> bool,
) -> Result<bool, CorpusError> {
    for component in completed.publication_components() {
        if !should_continue() {
            return Ok(false);
        }
        ops.fsync(&component)?;
    }
    if !should_continue() {
        return Ok(false);
    }
    ops.fsync(completed.path())?;

    // The caller's atomic claim is the final cooperative-cancellation
    // boundary. Once it succeeds, publication must run through the no-replace
    // rename and parent durability result without reporting cancellation.
    if !claim_commit() {
        return Ok(false);
    }
    let staging = completed.path();
    let destination = completed.destination();
    ops.rename_noreplace(staging, destination)
        .map_err(|error| map_rename_error(error, destination))?;
    completed.mark_published();

    let parent = destination_parent(completed.destination());
    if ops.fsync(parent).is_err() {
        return Err(CorpusError::PublicationDurabilityUncertain {
            path: completed.destination().to_path_buf(),
        });
    }
    Ok(true)
}

/// Errno values meaning "no-replace rename is unavailable here", as opposed to
/// "the destination is already taken".
///
/// Linux reports an unimplemented `renameat2` as `ENOSYS` and a filesystem
/// driver without `RENAME_NOREPLACE` as `EINVAL` or `EOPNOTSUPP`. macOS reports
/// a volume without `VOL_CAP_INT_RENAME_EXCL` as `ENOTSUP`, which is a
/// **different value there** — 45 against `EOPNOTSUPP`'s 102. On Linux the two
/// names are one value, so a single list naming both would be an unreachable
/// pattern; hence the split rather than one longer array.
#[cfg(target_os = "macos")]
const UNSUPPORTED_RENAME_ERRNOS: &[i32] =
    &[libc::ENOSYS, libc::EINVAL, libc::EOPNOTSUPP, libc::ENOTSUP];

#[cfg(not(target_os = "macos"))]
const UNSUPPORTED_RENAME_ERRNOS: &[i32] = &[libc::ENOSYS, libc::EINVAL, libc::EOPNOTSUPP];

fn map_rename_error(error: io::Error, destination: &Path) -> CorpusError {
    let errno = error.raw_os_error().unwrap_or(libc::EIO);
    if matches!(errno, libc::EEXIST | libc::ENOTEMPTY) {
        CorpusError::DestinationExists {
            path: destination.to_path_buf(),
        }
    } else if UNSUPPORTED_RENAME_ERRNOS.contains(&errno) {
        CorpusError::UnsupportedPublication {
            path: destination.to_path_buf(),
            errno,
        }
    } else {
        CorpusError::Io(error)
    }
}

fn destination_parent(destination: &Path) -> &Path {
    destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// One path as a syscall argument, refusing an interior NUL.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn path_argument(path: &Path, message: &'static str) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, message))
}

#[cfg(target_os = "linux")]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
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

/// macOS spells the same commit point `renamex_np` with `RENAME_EXCL`.
///
/// It is the exact counterpart, not an approximation: the rename is atomic and
/// fails with `EEXIST` when the destination is taken, which is the one property
/// publication depends on. Staging is always a sibling of the destination, so
/// the absence of a directory-relative form here costs nothing.
#[cfg(target_os = "macos")]
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
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
fn rename_noreplace(_from: &Path, _to: &Path) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOSYS))
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

    use super::{PublicationOps, publish, publish_with_ops};
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
                None => super::rename_noreplace(from, to),
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
            super::rename_noreplace(from, to)
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
    async fn unsupported_no_replace_never_falls_back_to_a_replacing_rename() {
        // Driven by the production set so a platform that adds an errno — macOS
        // adds ENOTSUP — is covered without restating the list.
        for &errno in super::UNSUPPORTED_RENAME_ERRNOS {
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
}
