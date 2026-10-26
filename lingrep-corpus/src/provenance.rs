use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    sync::Arc,
};

use sqlx::{
    Connection, Executor, Row, SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};

use crate::{AnalysisIdentity, CorpusError, DocumentId, SentenceId, SentenceLocation};
use lingrep_core::analysis::{AnalyzedInput, NormalizationPolicy, TextUnitKind};

mod candidate;

const COMPONENT: &str = "provenance.sqlite";

/// Schema of the immutable lingrep provenance component.
pub(crate) const SCHEMA_VERSION: u32 = 4;

/// Hard application-memory bound for one section-coverage result.
pub const MAX_SECTION_LABELS: usize = 100_000;

const SCHEMA: &str = r#"
CREATE TABLE provenance_identity (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL,
    analysis_identity BLOB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('building', 'ready')),
    sentence_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE document_metadata (
    document_id INTEGER PRIMARY KEY,
    first_sentence INTEGER NOT NULL UNIQUE CHECK (first_sentence >= 0),
    sentence_count INTEGER NOT NULL CHECK (sentence_count > 0),
    metadata_json TEXT
);

CREATE TABLE sentence_provenance (
    sentence_id INTEGER PRIMARY KEY,
    source_name TEXT NOT NULL,
    document_id INTEGER NOT NULL,
    document_ordinal INTEGER NOT NULL,
    document_sentence_ordinal INTEGER NOT NULL,
    region_labels_json TEXT NOT NULL,
    region_span_ids_json TEXT NOT NULL,
    division_ids_json TEXT NOT NULL,
    analysis_blob BLOB NOT NULL,
    input_unit_id INTEGER,
    input_kind TEXT,
    normalization TEXT,
    input_identity BLOB,
    input_source_identity BLOB,
    input_document_identity INTEGER,
    input_paragraph_identity INTEGER,
    input_metadata_digest BLOB,
    analyzer_sentence_ordinal INTEGER,
    source_start INTEGER,
    source_end INTEGER
);

CREATE TABLE divisions (
    division_id INTEGER PRIMARY KEY,
    document_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    source_id TEXT,
    -- Children close before parents; check their references at build commit.
    parent_id INTEGER REFERENCES divisions(division_id) DEFERRABLE INITIALLY DEFERRED,
    first_sentence INTEGER NOT NULL CHECK(first_sentence>=0),
    end_sentence INTEGER NOT NULL CHECK(end_sentence>=first_sentence),
    metadata_json TEXT NOT NULL
);
CREATE INDEX division_kind ON divisions(kind, division_id);
CREATE INDEX division_parent ON divisions(parent_id);
CREATE TABLE division_observations (
    document_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    observation_json TEXT NOT NULL,
    PRIMARY KEY(document_id, kind)
);
CREATE INDEX division_observation_kind ON division_observations(kind, document_id);
CREATE TABLE region_spans (
    span_id INTEGER PRIMARY KEY,
    label TEXT NOT NULL,
    first_sentence INTEGER NOT NULL,
    end_sentence INTEGER NOT NULL,
    CHECK (end_sentence > first_sentence)
);

CREATE TABLE section_coverage (
    label TEXT PRIMARY KEY,
    coverage INTEGER NOT NULL CHECK (coverage > 0)
);

CREATE TABLE sentence_metadata (
    sentence_id INTEGER PRIMARY KEY,
    metadata_json TEXT NOT NULL
);

CREATE TABLE sentence_metadata_keys (
    key TEXT PRIMARY KEY
);

CREATE TRIGGER region_span_insert_coverage
AFTER INSERT ON region_spans
BEGIN
    INSERT INTO section_coverage(label, coverage)
    VALUES(NEW.label, NEW.end_sentence - NEW.first_sentence)
    ON CONFLICT(label) DO UPDATE SET coverage=coverage + excluded.coverage;
END;

CREATE INDEX region_span_label ON region_spans(label);
"#;

/// Rich, sentence-scoped context that supplements authoritative locations.
///
/// Location ownership remains with `locations.v1`; this type deliberately
/// cannot expose a source name or document ordinal.
///
/// ```compile_fail
/// use lingrep_corpus::RichSentenceMetadata;
///
/// let rich = RichSentenceMetadata {
///     structure: Default::default(),
///     region_labels: Box::new([]),
///     metadata_json: None,
///     sentence_metadata_json: None,
/// };
/// let _ = rich.source_name;
/// ```
///
/// ```compile_fail
/// use lingrep_corpus::RichSentenceMetadata;
///
/// let rich = RichSentenceMetadata {
///     structure: Default::default(),
///     region_labels: Box::new([]),
///     metadata_json: None,
///     sentence_metadata_json: None,
/// };
/// let _ = rich.document_id;
/// ```
///
/// ```compile_fail
/// use lingrep_corpus::RichSentenceMetadata;
///
/// let rich = RichSentenceMetadata {
///     structure: Default::default(),
///     region_labels: Box::new([]),
///     metadata_json: None,
///     sentence_metadata_json: None,
/// };
/// let _ = rich.document_sentence_ordinal;
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichSentenceMetadata {
    pub structure: Arc<lingrep_core::corpus::StructuralContext>,
    pub region_labels: Box<[String]>,
    /// The document metadata selected by this sentence's document identity.
    pub metadata_json: Option<String>,
    /// Sentence metadata, absent when the reader supplied none.
    pub sentence_metadata_json: Option<String>,
}

/// One contiguous authored section region active over a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveRegion {
    pub label: String,
    pub first_sentence: u64,
    pub end_sentence: u64,
}

/// One authored section region, as a half-open sentence interval.
///
/// Returned as stored. The interval is over **corpus** sentence ordinals, which
/// are artifact identity and are never renumbered, so a stored span continues
/// to name the same sentences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionSpanRow {
    pub label: String,
    pub first_sentence: u64,
    pub end_sentence: u64,
}

/// Sentence coverage for one authored section label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionCoverage {
    pub label: String,
    pub coverage: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichSentenceProvenance {
    pub metadata: RichSentenceMetadata,
    pub active_regions: Box<[ActiveRegion]>,
}

/// Exact admitted text and analyzer surface-to-word alignment for one sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSentenceAnalysis {
    pub text: String,
    pub surfaces: Box<[StoredSurfaceAlignment]>,
    pub input_unit_id: Option<u64>,
    pub input_kind: Option<TextUnitKind>,
    pub normalization: Option<NormalizationPolicy>,
    pub input_identity: Option<[u8; 32]>,
    pub source_range: Option<std::ops::Range<u32>>,
}

/// One source surface and the contiguous analyzer-word interval it covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSurfaceAlignment {
    pub source_range: std::ops::Range<u32>,
    pub word_range: std::ops::Range<u32>,
}

#[derive(Clone, Copy)]
struct ActiveSpan {
    id: u64,
    first_sentence: u64,
}

/// Commands carried by one hand-off to the recorder.
///
/// The unit that crosses the channel is the unit that gets written: two
/// commands per sentence, so this is exactly one [`PROVENANCE_BATCH_ROWS`]
/// insert's worth.
///
/// Sending one command per call was measured at 930 ns each over 1,047,766
/// calls — 0.97 s of caller time. Almost none of that is queueing; it is the
/// futex wake a bounded channel owes a receiver that keeps draining the queue
/// empty, and the only way to stop paying it that many times is to cross the
/// boundary that many times fewer.
///
/// Batching removes that 0.97 s and 0.07% of the build's instructions; it is
/// strictly less work for the same bytes.
const COMMANDS_PER_SEND: usize = PROVENANCE_BATCH_ROWS * 2;

/// Batches in flight between the builder and its recorder.
///
/// Batches have command and metadata-byte limits, so queue residency does not
/// grow with document length. Depth absorbs recorder stalls without collecting
/// an entire analysis window for every queued command.
const PROVENANCE_QUEUE_DEPTH: usize = 8;

/// One unit of recorded provenance.
enum ProvenanceCommand {
    Structure(Box<lingrep_core::corpus::AdmittedCorpusEvent>),
    Sentence(Box<SentenceRecord>),
    Analysis(Box<AnalysisRecord>),
    Finish(AnalysisIdentity),
}

/// The builder's end of a [`ProvenanceWriter`] running on its own thread.
///
/// Assembling a row is ordered with respect to the caller and needs the
/// caller's thread, because a row is read off an `AnalyzedInput` the caller
/// consumes immediately afterwards. *Recording* one — the normalization check,
/// the analysis-blob encode, the region transitions, and the batched `INSERT`
/// — is a function of the record plus an append-only position, and needs
/// nothing from the caller. Measured over a 36 MB corpus, that second half was
/// 3.74 s of a 6.8 s build loop, more than half of it either compressing a
/// 200-byte blob or waiting on SQLite.
///
/// **Ordering is inherent, not reconstructed.** One producer, one consumer,
/// one FIFO channel: commands are applied in the order they were sent, which
/// is exactly the property the single-threaded writer had for free. Region
/// spans and sentence rows keep their relative order for the same reason.
///
/// **Errors travel on the thread's return value.** A recording failure ends
/// the thread, which disconnects the channel, so the next send fails; the send
/// answers a disconnect by joining, which surfaces the error the recorder
/// actually hit.
///
/// **The connection never crosses a runtime.** The recorder thread builds its
/// own current-thread runtime and opens the database inside it, so the sqlx
/// connection is created, used and closed on one thread. That is also why
/// [`Self::create`] is not `async`: it waits for the recorder to report the
/// database open before returning.
///
/// **A dropped handle joins.** A cancelled or failed build must not leave the
/// recorder holding a file inside the staging directory the builder is about
/// to remove, so `drop` closes the channel and joins.
pub(crate) struct ProvenanceHandle {
    batches: Option<std::sync::mpsc::SyncSender<Vec<ProvenanceCommand>>>,
    /// Commands accumulated since the last hand-off. See [`COMMANDS_PER_SEND`].
    pending: Vec<ProvenanceCommand>,
    pending_bytes: usize,
    thread: Option<std::thread::JoinHandle<Result<(), CorpusError>>>,
}

impl ProvenanceHandle {
    /// Opens the component on a recorder thread and waits for it to be ready.
    ///
    /// `sentence_metadata_keys` is the reader's static declaration of
    /// sentence-grain metadata keys, recorded so a `--where` clause over one
    /// is answered at the same grain over the artifact as over the source.
    pub(crate) fn create(
        path: &Path,
        sentence_metadata_keys: Vec<String>,
    ) -> Result<Self, CorpusError> {
        let (ready, opened) = std::sync::mpsc::channel();
        let (commands, queue) = std::sync::mpsc::sync_channel(PROVENANCE_QUEUE_DEPTH);
        let owned = path.to_path_buf();
        let thread = std::thread::Builder::new()
            .name("lingrep-provenance".to_owned())
            .spawn(move || record_provenance(&owned, sentence_metadata_keys, &ready, &queue))
            .map_err(CorpusError::Io)?;
        match opened.recv() {
            Ok(Ok(())) => Ok(Self {
                batches: Some(commands),
                pending: Vec::with_capacity(COMMANDS_PER_SEND),
                pending_bytes: 0,
                thread: Some(thread),
            }),
            // The recorder failed to open the database and has already
            // returned that error; joining is what retrieves it.
            Ok(Err(error)) => {
                drop(commands);
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                drop(commands);
                Err(join_failure(thread.join()))
            }
        }
    }

    pub(crate) fn structure_event(
        &mut self,
        event: lingrep_core::corpus::AdmittedCorpusEvent,
    ) -> Result<(), CorpusError> {
        self.send(ProvenanceCommand::Structure(Box::new(event)))
    }

    /// Records one admitted sentence and the exact bytes it was admitted with.
    pub(crate) fn push_sentence_with_text(
        &mut self,
        location: &SentenceLocation,
        rich: &RichSentenceMetadata,
        text: &str,
        words: Option<&lingrep_core::token_match::BoundTokenSentence>,
    ) -> Result<(), CorpusError> {
        let mut record = SentenceRecord::new(location, rich, text);
        if let Some(words) = words {
            record.surfaces = candidate::from_bound(text, words)?;
        }
        self.send(ProvenanceCommand::Sentence(Box::new(record)))
    }

    /// Completes the row held for the sentence this analysis belongs to.
    pub(crate) fn attach(&mut self, record: AnalysisRecord) -> Result<(), CorpusError> {
        self.send(ProvenanceCommand::Analysis(Box::new(record)))
    }

    /// Binds the analysis identity, commits, and closes the component.
    pub(crate) fn finish(mut self, analysis: AnalysisIdentity) -> Result<(), CorpusError> {
        self.pending.push(ProvenanceCommand::Finish(analysis));
        self.flush()?;
        self.batches = None;
        match self.thread.take() {
            Some(thread) => match thread.join() {
                Ok(result) => result,
                other => Err(join_failure(other)),
            },
            None => Err(invalid("provenance recorder was already finished")),
        }
    }

    fn send(&mut self, command: ProvenanceCommand) -> Result<(), CorpusError> {
        let bytes = match &command {
            ProvenanceCommand::Structure(event) => match event.as_ref() {
                lingrep_core::corpus::AdmittedCorpusEvent::DivisionStart(division) => {
                    lingrep_core::corpus::divisions::validate_metadata(&division.metadata)
                        .map_err(CorpusError::Io)?
                }
                _ => 256,
            },
            ProvenanceCommand::Sentence(record) => {
                record.text.len()
                    + record
                        .region_labels
                        .iter()
                        .map(|label| label.len() + std::mem::size_of::<String>())
                        .sum::<usize>()
                    + record.division_ids.len() * std::mem::size_of::<u64>()
                    + record.surfaces.len() * std::mem::size_of::<candidate::CandidateSurface>()
                    + record.metadata_json.as_ref().map_or(0, String::len)
                    + record
                        .sentence_metadata_json
                        .as_ref()
                        .map_or(0, String::len)
            }
            ProvenanceCommand::Analysis(record) => {
                record.candidate.text.len()
                    + record.candidate.surfaces.len()
                        * std::mem::size_of::<candidate::CandidateSurface>()
            }
            _ => 256,
        };
        self.pending_bytes = self.pending_bytes.saturating_add(bytes);
        self.pending.push(command);
        if self.pending.len() < COMMANDS_PER_SEND && self.pending_bytes < 1024 * 1024 {
            return Ok(());
        }
        self.flush()
    }

    fn flush(&mut self) -> Result<(), CorpusError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let Some(batches) = self.batches.as_ref() else {
            return Err(invalid("provenance recorder is not running"));
        };
        let batch = std::mem::replace(&mut self.pending, Vec::with_capacity(COMMANDS_PER_SEND));
        self.pending_bytes = 0;
        if batches.send(batch).is_ok() {
            return Ok(());
        }
        // A disconnected channel means the recorder returned. Whatever it
        // returned is the real error; this send is only how we noticed.
        self.batches = None;
        Err(match self.thread.take() {
            Some(thread) => match thread.join() {
                Ok(Err(error)) => error,
                Ok(Ok(())) => invalid("provenance recorder stopped before the build did"),
                other => join_failure(other),
            },
            None => invalid("provenance recorder is not running"),
        })
    }
}

impl Drop for ProvenanceHandle {
    fn drop(&mut self) {
        self.batches = None;
        if let Some(thread) = self.thread.take() {
            // Nobody is left to report this to, but the join is not optional:
            // the recorder holds a file inside a staging directory the builder
            // may be about to remove.
            let _ = thread.join();
        }
    }
}

/// Applies commands to a writer until the channel closes or one fails.
fn record_provenance(
    path: &Path,
    sentence_metadata_keys: Vec<String>,
    ready: &std::sync::mpsc::Sender<Result<(), CorpusError>>,
    queue: &std::sync::mpsc::Receiver<Vec<ProvenanceCommand>>,
) -> Result<(), CorpusError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CorpusError::Io)?;
    runtime.block_on(async move {
        let mut writer = match ProvenanceWriter::create(path, &sentence_metadata_keys).await {
            Ok(writer) => {
                let _ = ready.send(Ok(()));
                writer
            }
            Err(error) => {
                let _ = ready.send(Err(error));
                // The caller reports what `ready` carried; returning it twice
                // would have `create` choose between two copies of one fact.
                return Ok(());
            }
        };
        while let Ok(batch) = queue.recv() {
            for command in batch {
                match command {
                    ProvenanceCommand::Structure(event) => writer.structure_event(*event).await?,
                    ProvenanceCommand::Sentence(record) => {
                        writer.push_sentence_record(&record).await?;
                    }
                    ProvenanceCommand::Analysis(record) => writer.attach_analyzed(*record).await?,
                    ProvenanceCommand::Finish(analysis) => return writer.finish(analysis).await,
                }
            }
        }
        // The channel closed without a `Finish`, so the build was abandoned.
        // Dropping the writer closes the connection without committing.
        Ok(())
    })
}

fn join_failure<T>(_: std::thread::Result<T>) -> CorpusError {
    invalid("provenance recorder thread panicked")
}

/// Streaming writer for the immutable rich-provenance sidecar.
pub(crate) struct ProvenanceWriter {
    connection: SqliteConnection,
    path: PathBuf,
    next_sentence: u64,
    next_span_id: u64,
    current_document: Option<crate::DocumentId>,
    document_metadata: Option<DocumentMetadataRow>,
    open_divisions: BTreeMap<lingrep_core::corpus::DivisionId, PendingDivision>,
    active_regions: BTreeMap<String, ActiveSpan>,
    /// The one sentence whose row has been assembled but not yet written.
    ///
    /// See [`PendingSentence`] for why the row waits.
    pending: Option<PendingSentence>,
    /// Finalized rows not yet sent to SQLite. See [`PROVENANCE_BATCH_ROWS`].
    batch: Vec<StoredSentence>,
}

struct PendingDivision {
    division: Arc<lingrep_core::corpus::Division>,
    first_sentence: u64,
    metadata_bytes: usize,
}

/// Rows per `sentence_provenance` insert.
///
/// One statement per sentence is a round trip to the sqlx worker for ~1.5 µs
/// of work — measured at ~14 µs each, which is 0.9 s of a `corpus build` over
/// 103,007 sentences spent crossing a thread boundary rather than writing.
///
/// The batch fits SQLite's guaranteed parameter minimum. Schema additions
/// must preserve that bound. Retained rows also obey the admission byte limits.
const PROVENANCE_BATCH_ROWS: usize = 32;

/// One admitted sentence, as the recorder needs it.
struct SentenceRecord {
    location: SentenceLocation,
    region_labels: Box<[String]>,
    metadata_json: Option<String>,
    sentence_metadata_json: Option<String>,
    division_ids: Box<[u64]>,
    text: String,
    surfaces: Vec<candidate::CandidateSurface>,
}

impl SentenceRecord {
    fn new(location: &SentenceLocation, rich: &RichSentenceMetadata, text: &str) -> Self {
        Self {
            location: location.clone(),
            region_labels: rich.region_labels.clone(),
            metadata_json: rich.metadata_json.clone(),
            sentence_metadata_json: rich.sentence_metadata_json.clone(),
            division_ids: rich
                .structure
                .divisions
                .iter()
                .map(|division| division.id.0)
                .collect(),
            text: text.to_owned(),
            surfaces: Vec::new(),
        }
    }
}

/// One analyzed sentence, as the recorder needs it.
///
/// Everything here is read off an `AnalyzedInput` that the caller consumes for
/// its tokens immediately afterwards. Extracting it on the caller's thread is
/// what lets the rest of the row — the normalization check, the analysis-blob
/// encode, and the insert — happen somewhere else.
pub(crate) struct AnalysisRecord {
    sentence_id: u64,
    normalization: NormalizationPolicy,
    candidate: candidate::CandidateSentence,
    analyzer: StoredAnalysis,
}

impl AnalysisRecord {
    /// Reads one analyzed sentence into an owned, sendable record.
    pub(crate) fn extract(sentence_id: u64, analyzed: &AnalyzedInput) -> Result<Self, CorpusError> {
        let [sentence] = analyzed.sentences() else {
            return Err(invalid("prepared analyzed input must contain one sentence"));
        };
        let candidate = candidate::from_analyzed_sentence(analyzed, sentence)?;
        let input = analyzed.input();
        let provenance = input.provenance();
        let analyzer = StoredAnalysis {
            input_unit_id: sqlite_integer(input.id().get(), "input unit ID")?,
            input_kind: match input.kind() {
                TextUnitKind::Sentence => "sentence",
                TextUnitKind::Segment => "segment",
            },
            normalization: match input.normalization() {
                NormalizationPolicy::None => "none",
                NormalizationPolicy::Nfc => "nfc",
            },
            input_identity: input.identity().as_bytes().to_vec(),
            input_source_identity: provenance.source_identity().to_vec(),
            input_document_identity: sqlite_integer(
                provenance.document_identity(),
                "input document identity",
            )?,
            input_paragraph_identity: provenance
                .paragraph_identity()
                .map(|value| sqlite_integer(value, "input paragraph identity"))
                .transpose()?,
            input_metadata_digest: provenance.metadata_digest().to_vec(),
            analyzer_sentence_ordinal: i64::from(sentence.analyzer_sentence_ordinal()),
            source_start: i64::from(sentence.source_range().start()),
            source_end: i64::from(sentence.source_range().end()),
        };
        Ok(Self {
            sentence_id,
            normalization: input.normalization(),
            candidate,
            analyzer,
        })
    }
}

/// One sentence's finalized row, awaiting the batch it will be written in.
///
/// The analyzer half is `None` for a sentence no analysis claimed, which is
/// exactly the shape the provisional insert gave such a row before rows were
/// batched.
struct StoredSentence {
    sentence_id: i64,
    source_name: Arc<str>,
    document_id: i64,
    document_ordinal: i64,
    document_sentence_ordinal: i64,
    region_labels_json: String,
    region_span_ids_json: String,
    division_ids_json: String,
    sentence_metadata_json: Option<String>,
    analysis_blob: Vec<u8>,
    analyzer: Option<StoredAnalysis>,
}

/// The columns only an analyzed sentence has.
struct StoredAnalysis {
    input_unit_id: i64,
    input_kind: &'static str,
    normalization: &'static str,
    input_identity: Vec<u8>,
    input_source_identity: Vec<u8>,
    input_document_identity: i64,
    input_paragraph_identity: Option<i64>,
    input_metadata_digest: Vec<u8>,
    analyzer_sentence_ordinal: i64,
    source_start: i64,
    source_end: i64,
}

/// One sentence's row, held until that sentence's fate is known.
///
/// [`ProvenanceWriter::push_sentence_with_text`] holds the row and
/// [`ProvenanceWriter::attach_analyzed_sentence`], called for the same sentence
/// immediately afterwards, completes it, so each sentence costs one statement.
/// Holding the row costs one sentence of lookahead, which also lets the text
/// check below read the admitted text without reading the blob back out of
/// SQLite. A held row leaves in one of two ways: the
/// analysis for that sentence arrives and completes it, or the next sentence
/// arrives — or the component is finished — and it is written as provisional.
///
/// Integers are converted at the point the caller supplies them rather than at
/// the point the row is written, so a value SQLite cannot hold is still
/// refused against the call that introduced it.
struct PendingSentence {
    ordinal: u64,
    sentence_id: i64,
    source_name: Arc<str>,
    document_id: i64,
    document_ordinal: i64,
    document_sentence_ordinal: i64,
    region_labels_json: String,
    region_span_ids_json: String,
    division_ids_json: String,
    sentence_metadata_json: Option<String>,
    /// The exact admitted bytes, which
    /// [`ProvenanceWriter::attach_analyzed_sentence`] checks the analyzed text
    /// against under the analyzer's own normalization policy.
    text: String,
    surfaces: Vec<candidate::CandidateSurface>,
}

impl PendingSentence {
    fn into_stored(
        self,
        analysis_blob: Vec<u8>,
        analyzer: Option<StoredAnalysis>,
    ) -> StoredSentence {
        StoredSentence {
            sentence_id: self.sentence_id,
            source_name: self.source_name,
            document_id: self.document_id,
            document_ordinal: self.document_ordinal,
            document_sentence_ordinal: self.document_sentence_ordinal,
            region_labels_json: self.region_labels_json,
            region_span_ids_json: self.region_span_ids_json,
            division_ids_json: self.division_ids_json,
            sentence_metadata_json: self.sentence_metadata_json,
            analysis_blob,
            analyzer,
        }
    }
}

/// Immutable read view of rich sentence metadata.
/// One document's identity, sentence span, and stored frontmatter.
///
/// The frontmatter is returned **as stored**, unparsed: the scope language's
/// typing rules belong on the caller's side of this boundary, because the
/// corpus stores every numeric-looking key as a string and a reader that typed
/// them here would have to guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentMetadataRow {
    pub document_id: DocumentId,
    pub first_sentence: u64,
    pub sentence_count: u64,
    pub metadata_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DivisionMetadataRow {
    pub division: Arc<lingrep_core::corpus::Division>,
    pub first_sentence: u64,
    pub end_sentence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParagraphRange {
    pub id: u64,
    pub sentences: std::ops::Range<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentenceReadingRanges {
    pub document_id: DocumentId,
    pub document: std::ops::Range<u64>,
    pub paragraphs: Vec<ParagraphRange>,
}

// CROSS JOIN keeps the hit's bounded membership list before primary-key lookups.
const READING_PARAGRAPHS_QUERY: &str = "SELECT d.division_id,d.document_id,d.first_sentence,d.end_sentence FROM sentence_provenance sp CROSS JOIN json_each(sp.division_ids_json) ids CROSS JOIN divisions d WHERE sp.sentence_id=? AND d.division_id=ids.value AND d.kind='paragraph' ORDER BY d.division_id LIMIT ?";

pub struct ProvenanceView {
    pool: SqlitePool,
    analysis_identity: AnalysisIdentity,
    sentence_count: u64,
    artifact_binding: Option<[u8; 32]>,
}

/// An immutable provenance view and the runtime that owns its I/O.
/// Construct and use on blocking threads outside a Tokio runtime.
pub struct BlockingProvenance {
    runtime: tokio::runtime::Runtime,
    view: ProvenanceView,
}

impl BlockingProvenance {
    pub(crate) fn new(runtime: tokio::runtime::Runtime, view: ProvenanceView) -> Self {
        Self { runtime, view }
    }

    pub fn view(&self) -> &ProvenanceView {
        &self.view
    }

    pub fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }
}

impl ProvenanceWriter {
    /// Creates a building provenance component whose identity is bound at finish.
    pub(crate) async fn create(
        path: &Path,
        sentence_metadata_keys: &[String],
    ) -> Result<Self, CorpusError> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    CorpusError::DestinationExists {
                        path: path.to_path_buf(),
                    }
                } else {
                    CorpusError::Io(error)
                }
            })?;
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false);
        let mut connection = SqliteConnection::connect_with(&options)
            .await
            .map_err(database)?;
        connection
            .execute("PRAGMA foreign_keys=ON")
            .await
            .map_err(database)?;
        connection
            .execute("PRAGMA journal_mode=WAL")
            .await
            .map_err(database)?;
        connection
            .execute("BEGIN IMMEDIATE")
            .await
            .map_err(database)?;
        sqlx::raw_sql(SCHEMA)
            .execute(&mut connection)
            .await
            .map_err(database)?;
        sqlx::query(
            "INSERT INTO provenance_identity(singleton, schema_version, analysis_identity, state, sentence_count) VALUES(1, ?, ?, 'building', 0)",
        )
        .bind(i64::from(SCHEMA_VERSION))
        .bind([0_u8; 32].as_slice())
        .execute(&mut connection)
        .await
        .map_err(database)?;
        for key in sentence_metadata_keys {
            sqlx::query("INSERT INTO sentence_metadata_keys(key) VALUES(?)")
                .bind(key)
                .execute(&mut connection)
                .await
                .map_err(database)?;
        }
        Ok(Self {
            connection,
            path: path.to_path_buf(),
            next_sentence: 0,
            next_span_id: 1,
            current_document: None,
            document_metadata: None,
            open_divisions: BTreeMap::new(),
            active_regions: BTreeMap::new(),
            pending: None,
            batch: Vec::with_capacity(PROVENANCE_BATCH_ROWS),
        })
    }

    /// Stores rich metadata for exactly the supplied authoritative location.
    #[cfg(test)]
    pub(crate) async fn push_sentence(
        &mut self,
        location: &SentenceLocation,
        rich: &RichSentenceMetadata,
    ) -> Result<(), CorpusError> {
        self.push_sentence_with_text(location, rich, "").await
    }

    #[cfg(test)]
    async fn push_sentence_with_text(
        &mut self,
        location: &SentenceLocation,
        rich: &RichSentenceMetadata,
        text: &str,
    ) -> Result<(), CorpusError> {
        self.push_sentence_record(&SentenceRecord::new(location, rich, text))
            .await
    }

    /// Stores structural provenance plus the exact admitted sentence bytes.
    async fn push_sentence_record(&mut self, record: &SentenceRecord) -> Result<(), CorpusError> {
        let location = &record.location;
        if location.corpus_sentence_ordinal != self.next_sentence {
            return Err(CorpusError::InvalidFormat {
                component: COMPONENT,
                message: "sentence ordinals must be contiguous and zero-based".into(),
            });
        }
        self.flush_pending_provisional().await?;
        self.record_document(record).await?;
        self.transition_regions(location, &record.region_labels)
            .await?;
        let region_labels_json = serde_json::to_string(&record.region_labels)
            .map_err(|error| invalid(error.to_string()))?;
        let region_span_ids_json = serde_json::to_string(
            &self
                .active_regions
                .values()
                .map(|span| span.id)
                .collect::<Vec<_>>(),
        )
        .map_err(|error| invalid(error.to_string()))?;
        self.pending = Some(PendingSentence {
            ordinal: location.corpus_sentence_ordinal,
            sentence_id: sqlite_integer(location.corpus_sentence_ordinal, "sentence ordinal")?,
            source_name: Arc::clone(&location.source_name),
            document_id: sqlite_integer(location.document_id.get(), "document ID")?,
            document_ordinal: sqlite_integer(location.document_ordinal, "document ordinal")?,
            document_sentence_ordinal: sqlite_integer(
                location.document_sentence_ordinal,
                "document sentence ordinal",
            )?,
            region_labels_json,
            region_span_ids_json,
            division_ids_json: serde_json::to_string(&record.division_ids)
                .map_err(|error| invalid(error.to_string()))?,
            sentence_metadata_json: record.sentence_metadata_json.clone(),
            text: record.text.clone(),
            surfaces: record.surfaces.clone(),
        });
        self.next_sentence =
            self.next_sentence
                .checked_add(1)
                .ok_or_else(|| CorpusError::InvalidFormat {
                    component: COMPONENT,
                    message: "sentence count overflow".into(),
                })?;
        Ok(())
    }

    /// Writes the held row complete: structural provenance, validated analyzer
    /// alignment, and immutable input-unit identity, in one statement.
    ///
    /// Takes an [`AnalysisRecord`] rather than the `AnalyzedInput` it came
    /// from, because the caller consumes that input for its tokens the moment
    /// this returns and so cannot lend it across a thread boundary. The split
    /// is where the borrow ends, not where the work divides: everything here
    /// is a function of the record and the row already held.
    async fn attach_analyzed(&mut self, record: AnalysisRecord) -> Result<(), CorpusError> {
        // The analysis attaches to the provisional row this writer wrote
        // immediately before it — `CorpusBuilder` interleaves them one for one
        // and refuses anything else — so the admitted bytes are still in hand.
        let Some(pending) = self.pending.take() else {
            return Err(invalid("analyzed sentence has no provisional record"));
        };
        if pending.ordinal != record.sentence_id {
            return Err(invalid(
                "analyzed sentence does not attach to the provisional record before it",
            ));
        }
        // Under the analyzer's *own* recorded policy, not by raw equality. The
        // provisional row keeps the admitted bytes exactly as the corpus wrote
        // them, while the analyzer records what it actually analyzed — the
        // text after normalization. Comparing those two directly refused every
        // corpus that was not already NFC, which is most Japanese text, under
        // the default `--normalization nfc`. Reading the policy off the
        // analyzed input is what keeps this check unable to disagree with the
        // analyzer about which derivation it is asking about.
        if !record
            .normalization
            .normalizes(&pending.text, &record.candidate.text)
        {
            return Err(invalid(
                "analyzed text differs from admitted sentence bytes",
            ));
        }
        let analysis_blob = candidate::encode(&[record.candidate])?;
        self.enqueue(pending.into_stored(analysis_blob, Some(record.analyzer)))
            .await
    }

    /// Extracts and attaches in one call, for tests that hold the input.
    #[cfg(test)]
    pub(crate) async fn attach_analyzed_sentence(
        &mut self,
        sentence_id: u64,
        analyzed: &AnalyzedInput,
    ) -> Result<(), CorpusError> {
        self.attach_analyzed(AnalysisRecord::extract(sentence_id, analyzed)?)
            .await
    }

    /// Queues a held row that no analysis claimed.
    ///
    /// A sentence can be admitted and never analyzed — `push_tokenized_sentence`
    /// stores tokens without an `AnalyzedInput`, and a run may end between the
    /// two calls. Such a row is written as provisional: the structural
    /// columns, a surface-less analysis blob carrying the admitted text, and
    /// NULL in every analyzer column.
    async fn flush_pending_provisional(&mut self) -> Result<(), CorpusError> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        let analysis_blob = candidate::encode(&[candidate::CandidateSentence {
            text: pending.text.clone(),
            surfaces: pending.surfaces.clone(),
        }])?;
        self.enqueue(pending.into_stored(analysis_blob, None)).await
    }

    /// Adds one finalized row to the batch, writing the batch when it fills.
    async fn enqueue(&mut self, row: StoredSentence) -> Result<(), CorpusError> {
        self.batch.push(row);
        let bytes = self
            .batch
            .iter()
            .map(|row| {
                row.analysis_blob.len()
                    + row.region_labels_json.len()
                    + row.region_span_ids_json.len()
                    + row.division_ids_json.len()
                    + row.sentence_metadata_json.as_ref().map_or(0, String::len)
            })
            .sum::<usize>();
        if self.batch.len() >= PROVENANCE_BATCH_ROWS || bytes >= 1024 * 1024 {
            self.write_batch().await?;
        }
        Ok(())
    }

    /// Writes every queued row in one statement, plus one statement for the
    /// batch's sentence-metadata rows when any sentence carries one.
    async fn write_batch(&mut self) -> Result<(), CorpusError> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let sentence_metadata = self
            .batch
            .iter()
            .filter_map(|stored| {
                stored
                    .sentence_metadata_json
                    .clone()
                    .map(|json| (stored.sentence_id, json))
            })
            .collect::<Vec<_>>();
        let mut builder = sqlx::QueryBuilder::new(
            "INSERT INTO sentence_provenance(
                sentence_id, source_name, document_id, document_ordinal,
                document_sentence_ordinal, region_labels_json,
                region_span_ids_json, division_ids_json, analysis_blob,
                input_unit_id, input_kind, normalization, input_identity,
                input_source_identity, input_document_identity,
                input_paragraph_identity, input_metadata_digest,
                analyzer_sentence_ordinal, source_start, source_end)",
        );
        builder.push_values(self.batch.drain(..), |mut row, stored| {
            row.push_bind(stored.sentence_id)
                .push_bind(stored.source_name.as_ref().to_owned())
                .push_bind(stored.document_id)
                .push_bind(stored.document_ordinal)
                .push_bind(stored.document_sentence_ordinal)
                .push_bind(stored.region_labels_json)
                .push_bind(stored.region_span_ids_json)
                .push_bind(stored.division_ids_json)
                .push_bind(stored.analysis_blob);
            match stored.analyzer {
                Some(analyzer) => {
                    row.push_bind(Some(analyzer.input_unit_id))
                        .push_bind(Some(analyzer.input_kind))
                        .push_bind(Some(analyzer.normalization))
                        .push_bind(Some(analyzer.input_identity))
                        .push_bind(Some(analyzer.input_source_identity))
                        .push_bind(Some(analyzer.input_document_identity))
                        .push_bind(analyzer.input_paragraph_identity)
                        .push_bind(Some(analyzer.input_metadata_digest))
                        .push_bind(Some(analyzer.analyzer_sentence_ordinal))
                        .push_bind(Some(analyzer.source_start))
                        .push_bind(Some(analyzer.source_end));
                }
                None => {
                    row.push_bind(None::<i64>)
                        .push_bind(None::<&str>)
                        .push_bind(None::<&str>)
                        .push_bind(None::<Vec<u8>>)
                        .push_bind(None::<Vec<u8>>)
                        .push_bind(None::<i64>)
                        .push_bind(None::<i64>)
                        .push_bind(None::<Vec<u8>>)
                        .push_bind(None::<i64>)
                        .push_bind(None::<i64>)
                        .push_bind(None::<i64>);
                }
            }
        });
        builder
            .build()
            .execute(&mut self.connection)
            .await
            .map_err(database)?;
        if !sentence_metadata.is_empty() {
            let mut builder = sqlx::QueryBuilder::new(
                "INSERT INTO sentence_metadata(sentence_id, metadata_json)",
            );
            builder.push_values(sentence_metadata, |mut row, (sentence_id, json)| {
                row.push_bind(sentence_id).push_bind(json);
            });
            builder
                .build()
                .execute(&mut self.connection)
                .await
                .map_err(database)?;
        }
        Ok(())
    }

    /// Holds bounded open divisions and inserts each completed row at close.
    async fn structure_event(
        &mut self,
        event: lingrep_core::corpus::AdmittedCorpusEvent,
    ) -> Result<(), CorpusError> {
        use lingrep_core::corpus::{
            AdmittedCorpusEvent,
            divisions::{MAX_ACTIVE_DIVISIONS, MAX_STRUCTURAL_CONTEXT_BYTES},
        };
        match event {
            AdmittedCorpusEvent::DivisionStart(division) => {
                let metadata_bytes =
                    lingrep_core::corpus::divisions::validate_metadata(&division.metadata)
                        .map_err(CorpusError::Io)?;
                if self.open_divisions.len() >= MAX_ACTIVE_DIVISIONS
                    || self
                        .open_divisions
                        .values()
                        .map(|row| row.metadata_bytes)
                        .sum::<usize>()
                        + metadata_bytes
                        > MAX_STRUCTURAL_CONTEXT_BYTES
                {
                    return Err(invalid("open division recorder exceeds its context budget"));
                }
                if self.open_divisions.contains_key(&division.id)
                    || division.parent_id.is_some_and(|parent| {
                        parent.0 >= division.id.0 || !self.open_divisions.contains_key(&parent)
                    })
                {
                    return Err(invalid("invalid open division identity or parent"));
                }
                self.open_divisions.insert(
                    division.id,
                    PendingDivision {
                        division,
                        first_sentence: self.next_sentence,
                        metadata_bytes,
                    },
                );
            }
            AdmittedCorpusEvent::DivisionEnd { id } => {
                if self
                    .open_divisions
                    .values()
                    .any(|row| row.division.parent_id == Some(id))
                {
                    return Err(invalid("division closed before its child"));
                }
                let row = self
                    .open_divisions
                    .remove(&id)
                    .ok_or_else(|| invalid("division close has no open identity"))?;
                let division = row.division;
                let json =
                    lingrep_core::corpus::divisions::metadata_json(&division.metadata).to_string();
                sqlx::query("INSERT INTO divisions(division_id,document_id,kind,source_id,parent_id,first_sentence,end_sentence,metadata_json) VALUES(?,?,?,?,?,?,?,?)")
                    .bind(sqlite_integer(division.id.0,"division ID")?).bind(sqlite_integer(division.document_id.get(),"document ID")?)
                    .bind(&division.kind).bind(&division.source_id).bind(division.parent_id.map(|id|sqlite_integer(id.0,"parent ID")).transpose()?)
                    .bind(sqlite_integer(row.first_sentence,"division start")?).bind(sqlite_integer(self.next_sentence,"division end")?).bind(json)
                    .execute(&mut self.connection).await.map_err(database)?;
            }
            AdmittedCorpusEvent::DivisionObservation {
                document_id,
                kind,
                observation,
            } => {
                let json = serde_json::to_string(&observation)
                    .map_err(|error| invalid(error.to_string()))?;
                sqlx::query("INSERT INTO division_observations(document_id,kind,observation_json) VALUES(?,?,?) ON CONFLICT(document_id,kind) DO UPDATE SET observation_json=excluded.observation_json")
                    .bind(sqlite_integer(document_id.get(),"document ID")?).bind(kind).bind(json)
                    .execute(&mut self.connection).await.map_err(database)?;
            }
            _ => return Err(invalid("nonstructural event sent to division recorder")),
        }
        Ok(())
    }
    pub(crate) async fn finish(mut self, analysis: AnalysisIdentity) -> Result<(), CorpusError> {
        if !self.open_divisions.is_empty() {
            return Err(invalid("provenance finished with open divisions"));
        }
        self.flush_pending_provisional().await?;
        self.write_batch().await?;
        self.close_regions().await?;
        self.flush_document().await?;
        sqlx::query(
            "UPDATE provenance_identity
             SET analysis_identity=?, state='ready', sentence_count=?
             WHERE singleton=1",
        )
        .bind(analysis.as_bytes().as_slice())
        .bind(sqlite_integer(self.next_sentence, "sentence count")?)
        .execute(&mut self.connection)
        .await
        .map_err(database)?;
        self.connection.execute("COMMIT").await.map_err(database)?;
        self.connection
            .execute("PRAGMA journal_mode=DELETE")
            .await
            .map_err(database)?;
        self.connection.close().await.map_err(database)?;
        reject_sidecars(&self.path)
    }
}

impl ProvenanceView {
    /// Opens a ready sidecar in SQLite immutable/read-only mode.
    pub async fn open(path: &Path, expected: &AnalysisIdentity) -> Result<Self, CorpusError> {
        reject_sidecars(path)?;
        let options = SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .immutable(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .map_err(database)?;
        // The identity table's shape is identical across every readable
        // version, so it is validated before the version is known and the
        // version then selects which shape the rest must have.
        validate_identity_table(&pool).await?;
        let rows = sqlx::query(
            "SELECT schema_version, analysis_identity, state, sentence_count FROM provenance_identity",
        )
        .fetch_all(&pool)
        .await
        .map_err(database)?;
        let [row] = rows.as_slice() else {
            return Err(invalid("provenance identity must contain exactly one row"));
        };
        let schema_version: i64 = row.try_get("schema_version").map_err(database)?;
        let schema_version = u32::try_from(schema_version)
            .map_err(|_| invalid("provenance schema version is out of range"))?;
        if schema_version != SCHEMA_VERSION {
            return Err(CorpusError::UnsupportedSchema {
                component: COMPONENT,
                version: schema_version,
            });
        }
        validate_schema(&pool).await?;
        let identity: Vec<u8> = row.try_get("analysis_identity").map_err(database)?;
        if identity.len() != expected.as_bytes().len() {
            return Err(invalid("analysis identity has the wrong length"));
        }
        let state: String = row.try_get("state").map_err(database)?;
        if state != "building" && state != "ready" {
            return Err(invalid("provenance state is invalid"));
        }
        if state != "ready" {
            return Err(verification("provenance artifact is not ready"));
        }
        if identity.as_slice() != expected.as_bytes() {
            return Err(verification(
                "analysis identity differs from the expected artifact identity",
            ));
        }
        let sentence_count: i64 = row.try_get("sentence_count").map_err(database)?;
        let sentence_count =
            u64::try_from(sentence_count).map_err(|_| invalid("sentence count is negative"))?;
        let count = sqlx::query(
            "SELECT COUNT(*) AS row_count, MIN(sentence_id) AS first_id, MAX(sentence_id) AS last_id FROM sentence_provenance",
        )
        .fetch_one(&pool)
        .await
        .map_err(database)?;
        let row_count: i64 = count.try_get("row_count").map_err(database)?;
        let row_count = u64::try_from(row_count).map_err(|_| invalid("row count is negative"))?;
        if row_count != sentence_count {
            return Err(verification("stored sentence count differs from row count"));
        }
        let first_id: Option<i64> = count.try_get("first_id").map_err(database)?;
        let last_id: Option<i64> = count.try_get("last_id").map_err(database)?;
        match (sentence_count, first_id, last_id) {
            (0, None, None) => {}
            (0, _, _) => return Err(invalid("empty sentence relation has bounds")),
            (count, Some(0), Some(last)) if u64::try_from(last).ok() == count.checked_sub(1) => {}
            (_, Some(_), Some(_)) => {
                return Err(verification(
                    "sentence IDs are not contiguous and zero-based",
                ));
            }
            _ => return Err(invalid("sentence relation has malformed bounds")),
        }
        let mut first = 0_u64;
        let mut document_end = 0_u64;
        let mut document_id = None;
        let mut document_count = 0_i64;
        while first < sentence_count {
            let rows = sqlx::query(
                "SELECT sp.sentence_id, sp.document_id, sp.analysis_blob, sp.input_unit_id, sp.input_kind,
                        normalization, input_identity, input_source_identity,
                        input_document_identity, input_metadata_digest,
                        analyzer_sentence_ordinal, source_start, source_end,
                        dm.first_sentence, dm.sentence_count AS document_sentence_count
                 FROM sentence_provenance sp
                 LEFT JOIN document_metadata dm ON dm.document_id=sp.document_id
                 WHERE sp.sentence_id>=?
                 ORDER BY sp.sentence_id LIMIT 1024",
            )
            .bind(sqlite_integer(first, "sentence ID")?)
            .fetch_all(&pool)
            .await
            .map_err(database)?;
            if rows.is_empty() {
                return Err(verification("sentence analysis rows are incomplete"));
            }
            for row in rows {
                let sentence_id =
                    sqlite_u64(row.try_get("sentence_id").map_err(database)?, "sentence ID")?;
                if sentence_id != first {
                    return Err(verification("sentence analysis rows are not contiguous"));
                }
                let stored_document: i64 = row.try_get("document_id").map_err(database)?;
                let document_first: Option<i64> =
                    row.try_get("first_sentence").map_err(database)?;
                let stored_count: Option<i64> =
                    row.try_get("document_sentence_count").map_err(database)?;
                if sentence_id == document_end {
                    let start = document_first
                        .ok_or_else(|| verification("sentence has no document metadata"))?;
                    let count = stored_count
                        .ok_or_else(|| verification("sentence has no document range"))?;
                    if u64::try_from(start).ok() != Some(sentence_id) || count <= 0 {
                        return Err(verification("document metadata ranges are not contiguous"));
                    }
                    document_end = sentence_id
                        .checked_add(sqlite_u64(count, "document sentence count")?)
                        .ok_or_else(|| invalid("document range overflow"))?;
                    document_id = Some(stored_document);
                    document_count += 1;
                }
                if document_id != Some(stored_document) || sentence_id >= document_end {
                    return Err(verification(
                        "sentence is outside its document metadata range",
                    ));
                }
                let blob: Vec<u8> = row.try_get("analysis_blob").map_err(database)?;
                let decoded = candidate::decode(&blob)?;
                if decoded.len() != 1 {
                    return Err(invalid("sentence analysis has wrong cardinality"));
                }
                let input_unit_id: Option<i64> = row.try_get("input_unit_id").map_err(database)?;
                let required = [
                    row.try_get::<Option<String>, _>("input_kind")
                        .map_err(database)?
                        .is_some(),
                    row.try_get::<Option<String>, _>("normalization")
                        .map_err(database)?
                        .is_some(),
                    optional_digest(
                        row.try_get("input_identity").map_err(database)?,
                        "input identity",
                    )?
                    .is_some(),
                    optional_digest(
                        row.try_get("input_source_identity").map_err(database)?,
                        "input source identity",
                    )?
                    .is_some(),
                    row.try_get::<Option<i64>, _>("input_document_identity")
                        .map_err(database)?
                        .is_some(),
                    optional_digest(
                        row.try_get("input_metadata_digest").map_err(database)?,
                        "input metadata digest",
                    )?
                    .is_some(),
                    row.try_get::<Option<i64>, _>("analyzer_sentence_ordinal")
                        .map_err(database)?
                        .is_some(),
                    row.try_get::<Option<i64>, _>("source_start")
                        .map_err(database)?
                        .is_some(),
                    row.try_get::<Option<i64>, _>("source_end")
                        .map_err(database)?
                        .is_some(),
                ];
                if required
                    .iter()
                    .any(|present| *present != input_unit_id.is_some())
                {
                    return Err(invalid("sentence input identity is only partially stored"));
                }
                first = first
                    .checked_add(1)
                    .ok_or_else(|| invalid("sentence count overflow"))?;
            }
        }
        let stored_documents: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM document_metadata")
            .fetch_one(&pool)
            .await
            .map_err(database)?;
        if document_end != sentence_count || document_count != stored_documents {
            return Err(verification(
                "document metadata ranges do not cover exactly the stored sentences",
            ));
        }
        let view = Self {
            pool,
            analysis_identity: *expected,
            sentence_count,
            artifact_binding: None,
        };
        view.validate_division_membership().await?;
        Ok(view)
    }

    pub(crate) async fn validate_document_count(&self, count: u64) -> Result<(), CorpusError> {
        let bad:i64=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM divisions WHERE document_id<0 OR document_id>=? UNION ALL SELECT 1 FROM division_observations WHERE document_id<0 OR document_id>=?)")
            .bind(sqlite_integer(count,"document count")?).bind(sqlite_integer(count,"document count")?).fetch_one(&self.pool).await.map_err(database)?;
        if bad != 0 {
            return Err(invalid("structural metadata names an unknown document"));
        }
        Ok(())
    }

    async fn validate_division_membership(&self) -> Result<(), CorpusError> {
        use futures::TryStreamExt;
        use lingrep_core::corpus::divisions::{MAX_ACTIVE_DIVISIONS, MAX_STRUCTURAL_CONTEXT_BYTES};
        let mut observations=sqlx::query("SELECT document_id,CASE WHEN length(CAST(kind AS BLOB))<=256 THEN kind END AS kind,CASE WHEN length(CAST(observation_json AS BLOB))<=4096 THEN observation_json END AS observation_json FROM division_observations ORDER BY document_id,kind").fetch(&self.pool);
        let mut previous_document = None;
        let mut kind_count = 0usize;
        while let Some(row) = observations.try_next().await.map_err(database)? {
            let document =
                sqlite_u64(row.try_get("document_id").map_err(database)?, "document ID")?;
            if previous_document != Some(document) {
                kind_count = 0;
                previous_document = Some(document);
            }
            kind_count += 1;
            if kind_count > lingrep_core::corpus::divisions::MAX_DIVISION_KINDS {
                return Err(invalid("too many structural kinds for a document"));
            }
            let kind: String = row.try_get("kind").map_err(database)?;
            lingrep_core::corpus::divisions::validate_kind(&kind).map_err(CorpusError::Io)?;
            let json: String = row.try_get("observation_json").map_err(database)?;
            let observation: lingrep_core::corpus::DivisionObservation =
                serde_json::from_str(&json).map_err(|error| invalid(error.to_string()))?;
            observation.validate().map_err(CorpusError::Io)?;
        }
        drop(observations);
        let mut queued = std::collections::VecDeque::new();
        let mut next_division = 0u64;
        let mut divisions_finished = false;
        let mut previous_start = 0;
        let mut active: BTreeMap<u64, (DivisionMetadataRow, usize, usize)> = BTreeMap::new();
        let mut sentence = 0u64;
        loop {
            let rows=sqlx::query("SELECT sentence_id,document_id,CASE WHEN length(division_ids_json)<=6146 THEN division_ids_json END AS division_ids_json FROM sentence_provenance WHERE sentence_id>=? ORDER BY sentence_id LIMIT 1024")
                .bind(sqlite_integer(sentence,"sentence ID")?).fetch_all(&self.pool).await.map_err(database)?;
            let at_end = rows.is_empty();
            let positions = if at_end {
                vec![(self.sentence_count, None, None)]
            } else {
                rows.into_iter()
                    .map(|row| {
                        Ok((
                            sqlite_u64(
                                row.try_get("sentence_id").map_err(database)?,
                                "sentence ID",
                            )?,
                            Some(sqlite_u64(
                                row.try_get("document_id").map_err(database)?,
                                "document ID",
                            )?),
                            Some(
                                row.try_get::<Option<String>, _>("division_ids_json")
                                    .map_err(database)?
                                    .ok_or_else(|| {
                                        invalid("division reference list exceeds its byte budget")
                                    })?,
                            ),
                        ))
                    })
                    .collect::<Result<Vec<_>, CorpusError>>()?
            };
            for (ordinal, document, references) in positions {
                active.retain(|_, (row, _, _)| row.end_sentence > ordinal);
                loop {
                    if queued.is_empty() && !divisions_finished {
                        let page = self.division_metadata_page(next_division).await?;
                        divisions_finished = page.is_empty();
                        queued.extend(page);
                    }
                    let Some(next) = queued.front() else {
                        break;
                    };
                    if next.first_sentence < previous_start {
                        return Err(invalid(
                            "division starts do not follow source identity order",
                        ));
                    }
                    if next.first_sentence > ordinal {
                        break;
                    }
                    let row = queued
                        .pop_front()
                        .ok_or_else(|| invalid("division page disappeared"))?;
                    previous_start = row.first_sentence;
                    next_division = row
                        .division
                        .id
                        .0
                        .checked_add(1)
                        .ok_or_else(|| invalid("division ID overflow"))?;
                    let depth = if let Some(parent) = row.division.parent_id {
                        if let Some((ancestor, _, depth)) = active.get(&parent.0) {
                            if ancestor.division.document_id != row.division.document_id
                                || ancestor.first_sentence > row.first_sentence
                                || ancestor.end_sentence < row.end_sentence
                            {
                                return Err(invalid(
                                    "division parent ownership or containment is invalid",
                                ));
                            }
                            depth + 1
                        } else {
                            self.validate_closed_division_parent(&row).await?
                        }
                    } else {
                        1
                    };
                    if depth > lingrep_core::corpus::divisions::MAX_DIVISION_DEPTH {
                        return Err(invalid("division nesting exceeds 64 levels"));
                    }
                    if row.end_sentence > ordinal {
                        let bytes = lingrep_core::corpus::divisions::validate_metadata(
                            &row.division.metadata,
                        )
                        .map_err(CorpusError::Io)?;
                        active.insert(row.division.id.0, (row, bytes, depth));
                        if active.len() > MAX_ACTIVE_DIVISIONS
                            || active.values().map(|(_, bytes, _)| bytes).sum::<usize>()
                                > MAX_STRUCTURAL_CONTEXT_BYTES
                        {
                            return Err(invalid(
                                "stored structural context exceeds its resource budget",
                            ));
                        }
                    }
                }
                if let (Some(document), Some(references)) = (document, references) {
                    if active
                        .values()
                        .any(|(row, _, _)| row.division.document_id.get() != document)
                    {
                        return Err(invalid("division extent crosses its document"));
                    }
                    let references: Vec<u64> = serde_json::from_str(&references)
                        .map_err(|error| invalid(error.to_string()))?;
                    if references != active.keys().copied().collect::<Vec<_>>() {
                        return Err(invalid(
                            "sentence division references disagree with authoritative extents",
                        ));
                    }
                    sentence = ordinal
                        .checked_add(1)
                        .ok_or_else(|| invalid("sentence ID overflow"))?;
                }
            }
            if at_end {
                if !queued.is_empty() || !divisions_finished || !active.is_empty() {
                    return Err(invalid(
                        "division extent remains beyond the corpus boundary",
                    ));
                }
                break;
            }
        }
        Ok(())
    }

    async fn validate_closed_division_parent(
        &self,
        row: &DivisionMetadataRow,
    ) -> Result<usize, CorpusError> {
        let parent = row
            .division
            .parent_id
            .ok_or_else(|| invalid("missing parent identity"))?;
        let ancestors=sqlx::query("WITH RECURSIVE ancestors(division_id,parent_id,document_id,first_sentence,end_sentence,depth) AS (SELECT division_id,parent_id,document_id,first_sentence,end_sentence,1 FROM divisions WHERE division_id=? UNION ALL SELECT d.division_id,d.parent_id,d.document_id,d.first_sentence,d.end_sentence,a.depth+1 FROM divisions d JOIN ancestors a ON d.division_id=a.parent_id WHERE a.depth<=64) SELECT * FROM ancestors ORDER BY depth")
            .bind(sqlite_integer(parent.0,"parent ID")?).fetch_all(&self.pool).await.map_err(database)?;
        if ancestors.is_empty() {
            return Err(invalid("dangling authored division parent"));
        }
        let mut previous = row.division.id.0;
        for ancestor in &ancestors {
            let id = sqlite_u64(
                ancestor.try_get("division_id").map_err(database)?,
                "ancestor ID",
            )?;
            let document = sqlite_u64(
                ancestor.try_get("document_id").map_err(database)?,
                "ancestor document",
            )?;
            let first = sqlite_u64(
                ancestor.try_get("first_sentence").map_err(database)?,
                "ancestor start",
            )?;
            let end = sqlite_u64(
                ancestor.try_get("end_sentence").map_err(database)?,
                "ancestor end",
            )?;
            if id >= previous
                || document != row.division.document_id.get()
                || first > row.first_sentence
                || end < row.end_sentence
            {
                return Err(invalid(
                    "division ancestor ownership, order or containment is invalid",
                ));
            }
            previous = id;
        }
        let last = ancestors
            .last()
            .ok_or_else(|| invalid("missing ancestor"))?;
        if last
            .try_get::<Option<i64>, _>("parent_id")
            .map_err(database)?
            .is_some()
        {
            return Err(invalid(
                "division ancestor chain is dangling or exceeds its depth budget",
            ));
        }
        Ok(ancestors.len() + 1)
    }

    /// The sentence-grain metadata keys the artifact's reader declared.
    ///
    /// Empty when the reader declares no sentence metadata keys.
    pub async fn sentence_metadata_keys(&self) -> Result<BTreeSet<String>, CorpusError> {
        let rows = sqlx::query("SELECT key FROM sentence_metadata_keys ORDER BY key")
            .fetch_all(&self.pool)
            .await
            .map_err(database)?;
        rows.into_iter()
            .map(|row| row.try_get::<String, _>("key").map_err(database))
            .collect()
    }

    pub async fn scope_metadata_page(
        &self,
        first_sentence: u64,
        include_structure: bool,
    ) -> Result<
        Vec<(
            u64,
            lingrep_core::corpus::Metadata,
            Arc<lingrep_core::corpus::StructuralContext>,
        )>,
        CorpusError,
    > {
        use futures::TryStreamExt;
        let mut stream=sqlx::query("SELECT sp.sentence_id,CASE WHEN length(CAST(sm.metadata_json AS BLOB))<=1048576 THEN sm.metadata_json END AS metadata_json,sm.metadata_json IS NOT NULL AS has_metadata FROM sentence_provenance sp LEFT JOIN sentence_metadata sm ON sm.sentence_id=sp.sentence_id WHERE sp.sentence_id>=? ORDER BY sp.sentence_id LIMIT 256")
            .bind(sqlite_integer(first_sentence,"sentence ID")?).fetch(&self.pool);
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = stream.try_next().await.map_err(database)? {
            let json: Option<String> = row.try_get("metadata_json").map_err(database)?;
            if json.is_none() && row.try_get::<bool, _>("has_metadata").map_err(database)? {
                return Err(invalid("sentence metadata exceeds 1 MiB"));
            }
            let size = json.as_ref().map_or(0, String::len);
            if !rows.is_empty() && bytes + size > 4 * 1024 * 1024 {
                break;
            }
            bytes += size;
            rows.push(row);
        }
        drop(stream);
        let ordinals = rows
            .iter()
            .map(|row| {
                sqlite_u64(row.try_get("sentence_id").map_err(database)?, "sentence ID")
                    .map(SentenceId::new)
            })
            .collect::<Result<Vec<_>, CorpusError>>()?;
        let mut contexts = if include_structure {
            self.structural_contexts(&ordinals).await?
        } else {
            BTreeMap::new()
        };
        rows.into_iter()
            .zip(ordinals)
            .map(|(row, ordinal)| {
                let json: Option<String> = row.try_get("metadata_json").map_err(database)?;
                let metadata = match json {
                    Some(json) => lingrep_core::corpus::metadata_from_json(
                        serde_json::from_str(&json).map_err(|error| invalid(error.to_string()))?,
                        "prepared sentence metadata",
                    )
                    .map_err(CorpusError::Io)?,
                    None => Default::default(),
                };
                Ok((
                    ordinal.get(),
                    metadata,
                    contexts.remove(&ordinal.get()).unwrap_or_default(),
                ))
            })
            .collect()
    }
    /// Returns up to 256 sentence-metadata rows at or after `first_sentence`.
    pub async fn sentence_metadata_page(
        &self,
        first_sentence: u64,
    ) -> Result<Vec<(u64, String)>, CorpusError> {
        use futures::TryStreamExt;
        let mut stream = sqlx::query(
            "SELECT sentence_id,CASE WHEN length(CAST(metadata_json AS BLOB))<=1048576 THEN metadata_json END AS metadata_json,length(CAST(metadata_json AS BLOB)) AS metadata_bytes FROM sentence_metadata WHERE sentence_id>=? ORDER BY sentence_id LIMIT 256",
        )
        .bind(sqlite_integer(first_sentence, "sentence ID")?)
        .fetch(&self.pool);
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = stream.try_next().await.map_err(database)? {
            let size = sqlite_usize(
                row.try_get("metadata_bytes").map_err(database)?,
                "metadata bytes",
            )?;
            if size > 1024 * 1024 {
                return Err(invalid("metadata record exceeds 1 MiB"));
            }
            if !rows.is_empty() && bytes + size > 4 * 1024 * 1024 {
                break;
            }
            bytes += size;
            rows.push(row);
        }
        rows.into_iter()
            .map(|row| {
                Ok((
                    sqlite_u64(row.try_get("sentence_id").map_err(database)?, "sentence ID")?,
                    row.try_get("metadata_json").map_err(database)?,
                ))
            })
            .collect()
    }

    /// Reads at most 256 document rows at or after `first_sentence`.
    pub async fn document_metadata_page(
        &self,
        first_sentence: u64,
    ) -> Result<Vec<DocumentMetadataRow>, CorpusError> {
        use futures::TryStreamExt;
        let mut stream = sqlx::query(
            "SELECT document_id, first_sentence, sentence_count,CASE WHEN length(CAST(metadata_json AS BLOB))<=1048576 THEN metadata_json END AS metadata_json,COALESCE(length(CAST(metadata_json AS BLOB)),0) AS metadata_bytes
             FROM document_metadata WHERE first_sentence>=? ORDER BY first_sentence LIMIT 256",
        )
        .bind(sqlite_integer(first_sentence, "first sentence")?)
        .fetch(&self.pool)
        ;
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = stream.try_next().await.map_err(database)? {
            let size = sqlite_usize(
                row.try_get("metadata_bytes").map_err(database)?,
                "metadata bytes",
            )?;
            if size > 1024 * 1024 {
                return Err(invalid("metadata record exceeds 1 MiB"));
            }
            if !rows.is_empty() && bytes + size > 4 * 1024 * 1024 {
                break;
            }
            bytes += size;
            rows.push(row);
        }
        rows.into_iter()
            .map(|row| {
                Ok(DocumentMetadataRow {
                    document_id: DocumentId::new(sqlite_u64(
                        row.try_get("document_id").map_err(database)?,
                        "document ID",
                    )?),
                    first_sentence: sqlite_u64(
                        row.try_get("first_sentence").map_err(database)?,
                        "first sentence",
                    )?,
                    sentence_count: sqlite_u64(
                        row.try_get("sentence_count").map_err(database)?,
                        "sentence count",
                    )?,
                    metadata_json: row.try_get("metadata_json").map_err(database)?,
                })
            })
            .collect()
    }

    /// Reads at most 256 divisions and 4 MiB of metadata, including empty extents.
    pub async fn division_metadata_page(
        &self,
        after_id: u64,
    ) -> Result<Vec<DivisionMetadataRow>, CorpusError> {
        use futures::TryStreamExt;
        let mut rows=sqlx::query("SELECT d.division_id,d.document_id,CASE WHEN length(CAST(d.kind AS BLOB))<=256 THEN d.kind END AS kind,CASE WHEN length(CAST(d.source_id AS BLOB))<=256 THEN d.source_id END AS source_id,d.source_id IS NOT NULL AS has_source_id,d.parent_id,d.first_sentence,d.end_sentence,CASE WHEN length(CAST(d.metadata_json AS BLOB))<=1048576 THEN d.metadata_json END AS metadata_json,COALESCE(dm.first_sentence,(SELECT first_sentence FROM document_metadata WHERE document_id>d.document_id ORDER BY document_id LIMIT 1),?) AS document_first,COALESCE(dm.sentence_count,0) AS document_count,CASE WHEN length(CAST(o.observation_json AS BLOB))<=4096 THEN o.observation_json END AS observation_json FROM divisions d LEFT JOIN document_metadata dm ON dm.document_id=d.document_id LEFT JOIN division_observations o ON o.document_id=d.document_id AND o.kind=d.kind WHERE d.division_id>=? ORDER BY d.division_id LIMIT 256")
            .bind(sqlite_integer(self.sentence_count,"sentence count")?).bind(sqlite_integer(after_id,"division ID")?).fetch(&self.pool);
        let mut result = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = rows.try_next().await.map_err(database)? {
            let json: Option<String> = row.try_get("metadata_json").map_err(database)?;
            let json = json.ok_or_else(|| invalid("division metadata exceeds 1 MiB"))?;
            if !result.is_empty() && bytes + json.len() > 4 * 1024 * 1024 {
                break;
            }
            bytes += json.len();
            let id = sqlite_u64(row.try_get("division_id").map_err(database)?, "division ID")?;
            let document =
                sqlite_u64(row.try_get("document_id").map_err(database)?, "document ID")?;
            let kind: String = row.try_get("kind").map_err(database)?;
            lingrep_core::corpus::divisions::validate_kind(&kind).map_err(CorpusError::Io)?;
            let parent: Option<i64> = row.try_get("parent_id").map_err(database)?;
            let parent_id = parent
                .map(|id| sqlite_u64(id, "parent ID").map(lingrep_core::corpus::DivisionId))
                .transpose()?;
            if parent_id.is_some_and(|parent| parent.0 >= id) {
                return Err(invalid("division parent must precede child"));
            }
            let first_sentence = sqlite_u64(
                row.try_get("first_sentence").map_err(database)?,
                "division start",
            )?;
            let end_sentence = sqlite_u64(
                row.try_get("end_sentence").map_err(database)?,
                "division end",
            )?;
            if first_sentence > end_sentence || end_sentence > self.sentence_count {
                return Err(invalid("division extent outside realized corpus"));
            }
            let document_first = sqlite_u64(
                row.try_get("document_first").map_err(database)?,
                "document start",
            )?;
            let document_count = sqlite_u64(
                row.try_get("document_count").map_err(database)?,
                "document size",
            )?;
            if first_sentence < document_first || end_sentence > document_first + document_count {
                return Err(invalid("division extent crosses its owning document"));
            }
            let observation: Option<String> = row.try_get("observation_json").map_err(database)?;
            if observation.as_deref().and_then(|json| {
                serde_json::from_str::<lingrep_core::corpus::DivisionObservation>(json).ok()
            }) != Some(lingrep_core::corpus::DivisionObservation::Observed)
            {
                return Err(invalid("authored division lacks observed-kind evidence"));
            }
            let metadata = lingrep_core::corpus::metadata_from_json(
                serde_json::from_str(&json).map_err(|error| invalid(error.to_string()))?,
                "prepared division",
            )
            .map_err(CorpusError::Io)?;
            let source_id: Option<String> = row.try_get("source_id").map_err(database)?;
            if (source_id.is_none() && row.try_get::<bool, _>("has_source_id").map_err(database)?)
                || source_id
                    .as_ref()
                    .is_some_and(|id| id.is_empty() || id.len() > 256)
            {
                return Err(invalid("invalid division source identity"));
            }
            result.push(DivisionMetadataRow {
                division: Arc::new(lingrep_core::corpus::Division {
                    source_id,
                    id: lingrep_core::corpus::DivisionId(id),
                    document_id: DocumentId::new(document),
                    kind,
                    parent_id,
                    metadata,
                }),
                first_sentence,
                end_sentence,
            });
        }
        Ok(result)
    }

    /// Reports the first 257 kinds so a consumer can bound and mark its inventory.
    pub async fn division_observation_counts(&self) -> Result<Vec<(String, u64)>, CorpusError> {
        let rows=sqlx::query("SELECT kind,SUM(CASE WHEN json_extract(observation_json,'$.state')='observed' THEN 1 ELSE 0 END) AS observed FROM division_observations GROUP BY kind ORDER BY kind LIMIT 257").fetch_all(&self.pool).await.map_err(database)?;
        rows.into_iter()
            .map(|row| {
                let kind: String = row.try_get("kind").map_err(database)?;
                lingrep_core::corpus::divisions::validate_kind(&kind).map_err(CorpusError::Io)?;
                Ok((
                    kind,
                    sqlite_u64(
                        row.try_get("observed").map_err(database)?,
                        "observed document count",
                    )?,
                ))
            })
            .collect()
    }
    /// Reads bounded sentence contexts, sharing each division's metadata within the page.
    async fn structural_contexts(
        &self,
        sentences: &[SentenceId],
    ) -> Result<BTreeMap<u64, Arc<lingrep_core::corpus::StructuralContext>>, CorpusError> {
        use futures::TryStreamExt;
        use lingrep_core::corpus::divisions::{
            MAX_ACTIVE_DIVISIONS, MAX_DIVISION_KINDS, MAX_DIVISION_METADATA_BYTES,
        };
        use lingrep_core::corpus::{
            Division, DivisionId, DivisionObservation, StructuralContext, metadata_from_json,
        };
        if sentences.len() > 1024 {
            return Err(invalid("structural context page exceeds 1024 sentences"));
        }
        let requested =
            serde_json::to_string(&sentences.iter().map(|id| id.get()).collect::<Vec<_>>())
                .map_err(|error| invalid(error.to_string()))?;
        let mut locations = BTreeMap::new();
        let mut division_ids = BTreeSet::new();
        let mut documents = BTreeSet::new();
        let mut rows=sqlx::query("SELECT sentence_id,document_id,division_ids_json FROM sentence_provenance WHERE sentence_id IN (SELECT value FROM json_each(?)) ORDER BY sentence_id").bind(requested).fetch(&self.pool);
        while let Some(row) = rows.try_next().await.map_err(database)? {
            let sentence =
                sqlite_u64(row.try_get("sentence_id").map_err(database)?, "sentence ID")?;
            let document =
                sqlite_u64(row.try_get("document_id").map_err(database)?, "document ID")?;
            let json: String = row.try_get("division_ids_json").map_err(database)?;
            if json.len() > MAX_ACTIVE_DIVISIONS * 24 + 2 {
                return Err(invalid("division references exceed context budget"));
            }
            let ids: Vec<u64> =
                serde_json::from_str(&json).map_err(|error| invalid(error.to_string()))?;
            if ids.len() > MAX_ACTIVE_DIVISIONS || ids.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(invalid(
                    "division references must be bounded, unique and ordered",
                ));
            }
            division_ids.extend(ids.iter().copied());
            documents.insert(document);
            locations.insert(sentence, (document, ids));
        }
        drop(rows);
        let docs = serde_json::to_string(&documents).map_err(|error| invalid(error.to_string()))?;
        let mut observations: BTreeMap<u64, BTreeMap<String, DivisionObservation>> =
            BTreeMap::new();
        let mut rows=sqlx::query("SELECT document_id,kind,observation_json FROM division_observations WHERE document_id IN (SELECT value FROM json_each(?)) ORDER BY document_id,kind").bind(docs).fetch(&self.pool);
        while let Some(row) = rows.try_next().await.map_err(database)? {
            let document =
                sqlite_u64(row.try_get("document_id").map_err(database)?, "document ID")?;
            let kind: String = row.try_get("kind").map_err(database)?;
            lingrep_core::corpus::divisions::validate_kind(&kind).map_err(CorpusError::Io)?;
            let json: String = row.try_get("observation_json").map_err(database)?;
            if json.len() > 4096 {
                return Err(invalid("division observation exceeds byte budget"));
            }
            let observation =
                serde_json::from_str(&json).map_err(|error| invalid(error.to_string()))?;
            let kinds = observations.entry(document).or_default();
            if kinds.len() >= MAX_DIVISION_KINDS {
                return Err(invalid("too many observed division kinds"));
            }
            kinds.insert(kind, observation);
        }
        drop(rows);
        let ids =
            serde_json::to_string(&division_ids).map_err(|error| invalid(error.to_string()))?;
        let mut divisions = BTreeMap::new();
        let mut extents = BTreeMap::new();
        let mut bytes = 0usize;
        let mut rows=sqlx::query("SELECT division_id,document_id,kind,source_id,parent_id,first_sentence,end_sentence,metadata_json FROM divisions WHERE division_id IN (SELECT value FROM json_each(?)) ORDER BY division_id").bind(ids).fetch(&self.pool);
        while let Some(row) = rows.try_next().await.map_err(database)? {
            let id = sqlite_u64(row.try_get("division_id").map_err(database)?, "division ID")?;
            let document =
                sqlite_u64(row.try_get("document_id").map_err(database)?, "document ID")?;
            let first = sqlite_u64(
                row.try_get("first_sentence").map_err(database)?,
                "division start",
            )?;
            let end = sqlite_u64(
                row.try_get("end_sentence").map_err(database)?,
                "division end",
            )?;
            if first > end || end > self.sentence_count {
                return Err(invalid("division extent outside realized sentences"));
            }
            let kind: String = row.try_get("kind").map_err(database)?;
            lingrep_core::corpus::divisions::validate_kind(&kind).map_err(CorpusError::Io)?;
            let parent: Option<i64> = row.try_get("parent_id").map_err(database)?;
            let parent_id = parent
                .map(|id| sqlite_u64(id, "division parent").map(DivisionId))
                .transpose()?;
            if parent_id.is_some_and(|parent| parent.0 >= id) {
                return Err(invalid("division parent must precede child"));
            }
            let json: String = row.try_get("metadata_json").map_err(database)?;
            bytes = bytes.saturating_add(json.len());
            if json.len() > MAX_DIVISION_METADATA_BYTES || bytes > 64 * 1024 * 1024 {
                return Err(invalid("structural metadata page exceeds byte budget"));
            }
            let metadata = metadata_from_json(
                serde_json::from_str(&json).map_err(|error| invalid(error.to_string()))?,
                "prepared division",
            )
            .map_err(CorpusError::Io)?;
            let metadata_bytes = lingrep_core::corpus::divisions::validate_metadata(&metadata)
                .map_err(CorpusError::Io)?;
            divisions.insert(
                id,
                Arc::new(Division {
                    source_id: row.try_get("source_id").map_err(database)?,
                    id: DivisionId(id),
                    document_id: crate::DocumentId::new(document),
                    kind,
                    parent_id,
                    metadata,
                }),
            );
            extents.insert(id, (first, end, metadata_bytes));
        }
        drop(rows);
        let mut result = BTreeMap::new();
        for (sentence, (document, ids)) in locations {
            let mut active = Vec::new();
            let mut active_bytes = 0;
            for id in &ids {
                let division = divisions
                    .get(id)
                    .ok_or_else(|| invalid("dangling division reference"))?;
                let (first, end, metadata_bytes) = extents[id];
                if division.document_id.get() != document || !(first <= sentence && sentence < end)
                {
                    return Err(invalid(
                        "division membership disagrees with its extent or document",
                    ));
                }
                if let Some(parent) = division.parent_id {
                    let Some((parent_first, parent_end, _)) = extents.get(&parent.0) else {
                        return Err(invalid("active division is missing its parent"));
                    };
                    if !ids.contains(&parent.0) || parent_first > &first || parent_end < &end {
                        return Err(invalid("division is outside its authored parent"));
                    }
                }
                active_bytes += metadata_bytes;
                if active_bytes > lingrep_core::corpus::divisions::MAX_STRUCTURAL_CONTEXT_BYTES {
                    return Err(invalid("structural context exceeds byte budget"));
                }
                active.push(Arc::clone(division));
            }
            result.insert(
                sentence,
                Arc::new(StructuralContext {
                    observations: observations.get(&document).cloned().unwrap_or_default(),
                    divisions: active.into_boxed_slice(),
                }),
            );
        }
        Ok(result)
    }
    /// Returns the verified number of contiguous rich-metadata rows.
    pub const fn sentence_count(&self) -> u64 {
        self.sentence_count
    }

    pub(crate) const fn analysis_identity(&self) -> &AnalysisIdentity {
        &self.analysis_identity
    }

    pub(crate) const fn artifact_binding(&self) -> Option<&[u8; 32]> {
        self.artifact_binding.as_ref()
    }

    pub(crate) fn bind_to_artifact(mut self, binding: [u8; 32]) -> Self {
        self.artifact_binding = Some(binding);
        self
    }

    /// Returns rich context only; locations stay owned by `MappedLocations`.
    pub async fn rich_sentence(
        &self,
        sentence: SentenceId,
    ) -> Result<RichSentenceMetadata, CorpusError> {
        self.rich_sentence_page(&[sentence])
            .await?
            .pop()
            .flatten()
            .map(|row| row.metadata)
            .ok_or_else(|| invalid("sentence provenance is missing"))
    }
    /// Reads the exact admitted sentence bytes and analyzer alignment.
    pub async fn sentence_analysis(
        &self,
        sentence: SentenceId,
    ) -> Result<StoredSentenceAnalysis, CorpusError> {
        self.sentence_analysis_bounded(sentence, usize::MAX)
            .await?
            .ok_or_else(|| invalid("sentence analysis exceeds its fixed bound"))
    }

    /// Returns `None` before decompression when the encoded or decoded analysis
    /// exceeds `limit` bytes. A missing sentence remains an error.
    pub async fn sentence_analysis_bounded(
        &self,
        sentence: SentenceId,
        limit: usize,
    ) -> Result<Option<StoredSentenceAnalysis>, CorpusError> {
        let row = sqlx::query(
            "SELECT CASE WHEN length(analysis_blob)<=? THEN analysis_blob END AS analysis_blob, input_unit_id, input_kind, normalization,
                    input_identity, source_start, source_end
             FROM sentence_provenance WHERE sentence_id=?",
        )
        .bind(i64::try_from(limit.saturating_add(25)).unwrap_or(i64::MAX))
        .bind(sqlite_integer(sentence.get(), "sentence ID")?)
        .fetch_one(&self.pool)
        .await
        .map_err(database)?;
        let artifact: Option<Vec<u8>> = row.try_get("analysis_blob").map_err(database)?;
        let Some(artifact) = artifact else {
            return Ok(None);
        };
        let Some(decoded) = candidate::decode_bounded(&artifact, limit)? else {
            return Ok(None);
        };
        let [stored] = decoded.as_slice() else {
            return Err(invalid("sentence analysis has wrong cardinality"));
        };
        let input_kind = match row
            .try_get::<Option<String>, _>("input_kind")
            .map_err(database)?
            .as_deref()
        {
            None => None,
            Some("sentence") => Some(TextUnitKind::Sentence),
            Some("segment") => Some(TextUnitKind::Segment),
            Some(_) => return Err(invalid("stored input kind is invalid")),
        };
        let normalization = match row
            .try_get::<Option<String>, _>("normalization")
            .map_err(database)?
            .as_deref()
        {
            None => None,
            Some("none") => Some(NormalizationPolicy::None),
            Some("nfc") => Some(NormalizationPolicy::Nfc),
            Some(_) => return Err(invalid("stored normalization is invalid")),
        };
        let input_identity = optional_digest(
            row.try_get::<Option<Vec<u8>>, _>("input_identity")
                .map_err(database)?,
            "input identity",
        )?;
        let source_start = row
            .try_get::<Option<i64>, _>("source_start")
            .map_err(database)?;
        let source_end = row
            .try_get::<Option<i64>, _>("source_end")
            .map_err(database)?;
        let source_range = match (source_start, source_end) {
            (None, None) => None,
            (Some(start), Some(end)) => Some(
                u32::try_from(start).map_err(|_| invalid("source start is out of range"))?
                    ..u32::try_from(end).map_err(|_| invalid("source end is out of range"))?,
            ),
            _ => return Err(invalid("stored source range is incomplete")),
        };
        Ok(Some(StoredSentenceAnalysis {
            text: stored.text.clone(),
            surfaces: stored
                .surfaces
                .iter()
                .map(|surface| StoredSurfaceAlignment {
                    source_range: surface.source_start..surface.source_end,
                    word_range: surface.word_start..surface.word_end,
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            input_unit_id: row
                .try_get::<Option<i64>, _>("input_unit_id")
                .map_err(database)?
                .map(|value| sqlite_u64(value, "input unit ID"))
                .transpose()?,
            input_kind,
            normalization,
            input_identity,
            source_range,
        }))
    }

    /// Indexed lookup of the owning document and authored paragraphs containing
    /// `sentence`. No query-scope filtering or paragraph inference is applied.
    pub async fn reading_ranges(
        &self,
        sentence: SentenceId,
    ) -> Result<SentenceReadingRanges, CorpusError> {
        let ordinal = sqlite_integer(sentence.get(), "sentence ID")?;
        let row = sqlx::query("SELECT dm.document_id,dm.first_sentence,dm.sentence_count FROM sentence_provenance sp JOIN document_metadata dm ON dm.document_id=sp.document_id WHERE sp.sentence_id=?")
            .bind(ordinal).fetch_one(&self.pool).await.map_err(database)?;
        let document_id = DocumentId::new(sqlite_u64(
            row.try_get("document_id").map_err(database)?,
            "document ID",
        )?);
        let first = sqlite_u64(
            row.try_get("first_sentence").map_err(database)?,
            "document start",
        )?;
        let count = sqlite_u64(
            row.try_get("sentence_count").map_err(database)?,
            "document count",
        )?;
        let end = first
            .checked_add(count)
            .filter(|end| *end <= self.sentence_count)
            .ok_or_else(|| invalid("document reading range is invalid"))?;
        if !(first..end).contains(&sentence.get()) {
            return Err(invalid("sentence is outside its document"));
        }
        let rows = sqlx::query(READING_PARAGRAPHS_QUERY)
            .bind(ordinal)
            .bind((lingrep_core::corpus::divisions::MAX_ACTIVE_DIVISIONS + 1) as i64)
            .fetch_all(&self.pool)
            .await
            .map_err(database)?;
        if rows.len() > lingrep_core::corpus::divisions::MAX_ACTIVE_DIVISIONS {
            return Err(invalid("too many active paragraphs"));
        }
        let mut paragraphs = Vec::with_capacity(rows.len());
        for row in rows {
            let id = sqlite_u64(row.try_get("division_id").map_err(database)?, "division ID")?;
            let owner = sqlite_u64(row.try_get("document_id").map_err(database)?, "document ID")?;
            let start = sqlite_u64(
                row.try_get("first_sentence").map_err(database)?,
                "paragraph start",
            )?;
            let stop = sqlite_u64(
                row.try_get("end_sentence").map_err(database)?,
                "paragraph end",
            )?;
            if owner != document_id.get()
                || start < first
                || stop > end
                || !(start..stop).contains(&sentence.get())
            {
                return Err(invalid(
                    "paragraph crosses its owning document or excludes its sentence",
                ));
            }
            paragraphs.push(ParagraphRange {
                id,
                sentences: start..stop,
            });
        }
        Ok(SentenceReadingRanges {
            document_id,
            document: first..end,
            paragraphs,
        })
    }

    /// Returns structurally ordered regions active over one sentence.
    pub async fn active_regions(
        &self,
        sentence: SentenceId,
    ) -> Result<Box<[ActiveRegion]>, CorpusError> {
        Ok(self
            .rich_sentence_page(&[sentence])
            .await?
            .pop()
            .flatten()
            .map(|row| row.active_regions)
            .unwrap_or_default())
    }

    pub(crate) async fn projected_source_metadata(
        &self,
        sentences: &[crate::SentenceLocation],
        projection: &lingrep_core::corpus::SourceMetadataProjection,
    ) -> Result<Vec<Arc<serde_json::Map<String, serde_json::Value>>>, CorpusError> {
        if sentences.len() > crate::MAX_PROVENANCE_PAGE_SIZE {
            return Err(CorpusError::ProvenancePageCapacity {
                limit: crate::MAX_PROVENANCE_PAGE_SIZE,
            });
        }
        let ordinals = sentences
            .iter()
            .map(|location| location.corpus_sentence_ordinal)
            .collect::<Vec<_>>();
        let requested =
            serde_json::to_string(&ordinals).map_err(|error| invalid(error.to_string()))?;
        use futures::TryStreamExt;
        let mut stream = sqlx::query("SELECT sp.sentence_id,CASE WHEN length(CAST(dm.metadata_json AS BLOB))<=1048576 THEN dm.metadata_json END AS document_metadata,CASE WHEN length(CAST(sm.metadata_json AS BLOB))<=1048576 THEN sm.metadata_json END AS sentence_metadata,COALESCE(length(CAST(dm.metadata_json AS BLOB)),0)+COALESCE(length(CAST(sm.metadata_json AS BLOB)),0) AS metadata_bytes,MAX(COALESCE(length(CAST(dm.metadata_json AS BLOB)),0),COALESCE(length(CAST(sm.metadata_json AS BLOB)),0))>1048576 AS oversized_metadata
            FROM json_each(?) requested
            JOIN sentence_provenance sp ON sp.sentence_id=CAST(requested.value AS INTEGER)
            JOIN document_metadata dm ON dm.document_id=sp.document_id
            LEFT JOIN sentence_metadata sm ON sm.sentence_id=sp.sentence_id
            ORDER BY CAST(requested.key AS INTEGER)")
            .bind(requested).fetch(&self.pool);
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = stream.try_next().await.map_err(database)? {
            bytes += sqlite_usize(
                row.try_get("metadata_bytes").map_err(database)?,
                "metadata bytes",
            )?;
            if row
                .try_get::<bool, _>("oversized_metadata")
                .map_err(database)?
                || bytes > 64 * 1024 * 1024
            {
                return Err(invalid("source metadata page exceeds its byte budget"));
            }
            rows.push(row);
        }
        drop(stream);
        if rows.len() != sentences.len() {
            return Err(verification("source metadata page has missing sentences"));
        }
        let contexts = if projection.needs_divisions() {
            self.structural_contexts(
                &ordinals
                    .iter()
                    .copied()
                    .map(SentenceId::new)
                    .collect::<Vec<_>>(),
            )
            .await?
        } else {
            BTreeMap::new()
        };
        rows.into_iter()
            .map(|row| {
                let ordinal: i64 = row.try_get("sentence_id").map_err(database)?;
                let parse = |column| -> Result<lingrep_core::corpus::Metadata, CorpusError> {
                    let json: Option<String> = row.try_get(column).map_err(database)?;
                    let Some(json) = json else {
                        return Ok(Default::default());
                    };
                    let value = serde_json::from_str(&json).map_err(|error| {
                        invalid(format!(
                            "sentence {ordinal}: invalid source metadata: {error}"
                        ))
                    })?;
                    lingrep_core::corpus::metadata_from_json(value, "prepared source metadata")
                        .map_err(CorpusError::Io)
                };
                projection
                    .project(
                        &parse("document_metadata")?,
                        &parse("sentence_metadata")?,
                        contexts.get(&(ordinal as u64)).map_or(
                            &lingrep_core::corpus::StructuralContext::default(),
                            |context| context.as_ref(),
                        ),
                    )
                    .map(Arc::new)
                    .map_err(CorpusError::Io)
            })
            .collect()
    }

    /// Evaluates section inclusion for at most `MAX_PROVENANCE_PAGE_SIZE` sentence IDs.
    /// Output follows input order; missing sentences and oversized stored label data are errors.
    pub async fn section_admissions(
        &self,
        sentences: &[SentenceId],
        included: Option<&[String]>,
        include_unlabelled: bool,
    ) -> Result<Vec<bool>, CorpusError> {
        if sentences.len() > crate::MAX_PROVENANCE_PAGE_SIZE {
            return Err(CorpusError::ProvenancePageCapacity {
                limit: crate::MAX_PROVENANCE_PAGE_SIZE,
            });
        }
        let ids = sentences
            .iter()
            .map(|id| sqlite_integer(id.get(), "sentence ID"))
            .collect::<Result<Vec<_>, _>>()?;
        let ids = serde_json::to_string(&ids).map_err(|error| invalid(error.to_string()))?;
        let all_sections = included.is_none();
        let included = serde_json::to_string(included.unwrap_or_default())
            .map_err(|error| invalid(error.to_string()))?;
        let rows = sqlx::query(
            r#"
            WITH requested(position,sentence_id) AS (
                SELECT CAST(key AS INTEGER), CAST(value AS INTEGER) FROM json_each(?)
            )
            SELECT sp.sentence_id,
                CASE WHEN length(CAST(sp.region_labels_json AS BLOB))<=16777216 THEN
                    CASE WHEN json_array_length(sp.region_labels_json)=0 THEN ?
                    ELSE (? OR EXISTS (
                        SELECT 1 FROM json_each(sp.region_labels_json) label
                        JOIN json_each(?) included ON included.value=label.value
                    )) END
                END AS admitted
            FROM requested
            LEFT JOIN sentence_provenance sp ON sp.sentence_id=requested.sentence_id
            ORDER BY requested.position
        "#,
        )
        .bind(ids)
        .bind(include_unlabelled)
        .bind(all_sections)
        .bind(included)
        .fetch_all(&self.pool)
        .await
        .map_err(database)?;
        rows.into_iter()
            .map(|row| {
                let id: Option<i64> = row.try_get("sentence_id").map_err(database)?;
                if id.is_none() {
                    return Err(invalid("scope sentence is missing"));
                }
                row.try_get::<Option<bool>, _>("admitted")
                    .map_err(database)?
                    .ok_or_else(|| invalid("scope labels exceed their byte limit"))
            })
            .collect()
    }

    /// Composes a bounded page of rich metadata and shared division contexts.
    pub async fn rich_sentence_page(
        &self,
        sentences: &[SentenceId],
    ) -> Result<Vec<Option<RichSentenceProvenance>>, CorpusError> {
        if sentences.len() > crate::MAX_PROVENANCE_PAGE_SIZE {
            return Err(CorpusError::ProvenancePageCapacity {
                limit: crate::MAX_PROVENANCE_PAGE_SIZE,
            });
        }
        if sentences.is_empty() {
            return Ok(Vec::new());
        }
        let requested = sentences
            .iter()
            .map(|sentence| sqlite_integer(sentence.get(), "sentence ID"))
            .collect::<Result<Vec<_>, _>>()?;
        let requested_json =
            serde_json::to_string(&requested).map_err(|error| invalid(error.to_string()))?;
        let statement = r#"WITH requested(request_ordinal, sentence_id) AS (
                   SELECT CAST(key AS INTEGER), CAST(value AS INTEGER)
                   FROM json_each(?)
               )
               SELECT requested.request_ordinal,
                      sentence_provenance.sentence_id AS stored_sentence_id,
                      CASE WHEN length(CAST(sentence_provenance.region_labels_json AS BLOB))<=16777216 THEN sentence_provenance.region_labels_json END AS region_labels_json,
                      CASE WHEN length(CAST(document_metadata.metadata_json AS BLOB))<=1048576 THEN document_metadata.metadata_json END AS metadata_json,
                      CASE WHEN length(CAST(sentence_metadata.metadata_json AS BLOB))<=1048576 THEN sentence_metadata.metadata_json END AS sentence_metadata_json,
                      COALESCE(length(CAST(document_metadata.metadata_json AS BLOB)),0)+COALESCE(length(CAST(sentence_metadata.metadata_json AS BLOB)),0)+COALESCE(length(CAST(sentence_provenance.region_labels_json AS BLOB)),0) AS metadata_bytes,
                      MAX(COALESCE(length(CAST(document_metadata.metadata_json AS BLOB)),0),COALESCE(length(CAST(sentence_metadata.metadata_json AS BLOB)),0))>1048576 OR COALESCE(length(CAST(sentence_provenance.region_labels_json AS BLOB)),0)>16777216 AS oversized_metadata
               FROM requested
               LEFT JOIN sentence_provenance
                 ON sentence_provenance.sentence_id=requested.sentence_id
               LEFT JOIN document_metadata
                 ON document_metadata.document_id=sentence_provenance.document_id
               LEFT JOIN sentence_metadata
                 ON sentence_metadata.sentence_id=sentence_provenance.sentence_id
               ORDER BY requested.request_ordinal"#;
        use futures::TryStreamExt;
        let mut rows = sqlx::query(statement)
            .bind(&requested_json)
            .fetch(&self.pool);
        let mut bytes = 0usize;

        let mut page = std::iter::repeat_with(|| None)
            .take(sentences.len())
            .collect::<Vec<Option<(RichSentenceMetadata, Vec<ActiveRegion>)>>>();
        while let Some(row) = rows.try_next().await.map_err(database)? {
            bytes += sqlite_usize(
                row.try_get("metadata_bytes").map_err(database)?,
                "metadata bytes",
            )?;
            if row
                .try_get::<bool, _>("oversized_metadata")
                .map_err(database)?
                || bytes > 64 * 1024 * 1024
            {
                return Err(invalid("rich metadata page exceeds its byte budget"));
            }
            let request_ordinal = sqlite_usize(
                row.try_get("request_ordinal").map_err(database)?,
                "request ordinal",
            )?;
            if request_ordinal >= page.len() {
                return Err(invalid(
                    "provenance page returned an invalid request ordinal",
                ));
            }
            let stored_sentence: Option<i64> =
                row.try_get("stored_sentence_id").map_err(database)?;
            if stored_sentence.is_none() {
                continue;
            }
            if page[request_ordinal].is_none() {
                page[request_ordinal] = Some((
                    RichSentenceMetadata {
                        structure: Arc::default(),
                        region_labels: decode_region_labels(
                            row.try_get("region_labels_json").map_err(database)?,
                        )?,
                        metadata_json: row.try_get("metadata_json").map_err(database)?,
                        sentence_metadata_json: row
                            .try_get("sentence_metadata_json")
                            .map_err(database)?,
                    },
                    Vec::new(),
                ));
            }
        }
        drop(rows);
        let mut rows=sqlx::query("WITH requested(request_ordinal,sentence_id) AS (SELECT CAST(key AS INTEGER),CAST(value AS INTEGER) FROM json_each(?)) SELECT requested.request_ordinal,CASE WHEN length(CAST(r.label AS BLOB))<=65536 THEN r.label END AS label,r.first_sentence,r.end_sentence FROM requested JOIN sentence_provenance sp ON sp.sentence_id=requested.sentence_id JOIN json_each(sp.region_span_ids_json) active_span LEFT JOIN region_spans r ON r.span_id=CAST(active_span.value AS INTEGER) ORDER BY requested.request_ordinal,r.first_sentence,r.end_sentence DESC,r.label")
            .bind(&requested_json).fetch(&self.pool);
        while let Some(row) = rows.try_next().await.map_err(database)? {
            let index = sqlite_usize(
                row.try_get("request_ordinal").map_err(database)?,
                "request ordinal",
            )?;
            let label: Option<String> = row.try_get("label").map_err(database)?;
            let label = label.ok_or_else(|| {
                invalid("active region is missing or exceeds its label byte budget")
            })?;
            bytes += label.len() + std::mem::size_of::<ActiveRegion>();
            if bytes > 64 * 1024 * 1024 {
                return Err(invalid("rich metadata page exceeds its byte budget"));
            }
            let entry = page
                .get_mut(index)
                .and_then(Option::as_mut)
                .ok_or_else(|| invalid("active region has no requested sentence"))?;
            entry.1.push(ActiveRegion {
                label,
                first_sentence: sqlite_u64(
                    row.try_get("first_sentence").map_err(database)?,
                    "region start",
                )?,
                end_sentence: sqlite_u64(
                    row.try_get("end_sentence").map_err(database)?,
                    "region end",
                )?,
            });
        }
        drop(rows);
        let contexts = self.structural_contexts(sentences).await?;
        for (index, entry) in page.iter_mut().enumerate() {
            if let Some((metadata, _)) = entry {
                metadata.structure = contexts
                    .get(&sentences[index].get())
                    .cloned()
                    .ok_or_else(|| invalid("sentence structure is missing"))?;
            }
        }
        Ok(page
            .into_iter()
            .map(|entry| {
                entry.map(|(metadata, active_regions)| RichSentenceProvenance {
                    metadata,
                    active_regions: active_regions.into_boxed_slice(),
                })
            })
            .collect())
    }

    /// The spans of the named labels only.
    ///
    /// One indexed query per label, against `region_span_label`. A scope clause
    /// names a handful of labels and the reference corpus records sixteen in
    /// total, so a loop beats assembling a dynamic `IN` list — and it keeps what
    /// is read tied to what the caller asked for rather than to the table's
    /// size.
    ///
    /// Labels this corpus does not record simply contribute no rows. Whether
    /// that is worth a diagnostic is the caller's question, not this reader's:
    /// [`Self::section_coverage`] is how a caller learns what *is* recorded.
    ///
    /// # Errors
    ///
    /// Returns an error if a query fails or a stored bound is not a `u64`.
    pub async fn region_spans_for(
        &self,
        labels: &BTreeSet<&str>,
    ) -> Result<Vec<RegionSpanRow>, CorpusError> {
        let mut spans = Vec::new();
        for label in labels {
            let rows = sqlx::query(
                "SELECT first_sentence, end_sentence FROM region_spans
                 WHERE label=? ORDER BY first_sentence",
            )
            .bind(*label)
            .fetch_all(&self.pool)
            .await
            .map_err(database)?;
            for row in rows {
                spans.push(RegionSpanRow {
                    label: (*label).to_owned(),
                    first_sentence: sqlite_u64(
                        row.try_get("first_sentence").map_err(database)?,
                        "region span start",
                    )?,
                    end_sentence: sqlite_u64(
                        row.try_get("end_sentence").map_err(database)?,
                        "region span end",
                    )?,
                });
            }
        }
        Ok(spans)
    }

    /// Yields spans in span-ID order from pages of at most 256 rows.
    /// Releases the database connection before yielding a page's rows.
    /// Callers must reject decreasing sentence starts; span-ID order is not a schema guarantee.
    pub fn region_spans_in_span_order(
        &self,
    ) -> impl futures::Stream<Item = Result<RegionSpanRow, CorpusError>> + Send + '_ {
        let rows = futures::stream::try_unfold(
            (Some(i64::MIN), std::collections::VecDeque::new()),
            move |(mut next, mut buffered): (
                Option<i64>,
                std::collections::VecDeque<sqlx::sqlite::SqliteRow>,
            )| async move {
                if buffered.is_empty() {
                    let Some(first) = next else {
                        return Ok(None);
                    };
                    let page = sqlx::query("SELECT span_id,CASE WHEN length(CAST(label AS BLOB))<=65536 THEN label END AS label,first_sentence,end_sentence FROM region_spans WHERE span_id>=? ORDER BY span_id LIMIT 256")
                        .bind(first).fetch_all(&self.pool).await.map_err(database)?;
                    let Some(last) = page.last() else {
                        return Ok(None);
                    };
                    next = last
                        .try_get::<i64, _>("span_id")
                        .map_err(database)?
                        .checked_add(1);
                    buffered = page.into();
                }
                let Some(row) = buffered.pop_front() else {
                    return Ok(None);
                };
                let label: Option<String> = row.try_get("label").map_err(database)?;
                let span = RegionSpanRow {
                    label: label.ok_or_else(|| invalid("region label exceeds 64 KiB"))?,
                    first_sentence: sqlite_u64(
                        row.try_get("first_sentence").map_err(database)?,
                        "region span start",
                    )?,
                    end_sentence: sqlite_u64(
                        row.try_get("end_sentence").map_err(database)?,
                        "region span end",
                    )?,
                };
                Ok(Some((span, (next, buffered))))
            },
        );
        Box::pin(rows)
    }

    /// Aggregates section spans with an explicit result-cardinality bound.
    pub async fn section_coverage(
        &self,
        max_labels: usize,
    ) -> Result<Vec<SectionCoverage>, CorpusError> {
        if max_labels > MAX_SECTION_LABELS {
            return Err(CorpusError::SectionLabelCapacity {
                limit: MAX_SECTION_LABELS,
            });
        }
        let query_limit = i64::try_from(max_labels + 1)
            .map_err(|_| invalid("section label limit is out of range"))?;
        let rows = sqlx::query(
            "SELECT label, coverage
             FROM section_coverage
             ORDER BY coverage DESC, label
             LIMIT ?",
        )
        .bind(query_limit)
        .fetch_all(&self.pool)
        .await
        .map_err(database)?;
        if rows.len() > max_labels {
            return Err(CorpusError::SectionLabelCapacity { limit: max_labels });
        }
        rows.into_iter()
            .map(|row| {
                Ok(SectionCoverage {
                    label: row.try_get("label").map_err(database)?,
                    coverage: sqlite_u64(
                        row.try_get("coverage").map_err(database)?,
                        "section coverage",
                    )?,
                })
            })
            .collect()
    }
}

impl ProvenanceWriter {
    async fn record_document(&mut self, record: &SentenceRecord) -> Result<(), CorpusError> {
        let location = &record.location;
        if let Some(json) = &record.sentence_metadata_json {
            lingrep_core::corpus::metadata_from_json(
                serde_json::from_str(json).map_err(|error| invalid(error.to_string()))?,
                "prepared sentence metadata",
            )
            .map_err(CorpusError::Io)?;
        }
        if let Some(document) = self.document_metadata.as_mut()
            && document.document_id == location.document_id
        {
            if document.metadata_json != record.metadata_json {
                return Err(invalid("document metadata changed within a document"));
            }
            document.sentence_count += 1;
            return Ok(());
        }
        self.flush_document().await?;
        self.document_metadata = Some(DocumentMetadataRow {
            document_id: location.document_id,
            first_sentence: location.corpus_sentence_ordinal,
            sentence_count: 1,
            metadata_json: record.metadata_json.clone(),
        });
        Ok(())
    }

    async fn flush_document(&mut self) -> Result<(), CorpusError> {
        let Some(document) = self.document_metadata.take() else {
            return Ok(());
        };
        sqlx::query("INSERT INTO document_metadata(document_id, first_sentence, sentence_count, metadata_json) VALUES(?, ?, ?, ?)")
            .bind(sqlite_integer(document.document_id.get(), "document ID")?)
            .bind(sqlite_integer(document.first_sentence, "first sentence")?)
            .bind(sqlite_integer(document.sentence_count, "sentence count")?)
            .bind(document.metadata_json)
            .execute(&mut self.connection).await.map_err(database)?;
        Ok(())
    }

    async fn transition_regions(
        &mut self,
        location: &SentenceLocation,
        region_labels: &[String],
    ) -> Result<(), CorpusError> {
        if self.current_document != Some(location.document_id) {
            self.close_regions().await?;
            self.current_document = Some(location.document_id);
        }
        let labels = region_labels.iter().cloned().collect::<BTreeSet<_>>();
        let closing = self
            .active_regions
            .keys()
            .filter(|label| !labels.contains(*label))
            .cloned()
            .collect::<Vec<_>>();
        for label in closing {
            self.close_region(&label).await?;
        }
        let opening = labels
            .into_iter()
            .filter(|label| !self.active_regions.contains_key(label))
            .collect::<Vec<_>>();
        for label in opening {
            self.open_region(label)?;
        }
        Ok(())
    }

    fn open_region(&mut self, label: String) -> Result<(), CorpusError> {
        sqlite_integer(self.next_span_id, "region span ID")?;
        let next_span_id = self
            .next_span_id
            .checked_add(1)
            .ok_or_else(|| invalid("region span ID overflow"))?;
        self.active_regions.insert(
            label,
            ActiveSpan {
                id: self.next_span_id,
                first_sentence: self.next_sentence,
            },
        );
        self.next_span_id = next_span_id;
        Ok(())
    }

    async fn close_regions(&mut self) -> Result<(), CorpusError> {
        let labels = self.active_regions.keys().cloned().collect::<Vec<_>>();
        for label in labels {
            self.close_region(&label).await?;
        }
        Ok(())
    }

    async fn close_region(&mut self, label: &str) -> Result<(), CorpusError> {
        let Some(span) = self.active_regions.remove(label) else {
            return Ok(());
        };
        sqlx::query(
            "INSERT INTO region_spans(span_id, label, first_sentence, end_sentence)
             VALUES(?, ?, ?, ?)",
        )
        .bind(sqlite_integer(span.id, "region span ID")?)
        .bind(label)
        .bind(sqlite_integer(span.first_sentence, "region start")?)
        .bind(sqlite_integer(self.next_sentence, "region end")?)
        .execute(&mut self.connection)
        .await
        .map_err(database)?;
        Ok(())
    }
}

struct ExpectedColumn {
    name: &'static str,
    declared_type: &'static str,
    not_null: i64,
    default_value: Option<&'static str>,
    primary_key: i64,
}

/// Validates the identity table alone, whose shape every readable version
/// shares — it must be checked before the stored version can be trusted.
async fn validate_identity_table(pool: &SqlitePool) -> Result<(), CorpusError> {
    const IDENTITY_COLUMNS: &[ExpectedColumn] = &[
        ExpectedColumn {
            name: "singleton",
            declared_type: "INTEGER",
            not_null: 0,
            default_value: None,
            primary_key: 1,
        },
        ExpectedColumn {
            name: "schema_version",
            declared_type: "INTEGER",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "analysis_identity",
            declared_type: "BLOB",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "state",
            declared_type: "TEXT",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "sentence_count",
            declared_type: "INTEGER",
            not_null: 1,
            default_value: Some("0"),
            primary_key: 0,
        },
    ];
    validate_table(
        pool,
        "provenance_identity",
        "PRAGMA table_info('provenance_identity')",
        IDENTITY_COLUMNS,
    )
    .await
}

async fn validate_schema(pool: &SqlitePool) -> Result<(), CorpusError> {
    const SENTENCE_COLUMNS: &[ExpectedColumn] = &[
        ExpectedColumn {
            name: "sentence_id",
            declared_type: "INTEGER",
            not_null: 0,
            default_value: None,
            primary_key: 1,
        },
        ExpectedColumn {
            name: "source_name",
            declared_type: "TEXT",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "document_id",
            declared_type: "INTEGER",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "document_ordinal",
            declared_type: "INTEGER",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "document_sentence_ordinal",
            declared_type: "INTEGER",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "region_labels_json",
            declared_type: "TEXT",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "region_span_ids_json",
            declared_type: "TEXT",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "division_ids_json",
            declared_type: "TEXT",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "analysis_blob",
            declared_type: "BLOB",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "input_unit_id",
            declared_type: "INTEGER",
            not_null: 0,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "input_kind",
            declared_type: "TEXT",
            not_null: 0,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "normalization",
            declared_type: "TEXT",
            not_null: 0,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "input_identity",
            declared_type: "BLOB",
            not_null: 0,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "input_source_identity",
            declared_type: "BLOB",
            not_null: 0,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "input_document_identity",
            declared_type: "INTEGER",
            not_null: 0,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "input_paragraph_identity",
            declared_type: "INTEGER",
            not_null: 0,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "input_metadata_digest",
            declared_type: "BLOB",
            not_null: 0,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "analyzer_sentence_ordinal",
            declared_type: "INTEGER",
            not_null: 0,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "source_start",
            declared_type: "INTEGER",
            not_null: 0,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "source_end",
            declared_type: "INTEGER",
            not_null: 0,
            default_value: None,
            primary_key: 0,
        },
    ];
    const REGION_COLUMNS: &[ExpectedColumn] = &[
        ExpectedColumn {
            name: "span_id",
            declared_type: "INTEGER",
            not_null: 0,
            default_value: None,
            primary_key: 1,
        },
        ExpectedColumn {
            name: "label",
            declared_type: "TEXT",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "first_sentence",
            declared_type: "INTEGER",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "end_sentence",
            declared_type: "INTEGER",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
    ];
    const COVERAGE_COLUMNS: &[ExpectedColumn] = &[
        ExpectedColumn {
            name: "label",
            declared_type: "TEXT",
            not_null: 0,
            default_value: None,
            primary_key: 1,
        },
        ExpectedColumn {
            name: "coverage",
            declared_type: "INTEGER",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
    ];
    const DOCUMENT_COLUMNS: &[ExpectedColumn] = &[
        ExpectedColumn {
            name: "document_id",
            declared_type: "INTEGER",
            not_null: 0,
            default_value: None,
            primary_key: 1,
        },
        ExpectedColumn {
            name: "first_sentence",
            declared_type: "INTEGER",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "sentence_count",
            declared_type: "INTEGER",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
        ExpectedColumn {
            name: "metadata_json",
            declared_type: "TEXT",
            not_null: 0,
            default_value: None,
            primary_key: 0,
        },
    ];
    const SENTENCE_METADATA_COLUMNS: &[ExpectedColumn] = &[
        ExpectedColumn {
            name: "sentence_id",
            declared_type: "INTEGER",
            not_null: 0,
            default_value: None,
            primary_key: 1,
        },
        ExpectedColumn {
            name: "metadata_json",
            declared_type: "TEXT",
            not_null: 1,
            default_value: None,
            primary_key: 0,
        },
    ];
    const SENTENCE_METADATA_KEY_COLUMNS: &[ExpectedColumn] = &[ExpectedColumn {
        name: "key",
        declared_type: "TEXT",
        not_null: 0,
        default_value: None,
        primary_key: 1,
    }];
    validate_table(
        pool,
        "divisions",
        "PRAGMA table_info('divisions')",
        &[
            ExpectedColumn {
                name: "division_id",
                declared_type: "INTEGER",
                not_null: 0,
                default_value: None,
                primary_key: 1,
            },
            ExpectedColumn {
                name: "document_id",
                declared_type: "INTEGER",
                not_null: 1,
                default_value: None,
                primary_key: 0,
            },
            ExpectedColumn {
                name: "kind",
                declared_type: "TEXT",
                not_null: 1,
                default_value: None,
                primary_key: 0,
            },
            ExpectedColumn {
                name: "source_id",
                declared_type: "TEXT",
                not_null: 0,
                default_value: None,
                primary_key: 0,
            },
            ExpectedColumn {
                name: "parent_id",
                declared_type: "INTEGER",
                not_null: 0,
                default_value: None,
                primary_key: 0,
            },
            ExpectedColumn {
                name: "first_sentence",
                declared_type: "INTEGER",
                not_null: 1,
                default_value: None,
                primary_key: 0,
            },
            ExpectedColumn {
                name: "end_sentence",
                declared_type: "INTEGER",
                not_null: 1,
                default_value: None,
                primary_key: 0,
            },
            ExpectedColumn {
                name: "metadata_json",
                declared_type: "TEXT",
                not_null: 1,
                default_value: None,
                primary_key: 0,
            },
        ],
    )
    .await?;
    validate_table(
        pool,
        "division_observations",
        "PRAGMA table_info('division_observations')",
        &[
            ExpectedColumn {
                name: "document_id",
                declared_type: "INTEGER",
                not_null: 1,
                default_value: None,
                primary_key: 1,
            },
            ExpectedColumn {
                name: "kind",
                declared_type: "TEXT",
                not_null: 1,
                default_value: None,
                primary_key: 2,
            },
            ExpectedColumn {
                name: "observation_json",
                declared_type: "TEXT",
                not_null: 1,
                default_value: None,
                primary_key: 0,
            },
        ],
    )
    .await?;
    validate_table(
        pool,
        "document_metadata",
        "PRAGMA table_info('document_metadata')",
        DOCUMENT_COLUMNS,
    )
    .await?;
    validate_table(
        pool,
        "sentence_provenance",
        "PRAGMA table_info('sentence_provenance')",
        SENTENCE_COLUMNS,
    )
    .await?;
    validate_table(
        pool,
        "region_spans",
        "PRAGMA table_info('region_spans')",
        REGION_COLUMNS,
    )
    .await?;
    validate_table(
        pool,
        "section_coverage",
        "PRAGMA table_info('section_coverage')",
        COVERAGE_COLUMNS,
    )
    .await?;
    let (metadata_columns, key_columns): (&[ExpectedColumn], &[ExpectedColumn]) =
        (SENTENCE_METADATA_COLUMNS, SENTENCE_METADATA_KEY_COLUMNS);
    validate_table(
        pool,
        "sentence_metadata",
        "PRAGMA table_info('sentence_metadata')",
        metadata_columns,
    )
    .await?;
    validate_table(
        pool,
        "sentence_metadata_keys",
        "PRAGMA table_info('sentence_metadata_keys')",
        key_columns,
    )
    .await
}

async fn validate_table(
    pool: &SqlitePool,
    table: &'static str,
    statement: &'static str,
    expected: &[ExpectedColumn],
) -> Result<(), CorpusError> {
    let rows = sqlx::query(statement)
        .fetch_all(pool)
        .await
        .map_err(database)?;
    if rows.len() != expected.len() {
        return Err(invalid(format!("{table} has a non-canonical column set")));
    }
    for (ordinal, (row, expected)) in rows.iter().zip(expected).enumerate() {
        let cid: i64 = row.try_get("cid").map_err(database)?;
        let name: String = row.try_get("name").map_err(database)?;
        let declared_type: String = row.try_get("type").map_err(database)?;
        let not_null: i64 = row.try_get("notnull").map_err(database)?;
        let default_value: Option<String> = row.try_get("dflt_value").map_err(database)?;
        let primary_key: i64 = row.try_get("pk").map_err(database)?;
        if cid != ordinal as i64
            || name != expected.name
            || declared_type != expected.declared_type
            || not_null != expected.not_null
            || default_value.as_deref() != expected.default_value
            || primary_key != expected.primary_key
        {
            return Err(invalid(format!("{table} has a non-canonical schema")));
        }
    }
    Ok(())
}

fn sqlite_integer(value: u64, field: &str) -> Result<i64, CorpusError> {
    i64::try_from(value).map_err(|_| CorpusError::InvalidFormat {
        component: COMPONENT,
        message: format!("{field} exceeds SQLite INTEGER"),
    })
}

fn sqlite_u64(value: i64, field: &str) -> Result<u64, CorpusError> {
    u64::try_from(value).map_err(|_| invalid(format!("{field} is negative")))
}

fn optional_digest(value: Option<Vec<u8>>, field: &str) -> Result<Option<[u8; 32]>, CorpusError> {
    value
        .map(|bytes| {
            bytes
                .try_into()
                .map_err(|_| invalid(format!("stored {field} has the wrong length")))
        })
        .transpose()
}

fn sqlite_usize(value: i64, field: &str) -> Result<usize, CorpusError> {
    usize::try_from(value).map_err(|_| invalid(format!("{field} is out of range")))
}

fn decode_region_labels(encoded: String) -> Result<Box<[String]>, CorpusError> {
    serde_json::from_str::<Vec<String>>(&encoded)
        .map(Vec::into_boxed_slice)
        .map_err(|error| invalid(error.to_string()))
}

fn database(error: sqlx::Error) -> CorpusError {
    invalid(error.to_string())
}

fn reject_sidecars(path: &Path) -> Result<(), CorpusError> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        match fs::symlink_metadata(&sidecar) {
            Ok(_) => {
                return Err(invalid(format!(
                    "immutable artifact retained SQLite sidecar at {}",
                    sidecar.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(CorpusError::Io(error)),
        }
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> CorpusError {
    CorpusError::InvalidFormat {
        component: COMPONENT,
        message: message.into(),
    }
}

fn verification(message: impl Into<String>) -> CorpusError {
    CorpusError::VerificationMismatch {
        component: COMPONENT,
        sentence_ordinal: None,
        message: message.into(),
    }
}

#[cfg(test)]
#[path = "provenance/characterization_tests.rs"]
mod characterization_tests;

#[cfg(test)]
#[path = "provenance/structural_tests.rs"]
mod structural_tests;

#[cfg(test)]
#[path = "provenance/candidate_tests.rs"]
mod candidate_tests;
