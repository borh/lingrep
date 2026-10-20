//! Resources shared by every corpus this server serves.
//!
//! Each of these is expensive and identical across corpora, so each is
//! constructed once for the process. While there was exactly one corpus the
//! distinction was invisible and free; plural, constructing them per corpus
//! would multiply an 8-thread pool by the number of declared corpora.
//!
//! The dictionary itself is not shared as decoded bytes:
//! [`lingrep_core::dictionary::create_tokenizer`] parses into anonymous heap,
//! and holding that buffer for the process lifetime multiplies resident
//! memory about twelvefold. What is process-wide and cheap to keep is the
//! dictionary's identity — its blake3 fingerprint, `[u8; 32]` — not its bytes. The tokenizer is
//! rebuilt from [`lingrep_core::dictionary::create_tokenizer_from_path`], which
//! memory-maps a globally-cached decompressed copy instead of holding a
//! second anonymous-heap copy live alongside it.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use rayon::ThreadPool;
use vibrato_rkyv::Tokenizer;

use lingrep_core::PipelineError;
use lingrep_core::dictionary::{create_tokenizer_from_path, dictionary_identity};

/// Threads in the shared corpus pool. One in-flight scan owns the whole pool
/// (see the scan permit in `coordinator.rs`), so this is sized for one scan.
const POOL_THREADS: usize = 8;

/// Artifact builds admitted at once.
///
/// One, because a build owns a full tokenizer over the shared dictionary and
/// saturates I/O. While there was one corpus this bound was structural — a
/// single lifecycle mutex made a second concurrent build impossible — and
/// lazily-built plural corpora would otherwise remove it silently.
const MAX_CONCURRENT_BUILDS: usize = 1;

pub struct ServerContext {
    dict_path: PathBuf,
    pool: Arc<ThreadPool>,
    fingerprint: Mutex<Option<[u8; 32]>>,
    build_permit: Arc<tokio::sync::Semaphore>,
    tokenizer: OnceLock<Arc<Tokenizer>>,
}

/// The sample-analysis dictionary could not be loaded.
#[derive(Debug, Clone, Copy)]
pub struct TokenizerUnavailable;

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
        Ok(Self {
            dict_path: dict_path.into(),
            pool: fixed_pool()?,
            fingerprint: Mutex::new(None),
            build_permit: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_BUILDS)),
            tokenizer: OnceLock::new(),
        })
    }

    /// The `--dict-path` this server was started with.
    #[must_use]
    pub fn dict_path(&self) -> &Path {
        &self.dict_path
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

    /// The dictionary's blake3 fingerprint, computed once for the process.
    ///
    /// The value is [`lingrep_core::dictionary::dictionary_identity`]'s — blake3 over
    /// the decompressed dictionary — which is what every build's
    /// `AnalysisInputs` needs and what an artifact's identity carries. That
    /// function keeps its own on-disk memo, so a warm machine answers without
    /// decompressing 1.31 GB; this mutex keeps the answer for the process on
    /// top of it.
    ///
    /// A failure is deliberately not memoized: `CorpusRuntime::retry` exists so
    /// an operator can fix a bad `--dict-path` and retry without a restart.
    pub fn dictionary_fingerprint(&self) -> Result<[u8; 32], PipelineError> {
        let mut guard = self.fingerprint.lock().expect("fingerprint mutex poisoned");
        if let Some(existing) = *guard {
            return Ok(existing);
        }
        let fingerprint = dictionary_identity(&self.dict_path.to_string_lossy())?;
        *guard = Some(fingerprint);
        Ok(fingerprint)
    }

    /// The sample-analysis tokenizer, built once on first use.
    ///
    /// This lives here rather than on `RequestStore` because `--dict-path` is
    /// a single global: the tokenizer is identical for every corpus, so a
    /// copy per corpus would be a copy of the same dictionary. It is built
    /// with [`create_tokenizer_from_path`], which memory-maps a
    /// globally-cached decompressed copy rather than parsing a second one
    /// into anonymous heap.
    pub fn tokenizer(&self) -> Result<Arc<Tokenizer>, TokenizerUnavailable> {
        if let Some(existing) = self.tokenizer.get() {
            return Ok(Arc::clone(existing));
        }
        let built = Arc::new(
            create_tokenizer_from_path(&self.dict_path.to_string_lossy())
                .map_err(|_| TokenizerUnavailable)?,
        );
        // First writer wins; a lost race discards an equivalent tokenizer.
        let _ = self.tokenizer.set(Arc::clone(&built));
        Ok(built)
    }
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
    use super::{POOL_THREADS, ServerContext};

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
        // Not memoized: a retry must be able to succeed after the operator
        // fixes the path, which is what CorpusRuntime::retry relies on.
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
        assert!(context.tokenizer().is_err());
    }
}
