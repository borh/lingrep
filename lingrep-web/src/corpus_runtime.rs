use std::{
    collections::VecDeque,
    fs::{self, File},
    io::{self, BufReader, Read},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
};

use lingrep_core::{
    analysis::{
        AnalyzerLanes, AnalyzerPlan, AnalyzerSession, ExtractionProfile, InputUnitId,
        NormalizationPolicy, SegmentationMode, TextUnit, VibratoPlan,
    },
    corpus::{
        AdmittedCorpus, AdmittedCorpusEvent, CorpusDiscovery, CorpusEvent, CorpusReadConfig,
        DiscoveryReport, InputFormat, MarkdownReaderConfig, SectionPolicy, SourceDocument,
        resolve_input_format, stream_input_events,
    },
    dictionary::create_tokenizer_from_path,
    execution::{BoundedRecordReader, DEFAULT_MAX_INPUT_RECORD_BYTES},
    spill::TempBudget,
    token_match::{TokenAttributeIndex, TokenInfo},
};
use lingrep_corpus::{
    AdmissionIdentity, AnalysisIdentity, AnalysisInputs, BuildConfig, CorpusArtifact,
    CorpusBuilder, CorpusError, DictionaryFingerprint, IdentityError, ObservedSources,
    PreparedSentenceSource, ProvenanceView, ReaderIdentity, SourceObservation, TrustClass,
    canonical_artifact_path, derive_analysis_identity, format_identity,
};
use rayon::ThreadPool;
use tokio::sync::{Mutex, watch};

use crate::server_context::ServerContext;

const ARTIFACT_TEMP_BUDGET: u64 = 64 * 1024 * 1024 * 1024;

/// Admitted sentences read before a build analyzes and replays them.
///
/// Matches `lingrep_core::execution::DEFAULT_MAX_STAGE_SENTENCES`, and bounds the
/// same thing: how much admitted text and analysis a build holds at once.
const BUILD_WINDOW_SENTENCES: usize = 2_048;

/// Admitted sentence bytes read before a build analyzes and replays them.
///
/// Matches `lingrep_core::execution::DEFAULT_MAX_STAGE_INPUT_BYTES`.
const BUILD_WINDOW_BYTES: usize = 128 * 1024;
pub const CORPUS_DECODE_CHUNK: usize = 128;

/// The reader configuration a server instance was started with.
///
/// `markdown` is a *parameter* rather than a default because section selection
/// decides which lines become sentences: a server started with one policy and a
/// cache built under another describe different corpora. It arrives already
/// resolved from the flags, so an invalid one has been refused at startup.
pub fn read_config(
    path: &Path,
    override_format: Option<lingrep_core::corpus::CorpusFormat>,
    markdown: MarkdownReaderConfig,
) -> CorpusReadConfig {
    CorpusReadConfig::new(
        resolve_input_format(Some(path), override_format),
        markdown.recover_conversion_artifacts(),
    )
}

/// The normalization every `lingrep-web` build admits under.
///
/// Named once because it is two separate identity inputs — the reader's, and
/// the analyzer descriptor's — and two literals that must agree is one literal
/// waiting to disagree.
const BUILD_NORMALIZATION: NormalizationPolicy = NormalizationPolicy::Nfc;

/// The identity of reading `config`, as `ling` derives it.
///
/// A thin call, and the point of it: this used to be
/// `reader_identity_digest(config)` unframed, because the framed derivation was
/// a private helper inside `ling`'s command module and unreachable from
/// here. `lingrep-web` and `ling corpus build` then named two different
/// artifacts for the same JSONL root, with byte-identical decoded content.
fn build_reader_identity(config: &CorpusReadConfig) -> ReaderIdentity {
    ReaderIdentity::derive(config, BUILD_NORMALIZATION)
}

/// The default section policy, for the artifact-backed constructors that read
/// no Markdown at all.
fn default_markdown() -> MarkdownReaderConfig {
    MarkdownReaderConfig::new(SectionPolicy::default())
}

fn classify<E: std::fmt::Display>(
    category: CacheFailureCategory,
    context: &'static str,
) -> impl FnOnce(E) -> CacheFailureCategory {
    move |error| {
        tracing::warn!(context, cause = %error, ?category, "corpus artifact step failed");
        category
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CorpusIdentity(String);

impl CorpusIdentity {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheFailureCategory {
    Configuration,
    Dictionary,
    Corpus,
    Cache,
    Corruption,
    Task,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheReadiness {
    Checking,
    Building { lines_processed: u64 },
    Ready { identity: CorpusIdentity },
    Failed { category: CacheFailureCategory },
}

enum CacheLifecycle {
    Idle,
    Building,
    Ready {
        identity: CorpusIdentity,
        artifact: Arc<CorpusArtifact>,
        artifact_path: Option<PathBuf>,
        random_source: Arc<PreparedSentenceSource>,
    },
    Failed,
}

/// One corpus's opened [`ProvenanceView`], held for as long as the artifact
/// that produced it is the ready one.
///
/// [`CorpusArtifact::provenance_view`] does not merely open the database: it
/// validates every sentence row, decoding each stored analysis blob. That is
/// O(corpus) work — around 3.3 µs per sentence, so roughly 1.5 s on a
/// 523 883-sentence corpus — which every request that reads provenance would
/// otherwise pay in full. The result is a pure function of the artifact, so one open
/// per ready corpus is enough.
///
/// The identity key is what makes that safe. `invalidate_corrupt` discards the
/// corrupt artifact and the corpus is then rebuilt from the same source under
/// the same analyzer, which yields the *same* analysis identity — so the key
/// alone would not notice the rebuild. Clearing the cache on invalidation is
/// what closes that; the key covers every other way one runtime can come to
/// serve a different artifact.
#[derive(Default)]
pub struct ProvenanceCache {
    view: Mutex<Option<(CorpusIdentity, Arc<ProvenanceView>)>>,
}

impl ProvenanceCache {
    /// The view for `identity`, opening and caching it on first use.
    ///
    /// The lock is deliberately held across the open. That makes the cache
    /// single-flight: N requests arriving together against a cold corpus run
    /// one validating scan between them rather than N concurrent ones.
    async fn get_or_open(
        &self,
        identity: &CorpusIdentity,
        artifact: &CorpusArtifact,
    ) -> Result<Arc<ProvenanceView>, CorpusError> {
        let mut guard = self.view.lock().await;
        if let Some((cached, view)) = guard.as_ref()
            && cached == identity
        {
            return Ok(Arc::clone(view));
        }
        let view = Arc::new(artifact.provenance_view().await?);
        *guard = Some((identity.clone(), Arc::clone(&view)));
        Ok(view)
    }

    async fn clear(&self) {
        *self.view.lock().await = None;
    }
}

#[derive(Clone)]
pub struct ReadyCorpus {
    pub identity: CorpusIdentity,
    pub artifact: Arc<CorpusArtifact>,
    pub pool: Arc<ThreadPool>,
    random_source: Arc<PreparedSentenceSource>,
    provenance: Arc<ProvenanceCache>,
}

impl std::fmt::Debug for ReadyCorpus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReadyCorpus")
            .field("identity", &self.identity)
            .field("analysis_identity", self.artifact.analysis_identity())
            .finish_non_exhaustive()
    }
}

impl ReadyCorpus {
    pub fn from_artifact(
        identity: CorpusIdentity,
        artifact: CorpusArtifact,
        pool: Arc<ThreadPool>,
    ) -> Result<Self, CorpusError> {
        let random_source = Arc::new(artifact.sentence_source()?);
        Ok(Self {
            identity,
            artifact: Arc::new(artifact),
            pool,
            random_source,
            provenance: Arc::new(ProvenanceCache::default()),
        })
    }

    pub fn sentence_count(&self) -> Result<u64, CorpusError> {
        Ok(self.random_source.sentence_count())
    }

    /// This corpus's provenance view, opened once and shared thereafter.
    ///
    /// Prefer this to `self.artifact.provenance_view()` on any per-request
    /// path: the uncached call revalidates the whole artifact, which is the
    /// cost [`ProvenanceCache`] exists to pay only once.
    pub async fn provenance(&self) -> Result<Arc<ProvenanceView>, CorpusError> {
        self.provenance
            .get_or_open(&self.identity, &self.artifact)
            .await
    }

    pub fn reader(&self) -> Result<CorpusReader, CorpusReadError> {
        Ok(CorpusReader {
            identity: self.identity.clone(),
            source: Arc::clone(&self.random_source),
            pool: Arc::clone(&self.pool),
        })
    }

    pub fn decoded_block_count(&self) -> usize {
        self.random_source.decoded_block_count()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CorpusReadError {
    #[error("the corpus artifact could not be decoded")]
    Corruption(#[source] CorpusError),
}

#[derive(Clone)]
pub struct CorpusReader {
    identity: CorpusIdentity,
    source: Arc<PreparedSentenceSource>,
    pool: Arc<ThreadPool>,
}

#[derive(Clone)]
pub struct BoundCorpusReader {
    source: Arc<PreparedSentenceSource>,
    pool: Arc<ThreadPool>,
}

impl CorpusReader {
    pub fn identity(&self) -> &CorpusIdentity {
        &self.identity
    }

    pub fn bind(&self, identity: &CorpusIdentity) -> Option<BoundCorpusReader> {
        (identity == &self.identity).then(|| BoundCorpusReader {
            source: Arc::clone(&self.source),
            pool: Arc::clone(&self.pool),
        })
    }

    pub fn decoded_block_count(&self) -> usize {
        self.source.decoded_block_count()
    }
}

impl BoundCorpusReader {
    pub async fn sentence(&self, index: u64) -> Result<Option<Vec<TokenInfo>>, CorpusReadError> {
        self.decode_sentences(&[index])
            .map(|mut values| values.pop().flatten())
    }

    pub fn decode_sentences(
        &self,
        indices: &[u64],
    ) -> Result<Vec<Option<Vec<TokenInfo>>>, CorpusReadError> {
        self.source
            .decode_sentences(&self.pool, indices)
            .map_err(CorpusReadError::Corruption)
    }

    pub fn decode_sentences_with_attributes(
        &self,
        indices: &[u64],
        attributes: &[TokenAttributeIndex],
    ) -> Result<Vec<Option<Vec<TokenInfo>>>, CorpusReadError> {
        self.source
            .decode_sentences_with_attributes(&self.pool, indices, attributes)
            .map_err(CorpusReadError::Corruption)
    }

    pub fn decoded_block_count(&self) -> usize {
        self.source.decoded_block_count()
    }
}

struct RuntimeInner {
    corpus_path: PathBuf,
    context: Arc<ServerContext>,
    input_format: Option<lingrep_core::corpus::CorpusFormat>,
    markdown: MarkdownReaderConfig,
    max_record_bytes: usize,
    lifecycle: Mutex<CacheLifecycle>,
    readiness_tx: watch::Sender<CacheReadiness>,
    corruption_reported: AtomicBool,
    /// Shared with every [`ReadyCorpus`] this runtime hands out, so one open
    /// serves every request against the same artifact.
    provenance: Arc<ProvenanceCache>,
}

#[derive(Clone)]
pub struct CorpusRuntime {
    inner: Arc<RuntimeInner>,
}

impl std::fmt::Debug for CorpusRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CorpusRuntime")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct CorpusRuntimeInvalidator {
    inner: Arc<RuntimeInner>,
}

impl CorpusRuntime {
    pub async fn open(
        path: impl AsRef<Path>,
        context: Arc<ServerContext>,
    ) -> Result<Self, CorpusError> {
        let artifact_path = path.as_ref().to_path_buf();
        let artifact = Arc::new(CorpusArtifact::open(&artifact_path)?);
        let _provenance = artifact.provenance_view().await?;
        let random_source = Arc::new(artifact.sentence_source()?);
        let identity =
            CorpusIdentity::new(format_identity(artifact.analysis_identity().as_bytes()));
        let (readiness_tx, _) = watch::channel(CacheReadiness::Ready {
            identity: identity.clone(),
        });
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                corpus_path: PathBuf::new(),
                context,
                input_format: None,
                markdown: default_markdown(),
                max_record_bytes: DEFAULT_MAX_INPUT_RECORD_BYTES,
                lifecycle: Mutex::new(CacheLifecycle::Ready {
                    identity,
                    artifact,
                    artifact_path: Some(artifact_path),
                    random_source,
                }),
                readiness_tx,
                corruption_reported: AtomicBool::new(false),
                provenance: Arc::new(ProvenanceCache::default()),
            }),
        })
    }

    pub fn from_ready(
        identity: CorpusIdentity,
        artifact: CorpusArtifact,
        context: Arc<ServerContext>,
    ) -> Self {
        let random_source = Arc::new(
            artifact
                .sentence_source()
                .expect("from_ready requires a valid V1 sentence source"),
        );
        let (readiness_tx, _) = watch::channel(CacheReadiness::Ready {
            identity: identity.clone(),
        });
        Self {
            inner: Arc::new(RuntimeInner {
                corpus_path: PathBuf::new(),
                context,
                input_format: None,
                markdown: default_markdown(),
                max_record_bytes: DEFAULT_MAX_INPUT_RECORD_BYTES,
                lifecycle: Mutex::new(CacheLifecycle::Ready {
                    identity,
                    artifact: Arc::new(artifact),
                    artifact_path: None,
                    random_source,
                }),
                readiness_tx,
                corruption_reported: AtomicBool::new(false),
                provenance: Arc::new(ProvenanceCache::default()),
            }),
        }
    }

    pub fn new(context: Arc<ServerContext>, corpus_path: impl Into<PathBuf>) -> io::Result<Self> {
        Self::with_reader(
            context,
            corpus_path,
            None,
            default_markdown(),
            DEFAULT_MAX_INPUT_RECORD_BYTES,
        )
    }

    pub fn new_with_override(
        context: Arc<ServerContext>,
        corpus_path: impl Into<PathBuf>,
        input_format: Option<lingrep_core::corpus::CorpusFormat>,
        markdown: MarkdownReaderConfig,
        max_record_bytes: usize,
    ) -> io::Result<Self> {
        Self::with_reader(
            context,
            corpus_path,
            input_format,
            markdown,
            max_record_bytes,
        )
    }

    fn with_reader(
        context: Arc<ServerContext>,
        corpus_path: impl Into<PathBuf>,
        input_format: Option<lingrep_core::corpus::CorpusFormat>,
        markdown: MarkdownReaderConfig,
        max_record_bytes: usize,
    ) -> io::Result<Self> {
        let (readiness_tx, _) = watch::channel(CacheReadiness::Checking);
        Ok(Self {
            inner: Arc::new(RuntimeInner {
                corpus_path: corpus_path.into(),
                context,
                input_format,
                markdown,
                max_record_bytes,
                lifecycle: Mutex::new(CacheLifecycle::Idle),
                readiness_tx,
                corruption_reported: AtomicBool::new(false),
                provenance: Arc::new(ProvenanceCache::default()),
            }),
        })
    }

    pub fn readiness(&self) -> watch::Receiver<CacheReadiness> {
        self.inner.readiness_tx.subscribe()
    }

    /// The process-wide shared resources this runtime was built with. Cloning
    /// shares the same `ServerContext`; it is never rebuilt.
    #[must_use]
    pub fn context(&self) -> Arc<ServerContext> {
        Arc::clone(&self.inner.context)
    }

    pub fn invalidator(&self) -> CorpusRuntimeInvalidator {
        CorpusRuntimeInvalidator {
            inner: Arc::clone(&self.inner),
        }
    }

    pub async fn ready(&self) -> Option<ReadyCorpus> {
        match &*self.inner.lifecycle.lock().await {
            CacheLifecycle::Ready {
                identity,
                artifact,
                artifact_path: _,
                random_source,
            } => Some(ReadyCorpus {
                identity: identity.clone(),
                artifact: Arc::clone(artifact),
                pool: self.inner.context.pool(),
                random_source: Arc::clone(random_source),
                provenance: Arc::clone(&self.inner.provenance),
            }),
            _ => None,
        }
    }

    pub async fn start_or_observe(&self) {
        let mut lifecycle = self.inner.lifecycle.lock().await;
        if !matches!(*lifecycle, CacheLifecycle::Idle) {
            return;
        }
        *lifecycle = CacheLifecycle::Building;
        self.inner
            .readiness_tx
            .send_replace(CacheReadiness::Building { lines_processed: 0 });
        drop(lifecycle);
        self.spawn_builder();
    }

    pub async fn retry(&self) {
        let mut lifecycle = self.inner.lifecycle.lock().await;
        if !matches!(*lifecycle, CacheLifecycle::Failed) {
            return;
        }
        *lifecycle = CacheLifecycle::Idle;
        self.inner
            .corruption_reported
            .store(false, Ordering::SeqCst);
        drop(lifecycle);
        self.start_or_observe().await;
    }

    fn spawn_builder(&self) {
        let inner = Arc::clone(&self.inner);
        let worker_inner = Arc::clone(&inner);
        let handle = tokio::runtime::Handle::current();
        let worker = tokio::task::spawn_blocking(move || prepare_artifact(&worker_inner, &handle));
        tokio::spawn(async move {
            let result = worker.await.unwrap_or(Err(CacheFailureCategory::Task));
            let mut lifecycle = inner.lifecycle.lock().await;
            match result {
                Ok((identity, artifact, artifact_path)) => {
                    let random_source = if let Ok(source) = artifact.sentence_source() {
                        Arc::new(source)
                    } else {
                        *lifecycle = CacheLifecycle::Failed;
                        inner.readiness_tx.send_replace(CacheReadiness::Failed {
                            category: CacheFailureCategory::Corruption,
                        });
                        return;
                    };
                    *lifecycle = CacheLifecycle::Ready {
                        identity: identity.clone(),
                        artifact: Arc::new(artifact),
                        artifact_path,
                        random_source,
                    };
                    inner
                        .readiness_tx
                        .send_replace(CacheReadiness::Ready { identity });
                }
                Err(category) => {
                    *lifecycle = CacheLifecycle::Failed;
                    inner
                        .readiness_tx
                        .send_replace(CacheReadiness::Failed { category });
                }
            }
        });
    }
}

impl CorpusRuntimeInvalidator {
    pub async fn invalidate_corrupt(&self) -> bool {
        if self.inner.corruption_reported.swap(true, Ordering::SeqCst) {
            return false;
        }
        tracing::error!("corpus artifact reported corrupt; flipping runtime to Failed");
        let mut lifecycle = self.inner.lifecycle.lock().await;
        let artifact_path = match &*lifecycle {
            CacheLifecycle::Ready { artifact_path, .. }
                if !self.inner.corpus_path.as_os_str().is_empty() =>
            {
                artifact_path.clone()
            }
            _ => None,
        };
        *lifecycle = CacheLifecycle::Failed;
        drop(lifecycle);
        // Required, not hygiene: the corrupt artifact is discarded and rebuilt
        // from the same source under the same analyzer, so the rebuild carries
        // the *same* analysis identity. The cache's identity key would hand
        // back the dead artifact's view; only clearing it here does not.
        self.inner.provenance.clear().await;
        if let Some(path) = artifact_path
            && let Err(error) = fs::remove_dir_all(&path)
        {
            tracing::error!(path = %path.display(), cause = %error, "failed to discard corrupt artifact");
        }
        self.inner
            .readiness_tx
            .send_replace(CacheReadiness::Failed {
                category: CacheFailureCategory::Corruption,
            });
        true
    }
}

struct HashingReader<R> {
    reader: R,
    observation: SourceObservation,
    ended: bool,
}

impl<R: Read> HashingReader<R> {
    fn new(
        reader: R,
        observation: SourceObservation,
        logical_name: &str,
    ) -> Result<Self, IdentityError> {
        observation.begin_source(logical_name)?;
        Ok(Self {
            reader,
            observation,
            ended: false,
        })
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return self.reader.read(buffer);
        }
        let read = self.reader.read(buffer)?;
        if read == 0 {
            if !self.ended {
                self.observation.end_source().map_err(io::Error::other)?;
                self.ended = true;
            }
        } else {
            self.observation
                .update_source_bytes(&buffer[..read])
                .map_err(io::Error::other)?;
        }
        Ok(read)
    }
}

#[derive(Clone, Default)]
struct DiscoveredDocuments(Arc<StdMutex<VecDeque<SourceDocument>>>);

impl DiscoveredDocuments {
    fn drain(&self) -> Result<Vec<SourceDocument>, CacheFailureCategory> {
        let mut documents = self.0.lock().map_err(classify(
            CacheFailureCategory::Task,
            "lock discovered document queue",
        ))?;
        Ok(documents.drain(..).collect())
    }
}

struct ArtifactCorpusEvents {
    discovery: Option<CorpusDiscovery>,
    current: Option<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>>,
    config: CorpusReadConfig,
    observation: SourceObservation,
    max_record_bytes: usize,
    documents: DiscoveredDocuments,
    report: Arc<StdMutex<Option<DiscoveryReport>>>,
}

impl ArtifactCorpusEvents {
    fn new(
        root: &Path,
        config: CorpusReadConfig,
        observation: SourceObservation,
        max_record_bytes: usize,
    ) -> Result<
        (
            AdmittedCorpus<Self>,
            DiscoveredDocuments,
            Arc<StdMutex<Option<DiscoveryReport>>>,
        ),
        CacheFailureCategory,
    > {
        let discovery =
            CorpusDiscovery::new(root, lingrep_core::corpus::TraversalLimits::default()).map_err(
                classify(
                    CacheFailureCategory::Configuration,
                    "open recursive source discovery",
                ),
            )?;
        let documents = DiscoveredDocuments::default();
        let report = Arc::new(StdMutex::new(None));
        let events = Self {
            discovery: Some(discovery),
            current: None,
            config,
            observation,
            max_record_bytes,
            documents: documents.clone(),
            report: Arc::clone(&report),
        };
        Ok((AdmittedCorpus::new(events), documents, report))
    }

    fn open_document(
        &self,
        document: &SourceDocument,
    ) -> io::Result<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>> {
        let file = File::open(&document.path)?;
        let reader = HashingReader::new(file, self.observation.clone(), &document.logical_name)
            .map_err(io::Error::other)?;
        let (reader, _) = BoundedRecordReader::new(
            BufReader::new(reader),
            self.max_record_bytes,
            document.logical_name.clone(),
        );
        let format = match &self.config {
            CorpusReadConfig::Plain => InputFormat::Plaintext,
            CorpusReadConfig::Jsonl => InputFormat::Jsonl,
            // Refused at declaration validation. Raw CoNLL-U must not pass
            // through this worker's configured analyzer; lingrep-web accepts the
            // prepared artifact produced by ling instead.
            CorpusReadConfig::Conllu => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a raw conllu corpus must be prepared with ling before lingrep-web serves it",
                ));
            }
            CorpusReadConfig::Markdown(config) => InputFormat::Markdown(config.clone()),
            // Refused at declaration validation (see `State` validation); this
            // arm keeps the mapping total for any construction path.
            CorpusReadConfig::Bccwj => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a bccwj corpus is not supported by lingrep-web; ling reads BCCWJ distributions",
                ));
            }
            CorpusReadConfig::Soranoha => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a soranoha archive must be prepared with ling corpus build before lingrep-web serves it",
                ));
            }
        };
        Ok(stream_input_events(
            format,
            reader,
            document.logical_name.clone(),
            None,
        ))
    }
}

impl Iterator for ArtifactCorpusEvents {
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.current.as_mut().and_then(Iterator::next) {
                return Some(event);
            }
            self.current = None;
            let discovery = self.discovery.as_mut()?;
            match discovery.next_document() {
                Ok(Some(document)) => match self.open_document(&document) {
                    Ok(events) => {
                        let mut pending = match self.documents.0.lock() {
                            Ok(pending) => pending,
                            Err(_) => {
                                return Some(Err(io::Error::other(
                                    "discovered document queue lock poisoned",
                                )));
                            }
                        };
                        pending.push_back(document);
                        drop(pending);
                        self.current = Some(events);
                    }
                    Err(error) => return Some(Err(error)),
                },
                Ok(None) => {
                    let discovery = self
                        .discovery
                        .take()
                        .expect("open discovery was just borrowed");
                    return match discovery.finish() {
                        Ok(report) => match self.report.lock() {
                            Ok(mut stored) => {
                                *stored = Some(report);
                                None
                            }
                            Err(_) => Some(Err(io::Error::other("discovery report lock poisoned"))),
                        },
                        Err(error) => Some(Err(io::Error::other(error))),
                    };
                }
                Err(error) => return Some(Err(io::Error::other(error))),
            }
        }
    }
}

fn finish_discovery(
    documents: &DiscoveredDocuments,
    report: &Arc<StdMutex<Option<DiscoveryReport>>>,
) -> Result<DiscoveryReport, CacheFailureCategory> {
    if !documents.drain()?.is_empty() {
        return Err(CacheFailureCategory::Corruption);
    }
    report
        .lock()
        .map_err(classify(
            CacheFailureCategory::Task,
            "lock discovery report",
        ))?
        .clone()
        .ok_or(CacheFailureCategory::Corruption)
}

fn finish_source_stream(
    observation: &SourceObservation,
    documents: &DiscoveredDocuments,
    report: &Arc<StdMutex<Option<DiscoveryReport>>>,
) -> Result<(ObservedSources, DiscoveryReport), CacheFailureCategory> {
    let report = finish_discovery(documents, report)?;
    let observed = observation.finish().map_err(classify(
        CacheFailureCategory::Corpus,
        "finish source observation",
    ))?;
    if observed.document_count() != report.inventory.documents {
        return Err(CacheFailureCategory::Corruption);
    }
    Ok((observed, report))
}

/// Bring this runtime's corpus to a ready artifact.
///
/// The returned path is the artifact **this runtime owns** and may discard on
/// corruption. It is `None` for an artifact the operator declared directly:
/// that one is not reproducible from anything lingrep-web holds — there is no
/// source beside it to rebuild from — so discarding it would destroy data
/// rather than reclaim a cache.
fn prepare_artifact(
    inner: &RuntimeInner,
    handle: &tokio::runtime::Handle,
) -> Result<(CorpusIdentity, CorpusArtifact, Option<PathBuf>), CacheFailureCategory> {
    if lingrep_corpus::is_prepared_artifact(&inner.corpus_path) {
        return adopt_declared_artifact(inner, handle);
    }
    let read_config = read_config(
        &inner.corpus_path,
        inner.input_format,
        inner.markdown.clone(),
    );
    let observed = scan_source(
        &inner.corpus_path,
        read_config.clone(),
        inner.max_record_bytes,
    )?;
    let fingerprint = inner.context.dictionary_fingerprint().map_err(classify(
        CacheFailureCategory::Dictionary,
        "load dictionary",
    ))?;
    // The descriptor is derived from the fingerprint rather than from a
    // loaded plan: naming a content-addressed artifact has to happen *before*
    // the ~1.31 GB dictionary load below, and the analyzer's semantics are a
    // pure function of the settings `build_artifact` will configure the plan
    // with.
    let descriptor = VibratoPlan::descriptor_for(
        fingerprint,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        BUILD_NORMALIZATION,
    );
    let analysis = AnalysisInputs::new(
        build_reader_identity(&read_config),
        AdmissionIdentity::from_bytes(lingrep_core::corpus::admission_identity_digest()),
        &descriptor,
    );
    let analysis_identity = derive_analysis_identity(&observed, &analysis);
    let identity = CorpusIdentity::new(format_identity(analysis_identity.as_bytes()));
    let destination = canonical_artifact_path(
        &inner.corpus_path,
        &analysis_identity,
        TrustClass::LocallyExecuted,
    );

    if destination.exists() {
        let artifact = CorpusArtifact::open(&destination).map_err(classify(
            CacheFailureCategory::Corruption,
            "open content-addressed V1 artifact",
        ))?;
        if artifact.source_identity() != observed.source_identity()
            || artifact.analysis_identity() != &analysis_identity
        {
            return Err(CacheFailureCategory::Corruption);
        }
        handle
            .block_on(artifact.provenance_view())
            .map_err(classify(
                CacheFailureCategory::Corruption,
                "open V1 provenance view",
            ))?;
        return Ok((identity, artifact, Some(destination)));
    }
    // The permit still bounds concurrent builds, and it is still acquired
    // before the dictionary is opened. It does not bound a ~1.31 GB decoded
    // buffer per queued build: the tokenizer is built inside
    // `build_artifact` from `create_tokenizer_from_path`, which memory-maps a
    // globally-cached decompressed copy. That is the same loader
    // `ServerContext::tokenizer` uses, and for the same reason its
    // documentation gives.
    let permit = handle
        .block_on(inner.context.build_permit().acquire_owned())
        .expect("the build semaphore is never closed");
    let artifact = build_artifact(
        inner,
        handle,
        &destination,
        read_config,
        analysis,
        analysis_identity,
        fingerprint,
    );
    drop(permit);
    let artifact = artifact?;
    handle
        .block_on(artifact.provenance_view())
        .map_err(classify(
            CacheFailureCategory::Corruption,
            "open V1 provenance view",
        ))?;
    Ok((identity, artifact, Some(destination)))
}

/// Adopt an artifact the operator named directly, instead of building one.
///
/// A prepared artifact is **producer-free**: nothing here reads, or needs, the
/// source it was built from. That is what lets lingrep-web serve a corpus format it
/// cannot itself read — a BCCWJ distribution prepared by
/// `ling corpus build --input-format bccwj` is an ordinary artifact by the
/// time it gets here — and it is why no reader configuration is consulted on
/// this path.
///
/// `CorpusArtifact::open` is the admission check: manifest, component presence
/// and component lengths. It hashes nothing, so a declared artifact starts
/// serving without a scan; complete verification remains
/// `ling corpus inspect --verify`.
fn adopt_declared_artifact(
    inner: &RuntimeInner,
    handle: &tokio::runtime::Handle,
) -> Result<(CorpusIdentity, CorpusArtifact, Option<PathBuf>), CacheFailureCategory> {
    let artifact = CorpusArtifact::open(&inner.corpus_path).map_err(classify(
        CacheFailureCategory::Corruption,
        "open declared V1 artifact",
    ))?;
    handle
        .block_on(artifact.provenance_view())
        .map_err(classify(
            CacheFailureCategory::Corruption,
            "open V1 provenance view",
        ))?;
    let identity = CorpusIdentity::new(format_identity(artifact.analysis_identity().as_bytes()));
    // `None`: declared, not built here, and so never this runtime's to delete.
    Ok((identity, artifact, None))
}

fn build_artifact(
    inner: &RuntimeInner,
    handle: &tokio::runtime::Handle,
    destination: &Path,
    read_config: CorpusReadConfig,
    analysis: AnalysisInputs,
    expected_analysis: AnalysisIdentity,
    fingerprint: [u8; 32],
) -> Result<CorpusArtifact, CacheFailureCategory> {
    let tokenizer =
        create_tokenizer_from_path(&inner.context.dict_path().to_string_lossy()).map_err(
            classify(CacheFailureCategory::Dictionary, "create tokenizer"),
        )?;
    let plan = VibratoPlan::configured(
        tokenizer,
        fingerprint,
        ExtractionProfile::Morphology,
        SegmentationMode::Input,
        BUILD_NORMALIZATION,
    );
    let analyzer_record = lingrep_corpus::AnalyzerRecord::from_descriptor(plan.descriptor());
    let schema = plan.schema().clone();
    let projection = schema
        .bind_projection(schema.keys().iter().cloned())
        .map_err(classify(
            CacheFailureCategory::Cache,
            "bind V1 analyzer schema",
        ))?;
    // Analysis lanes over the one shared scan pool, not a pool of their own:
    // thread count stays a property of the process, not of how many corpora
    // are declared, and this build already holds the single build permit.
    let pool = inner.context.pool();
    let mut session = AnalyzerLanes::new(&plan, pool.current_num_threads(), &pool);
    let mut builder = handle
        .block_on(CorpusBuilder::begin_with_schema(
            BuildConfig {
                analysis,
                analyzer: analyzer_record,
                dictionary: DictionaryFingerprint::from_bytes(fingerprint),
                trust: TrustClass::LocallyExecuted,
                temp_budget: Arc::new(TempBudget::new(ARTIFACT_TEMP_BUDGET)),
                sentence_metadata_keys: read_config
                    .format()
                    .sentence_metadata_keys()
                    .iter()
                    .map(|key| (*key).to_owned())
                    .collect(),
            },
            destination,
            schema,
        ))
        .map_err(classify(CacheFailureCategory::Cache, "begin V1 artifact"))?;
    let observation = builder.source_observation();
    let (mut events, documents, report) = ArtifactCorpusEvents::new(
        &inner.corpus_path,
        read_config,
        observation.clone(),
        inner.max_record_bytes,
    )?;
    let mut active_physical = None;
    let mut unit_index = 0_u64;
    // A bounded read window, analyzed as one batch and replayed in read order.
    // The reader's interleaving is buffered along with its events, so a
    // drained source still registers exactly where it did before; only the
    // tokenizing half moved off this thread.
    let mut window: Vec<(Vec<SourceDocument>, Option<AdmittedCorpusEvent>)> = Vec::new();
    let mut reached_end = false;
    while !reached_end {
        window.clear();
        let mut window_sentences = 0_usize;
        let mut window_bytes = 0_usize;
        while window_sentences < BUILD_WINDOW_SENTENCES && window_bytes < BUILD_WINDOW_BYTES {
            let event = events
                .next()
                .transpose()
                .map_err(classify(CacheFailureCategory::Corpus, "read corpus event"))?;
            let drained = documents.drain()?;
            if let Some(sentence) = event.as_ref().and_then(AdmittedCorpusEvent::sentence) {
                window_sentences += 1;
                window_bytes += sentence.text.len();
            }
            reached_end = event.is_none();
            window.push((drained, event));
            if reached_end {
                break;
            }
        }

        let inputs = window
            .iter()
            .filter_map(|(_, event)| event.as_ref().and_then(AdmittedCorpusEvent::sentence))
            .map(|sentence| {
                TextUnit::sentence(
                    InputUnitId::new(sentence.id.get()),
                    sentence.text.clone(),
                    BUILD_NORMALIZATION,
                )
                .map_err(|_| CacheFailureCategory::Corruption)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut analyzed = if inputs.is_empty() {
            Vec::new()
        } else {
            session
                .analyze_bounded_batch(inputs, &projection)
                .map_err(|_| CacheFailureCategory::Corruption)?
                .into_inputs()
                .into_vec()
        }
        .into_iter();

        for (drained, event) in window.drain(..) {
            for document in drained {
                builder
                    .register_discovered_source(&document)
                    .map_err(classify(
                        CacheFailureCategory::Cache,
                        "register discovered V1 source",
                    ))?;
                active_physical = Some(document);
            }
            let Some(event) = event else {
                break;
            };
            if let AdmittedCorpusEvent::DocumentStart {
                id, source_name, ..
            } = &event
            {
                let physical = active_physical
                    .as_ref()
                    .ok_or(CacheFailureCategory::Corruption)?;
                builder
                    .register_admitted_document_from_source(id.get(), source_name, physical)
                    .map_err(classify(
                        CacheFailureCategory::Cache,
                        "register admitted V1 document",
                    ))?;
            }
            let sentence = if event.sentence().is_some() {
                Some(analyzed.next().ok_or(CacheFailureCategory::Corruption)?)
            } else {
                None
            };
            let location = handle
                .block_on(builder.push_corpus_event(&event))
                .map_err(classify(
                    CacheFailureCategory::Cache,
                    "push V1 corpus event",
                ))?;
            if let (Some(location), Some(analyzed)) = (location, sentence) {
                handle
                    .block_on(builder.push_analyzed_sentence(&location, analyzed, &projection))
                    .map_err(classify(
                        CacheFailureCategory::Cache,
                        "push V1 tokenized sentence",
                    ))?;
            }
            if unit_index.is_multiple_of(1_024) {
                inner.readiness_tx.send_replace(CacheReadiness::Building {
                    lines_processed: unit_index + 1,
                });
            }
            unit_index += 1;
        }
    }
    let report = finish_discovery(&documents, &report)?;
    let artifact =
        match handle.block_on(builder.finish_expected_analysis(report, expected_analysis)) {
            Ok(artifact) => artifact,
            Err(CorpusError::DestinationExists { .. }) => CorpusArtifact::open(destination)
                .map_err(classify(
                    CacheFailureCategory::Cache,
                    "open concurrently published V1 artifact",
                ))?,
            Err(error) => {
                return Err(classify(
                    CacheFailureCategory::Cache,
                    "publish immutable V1 artifact",
                )(error));
            }
        };
    Ok(artifact)
}

fn scan_source(
    root: &Path,
    config: CorpusReadConfig,
    max_record_bytes: usize,
) -> Result<ObservedSources, CacheFailureCategory> {
    let observation = SourceObservation::new();
    let (events, documents, report) =
        ArtifactCorpusEvents::new(root, config, observation.clone(), max_record_bytes)?;
    for event in events {
        event.map_err(classify(
            CacheFailureCategory::Corpus,
            "preflight corpus event",
        ))?;
        documents.drain()?;
    }
    let (observed, _) = finish_source_stream(&observation, &documents, &report)?;
    Ok(observed)
}

#[cfg(test)]
pub(crate) fn test_ready_corpus(
    sentences: Vec<Vec<TokenInfo>>,
) -> (tempfile::TempDir, ReadyCorpus) {
    use lingrep_core::corpus::{
        AdmittedSentence, DiscoveryReport, DocumentId, InventoryStats, SourceDocument,
        TraversalHighWater, TraversalLimits,
    };

    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("unit.lingcorpus");
    let build_destination = destination.clone();
    let artifact = std::thread::spawn(move || {
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(async move {
                let mut builder = CorpusBuilder::begin(
                    BuildConfig {
                        analysis: AnalysisInputs::new(
                            ReaderIdentity::from_bytes([1; 32]),
                            AdmissionIdentity::from_bytes([2; 32]),
                            &lingrep_corpus::vibrato_fixture_descriptor(),
                        ),
                        analyzer: lingrep_corpus::AnalyzerRecord::vibrato_fixture(),
                        dictionary: DictionaryFingerprint::from_bytes([3; 32]),
                        trust: TrustClass::LocallyExecuted,
                        temp_budget: Arc::new(TempBudget::new(16 * 1024 * 1024)),
                        sentence_metadata_keys: Vec::new(),
                    },
                    &build_destination,
                )
                .await
                .unwrap();
                let source = SourceDocument {
                    ordinal: 0,
                    logical_name: "unit.md".into(),
                    path: PathBuf::from("/unused/unit.md"),
                    target_group_ordinal: 0,
                    external_target: false,
                };
                builder.register_source_document(&source).unwrap();
                let observation = builder.source_observation();
                observation.begin_source("unit.md").unwrap();
                observation.update_source_bytes(b"unit").unwrap();
                observation.end_source().unwrap();
                builder
                    .push_corpus_event(&AdmittedCorpusEvent::DocumentStart {
                        id: DocumentId::new(0),
                        source_name: "unit.md".into(),
                        metadata: Box::new([]),
                    })
                    .await
                    .unwrap();
                builder
                    .push_corpus_event(&AdmittedCorpusEvent::DivisionStart(std::sync::Arc::new(
                        lingrep_core::corpus::Division {
                            source_id: None,
                            id: lingrep_core::corpus::DivisionId(0),
                            document_id: DocumentId::new(0),
                            kind: "paragraph".into(),
                            parent_id: None,
                            metadata: Box::new([]),
                        },
                    )))
                    .await
                    .unwrap();
                for (ordinal, tokens) in sentences.iter().enumerate() {
                    let location = builder
                        .push_corpus_event(&AdmittedCorpusEvent::Sentence(AdmittedSentence {
                            structure: Default::default(),
                            id: lingrep_core::corpus::SentenceId::new(ordinal as u64),
                            document_id: DocumentId::new(0),
                            document_sentence_ordinal: ordinal as u64,
                            text: String::new(),
                            words: None,
                            enhanced_graph: None,
                            active_labels: Box::new([]),
                            metadata: Box::new([]),
                        }))
                        .await
                        .unwrap()
                        .unwrap();
                    builder
                        .push_tokenized_sentence(&location, tokens)
                        .await
                        .unwrap();
                }
                builder
                    .push_corpus_event(&AdmittedCorpusEvent::DivisionEnd {
                        id: lingrep_core::corpus::DivisionId(0),
                    })
                    .await
                    .unwrap();
                builder
                    .push_corpus_event(&AdmittedCorpusEvent::DocumentEnd {
                        id: DocumentId::new(0),
                    })
                    .await
                    .unwrap();
                let mut digest = blake3::Hasher::new();
                digest.update(&7_u64.to_le_bytes());
                digest.update(b"unit.md");
                builder
                    .finish(DiscoveryReport {
                        inventory: InventoryStats {
                            documents: 1,
                            distinct_targets: 1,
                            alias_documents: 0,
                            max_target_multiplicity: 1,
                            external_targets: 0,
                        },
                        configured_limits: TraversalLimits::default(),
                        observed: TraversalHighWater::default(),
                        ordered_inventory_digest: *digest.finalize().as_bytes(),
                    })
                    .await
                    .unwrap()
            })
    })
    .join()
    .unwrap();
    let ready = ReadyCorpus::from_artifact(
        CorpusIdentity::new("fixture"),
        artifact,
        Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(2)
                .build()
                .unwrap(),
        ),
    )
    .unwrap();
    (directory, ready)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `lingrep-web` and `ling corpus build` must address one JSONL root once.
    ///
    /// The literal is `ling`'s, pinned again on this side so the two are
    /// contested by a value rather than by an argument. `lingrep-web` used to derive
    /// `ea7b9486…` here — the unframed reader digest, because the framed
    /// derivation was private to `ling`'s command module — so one source
    /// produced two artifacts whose decoded sentences were byte-identical.
    ///
    /// A plaintext root is asserted alongside, since the documented contract
    /// covers both, and Markdown is asserted to *differ*: `lingrep-web` reads that
    /// dialect with conversion-artifact recovery and the CLI does not, which is
    /// part of `reader_tag`, so two names for one `.md` tree is correct.
    #[test]
    fn a_jsonl_or_plaintext_root_is_addressed_exactly_as_lingrep_addresses_it() {
        let hex = |identity: &ReaderIdentity| {
            identity
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        };

        assert_eq!(
            hex(&build_reader_identity(&CorpusReadConfig::Jsonl)),
            "3422793012a2f7130725d91afcdc995b56fd3c7df69d5e7fcfad2c5ef91a1d34"
        );
        assert_eq!(
            hex(&build_reader_identity(&CorpusReadConfig::Plain)),
            "b1ff0711e7ca79c5d0b98075acdde8b50368c121b8f7829aec6da7aaa9755ca2"
        );

        let markdown = CorpusReadConfig::Markdown(
            MarkdownReaderConfig::new(SectionPolicy::default()).recover_conversion_artifacts(),
        );
        assert_ne!(
            hex(&build_reader_identity(&markdown)),
            "e6e3551c916214f19813eca5e74f734f70598ba643888cad52b459516ccf9d53",
            "the Markdown dialects differ by design and must keep two names"
        );
    }

    /// The reader identity and the analyzer descriptor read one policy.
    ///
    /// They are two independent identity inputs written from the same constant,
    /// and nothing else forces them to agree.
    #[test]
    fn one_normalization_reaches_both_identity_inputs() {
        assert_eq!(
            build_reader_identity(&CorpusReadConfig::Jsonl),
            ReaderIdentity::derive(&CorpusReadConfig::Jsonl, BUILD_NORMALIZATION)
        );
        assert_eq!(
            VibratoPlan::descriptor_for(
                [0; 32],
                ExtractionProfile::Morphology,
                SegmentationMode::Input,
                BUILD_NORMALIZATION,
            )
            .normalization(),
            BUILD_NORMALIZATION
        );
    }

    #[test]
    fn ready_sentence_count_uses_the_already_open_source() {
        let (directory, ready) = test_ready_corpus(vec![vec![TokenInfo::default()]]);
        let token_path = directory.path().join("unit.lingcorpus/tokens.v1.zst");
        let moved = directory.path().join("tokens.v1.zst.moved");
        std::fs::rename(&token_path, &moved).unwrap();

        assert_eq!(ready.sentence_count().unwrap(), 1);
    }
}

#[cfg(test)]
mod cause_logging_tests {
    use super::{CacheFailureCategory, classify};

    #[test]
    fn classify_preserves_the_coarse_category() {
        let error = std::io::Error::other("disk gone");
        let category = classify(CacheFailureCategory::Cache, "open corpus component")(error);
        assert_eq!(category, CacheFailureCategory::Cache);
    }
}
