//! Structured corpus events and the single sentence-admission boundary.

use super::divisions::{
    MAX_ACTIVE_DIVISIONS, MAX_DIVISION_DEPTH, MAX_DIVISION_KINDS, MAX_STRUCTURAL_CONTEXT_BYTES,
};
use super::{Division, DivisionId, DivisionObservation, StructuralContext};
use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::Arc;

use crate::scope::{DocumentOutcome, ScopeClause, Verdict};

use super::scope_observation::{ScopeCounts, ScopeObservation};

pub const MAX_METADATA_FIELDS: usize = 256;
pub const MAX_METADATA_ARRAY_ELEMENTS: usize = 1_024;
pub const MAX_METADATA_KEY_BYTES: usize = 256;
pub const MAX_METADATA_SCALAR_BYTES: usize = 64 * 1_024;
pub const MAX_OPAQUE_METADATA_BYTES: usize = 1_024 * 1_024;

macro_rules! corpus_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(u64);

        impl $name {
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }

            pub const fn new(value: u64) -> Self {
                Self(value)
            }
        }
    };
}

corpus_id!(DocumentId);
corpus_id!(SentenceId);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataScalar {
    String(String),
    Number(String),
    Bool(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataValue {
    Scalar(MetadataScalar),
    Scalars(Box<[MetadataScalar]>),
    Opaque(serde_json::Value),
}

pub type Metadata = Box<[(String, MetadataValue)]>;

#[derive(Clone, Debug)]
pub struct SourceMetadataProjection {
    keys: Box<[String]>,
}

enum MetadataOwner<'a> {
    Document(&'a str),
    Sentence(&'a str),
    Division(&'a str, &'a str),
}

fn metadata_owner(key: &str) -> io::Result<MetadataOwner<'_>> {
    let owner = if let Some(name) = key
        .strip_prefix("@document.")
        .filter(|name| !name.is_empty())
    {
        MetadataOwner::Document(name)
    } else if let Some(name) = key
        .strip_prefix("@sentence.")
        .filter(|name| !name.is_empty())
    {
        MetadataOwner::Sentence(name)
    } else if let Some(rest) = key.strip_prefix("@division(") {
        let Some((kind, name)) = rest.split_once(").") else {
            return Err(invalid_metadata(
                "source metadata projection",
                "expected @division(KIND).KEY",
            ));
        };
        super::divisions::validate_kind(kind)?;
        if name.is_empty() {
            return Err(invalid_metadata(
                "source metadata projection",
                "division metadata key is empty",
            ));
        }
        MetadataOwner::Division(kind, name)
    } else {
        return Err(invalid_metadata(
            "source metadata projection",
            "use @document.KEY, @sentence.KEY or @division(KIND).KEY; metadata has no implicit inheritance",
        ));
    };
    let name = match owner {
        MetadataOwner::Document(name)
        | MetadataOwner::Sentence(name)
        | MetadataOwner::Division(_, name) => name,
    };
    if name.len() > 256 {
        return Err(invalid_metadata(
            "source metadata projection",
            "metadata key exceeds 256 bytes",
        ));
    }
    Ok(owner)
}

impl SourceMetadataProjection {
    pub fn new(mut keys: Vec<String>) -> io::Result<Self> {
        keys.sort_unstable();
        if keys.len() > MAX_METADATA_FIELDS
            || keys
                .iter()
                .any(|key| key.is_empty() || key.len() > MAX_METADATA_KEY_BYTES + 272)
            || keys.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(invalid_metadata(
                "source metadata projection",
                "requires at most 256 unique bounded selectors",
            ));
        }
        for key in &keys {
            metadata_owner(key)?;
        }
        Ok(Self {
            keys: keys.into_boxed_slice(),
        })
    }
    pub fn keys(&self) -> &[String] {
        &self.keys
    }
    pub fn needs_divisions(&self) -> bool {
        self.keys.iter().any(|key| key.starts_with("@division("))
    }

    /// Projects selected values under their actual owners, preserving division identities.
    pub fn project(
        &self,
        document: &Metadata,
        sentence: &Metadata,
        structure: &StructuralContext,
    ) -> io::Result<serde_json::Map<String, serde_json::Value>> {
        let mut document_fields = serde_json::Map::new();
        let mut sentence_fields = serde_json::Map::new();
        let mut document_selected = false;
        let mut sentence_selected = false;
        let mut kinds: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
        for key in &self.keys {
            match metadata_owner(key)? {
                MetadataOwner::Document(key) => {
                    document_selected = true;
                    if let Some((_, value)) = document.iter().find(|(name, _)| name == key) {
                        document_fields.insert(key.into(), value.to_json());
                    }
                }
                MetadataOwner::Sentence(key) => {
                    sentence_selected = true;
                    if let Some((_, value)) = sentence.iter().find(|(name, _)| name == key) {
                        sentence_fields.insert(key.into(), value.to_json());
                    }
                }
                MetadataOwner::Division(kind, key) => {
                    kinds.entry(kind).or_default().push(key);
                }
            }
        }
        let mut object = serde_json::Map::new();
        if document_selected {
            object.insert("document".into(), document_fields.into());
        }
        if sentence_selected {
            object.insert("sentence".into(), sentence_fields.into());
        }
        if !kinds.is_empty() {
            for kind in kinds.keys() {
                if !structure.observes(kind) {
                    return Err(invalid_metadata(
                        "source metadata projection",
                        &format!("division kind `{kind}` is unavailable"),
                    ));
                }
            }
            let mut divisions = Vec::new();
            for division in &structure.divisions {
                let Some(keys) = kinds.get(division.kind.as_str()) else {
                    continue;
                };
                let metadata = division
                    .metadata
                    .iter()
                    .filter(|(key, _)| keys.contains(&key.as_str()))
                    .map(|(key, value)| (key.clone(), value.to_json()))
                    .collect::<serde_json::Map<_, _>>();
                divisions.push(serde_json::json!({"id":division.id.0,"kind":division.kind,"parent_id":division.parent_id.map(|id|id.0),"source_id":division.source_id,"metadata":metadata}));
            }
            object.insert("divisions".into(), divisions.into());
        }
        self.validate_output(&object)?;
        Ok(object)
    }

    pub fn validate_output(
        &self,
        object: &serde_json::Map<String, serde_json::Value>,
    ) -> io::Result<()> {
        let mut allowed: std::collections::BTreeMap<&str, std::collections::BTreeSet<&str>> =
            Default::default();
        let mut kinds: std::collections::BTreeMap<&str, std::collections::BTreeSet<&str>> =
            Default::default();
        for key in &self.keys {
            match metadata_owner(key)? {
                MetadataOwner::Document(key) => {
                    allowed.entry("document").or_default().insert(key);
                }
                MetadataOwner::Sentence(key) => {
                    allowed.entry("sentence").or_default().insert(key);
                }
                MetadataOwner::Division(kind, key) => {
                    kinds.entry(kind).or_default().insert(key);
                }
            }
        }
        if object.len() != allowed.len() + usize::from(!kinds.is_empty()) {
            return Err(invalid_metadata(
                "source metadata projection",
                "owner selection differs from the run",
            ));
        }
        for (owner, keys) in allowed {
            let Some(fields) = object.get(owner).and_then(serde_json::Value::as_object) else {
                return Err(invalid_metadata(
                    "source metadata projection",
                    "selected owner must be an object",
                ));
            };
            if fields.keys().any(|key| !keys.contains(key.as_str())) {
                return Err(invalid_metadata(
                    "source metadata projection",
                    "unselected metadata field",
                ));
            }
        }
        if !kinds.is_empty() {
            let Some(divisions) = object
                .get("divisions")
                .and_then(serde_json::Value::as_array)
            else {
                return Err(invalid_metadata(
                    "source metadata projection",
                    "divisions must be a list",
                ));
            };
            if divisions.len() > MAX_ACTIVE_DIVISIONS {
                return Err(invalid_metadata(
                    "source metadata projection",
                    "too many active divisions",
                ));
            }
            let mut previous = None;
            for division in divisions {
                let Some(division) = division.as_object() else {
                    return Err(invalid_metadata(
                        "source metadata projection",
                        "division must be an object",
                    ));
                };
                let Some(id) = division.get("id").and_then(serde_json::Value::as_u64) else {
                    return Err(invalid_metadata(
                        "source metadata projection",
                        "division ID must be an unsigned integer",
                    ));
                };
                let Some(kind) = division.get("kind").and_then(serde_json::Value::as_str) else {
                    return Err(invalid_metadata(
                        "source metadata projection",
                        "division kind must be present",
                    ));
                };
                let Some(keys) = kinds.get(kind) else {
                    return Err(invalid_metadata(
                        "source metadata projection",
                        "unselected division kind",
                    ));
                };
                let Some(metadata) = division
                    .get("metadata")
                    .and_then(serde_json::Value::as_object)
                else {
                    return Err(invalid_metadata(
                        "source metadata projection",
                        "division metadata must be an object",
                    ));
                };
                if division.len() != 5
                    || !division.contains_key("parent_id")
                    || previous.is_some_and(|previous| previous >= id)
                    || metadata.keys().any(|key| !keys.contains(key.as_str()))
                {
                    return Err(invalid_metadata(
                        "source metadata projection",
                        "division fields or identity order are invalid",
                    ));
                }
                if !division["parent_id"].is_null()
                    && division["parent_id"]
                        .as_u64()
                        .is_none_or(|parent| parent >= id)
                {
                    return Err(invalid_metadata(
                        "source metadata projection",
                        "division parent must precede its child",
                    ));
                }
                if !division.get("source_id").is_some_and(|value| {
                    value.is_null()
                        || value
                            .as_str()
                            .is_some_and(|value| !value.is_empty() && value.len() <= 256)
                }) {
                    return Err(invalid_metadata(
                        "source metadata projection",
                        "invalid authored division source ID",
                    ));
                }
                previous = Some(id);
            }
        }
        let bytes = serde_json::to_vec(object).map_err(io::Error::other)?;
        if bytes.len() > MAX_METADATA_SCALAR_BYTES {
            return Err(invalid_metadata(
                "source metadata projection",
                "selected metadata exceeds 64 KiB per sentence",
            ));
        }
        Ok(())
    }
}

impl MetadataValue {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Scalar(value) => value.to_json(),
            Self::Scalars(values) => {
                serde_json::Value::Array(values.iter().map(MetadataScalar::to_json).collect())
            }
            Self::Opaque(value) => value.clone(),
        }
    }
}

impl MetadataScalar {
    fn to_json(&self) -> serde_json::Value {
        match self {
            Self::String(value) => serde_json::Value::String(value.clone()),
            Self::Number(value) => value.parse::<serde_json::Number>().map_or_else(
                |_| serde_json::Value::String(value.clone()),
                serde_json::Value::Number,
            ),
            Self::Bool(value) => serde_json::Value::Bool(*value),
        }
    }
}

/// Types a JSON object as document metadata.
///
/// Public because it is the **single** typing rule for both execution paths.
/// The direct reader types frontmatter here; a prepared artifact stores the
/// same object as `document_metadata.metadata_json` and must type it
/// identically, or a `--where` clause would mean two different things
/// depending on how the corpus was read.
///
/// # Errors
///
/// Returns an error when the value is not an object, or when it exceeds the
/// field-count, key-length, scalar-size or array-length bounds.
pub fn metadata_from_json(value: serde_json::Value, context: &str) -> io::Result<Metadata> {
    super::divisions::check_metadata_size(&value)?;
    if value.is_null() {
        return Ok(Box::new([]));
    }
    let serde_json::Value::Object(object) = value else {
        return Err(invalid_metadata(context, "metadata must be an object"));
    };
    if object.len() > MAX_METADATA_FIELDS {
        return Err(invalid_metadata(
            context,
            "metadata has more than 256 fields",
        ));
    }
    let mut fields = object.into_iter().collect::<Vec<_>>();
    fields.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    fields
        .into_iter()
        .map(|(key, value)| {
            if key.len() > MAX_METADATA_KEY_BYTES {
                return Err(invalid_metadata(
                    context,
                    "metadata key exceeds 256 UTF-8 bytes",
                ));
            }
            Ok((key, metadata_value(value, context)?))
        })
        .collect::<io::Result<Vec<_>>>()
        .map(Vec::into_boxed_slice)
}

fn metadata_value(value: serde_json::Value, context: &str) -> io::Result<MetadataValue> {
    match value {
        serde_json::Value::String(value) => {
            scalar_size(value.len(), context)?;
            Ok(MetadataValue::Scalar(MetadataScalar::String(value)))
        }
        serde_json::Value::Number(value) => {
            let value = value.to_string();
            scalar_size(value.len(), context)?;
            Ok(MetadataValue::Scalar(MetadataScalar::Number(value)))
        }
        serde_json::Value::Bool(value) => Ok(MetadataValue::Scalar(MetadataScalar::Bool(value))),
        serde_json::Value::Array(values)
            if values.iter().all(|value| {
                matches!(
                    value,
                    serde_json::Value::String(_)
                        | serde_json::Value::Number(_)
                        | serde_json::Value::Bool(_)
                )
            }) =>
        {
            if values.len() > MAX_METADATA_ARRAY_ELEMENTS {
                return Err(invalid_metadata(
                    context,
                    "metadata array has more than 1,024 elements",
                ));
            }
            let scalars = values
                .into_iter()
                .map(|value| match metadata_value(value, context)? {
                    MetadataValue::Scalar(value) => Ok(value),
                    _ => {
                        #[expect(
                            clippy::unreachable,
                            reason = "the scalar-array predicate was checked"
                        )]
                        {
                            unreachable!("the scalar-array predicate was checked")
                        }
                    }
                })
                .collect::<io::Result<Vec<_>>>()?;
            Ok(MetadataValue::Scalars(scalars.into_boxed_slice()))
        }
        value => {
            let bytes = serde_json::to_vec(&value).map_err(|error| {
                invalid_metadata(context, &format!("metadata serialization failed: {error}"))
            })?;
            if bytes.len() > MAX_OPAQUE_METADATA_BYTES {
                return Err(invalid_metadata(context, "opaque metadata exceeds 1 MiB"));
            }
            Ok(MetadataValue::Opaque(value))
        }
    }
}

fn scalar_size(bytes: usize, context: &str) -> io::Result<()> {
    if bytes > MAX_METADATA_SCALAR_BYTES {
        Err(invalid_metadata(context, "metadata scalar exceeds 64 KiB"))
    } else {
        Ok(())
    }
}

fn invalid_metadata(context: &str, message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("{context}: {message}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorpusEvent {
    DivisionStart {
        source_ordinal: u64,
        source_id: Option<String>,
        kind: String,
        parent_source_ordinal: Option<u64>,
        metadata: Metadata,
    },
    DivisionEnd {
        source_ordinal: u64,
    },
    DivisionObservation {
        kind: String,
        observation: DivisionObservation,
    },
    DocumentStart {
        source_name: String,
        metadata: Metadata,
    },
    ParagraphStart {
        metadata: Metadata,
    },
    Heading {
        level: u8,
        text: String,
        classes: Box<[String]>,
    },
    Sentence {
        text: String,
        /// Canonical words supplied by a pretokenized format. Text readers
        /// leave this absent and analysis supplies the words later.
        words: Option<crate::token_match::BoundTokenSentence>,
        /// Sentence-local enhanced graph evidence supplied by a syntax-capable
        /// reader. `None` means the reader has no enhanced-graph capability;
        /// `Some(Unavailable)` means it can observe the graph but this sentence
        /// supplied no DEPS evidence.
        enhanced_graph: Option<super::conllu::EnhancedGraphObservation>,
        active_labels: Box<[String]>,
        /// Sentence fields are queried explicitly through `@sentence.KEY`.
        metadata: Metadata,
    },
    ParagraphEnd,
    DocumentEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedSentence {
    pub structure: Arc<StructuralContext>,
    pub id: SentenceId,
    pub document_id: DocumentId,
    pub document_sentence_ordinal: u64,
    pub text: String,
    pub words: Option<crate::token_match::BoundTokenSentence>,
    /// Preserved reader capability and sentence-local enhanced graph evidence.
    /// Graph-capable prepared storage consumes it as the sentence sidecar
    /// observation; direct execution carries the same value with the words.
    pub enhanced_graph: Option<super::conllu::EnhancedGraphObservation>,
    pub active_labels: Box<[String]>,
    pub metadata: Metadata,
}

/// `Sentence` is far larger than its siblings, and deliberately so.
///
/// `clippy::large_enum_variant` fires when the largest variant exceeds the
/// second largest by more than 200 bytes, and `AdmittedSentence` sat exactly
/// at that boundary once `BoundTokenSentence` gained character offsets — so
/// the next small field added anywhere inside it would fail the lint on a file
/// its author never touched. The lint's own remedy is boxing, which reaches
/// roughly nine production sites plus `lingrep-corpus`'s suites and a bench, and
/// buys one heap allocation per sentence on a path that moves an event per
/// sentence through the reader iterators. That trade is worth measuring rather
/// than making incidentally, and until it is measured the size is the
/// deliberate choice. Allowed here rather than raising
/// `enum-variant-size-threshold` in `clippy.toml`, which would weaken the lint
/// for every other enum in the workspace to answer a fact about this one.
/// See `vibrato-pipe-ay7`.
#[allow(
    clippy::large_enum_variant,
    reason = "moving the sentence inline beats one allocation per sentence on the streaming path"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmittedCorpusEvent {
    DocumentStart {
        id: DocumentId,
        source_name: String,
        metadata: Metadata,
    },
    DivisionStart(Arc<Division>),
    DivisionEnd {
        id: DivisionId,
    },
    DivisionObservation {
        document_id: DocumentId,
        kind: String,
        observation: DivisionObservation,
    },
    Sentence(AdmittedSentence),
    DocumentEnd {
        id: DocumentId,
    },
}

impl AdmittedCorpusEvent {
    #[must_use]
    pub const fn sentence(&self) -> Option<&AdmittedSentence> {
        match self {
            Self::Sentence(sentence) => Some(sentence),
            _ => None,
        }
    }
}

pub struct AdmittedCorpus<I> {
    events: I,
    next_document: u64,
    next_sentence: u64,
    document: Option<(DocumentId, u64)>,
    paragraph: Option<DivisionId>,
    headings: Vec<(u8, DivisionId)>,
    active_divisions: BTreeMap<DivisionId, (Arc<Division>, usize)>,
    reader_divisions: BTreeMap<u64, DivisionId>,
    last_reader_division: Option<u64>,
    observations: BTreeMap<String, DivisionObservation>,
    next_division: u64,
    pending_events: VecDeque<AdmittedCorpusEvent>,
    finished: bool,
    /// `None` on an unscoped run, and structurally so: there is no clause to
    /// evaluate *and* no handle to lock. Guarding the two separately would
    /// leave a per-sentence mutex on the path every existing run takes.
    scope: Option<Scoped>,
    /// The current document's outcome, recomputed once per `DocumentStart`.
    outcome: DocumentOutcome,
}

struct Scoped {
    clause: ScopeClause,
    observation: ScopeObservation,
    /// Tallied here and published per document, keeping locking off the
    /// per-sentence ingestion path.
    pending: ScopeCounts,
    /// The current document's metadata, retained **only** while its outcome is
    /// [`DocumentOutcome::RequiresSentences`]. A metadata-only clause therefore
    /// never clones a document's frontmatter, which is the common case and the
    /// one every run took before section scope existed.
    document_metadata: Metadata,
}

impl<I> AdmittedCorpus<I> {
    #[must_use]
    pub fn new(events: I) -> Self {
        Self {
            events,
            next_document: 0,
            next_sentence: 0,
            document: None,
            paragraph: None,
            headings: Vec::new(),
            active_divisions: BTreeMap::new(),
            reader_divisions: BTreeMap::new(),
            last_reader_division: None,
            observations: BTreeMap::new(),
            next_division: 0,
            pending_events: VecDeque::new(),
            finished: false,
            scope: None,
            outcome: DocumentOutcome::Decided(Verdict::Admitted),
        }
    }

    /// Restricts the stream to documents the clause admits.
    ///
    /// The caller keeps its own clone of `observation`: this reader is handed
    /// to a source by value and the source is moved into the execution runner,
    /// so the counts have to leave through a shared handle rather than through
    /// the reader itself.
    ///
    #[must_use]
    pub fn with_scope(mut self, clause: ScopeClause, observation: ScopeObservation) -> Self {
        self.scope = Some(Scoped {
            clause,
            observation,
            pending: ScopeCounts::default(),
            document_metadata: Metadata::default(),
        });
        self
    }

    fn invalid(message: &'static str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message)
    }

    fn take(counter: &mut u64, name: &'static str) -> io::Result<u64> {
        let value = *counter;
        *counter = counter.checked_add(1).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, format!("{name} overflow"))
        })?;
        Ok(value)
    }
}

impl<I> Iterator for AdmittedCorpus<I>
where
    I: Iterator<Item = io::Result<CorpusEvent>>,
{
    type Item = io::Result<AdmittedCorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(event) = self.pending_events.pop_front() {
                return Some(Ok(event));
            }
            let Some(event) = self.events.next() else {
                if self.finished {
                    return None;
                }
                self.finished = true;
                if self.paragraph.is_some()
                    || self.document.is_some()
                    || !self.active_divisions.is_empty()
                {
                    return Some(Err(Self::invalid(
                        "corpus event stream ended with open structure",
                    )));
                }
                return None;
            };
            let event = match event {
                Ok(event) => event,
                Err(error) => return Some(Err(error)),
            };
            let admitted = match self.admit(event) {
                Ok(Some(event)) => event,
                Ok(None) => continue,
                Err(error) => return Some(Err(error)),
            };
            return Some(Ok(admitted));
        }
    }
}

impl<I> AdmittedCorpus<I> {
    fn open_division(
        &mut self,
        kind: String,
        parent_id: Option<DivisionId>,
        metadata: Metadata,
        source_id: Option<String>,
    ) -> io::Result<Arc<Division>> {
        super::divisions::validate_kind(&kind)?;
        if source_id
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > 256)
        {
            return Err(Self::invalid("division source ID must contain 1–256 bytes"));
        }
        let document_id = self
            .document
            .ok_or_else(|| Self::invalid("division outside document"))?
            .0;
        let bytes = super::divisions::validate_metadata(&metadata)?;
        if self.active_divisions.len() >= MAX_ACTIVE_DIVISIONS
            || self
                .active_divisions
                .values()
                .map(|(_, bytes)| bytes)
                .sum::<usize>()
                .saturating_add(bytes)
                > MAX_STRUCTURAL_CONTEXT_BYTES
        {
            return Err(Self::invalid(
                "active structural context exceeds count or byte budget",
            ));
        }
        let mut parent = parent_id;
        let mut depth = 0;
        while let Some(id) = parent {
            let (division, _) = self
                .active_divisions
                .get(&id)
                .ok_or_else(|| Self::invalid("division parent is not open"))?;
            depth += 1;
            if depth >= MAX_DIVISION_DEPTH {
                return Err(Self::invalid("division nesting exceeds 64 levels"));
            }
            parent = division.parent_id;
        }
        if self.observations.len() >= MAX_DIVISION_KINDS && !self.observations.contains_key(&kind) {
            return Err(Self::invalid("too many observed division kinds"));
        }
        self.observations
            .entry(kind.clone())
            .or_insert(DivisionObservation::Observed);
        let division = Arc::new(Division {
            source_id,
            id: DivisionId(Self::take(&mut self.next_division, "division ID")?),
            document_id,
            kind,
            parent_id,
            metadata,
        });
        self.active_divisions
            .insert(division.id, (Arc::clone(&division), bytes));
        Ok(division)
    }

    fn close_division(&mut self, id: DivisionId) -> io::Result<()> {
        if self
            .active_divisions
            .values()
            .any(|(division, _)| division.parent_id == Some(id))
        {
            return Err(Self::invalid("division closed before its child"));
        }
        self.active_divisions
            .remove(&id)
            .ok_or_else(|| Self::invalid("division closed twice"))?;
        self.pending_events
            .push_back(AdmittedCorpusEvent::DivisionEnd { id });
        Ok(())
    }

    fn admit(&mut self, event: CorpusEvent) -> io::Result<Option<AdmittedCorpusEvent>> {
        match event {
            CorpusEvent::DocumentStart {
                source_name,
                metadata,
            } => {
                if self.document.is_some() || self.paragraph.is_some() {
                    return Err(Self::invalid(
                        "document started while another structure is open",
                    ));
                }
                let id = DocumentId::new(Self::take(&mut self.next_document, "document ID")?);
                self.document = Some((id, 0));
                self.headings.clear();
                self.observations.clear();
                self.reader_divisions.clear();
                self.last_reader_division = None;
                if let Some(scoped) = &mut self.scope {
                    self.outcome = scoped.clause.evaluate_document(&metadata);
                    // A document that reaches sentence evaluation is admitted
                    // at document grain: it was *in scope*, whether or not any
                    // of its sentences turn out to be. The two grains no longer
                    // move together, and the report says so.
                    scoped.pending.record_document(match self.outcome {
                        DocumentOutcome::Decided(verdict) => verdict,
                        DocumentOutcome::RequiresSentences => Verdict::Admitted,
                    })?;
                    scoped.document_metadata =
                        if matches!(self.outcome, DocumentOutcome::RequiresSentences) {
                            metadata.clone()
                        } else {
                            Metadata::default()
                        };
                }
                Ok(Some(AdmittedCorpusEvent::DocumentStart {
                    id,
                    source_name,
                    metadata,
                }))
            }
            CorpusEvent::ParagraphStart { metadata } => {
                let Some(_) = self.document else {
                    return Err(Self::invalid("paragraph started outside a document"));
                };
                if self.paragraph.is_some() {
                    return Err(Self::invalid(
                        "paragraph started while another paragraph is open",
                    ));
                }
                let division = self.open_division("paragraph".into(), None, metadata, None)?;
                self.paragraph = Some(division.id);
                Ok(Some(AdmittedCorpusEvent::DivisionStart(division)))
            }
            CorpusEvent::Heading {
                level,
                text,
                classes,
            } => {
                if !(1..=6).contains(&level) {
                    return Err(Self::invalid("heading level is outside 1..=6"));
                }
                while self
                    .headings
                    .last()
                    .is_some_and(|(open_level, _)| *open_level >= level)
                {
                    if let Some((_, id)) = self.headings.pop() {
                        self.close_division(id)?;
                    }
                }
                let parent = self.headings.last().map(|(_, id)| *id);
                let metadata = metadata_from_json(
                    serde_json::json!({"title":text,"level":level,"classes":classes}),
                    "heading",
                )?;
                let division = self.open_division("section".into(), parent, metadata, None)?;
                self.headings.push((level, division.id));
                self.pending_events
                    .push_back(AdmittedCorpusEvent::DivisionStart(division));
                Ok(None)
            }
            CorpusEvent::DivisionObservation { kind, observation } => {
                observation.validate()?;
                super::divisions::validate_kind(&kind)?;
                let document_id = self
                    .document
                    .ok_or_else(|| Self::invalid("structural observation outside document"))?
                    .0;
                if self.observations.len() >= MAX_DIVISION_KINDS
                    && !self.observations.contains_key(&kind)
                {
                    return Err(Self::invalid("too many observed division kinds"));
                }
                self.observations.insert(kind.clone(), observation.clone());
                Ok(Some(AdmittedCorpusEvent::DivisionObservation {
                    document_id,
                    kind,
                    observation,
                }))
            }
            CorpusEvent::DivisionStart {
                source_ordinal,
                source_id,
                kind,
                parent_source_ordinal,
                metadata,
            } => {
                if self
                    .last_reader_division
                    .is_some_and(|last| source_ordinal <= last)
                {
                    return Err(Self::invalid(
                        "division source IDs must increase in opening order",
                    ));
                }
                self.last_reader_division = Some(source_ordinal);
                let parent_id = parent_source_ordinal
                    .map(|id| {
                        self.reader_divisions.get(&id).copied().ok_or_else(|| {
                            Self::invalid("division parent must be open in the same document")
                        })
                    })
                    .transpose()?;
                let division = self.open_division(kind, parent_id, metadata, source_id)?;
                self.reader_divisions.insert(source_ordinal, division.id);
                Ok(Some(AdmittedCorpusEvent::DivisionStart(division)))
            }
            CorpusEvent::DivisionEnd { source_ordinal } => {
                let id = self
                    .reader_divisions
                    .remove(&source_ordinal)
                    .ok_or_else(|| Self::invalid("division close has no open identity"))?;
                self.close_division(id)?;
                Ok(None)
            }
            CorpusEvent::Sentence {
                text,
                words,
                enhanced_graph,
                active_labels,
                metadata,
            } => {
                let Some((document_id, document_ordinal)) = self.document else {
                    return Err(Self::invalid("sentence emitted outside a document"));
                };
                if text.trim().is_empty() {
                    return Ok(None);
                }
                let label_bytes = active_labels
                    .iter()
                    .map(|label| label.len() + std::mem::size_of::<String>())
                    .fold(0usize, usize::saturating_add);
                if label_bytes > MAX_STRUCTURAL_CONTEXT_BYTES {
                    return Err(Self::invalid(
                        "active section labels exceed their byte budget",
                    ));
                }
                let structure = Arc::new(StructuralContext {
                    observations: self.observations.clone(),
                    divisions: self
                        .active_divisions
                        .values()
                        .map(|(division, _)| Arc::clone(division))
                        .collect(),
                });
                if let Some(scoped) = &mut self.scope {
                    if self.outcome == DocumentOutcome::RequiresSentences {
                        scoped
                            .clause
                            .validate_structure(&structure)
                            .map_err(|message| {
                                io::Error::new(io::ErrorKind::InvalidData, message)
                            })?;
                    }
                    let verdict = match self.outcome {
                        DocumentOutcome::Decided(verdict) => verdict,
                        DocumentOutcome::RequiresSentences => scoped.clause.evaluate_sentence(
                            &scoped.document_metadata,
                            &metadata,
                            &*active_labels,
                            &structure,
                        ),
                    };
                    if let Verdict::Excluded(reason) = verdict {
                        // Before `SentenceId::new`, so a skipped sentence
                        // consumes no ordinal and direct-input ordinals
                        // compact. That is not a new semantics: the empty-line
                        // skip immediately above has behaved this way since it
                        // was written.
                        scoped.pending.record_excluded_sentence(reason)?;
                        return Ok(None);
                    }
                    scoped.pending.record_admitted_sentence()?;
                }
                let id = SentenceId::new(Self::take(&mut self.next_sentence, "sentence ID")?);
                let next_ordinal = document_ordinal
                    .checked_add(1)
                    .ok_or_else(|| Self::invalid("document sentence ordinal overflow"))?;
                self.document = Some((document_id, next_ordinal));
                Ok(Some(AdmittedCorpusEvent::Sentence(AdmittedSentence {
                    structure,
                    id,
                    document_id,
                    document_sentence_ordinal: document_ordinal,
                    text,
                    words,
                    enhanced_graph,
                    active_labels,
                    metadata,
                })))
            }
            CorpusEvent::ParagraphEnd => {
                let id = self
                    .paragraph
                    .take()
                    .ok_or_else(|| Self::invalid("paragraph ended without an open paragraph"))?;
                self.close_division(id)?;
                Ok(None)
            }
            CorpusEvent::DocumentEnd => {
                if self.paragraph.is_some() {
                    return Err(Self::invalid("document ended with an open paragraph"));
                }
                let Some((id, _)) = self.document.take() else {
                    return Err(Self::invalid("document ended without an open document"));
                };
                while let Some((_, division_id)) = self.headings.pop() {
                    self.close_division(division_id)?;
                }
                if !self.active_divisions.is_empty() {
                    return Err(Self::invalid("document ended with open divisions"));
                }
                if let Some(scoped) = &mut self.scope {
                    let pending = std::mem::take(&mut scoped.pending);
                    scoped.observation.publish(pending)?;
                }
                self.pending_events
                    .push_back(AdmittedCorpusEvent::DocumentEnd { id });
                Ok(None)
            }
        }
    }
}

impl<I> Drop for AdmittedCorpus<I> {
    /// Publishes a partially-read document's tallies.
    ///
    /// A `--limit`ed run stops with the reader inside a document, so the
    /// `DocumentEnd` that would have published it never arrives. Without this
    /// the run would report a universe smaller than the reader actually
    /// observed.
    fn drop(&mut self) {
        if let Some(scoped) = &mut self.scope {
            let pending = std::mem::take(&mut scoped.pending);
            if pending != ScopeCounts::default() {
                scoped.observation.publish_on_drop(pending);
            }
        }
    }
}

pub fn sentence_texts<I>(events: I) -> impl Iterator<Item = io::Result<String>>
where
    I: Iterator<Item = io::Result<AdmittedCorpusEvent>>,
{
    events.filter_map(|event| match event {
        Ok(AdmittedCorpusEvent::Sentence(sentence)) => Some(Ok(sentence.text)),
        Ok(_) => None,
        Err(error) => Some(Err(error)),
    })
}
