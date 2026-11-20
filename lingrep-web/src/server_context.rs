//! Shared scan pool, dictionary identity, and Vibrato plan for all served corpora.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use lingrep_core::analysis::VibratoPlan;
use rayon::ThreadPool;

use lingrep_core::PipelineError;
use lingrep_core::dictionary::{create_tokenizer_from_path, dictionary_identity};

const POOL_THREADS: usize = 8;

const MAX_CONCURRENT_BUILDS: usize = 1;

pub struct ServerContext {
    dict_path: Option<PathBuf>,
    pool: Arc<ThreadPool>,
    fingerprint: Mutex<Option<[u8; 32]>>,
    build_permit: Arc<tokio::sync::Semaphore>,
    builds_stopping: AtomicBool,
    plan: Mutex<Option<Arc<VibratoPlan>>>,
}

#[derive(Debug, Clone, Copy)]
pub enum TokenizerUnavailable {
    NoDictionary,
    LoadFailed,
}

impl std::fmt::Debug for ServerContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerContext")
            .field("dict_path", &self.dict_path)
            .finish_non_exhaustive()
    }
}

impl ServerContext {
    pub fn new(dict_path: impl Into<PathBuf>) -> io::Result<Self> {
        Self::with_dictionary(Some(dict_path.into()))
    }

    /// When `dict_path` is `None`, [`Self::dictionary_fingerprint`] and [`Self::plan`] return errors.
    pub fn with_dictionary(dict_path: Option<PathBuf>) -> io::Result<Self> {
        Ok(Self {
            dict_path,
            pool: fixed_pool()?,
            fingerprint: Mutex::new(None),
            build_permit: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_BUILDS)),
            builds_stopping: AtomicBool::new(false),
            plan: Mutex::new(None),
        })
    }

    /// The dictionary this server was started with, if any.
    #[must_use]
    pub fn dict_path(&self) -> Option<&Path> {
        self.dict_path.as_deref()
    }

    /// The one scan pool. Cloning shares it; it is never rebuilt.
    #[must_use]
    pub fn pool(&self) -> Arc<ThreadPool> {
        Arc::clone(&self.pool)
    }

    /// The permit bounding concurrent artifact builds. Cloning shares it; it
    /// is never rebuilt.
    #[must_use]
    pub fn build_permit(&self) -> Arc<tokio::sync::Semaphore> {
        Arc::clone(&self.build_permit)
    }

    /// Stops this context's artifact builds and returns once none is running.
    ///
    /// Later builds create no staging directory, and builds remain stopped
    /// for the life of this context.
    pub async fn stop_builds(&self) {
        self.builds_stopping.store(true, Ordering::Release);
        let all = u32::try_from(MAX_CONCURRENT_BUILDS).unwrap_or(u32::MAX);
        drop(self.build_permit.acquire_many(all).await);
    }

    /// Whether [`Self::stop_builds`] has been called.
    #[must_use]
    pub fn builds_stopping(&self) -> bool {
        self.builds_stopping.load(Ordering::Acquire)
    }

    /// The dictionary's blake3 fingerprint, computed once for the process.
    ///
    /// The value is [`lingrep_core::dictionary::dictionary_identity`]'s — blake3 over
    /// the decompressed dictionary — which is what every build's
    /// `AnalysisInputs` needs and what an artifact's identity carries. That
    /// function keeps its own on-disk memo, so a warm machine answers without
    /// decompressing 1.31 GB; this mutex keeps the answer for the process on
    /// top of it. A failure is not kept: the next call derives it again.
    pub fn dictionary_fingerprint(&self) -> Result<[u8; 32], PipelineError> {
        let mut guard = self.fingerprint.lock().expect("fingerprint mutex poisoned");
        if let Some(existing) = *guard {
            return Ok(existing);
        }
        let path = self
            .dict_path
            .as_ref()
            .ok_or_else(|| PipelineError::Dictionary("no dictionary is configured".to_owned()))?;
        let fingerprint = dictionary_identity(&path.to_string_lossy())?;
        *guard = Some(fingerprint);
        Ok(fingerprint)
    }

    /// The Vibrato plan over this server's dictionary, built once on first
    /// use: sample analysis tokenizes with it and every artifact build
    /// derives its plan from it, so the dictionary is loaded and its
    /// protected-surface table built once per process.
    ///
    /// Callers that arrive during the build block until it ends. A failure is
    /// not kept: the next call builds again.
    ///
    /// The tokenizer is built with [`create_tokenizer_from_path`], which
    /// memory-maps a globally-cached decompressed copy rather than parsing a
    /// second one into anonymous heap.
    pub fn plan(&self) -> Result<Arc<VibratoPlan>, TokenizerUnavailable> {
        build_once(&self.plan, || {
            let path = self
                .dict_path
                .as_ref()
                .ok_or(TokenizerUnavailable::NoDictionary)?;
            let tokenizer = create_tokenizer_from_path(&path.to_string_lossy())
                .map_err(|_| TokenizerUnavailable::LoadFailed)?;
            let plan = VibratoPlan::new(tokenizer, path).map_err(|error| {
                tracing::warn!(%error, cause = %error.source, "Vibrato plan construction failed");
                TokenizerUnavailable::LoadFailed
            })?;
            Ok(Arc::new(plan))
        })
    }
}

/// The value in `slot`, from `build` when the slot is empty. The slot stays
/// locked while `build` runs and stays empty when `build` fails.
fn build_once<T: Clone, E>(
    slot: &Mutex<Option<T>>,
    build: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    // A panic in `build` leaves the slot empty, so a poisoned lock is usable.
    let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(existing) = &*slot {
        return Ok(existing.clone());
    }
    let built = build()?;
    *slot = Some(built.clone());
    Ok(built)
}

fn fixed_pool() -> io::Result<Arc<ThreadPool>> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(POOL_THREADS)
        .build()
        .map(Arc::new)
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::{POOL_THREADS, ServerContext, build_once};

    #[test]
    fn one_context_builds_one_pool_of_the_documented_size() {
        let context = ServerContext::new("/tmp/dict").unwrap();
        assert_eq!(context.pool().current_num_threads(), POOL_THREADS);
    }

    /// The property Stage A exists for: N corpora share one pool rather than
    /// each building its own, so thread count is independent of corpus count.
    #[test]
    fn the_pool_is_shared_by_pointer_not_rebuilt_per_caller() {
        let context = ServerContext::new("/tmp/dict").unwrap();
        assert!(std::sync::Arc::ptr_eq(&context.pool(), &context.pool()));
    }

    #[test]
    fn a_missing_dictionary_reports_an_error_and_is_not_memoized() {
        let context = ServerContext::new("/lingrep-web/no/such/dict.zst").unwrap();
        assert!(context.dictionary_fingerprint().is_err());
        assert!(context.dictionary_fingerprint().is_err());
    }

    #[test]
    fn the_fingerprint_is_derived_once_and_stable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dict.dic");
        std::fs::write(&path, b"not a real dictionary").unwrap();
        let context = ServerContext::new(&path).unwrap();

        let first = context.dictionary_fingerprint().unwrap();
        let second = context.dictionary_fingerprint().unwrap();

        // The fingerprint is derived once, not per build, but is
        // byte-identical to hashing the bytes directly every time.
        assert_eq!(first, second);
        assert_eq!(first, *blake3::hash(b"not a real dictionary").as_bytes());
    }

    #[test]
    fn a_missing_dictionary_makes_the_tokenizer_unavailable() {
        let context = ServerContext::new("/lingrep-web/no/such/dict.zst").unwrap();
        assert!(context.plan().is_err());
    }

    #[test]
    fn concurrent_callers_of_an_empty_slot_run_one_build() {
        const CALLERS: usize = 8;
        let slot = std::sync::Mutex::new(None);
        let builds = std::sync::atomic::AtomicUsize::new(0);
        let start = std::sync::Barrier::new(CALLERS);
        std::thread::scope(|scope| {
            for _ in 0..CALLERS {
                scope.spawn(|| {
                    start.wait();
                    let value = build_once(&slot, || {
                        let build = builds.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        Ok::<_, ()>(build)
                    });
                    assert_eq!(value, Ok(0));
                });
            }
        });
        assert_eq!(builds.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failed_build_leaves_the_slot_empty_for_the_next_caller() {
        let slot = std::sync::Mutex::new(None);
        assert_eq!(build_once(&slot, || Err::<u8, _>("failed")), Err("failed"));
        assert_eq!(build_once(&slot, || Ok::<_, &str>(1)), Ok(1));
        assert_eq!(build_once(&slot, || Err("not run")), Ok(1));
    }
}
