//! Binds count tables to served corpora and catalogs, then answers document-metadata queries from those tables.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lingrep_core::corpus::{Metadata, MetadataScalar, MetadataValue, metadata_from_json};
use lingrep_core::scope::{DocumentOutcome, ExclusionReason, ScopeClause, Verdict};
use lingrep_corpus::{
    CountTable, CountTableError, CountTableKey, DocumentMask, FieldValue, Group, format_identity,
};
use serde::{Deserialize, Serialize};

mod documents;

use lingrep_core::spill::{SortedRunCounts, SpillPolicy, TempBudget};

const SPILL_POLICY: SpillPolicy = SpillPolicy {
    max_pending_bytes: 8 * 1024 * 1024,
    max_pending_keys: 100_000,
    merge_fan_in: 16,
};
const MAX_TEMP_BYTES: u64 = 64 * 1024 * 1024 * 1024;

use crate::catalog_index::CatalogIndex;
use crate::corpus_runtime::{CacheReadiness, CorpusRuntime};
use crate::corpus_set::{CorpusName, CorpusSet};
use crate::metadata_labels::KeyLabels;

/// Rows a query returns when it names no limit.
pub const PAGE_ROWS: usize = 100;
const MAX_PAGE_ROWS: usize = 1_000;
const MAX_QUERY_ITEMS: usize = 100_000;
const MAX_MASK_BYTES: usize = 32 * 1024 * 1024;
const MAX_PROJECTED_BYTES: usize = 32 * 1024 * 1024;

#[derive(Default)]
pub struct FrequencyViews {
    pairs: Vec<FrequencyPair>,
    dimensions: HashMap<CorpusName, Arc<[Dimension]>>,
    metadata_keys: BTreeSet<String>,
    labels: KeyLabels,
}

/// Queries cap masks at 32 MiB and source strings in returned projections at
/// 32 MiB; individual groups contain at most 100,000 names and values.
/// Ranking and facet aggregation spill to temporary files.
pub struct FrequencyPair {
    pub corpus: CorpusName,
    pub corpus_display: String,
    pub catalog: String,
    table: CountTable,
    command_inputs: Option<[String; 3]>,
    documents: Arc<documents::Documents>,
    patterns: Vec<PatternSource>,
    workbench: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PatternSource {
    pub name: String,
    /// The definitions named `name`, as written in the catalog.
    pub source: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Dimension {
    pub key: String,
    /// Documents that carry the key.
    pub documents: u64,
    #[serde(flatten)]
    pub kind: DimensionKind,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DimensionKind {
    /// Text values compared with `in` and `not in`, or for list values with
    /// `contains` and `not contains`.
    Categorical { list: bool, values: usize },
    /// Numbers compared with `>=` and `<=`.
    Range { min: f64, max: f64 },
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(tag = "m", rename_all = "snake_case")]
pub enum Condition {
    In {
        #[serde(default)]
        v: Vec<String>,
    },
    Out {
        #[serde(default)]
        v: Vec<String>,
    },
    Range {
        #[serde(default)]
        lo: Option<serde_json::Number>,
        #[serde(default)]
        hi: Option<serde_json::Number>,
    },
}

/// Reasons a frequency query cannot return rows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Unsupported {
    Syntax {
        message: String,
    },
    /// The clause names `@section`; a count table holds counts per document.
    Section,
    /// The clause references sentence metadata or divisions.
    Sentence,
    /// A condition names a key this corpus does not offer.
    UnknownKey {
        key: String,
    },
    /// A value cannot be written in a clause.
    Unwritable {
        key: String,
        value: String,
    },
    /// A condition does not fit its key's kind.
    Shape {
        key: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Count,
    Works,
    Key,
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub pattern: String,
    pub conditions: BTreeMap<String, Condition>,
    /// A clause written as text, used instead of `conditions`.
    pub clause: Option<String>,
    pub sort: Sort,
    pub filter: String,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Kept {
    pub kept: u64,
    pub of: u64,
    /// Documents admitted by the other conditions but excluded here for a missing field.
    pub excluded_missing_field_documents: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Row {
    pub rank: u64,
    pub group: serde_json::Value,
    pub text: String,
    pub count: u64,
    pub works: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Answer {
    pub clause: String,
    pub command: String,
    pub occurrences: u64,
    /// Admitted documents in which the pattern occurs.
    pub works: u64,
    pub documents_admitted: u64,
    pub tokens_admitted: u64,
    /// Entries with a nonzero count, and those the filter keeps.
    pub entries: u64,
    pub matching: u64,
    /// For each condition that writes a predicate, the occurrences under
    /// every condition and under every condition except it.
    pub kept: BTreeMap<String, Kept>,
    pub rows: Vec<Row>,
}

#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("unknown pattern {0:?}")]
    UnknownPattern(String),
    #[error(
        "frequency query exceeds the {resource} limit of {limit}; use `ling table` for larger queries"
    )]
    ResourceLimit { resource: &'static str, limit: u64 },
    #[error(transparent)]
    Table(#[from] CountTableError),
    #[error(transparent)]
    Metadata(#[from] std::io::Error),
}

#[derive(Default)]
struct ProjectionBudget {
    bytes: usize,
    values: usize,
}

impl ProjectionBudget {
    fn charge(&mut self, text: &str) -> Result<(), QueryError> {
        self.bytes = self
            .bytes
            .checked_add(text.len())
            .filter(|bytes| *bytes <= MAX_PROJECTED_BYTES)
            .ok_or(QueryError::ResourceLimit {
                resource: "projected bytes",
                limit: MAX_PROJECTED_BYTES as u64,
            })?;
        if self.values == MAX_QUERY_ITEMS {
            return Err(QueryError::ResourceLimit {
                resource: "projected values",
                limit: MAX_QUERY_ITEMS as u64,
            });
        }
        self.values += 1;
        Ok(())
    }

    fn group(&mut self, group: &Group<'_>) -> Result<(), QueryError> {
        match group {
            Group::Surface(text) | Group::Unit(text) | Group::Term(text) => self.charge(text),
            Group::Key(fields) => {
                for (name, value) in fields {
                    self.charge(name)?;
                    match value {
                        FieldValue::Scalar(text) => self.charge(text)?,
                        FieldValue::List(values) => {
                            for text in values {
                                self.charge(text)?;
                            }
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ValueQuery {
    pub filter: String,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Serialize)]
pub struct ValuePage {
    pub values: Vec<(String, u64)>,
    pub selected: Vec<(String, u64)>,
    pub matching: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct ValueOrder {
    inverted_count: u64,
    value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum ViewOrder {
    Count,
    Works(u64),
    Key(String),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct ViewRow {
    order: ViewOrder,
    rank: u64,
    entry: u32,
    count: u64,
    works: u64,
}

pub enum Outcome {
    Answer(Answer),
    Unsupported(Unsupported),
}

impl FrequencyViews {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub fn pairs(&self) -> impl Iterator<Item = &FrequencyPair> {
        self.pairs.iter()
    }

    #[must_use]
    pub fn find(&self, corpus: &str, catalog: &str) -> Option<&FrequencyPair> {
        self.pairs
            .iter()
            .find(|pair| pair.corpus.as_str() == corpus && pair.catalog == catalog)
    }

    #[must_use]
    pub fn labels(&self) -> &KeyLabels {
        &self.labels
    }

    /// Installs labels and logs each key absent from every corpus with a count table.
    #[must_use]
    pub fn with_labels(self, labels: KeyLabels) -> Self {
        if !self.is_empty() {
            for (key, path) in labels.unused(&self.metadata_keys) {
                tracing::warn!(
                    "--metadata-labels {}: no corpus with a count table carries @document.{key}",
                    path.display()
                );
            }
        }
        Self { labels, ..self }
    }

    #[must_use]
    pub fn dimensions(&self, corpus: &CorpusName) -> &[Dimension] {
        self.dimensions
            .get(corpus)
            .map_or(&[], |dimensions| dimensions)
    }

    /// Binds each table to the served corpus and catalog identified by its
    /// analysis identity and catalog fingerprint, then opens and verifies it
    /// after the referenced corpora are ready. Metadata indexes and sorting use
    /// the system temporary directory with a shared 64 GiB limit during binding.
    ///
    /// # Errors
    ///
    /// A message naming the table for a table that cannot be read, does not
    /// verify, was written by another matcher revision, matches no served
    /// corpus or no catalog, or binds a pair another table binds; and a
    /// message naming the corpus for a corpus that fails to open.
    pub async fn bind(
        tables: &[PathBuf],
        declared: &[(CorpusName, PathBuf, String)],
        corpora: &CorpusSet,
        catalogs: &CatalogIndex,
        pattern_root: &Path,
    ) -> Result<Self, String> {
        if tables.is_empty() {
            return Ok(Self::default());
        }
        let mut identities = Vec::new();
        for (name, path, display) in declared {
            if lingrep_corpus::is_prepared_artifact(path) {
                let artifact = lingrep_corpus::CorpusArtifact::open(path)
                    .map_err(|error| format!("corpus {:?}: {error}", name.as_str()))?;
                identities.push((
                    name.clone(),
                    display.clone(),
                    path.clone(),
                    format_identity(artifact.analysis_identity().as_bytes()),
                ));
            }
        }
        let fingerprints = catalogs
            .keys()
            .map(|key| {
                let source = catalogs.get(key).map_or("", |source| source);
                (
                    key.to_owned(),
                    lingrep_core::fsm::catalog_fingerprint(source),
                )
            })
            .collect::<Vec<_>>();

        let mut bound: Vec<(PathBuf, CountTableKey, usize, String)> = Vec::new();
        for path in tables {
            let named = |message: String| format!("--count-table {}: {message}", path.display());
            let key = CountTable::read_key(path).map_err(|error| named(error.to_string()))?;
            if key.matcher_revision != lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY {
                return Err(named(format!(
                    "written by matcher revision {}, but this server counts with {}; rebuild it \
                     with `ling summarize --write-table`",
                    key.matcher_revision,
                    lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY
                )));
            }
            let corpus_matches = identities
                .iter()
                .enumerate()
                .filter(|(_, (_, _, _, identity))| *identity == key.analysis_identity)
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if corpus_matches.is_empty() {
                return Err(named(format!(
                    "matches no served corpus: it counts analysis identity {}",
                    key.analysis_identity
                )));
            }
            let catalog_matches = fingerprints
                .iter()
                .filter(|(_, fingerprint)| *fingerprint == key.catalog_fingerprint)
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            if catalog_matches.is_empty() {
                return Err(named(format!(
                    "matches no catalog under the pattern root: it counts catalog fingerprint {}",
                    key.catalog_fingerprint
                )));
            }
            for corpus in &corpus_matches {
                for catalog in &catalog_matches {
                    if let Some((other, ..)) = bound
                        .iter()
                        .find(|(_, _, c, k)| c == corpus && k == catalog)
                    {
                        return Err(format!(
                            "--count-table {} and --count-table {} both count catalog {catalog} \
                             over corpus {:?}",
                            other.display(),
                            path.display(),
                            identities[*corpus].0.as_str()
                        ));
                    }
                    bound.push((path.clone(), key.clone(), *corpus, catalog.clone()));
                }
            }
        }

        let mut views = Self::default();
        let mut loaded: HashMap<CorpusName, Arc<CorpusDocuments>> = HashMap::new();
        for (path, key, corpus, catalog) in bound {
            let (name, display, corpus_path, _) = &identities[corpus];
            let documents = if let Some(documents) = loaded.get(name) {
                Arc::clone(documents)
            } else {
                let slot = corpora
                    .resolve(Some(name.as_str()))
                    .ok_or_else(|| format!("corpus {:?} is not served", name.as_str()))?;
                let documents = Arc::new(
                    load_documents(&slot.runtime)
                        .await
                        .map_err(|error| format!("corpus {:?}: {error}", name.as_str()))?,
                );
                views
                    .dimensions
                    .insert(name.clone(), documents.dimensions.clone());
                views.metadata_keys.extend(documents.keys.iter().cloned());
                loaded.insert(name.clone(), Arc::clone(&documents));
                documents
            };
            let named = |message: String| format!("--count-table {}: {message}", path.display());
            let table = CountTable::open(&path, &key).map_err(|error| named(error.to_string()))?;
            table.verify().map_err(|error| named(error.to_string()))?;
            let mut reader = documents
                .store
                .reader()
                .map_err(|error| named(error.to_string()))?;
            reader
                .check_ids(table.document_ids())
                .map_err(|error| named(error.to_string()))?;
            let source = catalogs.get(&catalog).map_or("", |source| source);
            let patterns = table
                .pattern_names()
                .map(|name| PatternSource {
                    name: name.to_owned(),
                    source: definition_source(source, name),
                })
                .collect();
            let workbench = crate::permalink::encode_query_only(source, name.as_str(), None, None)
                .ok()
                .map(|query| format!("./{query}"));
            let command_inputs = command_paths([&path, corpus_path, &pattern_root.join(&catalog)]);
            views.pairs.push(FrequencyPair {
                corpus: name.clone(),
                corpus_display: display.clone(),
                catalog,
                command_inputs,
                table,
                documents: Arc::clone(&documents.store),
                patterns,
                workbench,
            });
        }
        Ok(views)
    }
}

fn command_paths(paths: [&Path; 3]) -> Option<[String; 3]> {
    let paths = paths
        .map(std::path::absolute)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let mut parent = paths[0].parent()?;
    while !paths.iter().all(|path| path.starts_with(parent)) {
        parent = parent.parent()?;
    }
    parent.parent()?;
    let relative = paths
        .iter()
        .map(|path| {
            let path = path.strip_prefix(parent).ok()?;
            if path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
            {
                return None;
            }
            Some(format!("./{}", path.to_str()?))
        })
        .collect::<Option<Vec<_>>>()?;
    relative.try_into().ok()
}

struct CorpusDocuments {
    store: Arc<documents::Documents>,
    keys: BTreeSet<String>,
    dimensions: Arc<[Dimension]>,
}

async fn load_documents(runtime: &CorpusRuntime) -> Result<CorpusDocuments, String> {
    runtime.start_or_observe().await;
    let mut readiness = runtime.readiness();
    loop {
        match &*readiness.borrow_and_update() {
            CacheReadiness::Ready { .. } => break,
            CacheReadiness::Failed { category } => {
                return Err(format!("the corpus failed to open ({category:?})"));
            }
            CacheReadiness::Checking | CacheReadiness::Building { .. } => {}
        }
        readiness
            .changed()
            .await
            .map_err(|_| "the corpus runtime stopped".to_owned())?;
    }
    let ready = runtime
        .ready()
        .await
        .ok_or_else(|| "the corpus is no longer ready".to_owned())?;
    let view = ready
        .provenance()
        .await
        .map_err(|error| error.to_string())?;
    let budget = Arc::new(TempBudget::new(MAX_TEMP_BYTES));
    let mut writer =
        documents::Writer::new(Arc::clone(&budget)).map_err(|error| error.to_string())?;
    let mut next = 0;
    loop {
        let page = view
            .document_metadata_page(next)
            .await
            .map_err(|error| error.to_string())?;
        let Some(last) = page.last() else { break };
        next = last.first_sentence + last.sentence_count;
        for row in page {
            let metadata = match row.metadata_json.as_deref() {
                None => Metadata::default(),
                Some(raw) => serde_json::from_str::<serde_json::Value>(raw)
                    .map_err(|error| error.to_string())
                    .and_then(|value| {
                        metadata_from_json(value, "prepared document metadata")
                            .map_err(|error| error.to_string())
                    })?,
            };
            writer
                .push(row.document_id.get(), &metadata)
                .map_err(|error| error.to_string())?;
        }
    }
    let store = Arc::new(writer.finish().map_err(|error| error.to_string())?);
    let mut reader = store.reader().map_err(|error| error.to_string())?;
    let mut keys = BTreeSet::new();
    let mut key_budget = ProjectionBudget::default();
    let rows = std::iter::from_fn(|| reader.next().transpose()).map(|row| {
        let metadata = row?;
        for (key, _) in &metadata {
            if !keys.contains(key) {
                key_budget.charge(key).map_err(std::io::Error::other)?;
                keys.insert(key.clone());
            }
        }
        Ok(metadata)
    });
    let dimensions = derive_dimensions(rows, budget)
        .map_err(|error| error.to_string())?
        .into();
    Ok(CorpusDocuments {
        store,
        keys,
        dimensions,
    })
}

fn scalar_text(scalar: &MetadataScalar) -> String {
    match scalar {
        MetadataScalar::String(text) | MetadataScalar::Number(text) => text.clone(),
        MetadataScalar::Bool(value) => value.to_string(),
    }
}

fn lookup<'a>(metadata: &'a Metadata, key: &str) -> Option<&'a MetadataValue> {
    metadata
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

fn derive_dimensions(
    documents: impl Iterator<Item = std::io::Result<Metadata>>,
    budget: Arc<TempBudget>,
) -> std::io::Result<Vec<Dimension>> {
    #[derive(Default)]
    struct Seen {
        documents: u64,
        numbers: bool,
        scalars: bool,
        lists: bool,
        other: bool,
        min: f64,
        max: f64,
        values: usize,
    }
    let mut keys: BTreeMap<String, Seen> = BTreeMap::new();
    let mut distinct = SortedRunCounts::with_policy(SPILL_POLICY, budget)?;
    for metadata in documents {
        for (key, value) in &metadata? {
            let seen = keys.entry(key.clone()).or_insert_with(|| Seen {
                numbers: true,
                min: f64::INFINITY,
                max: f64::NEG_INFINITY,
                ..Seen::default()
            });
            seen.documents += 1;
            match value {
                MetadataValue::Scalar(scalar) => {
                    seen.scalars = true;
                    match scalar {
                        MetadataScalar::Number(text) => match text.parse::<f64>() {
                            Ok(number) if number.is_finite() => {
                                seen.min = seen.min.min(number);
                                seen.max = seen.max.max(number);
                            }
                            _ => seen.numbers = false,
                        },
                        _ => seen.numbers = false,
                    }
                    distinct.add((key.clone(), scalar_text(scalar)), 1)?;
                }
                MetadataValue::Scalars(scalars) => {
                    seen.lists = true;
                    seen.numbers = false;
                    for scalar in scalars {
                        distinct.add((key.clone(), scalar_text(scalar)), 1)?;
                    }
                }
                MetadataValue::Opaque(_) => seen.other = true,
            }
        }
    }
    let mut distinct = distinct.into_sorted_drain()?;
    while let Some(((key, _), _)) = distinct.next_entry()? {
        keys.get_mut(&key).expect("observed key").values += 1;
    }
    Ok(keys
        .into_iter()
        .filter(|(key, seen)| !seen.other && writable_key(key) && !(seen.scalars && seen.lists))
        .filter_map(|(key, seen)| {
            let kind = if seen.numbers && seen.scalars {
                DimensionKind::Range {
                    min: seen.min,
                    max: seen.max,
                }
            } else {
                let distinct = seen.values as u64;
                if distinct < 2 || distinct * 2 > seen.documents {
                    return None;
                }
                DimensionKind::Categorical {
                    list: seen.lists,
                    values: seen.values,
                }
            };
            Some(Dimension {
                key,
                documents: seen.documents,
                kind,
            })
        })
        .collect())
}

pub(crate) fn writable_key(key: &str) -> bool {
    ScopeClause::parse(&format!("@document.{key} = \"x\""))
        .is_ok_and(|clause| clause.metadata_keys().len() == 1)
}

fn quoted(key: &str, value: &str) -> Result<String, Unsupported> {
    if value.contains('"') {
        return Err(Unsupported::Unwritable {
            key: key.to_owned(),
            value: value.to_owned(),
        });
    }
    Ok(format!("\"{value}\""))
}

fn predicate(dimension: &Dimension, condition: &Condition) -> Result<Option<String>, Unsupported> {
    let key = &dimension.key;
    let shape = || Unsupported::Shape { key: key.clone() };
    match (&dimension.kind, condition) {
        (DimensionKind::Categorical { list, .. }, Condition::In { v } | Condition::Out { v }) => {
            if v.is_empty() {
                return Ok(None);
            }
            let values = v
                .iter()
                .map(|value| quoted(key, value))
                .collect::<Result<Vec<_>, _>>()?;
            let out = matches!(condition, Condition::Out { .. });
            Ok(Some(match (list, out, values.as_slice()) {
                (false, false, [one]) => format!("@document.{key} = {one}"),
                (false, true, [one]) => format!("@document.{key} != {one}"),
                (false, false, _) => format!("@document.{key} in {{{}}}", values.join(", ")),
                (false, true, _) => format!("@document.{key} not in {{{}}}", values.join(", ")),
                (true, false, [one]) => format!("@document.{key} contains {one}"),
                (true, false, _) => format!(
                    "({})",
                    values
                        .iter()
                        .map(|value| format!("@document.{key} contains {value}"))
                        .collect::<Vec<_>>()
                        .join(" | ")
                ),
                (true, true, _) => values
                    .iter()
                    .map(|value| format!("@document.{key} not contains {value}"))
                    .collect::<Vec<_>>()
                    .join(" & "),
            }))
        }
        (DimensionKind::Range { .. }, Condition::Range { lo, hi }) => {
            let bound = |op: &str, value: &serde_json::Number| {
                let text = value.to_string();
                if text.parse::<f64>().is_ok_and(f64::is_finite) {
                    Ok(format!("@document.{key} {op} {text}"))
                } else {
                    Err(shape())
                }
            };
            let parts = [
                lo.as_ref().map(|lo| bound(">=", lo)),
                hi.as_ref().map(|hi| bound("<=", hi)),
            ]
            .into_iter()
            .flatten()
            .collect::<Result<Vec<_>, _>>()?;
            Ok((!parts.is_empty()).then(|| parts.join(" & ")))
        }
        _ => Err(shape()),
    }
}

fn predicates(
    dimensions: &[Dimension],
    conditions: &BTreeMap<String, Condition>,
) -> Result<Vec<(String, String)>, Unsupported> {
    if let Some(key) = conditions
        .keys()
        .find(|key| !dimensions.iter().any(|dimension| &dimension.key == *key))
    {
        return Err(Unsupported::UnknownKey { key: key.clone() });
    }
    let mut written = Vec::new();
    for dimension in dimensions {
        if let Some(condition) = conditions.get(&dimension.key)
            && let Some(predicate) = predicate(dimension, condition)?
        {
            written.push((dimension.key.clone(), predicate));
        }
    }
    Ok(written)
}

fn document_clause(source: &str) -> Result<Option<ScopeClause>, Unsupported> {
    if source.trim().is_empty() {
        return Ok(None);
    }
    let clause = ScopeClause::parse(source).map_err(|error| Unsupported::Syntax {
        message: error.message,
    })?;
    if clause.names_sections() {
        return Err(Unsupported::Section);
    }
    if clause.requires_sentence_values() {
        return Err(Unsupported::Sentence);
    }
    Ok(Some(clause))
}

fn shell_word(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn group_value(group: &Group<'_>) -> serde_json::Value {
    match group {
        Group::Surface(value) => serde_json::json!({"kind": "surface", "value": value}),
        Group::Unit(value) => serde_json::json!({"kind": "unit", "value": value}),
        Group::Term(value) => serde_json::json!({"kind": "term", "value": value}),
        Group::Key(fields) => serde_json::json!({
            "kind": "key",
            "fields": fields.iter().map(|(name, value)| match value {
                FieldValue::Scalar(value) =>
                    serde_json::json!({"kind": "scalar", "name": name, "value": value}),
                FieldValue::List(values) =>
                    serde_json::json!({"kind": "list", "name": name, "values": values}),
            }).collect::<Vec<_>>(),
        }),
    }
}

fn group_text(group: &Group<'_>) -> String {
    match group {
        Group::Surface(value) | Group::Unit(value) | Group::Term(value) => (*value).to_owned(),
        Group::Key(fields) => fields
            .iter()
            .map(|(_, value)| match value {
                FieldValue::Scalar(value) => (*value).to_owned(),
                FieldValue::List(values) => values.join(","),
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

fn definition_source(source: &str, name: &str) -> Option<String> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_lingrep::LANGUAGE.into())
        .ok()?;
    let text = source.replace('\r', "\n");
    let tree = parser.parse(&text, None)?;
    let mut found = Vec::new();
    let mut run_start = None;
    let root = tree.root_node();
    let mut cursor = root.walk();
    for node in root.named_children(&mut cursor) {
        match node.kind() {
            "comment" => {}
            "metadata_entry" | "key_entry" | "skip_entry" => {
                run_start.get_or_insert(node.start_byte());
            }
            "pattern_definition" => {
                let start = run_start.take().unwrap_or(node.start_byte());
                if node
                    .child_by_field_name("name")
                    .and_then(|named| text.get(named.byte_range()))
                    == Some(name)
                    && let Some(definition) = text.get(start..node.end_byte())
                {
                    found.push(definition.trim_end().to_owned());
                }
            }
            _ => run_start = None,
        }
    }
    (!found.is_empty()).then(|| found.join("\n\n"))
}

impl FrequencyPair {
    fn admit_query(&self) -> Result<(), QueryError> {
        if self.table.document_ids().len() > MAX_MASK_BYTES / 4 {
            return Err(QueryError::ResourceLimit {
                resource: "document mask bytes",
                limit: MAX_MASK_BYTES as u64,
            });
        }
        if self.patterns.len() > MAX_QUERY_ITEMS {
            return Err(QueryError::ResourceLimit {
                resource: "patterns",
                limit: MAX_QUERY_ITEMS as u64,
            });
        }
        Ok(())
    }

    #[must_use]
    pub fn patterns(&self) -> &[PatternSource] {
        &self.patterns
    }

    #[must_use]
    pub fn workbench(&self) -> Option<&str> {
        self.workbench.as_deref()
    }

    fn pattern_index(&self, name: &str) -> Result<usize, QueryError> {
        self.patterns
            .iter()
            .position(|pattern| pattern.name == name)
            .ok_or_else(|| QueryError::UnknownPattern(name.to_owned()))
    }

    fn admitted(&self, clause: Option<&ScopeClause>) -> Result<Vec<bool>, QueryError> {
        let mut reader = self.documents.reader()?;
        self.table
            .document_ids()
            .iter()
            .map(|id| {
                let metadata = reader.document(*id)?;
                Ok(clause.is_none_or(|clause| {
                    clause.evaluate_document(&metadata)
                        == DocumentOutcome::Decided(Verdict::Admitted)
                }))
            })
            .collect()
    }

    fn mask(&self, admitted: &[bool]) -> DocumentMask {
        let ids = self.table.document_ids();
        self.table.mask(|id| {
            ids.binary_search(&id)
                .is_ok_and(|index| admitted.get(index).copied() == Some(true))
        })
    }

    fn command(&self, clause: &str) -> String {
        let Some([table, corpus, catalog]) = &self.command_inputs else {
            return String::new();
        };
        let mut command = format!("ling table {} --top {PAGE_ROWS}", shell_word(table));
        if !clause.is_empty() {
            command.push_str(" --where ");
            command.push_str(&shell_word(clause));
        }
        command.push_str(&format!(
            " --corpus {} --patterns {}",
            shell_word(corpus),
            shell_word(catalog)
        ));
        command
    }

    /// Answers `query` over `dimensions`, the dimensions of this pair's
    /// corpus.
    ///
    /// # Errors
    ///
    /// An unknown pattern, a table that fails to read, or a query above the
    /// resource limits.
    pub fn answer(&self, dimensions: &[Dimension], query: &Query) -> Result<Outcome, QueryError> {
        let pattern = self.pattern_index(&query.pattern)?;
        let (clause_text, written) = match &query.clause {
            Some(clause) => (clause.trim().to_owned(), Vec::new()),
            None => match predicates(dimensions, &query.conditions) {
                Ok(written) => (
                    written
                        .iter()
                        .map(|(_, predicate)| predicate.as_str())
                        .collect::<Vec<_>>()
                        .join(" & "),
                    written,
                ),
                Err(unsupported) => return Ok(Outcome::Unsupported(unsupported)),
            },
        };
        let clause = match document_clause(&clause_text) {
            Ok(clause) => clause,
            Err(unsupported) => return Ok(Outcome::Unsupported(unsupported)),
        };
        self.admit_query()?;
        let mask = self.mask(&self.admitted(clause.as_ref())?);
        let totals = self.table.pattern_totals(&mask)?;
        let total = totals[pattern];
        let mut kept = BTreeMap::new();
        for (index, (key, _)) in written.iter().enumerate() {
            let others = written
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, (_, predicate))| predicate.as_str())
                .collect::<Vec<_>>()
                .join(" & ");
            let others = document_clause(&others).unwrap_or(None);
            let admitted_others = self.admitted(others.as_ref())?;
            let mut excluded_missing_field_documents = 0;
            if matches!(query.conditions.get(key), Some(Condition::Out { .. })) {
                let predicate = document_clause(&written[index].1).expect("written predicate");
                let mut reader = self.documents.reader()?;
                for (id, admitted) in self.table.document_ids().iter().zip(&admitted_others) {
                    if *admitted {
                        let metadata = reader.document(*id)?;
                        if predicate.as_ref().is_some_and(|predicate| {
                            predicate.evaluate_document(&metadata)
                                == DocumentOutcome::Decided(Verdict::Excluded(
                                    ExclusionReason::MissingField,
                                ))
                        }) {
                            excluded_missing_field_documents += 1;
                        }
                    }
                }
            }
            let others = self.mask(&admitted_others);
            let of = self.table.pattern_totals(&others)?[pattern].occurrences;
            kept.insert(
                key.clone(),
                Kept {
                    kept: total.occurrences,
                    of,
                    excluded_missing_field_documents,
                },
            );
        }
        let budget = Arc::new(TempBudget::new(MAX_TEMP_BYTES));
        let entries =
            self.table
                .ranked_pattern_rows(pattern, &mask, SPILL_POLICY, Arc::clone(&budget))?;
        let mut reordered = (query.sort != Sort::Count)
            .then(|| SortedRunCounts::<ViewRow>::with_policy(SPILL_POLICY, budget))
            .transpose()?;
        let limit = if query.limit == 0 {
            PAGE_ROWS
        } else {
            query.limit.min(MAX_PAGE_ROWS)
        };
        let mut page = Vec::with_capacity(limit);
        let mut entry_count = 0;
        let mut matching = 0;
        for entry in entries {
            let entry = entry?;
            entry_count += 1;
            let text = if query.sort == Sort::Key || !query.filter.is_empty() {
                let group = self.table.bounded_group(entry.entry, MAX_QUERY_ITEMS)?;
                ProjectionBudget::default().group(&group)?;
                group_text(&group)
            } else {
                String::new()
            };
            if !query.filter.is_empty() && !text.contains(&query.filter) {
                continue;
            }
            matching += 1;
            let row = ViewRow {
                order: match query.sort {
                    Sort::Count => ViewOrder::Count,
                    Sort::Works => ViewOrder::Works(u64::MAX - entry.works),
                    Sort::Key => ViewOrder::Key(text),
                },
                rank: entry_count,
                entry: entry.entry,
                count: entry.count,
                works: entry.works,
            };
            if let Some(reordered) = &mut reordered {
                reordered.add(row, 1)?;
            } else if matching > query.offset as u64 && page.len() < limit {
                page.push(row);
            }
        }
        if let Some(reordered) = reordered {
            let mut rows = reordered.into_sorted_drain()?;
            let mut offset = query.offset;
            while let Some((mut row, _)) = rows.next_entry()? {
                if offset > 0 {
                    offset -= 1;
                    continue;
                }
                row.order = ViewOrder::Count;
                page.push(row);
                if page.len() == limit {
                    break;
                }
            }
        }
        let mut projection = ProjectionBudget::default();
        let rows = page
            .into_iter()
            .map(|row| {
                let group = self.table.bounded_group(row.entry, MAX_QUERY_ITEMS)?;
                projection.group(&group)?;
                Ok(Row {
                    rank: row.rank,
                    group: group_value(&group),
                    text: group_text(&group),
                    count: row.count,
                    works: row.works,
                })
            })
            .collect::<Result<_, QueryError>>()?;
        let admission = self.table.top(&mask, 0)?;
        Ok(Outcome::Answer(Answer {
            command: self.command(&clause_text),
            clause: clause_text,
            occurrences: total.occurrences,
            works: total.works,
            documents_admitted: admission.documents_admitted,
            tokens_admitted: admission.tokens_admitted,
            entries: entry_count,
            matching,
            kept,
            rows,
        }))
    }

    /// Pages values by descending occurrences then value, including zero counts.
    /// Counts apply every condition except the active key's; selected values are
    /// returned separately even when they fall outside the page or text filter.
    ///
    /// # Errors
    ///
    /// An unknown pattern, a table that fails to read, or a query above the
    /// resource limits.
    pub fn values(
        &self,
        dimensions: &[Dimension],
        pattern: &str,
        key: &str,
        conditions: &BTreeMap<String, Condition>,
        query: &ValueQuery,
    ) -> Result<Result<ValuePage, Unsupported>, QueryError> {
        let pattern = self.pattern_index(pattern)?;
        if !dimensions.iter().any(|dimension| dimension.key == key) {
            return Ok(Err(Unsupported::UnknownKey {
                key: key.to_owned(),
            }));
        }
        self.admit_query()?;
        let mut others = conditions.clone();
        others.remove(key);
        let clause = match predicates(dimensions, &others).and_then(|written| {
            document_clause(
                &written
                    .iter()
                    .map(|(_, predicate)| predicate.as_str())
                    .collect::<Vec<_>>()
                    .join(" & "),
            )
        }) {
            Ok(clause) => clause,
            Err(unsupported) => return Ok(Err(unsupported)),
        };
        let admitted = self.admitted(clause.as_ref())?;
        let counts = self.table.pattern_document_counts_iter(pattern)?;
        let budget = Arc::new(TempBudget::new(MAX_TEMP_BYTES));
        let mut values = SortedRunCounts::<String>::with_policy(SPILL_POLICY, Arc::clone(&budget))?;
        let mut reader = self.documents.reader()?;
        for ((id, count), admitted) in self.table.document_ids().iter().zip(counts).zip(&admitted) {
            let count = count?;
            let metadata = reader.document(*id)?;
            let scalars = match lookup(&metadata, key) {
                Some(MetadataValue::Scalar(scalar)) => std::slice::from_ref(scalar),
                Some(MetadataValue::Scalars(scalars)) => scalars,
                _ => continue,
            };
            let distinct = scalars.iter().map(scalar_text).collect::<BTreeSet<_>>();
            for value in distinct {
                if value.contains('"') {
                    continue;
                }
                values.add(
                    value,
                    usize::try_from(if *admitted { count } else { 0 })
                        .map_err(std::io::Error::other)?,
                )?;
            }
        }
        let mut selected = match conditions.get(key) {
            Some(Condition::In { v } | Condition::Out { v }) => v
                .iter()
                .map(|value| (value.clone(), 0))
                .collect::<BTreeMap<_, _>>(),
            _ => BTreeMap::new(),
        };
        let mut values = values.into_sorted_drain()?;
        let mut ordered = SortedRunCounts::<ValueOrder>::with_policy(SPILL_POLICY, budget)?;
        let mut matching = 0;
        while let Some((value, count)) = values.next_entry()? {
            if let Some(selected_count) = selected.get_mut(&value) {
                *selected_count = count as u64;
            }
            if !value.contains(&query.filter) {
                continue;
            }
            matching += 1;
            ordered.add(
                ValueOrder {
                    inverted_count: u64::MAX - count as u64,
                    value,
                },
                1,
            )?;
        }
        let mut rows = ordered.into_sorted_drain()?;
        let limit = if query.limit == 0 {
            PAGE_ROWS
        } else {
            query.limit.min(MAX_PAGE_ROWS)
        };
        let mut values = Vec::with_capacity(limit);
        let mut offset = query.offset;
        let mut projection = ProjectionBudget::default();
        for value in selected.keys() {
            projection.charge(value)?;
        }
        while let Some((row, _)) = rows.next_entry()? {
            if offset > 0 {
                offset -= 1;
                continue;
            }
            projection.charge(&row.value)?;
            values.push((row.value, u64::MAX - row.inverted_count));
            if values.len() == limit {
                break;
            }
        }
        Ok(Ok(ValuePage {
            values,
            selected: selected.into_iter().collect(),
            matching,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_pair(
        values: Vec<String>,
        schema: lingrep_corpus::GroupSchema,
        metadata: Metadata,
    ) -> (tempfile::TempDir, FrequencyPair) {
        table_pair_documents(values, schema, vec![metadata])
    }

    fn table_pair_documents(
        values: Vec<String>,
        schema: lingrep_corpus::GroupSchema,
        metadata: Vec<Metadata>,
    ) -> (tempfile::TempDir, FrequencyPair) {
        use lingrep_corpus::{CountTableWriter, DocumentTotals};
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("table");
        let key = CountTableKey {
            analysis_identity: "analysis".into(),
            catalog_fingerprint: "catalog".into(),
            matcher_revision: "matcher".into(),
        };
        let mut writer = CountTableWriter::create(
            &path,
            key.clone(),
            vec!["p".into()],
            vec![schema],
            &values,
            &(0..metadata.len())
                .map(|index| DocumentTotals {
                    document_id: index as u64,
                    sentences: 1,
                    tokens: 1,
                })
                .collect::<Vec<_>>(),
        )
        .expect("writer");
        for index in 0..values.len() {
            writer.begin_entry(0, 0, &[index as u32]).expect("entry");
            for document in 0..metadata.len() {
                writer.add_pair(document as u32, 1).expect("pair");
            }
        }
        writer
            .finish(&lingrep_core::execution::CancellationFlag::new())
            .expect("finish")
            .publish()
            .expect("publish");
        let table = CountTable::open(&path, &key).expect("table");
        (
            directory,
            FrequencyPair {
                corpus: CorpusName::parse("default").expect("corpus"),
                corpus_display: "default".into(),
                catalog: "p.lx".into(),
                table,
                command_inputs: Some([
                    "table.lingtable".into(),
                    "corpus.lingcorpus".into(),
                    "catalog.lx".into(),
                ]),
                documents: {
                    let mut writer =
                        documents::Writer::new(Arc::new(TempBudget::new(MAX_TEMP_BYTES)))
                            .expect("metadata writer");
                    for (id, metadata) in metadata.iter().enumerate() {
                        writer.push(id as u64, metadata).expect("metadata");
                    }
                    Arc::new(writer.finish().expect("metadata store"))
                },
                patterns: vec![PatternSource {
                    name: "p".into(),
                    source: None,
                }],
                workbench: None,
            },
        )
    }

    fn surface_schema() -> lingrep_corpus::GroupSchema {
        lingrep_corpus::GroupSchema {
            kind: lingrep_corpus::GroupKind::Surface,
            names: Vec::new(),
        }
    }

    #[test]
    fn one_row_pages_accept_large_entry_tables() {
        for entries in [100_000, 100_001] {
            let (_directory, pair) = table_pair(
                (0..entries).map(|n| format!("v{n:06}")).collect(),
                surface_schema(),
                Box::new([]),
            );
            let result = pair.answer(
                &[],
                &Query {
                    pattern: "p".into(),
                    limit: 1,
                    ..Query::default()
                },
            );
            let Ok(Outcome::Answer(answer)) = result else {
                panic!("admitted table failed");
            };
            assert_eq!(answer.rows.len(), 1);
            assert_eq!(answer.entries, entries);
            assert_eq!(answer.occurrences, entries);
        }
    }

    #[test]
    fn individual_row_payloads_are_bounded_before_projection() {
        for (length, resource) in [
            (32 * 1024 * 1024 + 1, "projected bytes"),
            (64 * 1024 * 1024 + 1, "projected bytes"),
        ] {
            let (_directory, pair) =
                table_pair(vec!["x".repeat(length)], surface_schema(), Box::new([]));
            assert!(
                matches!(pair.answer(&[], &Query { pattern: "p".into(), limit: 1, ..Query::default() }), Err(error) if error.to_string().contains(resource))
            );
        }
        let (_directory, pair) = table_pair(
            vec!["x".into()],
            lingrep_corpus::GroupSchema {
                kind: lingrep_corpus::GroupKind::Key,
                names: vec!["n".repeat(32 * 1024 * 1024 + 1)],
            },
            Box::new([]),
        );
        assert!(
            matches!(pair.answer(&[], &Query { pattern: "p".into(), limit: 1, ..Query::default() }), Err(error) if error.to_string().contains("projected bytes"))
        );
    }

    #[test]
    fn unreturned_large_groups_do_not_block_count_or_works_pages() {
        let (_directory, pair) = table_pair(
            vec!["a".into(), "z".repeat(MAX_PROJECTED_BYTES + 1)],
            surface_schema(),
            Box::new([]),
        );
        for sort in [Sort::Count, Sort::Works] {
            let Outcome::Answer(answer) = pair
                .answer(
                    &[],
                    &Query {
                        pattern: "p".into(),
                        sort,
                        limit: 1,
                        ..Query::default()
                    },
                )
                .expect("query")
            else {
                panic!("supported")
            };
            assert_eq!(answer.rows[0].text, "a");
            assert_eq!(answer.entries, 2);
        }
    }

    #[test]
    fn facet_pages_and_search_accept_large_vocabularies_and_repeated_values() {
        let dimension = categorical("genre", true);
        let documents = (0..10_001)
            .map(|n| {
                metadata(&[(
                    "genre",
                    serde_json::json!([format!("v{n:05}"), format!("v{n:05}")]),
                )])
            })
            .collect();
        let (_directory, pair) =
            table_pair_documents(vec!["x".into()], surface_schema(), documents);
        let result = pair
            .values(
                std::slice::from_ref(&dimension),
                "p",
                "genre",
                &BTreeMap::new(),
                &ValueQuery {
                    offset: 10_000,
                    limit: 1,
                    ..ValueQuery::default()
                },
            )
            .expect("query")
            .expect("supported");
        assert_eq!(result.matching, 10_001);
        assert_eq!(result.values, [("v10000".into(), 1)]);
        let conditions = [(
            "genre".into(),
            Condition::In {
                v: vec!["v00000".into()],
            },
        )]
        .into_iter()
        .collect();
        let filtered = pair
            .values(
                &[dimension],
                "p",
                "genre",
                &conditions,
                &ValueQuery {
                    filter: "v10000".into(),
                    ..ValueQuery::default()
                },
            )
            .expect("query")
            .expect("supported");
        assert_eq!(filtered.values, result.values);
        assert_eq!(filtered.matching, 1);
        assert_eq!(filtered.selected, [("v00000".into(), 1)]);
        let documents = (0..513)
            .map(|_| metadata(&[("genre", serde_json::json!("x".repeat(64 * 1024)))]))
            .collect();
        let (_directory, pair) =
            table_pair_documents(vec!["x".into()], surface_schema(), documents);
        let repeated = pair
            .values(
                &[categorical("genre", false)],
                "p",
                "genre",
                &BTreeMap::new(),
                &ValueQuery::default(),
            )
            .expect("query")
            .expect("supported");
        assert_eq!(repeated.values, [("x".repeat(64 * 1024), 513)]);
    }

    fn categorical(key: &str, list: bool) -> Dimension {
        Dimension {
            key: key.to_owned(),
            documents: 10,
            kind: DimensionKind::Categorical { list, values: 3 },
        }
    }

    fn values(v: &[&str]) -> Vec<String> {
        v.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn each_condition_writes_the_predicate_ling_reads() {
        let range = Dimension {
            key: "year".to_owned(),
            documents: 10,
            kind: DimensionKind::Range {
                min: 1900.0,
                max: 2000.0,
            },
        };
        let cases = [
            (
                categorical("a", false),
                Condition::In { v: values(&["x"]) },
                Some("@document.a = \"x\""),
            ),
            (
                categorical("a", false),
                Condition::Out { v: values(&["x"]) },
                Some("@document.a != \"x\""),
            ),
            (
                categorical("a", false),
                Condition::In {
                    v: values(&["x", "y z"]),
                },
                Some("@document.a in {\"x\", \"y z\"}"),
            ),
            (
                categorical("a", false),
                Condition::Out {
                    v: values(&["x", "y"]),
                },
                Some("@document.a not in {\"x\", \"y\"}"),
            ),
            (
                categorical("t", true),
                Condition::In { v: values(&["x"]) },
                Some("@document.t contains \"x\""),
            ),
            (
                categorical("t", true),
                Condition::In {
                    v: values(&["x", "y"]),
                },
                Some("(@document.t contains \"x\" | @document.t contains \"y\")"),
            ),
            (
                categorical("t", true),
                Condition::Out {
                    v: values(&["x", "y"]),
                },
                Some("@document.t not contains \"x\" & @document.t not contains \"y\""),
            ),
            (
                categorical("a", false),
                Condition::In { v: Vec::new() },
                None,
            ),
            (range.clone(), Condition::Range { lo: None, hi: None }, None),
            (
                range.clone(),
                Condition::Range {
                    lo: Some(1950.into()),
                    hi: None,
                },
                Some("@document.year >= 1950"),
            ),
            (
                range,
                Condition::Range {
                    lo: Some(1950.into()),
                    hi: Some(1960.into()),
                },
                Some("@document.year >= 1950 & @document.year <= 1960"),
            ),
        ];
        for (dimension, condition, expected) in cases {
            let written = predicate(&dimension, &condition).expect("writable");
            assert_eq!(written.as_deref(), expected, "{condition:?}");
            if let Some(written) = written {
                document_clause(&written)
                    .expect("parses")
                    .expect("a clause");
            }
        }
    }

    #[test]
    fn a_value_with_a_double_quote_or_a_mismatched_condition_is_unsupported() {
        assert_eq!(
            predicate(
                &categorical("a", false),
                &Condition::In {
                    v: values(&["say \"hi\""])
                }
            ),
            Err(Unsupported::Unwritable {
                key: "a".into(),
                value: "say \"hi\"".into()
            })
        );
        assert_eq!(
            predicate(
                &categorical("a", false),
                &Condition::Range { lo: None, hi: None }
            ),
            Err(Unsupported::Shape { key: "a".into() })
        );
        assert_eq!(
            predicates(
                &[categorical("a", false)],
                &BTreeMap::from([("b".to_owned(), Condition::In { v: Vec::new() })])
            ),
            Err(Unsupported::UnknownKey { key: "b".into() })
        );
    }

    #[test]
    fn section_and_sentence_clauses_are_unsupported() {
        assert_eq!(
            document_clause("@section = methods").err(),
            Some(Unsupported::Section)
        );
        assert_eq!(
            document_clause("@sentence.speaker = A").err(),
            Some(Unsupported::Sentence)
        );
        assert!(matches!(
            document_clause("@document.a = "),
            Err(Unsupported::Syntax { .. })
        ));
        assert!(document_clause("  ").expect("empty").is_none());
    }

    fn metadata(fields: &[(&str, serde_json::Value)]) -> Metadata {
        metadata_from_json(
            serde_json::Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), value.clone()))
                    .collect(),
            ),
            "test",
        )
        .expect("metadata")
    }

    #[test]
    fn dimensions_are_ranges_shared_categories_and_lists_by_key() {
        let documents = (0..6)
            .map(|index| {
                metadata(&[
                    ("title", serde_json::json!(format!("t{index}"))),
                    ("year", serde_json::json!(1990 + index)),
                    (
                        "style",
                        serde_json::json!(if index % 2 == 0 { "old" } else { "new" }),
                    ),
                    (
                        "tags",
                        serde_json::json!(["a", if index < 3 { "b" } else { "c" }]),
                    ),
                    ("release", serde_json::json!("r1")),
                ])
            })
            .collect::<Vec<_>>();
        let dimensions = derive_dimensions(
            documents.into_iter().map(Ok),
            Arc::new(TempBudget::new(MAX_TEMP_BYTES)),
        )
        .expect("dimensions");
        let shapes = dimensions
            .iter()
            .map(|dimension| {
                (
                    dimension.key.as_str(),
                    serde_json::to_value(&dimension.kind).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            shapes,
            [
                (
                    "style",
                    serde_json::json!({"kind": "categorical", "list": false, "values": 2})
                ),
                (
                    "tags",
                    serde_json::json!({"kind": "categorical", "list": true, "values": 3})
                ),
                (
                    "year",
                    serde_json::json!({"kind": "range", "min": 1990.0, "max": 1995.0})
                ),
            ]
        );
    }

    #[test]
    fn definition_source_is_the_named_definition_with_its_annotations() {
        let source =
            "fragment f := [text=a].\n\nkey: [text]\npattern p := @f [].\npattern q := [].\n";
        assert_eq!(
            definition_source(source, "p").as_deref(),
            Some("key: [text]\npattern p := @f [].")
        );
        assert_eq!(
            definition_source(source, "q").as_deref(),
            Some("pattern q := [].")
        );
        assert_eq!(definition_source(source, "absent"), None);
    }

    #[test]
    #[cfg(unix)]
    fn command_paths_hide_inputs_outside_the_shared_directory() {
        for paths in [
            [
                "/srv/table",
                "/home/alice/private/corpus",
                "/nix/store/catalog",
            ],
            [
                "/srv/results/../../home/alice/private/table",
                "/srv/results/corpus",
                "/srv/results/catalog",
            ],
        ] {
            assert!(command_paths(paths.map(Path::new)).is_none());
        }
        use std::os::unix::ffi::OsStrExt;
        let invalid = Path::new(std::ffi::OsStr::from_bytes(b"/srv/\xff"));
        assert!(
            command_paths([invalid, Path::new("/srv/corpus"), Path::new("/srv/catalog")]).is_none()
        );
    }

    #[test]
    fn the_command_quotes_the_clause_for_a_shell() {
        assert_eq!(
            shell_word("@document.a = \"it's\""),
            "'@document.a = \"it'\\''s\"'"
        );
    }
}
