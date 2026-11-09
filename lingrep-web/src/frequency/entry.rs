//! The entry level of the frequency view: one entry's counts per value of
//! each condition key, and its occurrences matched from the corpus.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use lingrep_core::analysis::VibratoSession;
use lingrep_core::execution::{
    AnalysisBinding, CancellationFlag, ExecutionError, ExecutionLimits, MatchSink, PolicySelection,
    RunOutcome, SentenceAdmission, SentenceLocation, SinkControl, execute_analyzed,
};
use lingrep_core::fsm::{
    BoundCatalog, CatalogPolicy, CompileLimits, GroupBy, LexicalProjectedKeyValue, Match,
    PatternSource, PublishedRecord, RecordView, compile_catalog_pattern,
};
use lingrep_core::token_match::{AttributeKey, TokenSequence};
use lingrep_corpus::{DocumentMask, FieldValue, Group, ValueCount};
use serde::Serialize;

use super::{
    Condition, Dimension, DimensionKind, FrequencyPair, MAX_MASK_BYTES, MAX_QUERY_ITEMS,
    ProjectionBudget, QueryError, Unsupported, document_clause, predicates, shell_word,
};
use crate::corpus_runtime::ReadyCorpus;

/// Lines a request returns when it names no limit, and the most it may name.
const LINE_PAGE: usize = 50;
const MAX_LINE_PAGE: usize = 200;
/// Characters of context kept on each side of a line's match.
const CONTEXT_CHARS: usize = 40;
/// A categorical chart lists every value up to this many, and otherwise the
/// most frequent values plus the chosen ones.
const LISTED_VALUES: usize = 24;
/// A range key whose values are integers spanning at most this many gets one
/// column per integer; otherwise its span is cut into `RANGE_BINS` columns.
const INTEGER_COLUMNS: f64 = 160.0;
const RANGE_BINS: usize = 40;

/// The served catalog bound to one corpus once per pattern, publishing only
/// that pattern, for matching documents' sentences.
pub struct EntryMatcher {
    patterns: HashMap<String, Arc<PatternCatalog>>,
    ready: ReadyCorpus,
    provenance: Arc<lingrep_corpus::ProvenanceView>,
    runtime: tokio::runtime::Handle,
}

struct PatternCatalog {
    bound: BoundCatalog,
    division_kinds: Vec<compact_str::CompactString>,
}

impl EntryMatcher {
    /// Compiles `source` for each of `patterns` and binds it to the corpus's
    /// token schema; a catalog that compiles whole for one pattern, as one
    /// with a policy block does, is compiled and bound once for all of them.
    /// Must be called inside a Tokio runtime, which later answers lookaround
    /// division queries.
    ///
    /// # Errors
    ///
    /// A catalog that does not compile or does not bind.
    pub fn new<'p>(
        name: &str,
        source: &str,
        patterns: impl IntoIterator<Item = &'p str>,
        ready: ReadyCorpus,
        provenance: Arc<lingrep_corpus::ProvenanceView>,
    ) -> Result<Self, String> {
        let schema = ready
            .artifact
            .token_schema()
            .map_err(|error| error.to_string())?;
        let text = AttributeKey::new("text").expect("text is a canonical attribute key");
        let patterns = compile_each(patterns, |pattern| {
            let output = compile_catalog_pattern(
                PatternSource::named(name, source),
                CompileLimits::default(),
                pattern,
            )
            .map_err(|error| error.to_string())?;
            let bound = output
                .matcher
                .bind_with_projection(
                    &schema,
                    output
                        .matcher
                        .execution_attributes()
                        .into_iter()
                        .chain(std::iter::once(text.clone())),
                )
                .map_err(|error| error.to_string())?;
            Ok((
                PatternCatalog {
                    division_kinds: output.matcher.lookaround_division_kinds(),
                    bound,
                },
                published_names(&output.matcher),
            ))
        })?;
        Ok(Self {
            patterns,
            ready,
            provenance,
            runtime: tokio::runtime::Handle::current(),
        })
    }

    /// Matches the sentences `sentences`, ascending, and passes each
    /// published record to `sink`, in canonical order.
    fn run(
        &self,
        catalog: &PatternCatalog,
        cancel: &CancellationFlag,
        sentences: Vec<u64>,
        sink: &mut EntrySink<'_>,
    ) -> Result<(), ExecutionError> {
        let Some(first) = sentences.first().copied() else {
            return Ok(());
        };
        let artifact = &self.ready.artifact;
        let failed = |error: lingrep_corpus::CorpusError| ExecutionError::Source {
            ordinal: first,
            message: error.to_string(),
        };
        let mut source = match catalog.bound.sentence_filter() {
            Some(filter) => artifact.filtered_sentence_source(catalog.bound.projection(), filter),
            None => artifact.bound_sentence_source(catalog.bound.projection()),
        }
        .map_err(failed)?;
        if !catalog.division_kinds.is_empty() {
            source = artifact
                .with_shared_lookaround_divisions(
                    source,
                    catalog.division_kinds.clone(),
                    Arc::clone(&self.provenance),
                    self.runtime.clone(),
                )
                .map_err(failed)?;
        }
        let source = source.with_ordinals(sentences).map_err(failed)?;
        let schema = artifact.token_schema().map_err(failed)?;
        let mut progress = |_, _: &EntrySink<'_>| {};
        let mut observer = |_: lingrep_core::execution::TokenizedBatchObservation| Ok(());
        let outcome = execute_analyzed::<_, VibratoSession, _, _, _>(
            source,
            AnalysisBinding::Prepared(schema),
            &catalog.bound,
            &self.ready.pool,
            &ExecutionLimits::default(),
            1_024,
            cancel,
            sink,
            &mut progress,
            &mut observer,
            PolicySelection::Apply,
            &SentenceAdmission::All,
        )?;
        match outcome {
            RunOutcome::Completed(_) => Ok(()),
            RunOutcome::Cancelled(_) => Err(ExecutionError::Sink("matching was cancelled".into())),
        }
    }
}

/// Each of `patterns` with a catalog that publishes it: the catalog
/// `compile` returns for it, with the names that catalog publishes, or an
/// earlier one that publishes it too.
fn compile_each<'p, T>(
    patterns: impl IntoIterator<Item = &'p str>,
    mut compile: impl FnMut(&str) -> Result<(T, Vec<String>), String>,
) -> Result<HashMap<String, Arc<T>>, String> {
    let mut publishing = HashMap::new();
    let mut compiled = HashMap::new();
    for pattern in patterns {
        let catalog = if let Some(catalog) = publishing.get(pattern) {
            Arc::clone(catalog)
        } else {
            let (catalog, published) = compile(pattern)?;
            let catalog = Arc::new(catalog);
            for name in published {
                publishing
                    .entry(name)
                    .or_insert_with(|| Arc::clone(&catalog));
            }
            catalog
        };
        compiled.insert(pattern.to_owned(), catalog);
    }
    Ok(compiled)
}

/// The names `catalog`'s records publish under: its patterns' and lexicons'
/// `output_name`, else their own.
fn published_names(catalog: &lingrep_core::fsm::CompiledCatalog) -> Vec<String> {
    let policy = catalog.catalog_policy();
    catalog
        .patterns()
        .keys()
        .map(compact_str::CompactString::as_str)
        .chain(
            catalog
                .lexicons()
                .iter()
                .map(lingrep_core::fsm::CompiledLexiconDefinition::name),
        )
        .map(|name| policy.published_name(name).to_owned())
        .collect()
}

/// The group a published record falls in, by [`RecordView::group_by`].
enum RecordGroup<'r> {
    Key(&'r [lingrep_core::fsm::ProjectedKeyField]),
    LexicalKey(&'r [lingrep_core::fsm::LexicalProjectedKeyField]),
    Unit(&'r str),
    Term(&'r str),
    Surface(String),
}

fn record_group<'r>(
    policy: &CatalogPolicy,
    record: &RecordView<'r>,
    sentence: &dyn TokenSequence,
    text: &AttributeKey,
) -> Option<RecordGroup<'r>> {
    match record.group_by(policy)? {
        GroupBy::Key => record.key.map(RecordGroup::Key),
        GroupBy::LexicalKey => record.lexical_key.map(RecordGroup::LexicalKey),
        GroupBy::Unit => record.unit.map(RecordGroup::Unit),
        GroupBy::Term => record
            .lexical
            .map(|lexical| RecordGroup::Term(lexical.term.as_str())),
        GroupBy::Surface => {
            let end = record.end.min(sentence.len());
            let mut surface = String::new();
            for index in record.start.min(end)..end {
                surface.push_str(sentence.get_key(index, text)?);
            }
            Some(RecordGroup::Surface(surface))
        }
    }
}

fn same_group(record: &RecordGroup<'_>, entry: &Group<'_>) -> bool {
    match (record, entry) {
        (RecordGroup::Surface(value), Group::Surface(entry)) => value == entry,
        (RecordGroup::Unit(value), Group::Unit(entry))
        | (RecordGroup::Term(value), Group::Term(entry)) => value == entry,
        (RecordGroup::Key(fields), Group::Key(entry)) => {
            fields.len() == entry.len()
                && fields.iter().zip(entry).all(|(field, (name, value))| {
                    field.name == *name && *value == FieldValue::Scalar(&field.value)
                })
        }
        (RecordGroup::LexicalKey(fields), Group::Key(entry)) => {
            fields.len() == entry.len()
                && fields.iter().zip(entry).all(|(field, (name, value))| {
                    field.name == *name
                        && match (&field.value, value) {
                            (
                                LexicalProjectedKeyValue::Scalar(field),
                                FieldValue::Scalar(value),
                            ) => field == value,
                            (
                                LexicalProjectedKeyValue::Segments(field),
                                FieldValue::List(values),
                            ) => {
                                field.len() == values.len()
                                    && field.iter().zip(values).all(|(a, b)| a == b)
                            }
                            _ => false,
                        }
                })
        }
        _ => false,
    }
}

/// The group of a frequency row's JSON `group`, if it has that form.
fn json_group(value: &serde_json::Value) -> Option<Group<'_>> {
    fn text(value: &serde_json::Value) -> Option<&str> {
        value.as_str()
    }
    Some(match value["kind"].as_str()? {
        "surface" => Group::Surface(text(&value["value"])?),
        "unit" => Group::Unit(text(&value["value"])?),
        "term" => Group::Term(text(&value["value"])?),
        "key" => Group::Key(
            value["fields"]
                .as_array()?
                .iter()
                .map(|field| {
                    let name = text(&field["name"])?;
                    Some(match field["kind"].as_str()? {
                        "scalar" => (name, FieldValue::Scalar(text(&field["value"])?)),
                        "list" => (
                            name,
                            FieldValue::List(
                                field["values"]
                                    .as_array()?
                                    .iter()
                                    .map(text)
                                    .collect::<Option<_>>()?,
                            ),
                        ),
                        _ => return None,
                    })
                })
                .collect::<Option<_>>()?,
        ),
        _ => return None,
    })
}

/// One occurrence of the entry, with its sentence split around the match.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Line {
    pub document: u64,
    pub sentence: u64,
    pub start: usize,
    pub end: usize,
    pub left: String,
    pub node: String,
    pub right: String,
}

struct EntrySink<'a> {
    pattern: &'a str,
    entry: &'a Group<'a>,
    policy: &'a CatalogPolicy,
    text: AttributeKey,
    lines: Vec<Line>,
}

impl EntrySink<'_> {
    fn take(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        record: &RecordView<'_>,
    ) {
        if record.pattern != self.pattern
            || !record_group(self.policy, record, sentence, &self.text)
                .is_some_and(|group| same_group(&group, self.entry))
        {
            return;
        }
        let texts = (0..sentence.len())
            .map(|index| {
                sentence
                    .get_key(index, &self.text)
                    .map_or("", |text| text.as_str())
            })
            .collect::<Vec<_>>();
        let end = record.end.min(texts.len());
        let start = record.start.min(end);
        let left = lingrep_core::utils::join_orth(texts[..start].iter().copied());
        let right = lingrep_core::utils::join_orth(texts[end..].iter().copied());
        let skip = left.chars().count().saturating_sub(CONTEXT_CHARS);
        self.lines.push(Line {
            document: location.document_id.get(),
            sentence: location.corpus_sentence_ordinal,
            start: record.start,
            end: record.end,
            left: left.chars().skip(skip).collect(),
            node: lingrep_core::utils::join_orth(texts[start..end].iter().copied()),
            right: right.chars().take(CONTEXT_CHARS).collect(),
        });
    }
}

impl MatchSink for EntrySink<'_> {
    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        for matched in matches {
            self.take(location, sentence, &RecordView::raw(matched));
        }
        Ok(SinkControl::Continue { records_emitted: 0 })
    }

    fn push_records(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        records: Vec<PublishedRecord>,
    ) -> Result<SinkControl, ExecutionError> {
        for record in &records {
            self.take(location, sentence, &RecordView::published(record));
        }
        Ok(SinkControl::Continue { records_emitted: 0 })
    }
}

/// One value's bar: the entry's occurrences in the admitted documents that
/// carry the value, and those documents with at least one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Bar {
    pub value: Option<String>,
    pub count: u64,
    pub works: u64,
}

/// A range key's column: the values from `lo` to `hi`, both present in the
/// table's documents, or no value when none falls in the column's span.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Column {
    pub lo: Option<serde_json::Number>,
    pub hi: Option<serde_json::Number>,
    pub count: u64,
    pub works: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Chart {
    /// Values with a nonzero count by descending count, then ascending
    /// value: every one up to `LISTED_VALUES`, otherwise the most frequent
    /// `LISTED_VALUES / 2` of them and the chosen ones. `none` counts the documents
    /// without a value.
    Categorical {
        key: String,
        bars: Vec<Bar>,
        distinct: usize,
        none: Option<Bar>,
    },
    Range {
        key: String,
        columns: Vec<Column>,
        none: Option<Bar>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Profile {
    pub group: serde_json::Value,
    pub count: u64,
    pub works: u64,
    pub charts: Vec<Chart>,
    pub clause: String,
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Lines {
    pub total: u64,
    pub offset: usize,
    pub lines: Vec<Line>,
}

impl FrequencyPair {
    /// The corpus sentences of the table document at `position`.
    fn sentences(&self, position: usize) -> Result<std::ops::Range<u64>, QueryError> {
        Ok(self.table_documents.sentences(position)?)
    }

    /// The corpus and catalog paths a copied command names, if every input
    /// shares a directory.
    fn command_inputs(&self) -> Option<(&str, &str)> {
        self.command_inputs
            .as_ref()
            .map(|[_, corpus, catalog]| (corpus.as_str(), catalog.as_str()))
    }

    /// The group of `entry`, within the per-group name and value limit.
    fn entry_group(&self, entry: u32) -> Result<Group<'_>, QueryError> {
        Ok(self.table.bounded_group(entry, MAX_QUERY_ITEMS)?)
    }

    fn key_index(&self, key: &str) -> Result<usize, QueryError> {
        Ok(self
            .table_documents
            .key(key)
            .ok_or_else(|| std::io::Error::other(format!("{key} is not a dimension")))?)
    }

    /// Sums `counts`, pairs of a table document position and a count, over
    /// the documents `mask` admits, per slot of `key`'s values, by
    /// [`CountTable::value_counts`]: the last slot is the documents without a
    /// value.
    fn value_sums(
        &self,
        key: &str,
        mask: &DocumentMask,
        counts: impl IntoIterator<Item = (usize, u64)>,
    ) -> Result<Vec<ValueCount>, QueryError> {
        let index = self.key_index(key)?;
        let values = self.table_documents.value_count(index) as usize;
        if values > MAX_MASK_BYTES / std::mem::size_of::<ValueCount>() {
            return Err(QueryError::ResourceLimit {
                resource: "value sum bytes",
                limit: MAX_MASK_BYTES as u64,
            });
        }
        let column = self.table_documents.numbered_column(index)?;
        Ok(self.table.value_counts(&column, mask, counts)?)
    }

    /// The slot of `value` among `key`'s values, if a table document has it.
    fn slot_of(&self, key: &str, value: &str) -> Result<Option<usize>, QueryError> {
        let index = self.key_index(key)?;
        let (mut low, mut high) = (0, self.table_documents.value_count(index));
        while low < high {
            let middle = low + (high - low) / 2;
            match self
                .table_documents
                .value(index, middle)?
                .as_str()
                .cmp(value)
            {
                std::cmp::Ordering::Less => low = middle + 1,
                std::cmp::Ordering::Greater => high = middle,
                std::cmp::Ordering::Equal => return Ok(Some(middle as usize)),
            }
        }
        Ok(None)
    }

    /// The values of `key` in `slots`, each slot numbering the key's values
    /// in ascending order, within the projection limits.
    fn values_of(&self, key: &str, slots: &[usize]) -> Result<Vec<String>, QueryError> {
        let index = self.key_index(key)?;
        let mut budget = ProjectionBudget::default();
        slots
            .iter()
            .map(|slot| {
                let value = self.table_documents.value(index, *slot as u32)?;
                budget.charge(&value)?;
                Ok(value)
            })
            .collect()
    }

    /// Each value of `key` read as a finite number, else NaN, by slot.
    fn numbers_of(&self, key: &str) -> Result<Vec<f64>, QueryError> {
        Ok(self.table_documents.numbers(self.key_index(key)?)?)
    }

    /// The entry of `pattern` whose group is `group`, in the JSON form of a
    /// frequency row.
    fn find_entry(
        &self,
        pattern: &str,
        group: &serde_json::Value,
    ) -> Result<Option<(usize, u32)>, QueryError> {
        self.admit_query()?;
        let pattern = self.pattern_index(pattern)?;
        let Some(group) = json_group(group) else {
            return Ok(None);
        };
        Ok(self
            .table
            .find_entry(pattern, &group)?
            .map(|entry| (pattern, entry)))
    }

    fn conditions_clause(
        &self,
        dimensions: &[Dimension],
        conditions: &BTreeMap<String, Condition>,
    ) -> Result<(String, Option<lingrep_core::scope::ScopeClause>), Unsupported> {
        let clause = predicates(dimensions, conditions)?
            .into_iter()
            .map(|(_, predicate)| predicate)
            .collect::<Vec<_>>()
            .join(" & ");
        let parsed = document_clause(&clause)?;
        Ok((clause, parsed))
    }

    /// The entry's count and works under `conditions`, and for each
    /// dimension its counts per value under the other conditions.
    ///
    /// # Errors
    ///
    /// An unknown pattern, or a table that fails to read.
    pub fn profile(
        &self,
        dimensions: &[Dimension],
        pattern: &str,
        group: &serde_json::Value,
        conditions: &BTreeMap<String, Condition>,
    ) -> Result<Result<Profile, Unsupported>, QueryError> {
        let Some((_, entry)) = self.find_entry(pattern, group)? else {
            return Ok(Err(Unsupported::UnknownEntry));
        };
        let (clause, parsed) = match self.conditions_clause(dimensions, conditions) {
            Ok(found) => found,
            Err(unsupported) => return Ok(Err(unsupported)),
        };
        let mask = self.mask(&self.admitted(parsed.as_ref())?);
        let (documents, counts) = self.table.entry_documents(entry)?;
        let pairs = || {
            documents
                .iter()
                .zip(counts)
                .map(|(document, count)| (*document as usize, u64::from(*count)))
        };
        let mut total = (0, 0);
        for (document, count) in pairs() {
            if mask.admits(document) {
                total.0 += count;
                total.1 += 1;
            }
        }
        let mut charts = Vec::with_capacity(dimensions.len());
        for dimension in dimensions {
            let mut others = conditions.clone();
            others.remove(&dimension.key);
            let (_, parsed) = match self.conditions_clause(dimensions, &others) {
                Ok(found) => found,
                Err(unsupported) => return Ok(Err(unsupported)),
            };
            let mask = self.mask(&self.admitted(parsed.as_ref())?);
            let sums = self.value_sums(&dimension.key, &mask, pairs())?;
            let (none, sums) = sums
                .split_last()
                .expect("value counts include the slot of documents without a value");
            let none = (none.count > 0).then_some(Bar {
                value: None,
                count: none.count,
                works: none.works,
            });
            charts.push(match &dimension.kind {
                DimensionKind::Categorical { .. } => {
                    let mut counted = (0..sums.len())
                        .filter(|slot| sums[*slot].count > 0)
                        .collect::<Vec<_>>();
                    counted.sort_by_key(|slot| std::cmp::Reverse(sums[*slot].count));
                    let distinct = counted.len();
                    if distinct > LISTED_VALUES {
                        let chosen = match conditions.get(&dimension.key) {
                            Some(Condition::In { v } | Condition::Out { v }) => v
                                .iter()
                                .map(|value| self.slot_of(&dimension.key, value))
                                .collect::<Result<Vec<_>, _>>()?,
                            _ => Vec::new(),
                        };
                        let mut index = 0;
                        counted.retain(|slot| {
                            index += 1;
                            index <= LISTED_VALUES / 2 || chosen.contains(&Some(*slot))
                        });
                    }
                    let values = self.values_of(&dimension.key, &counted)?;
                    let bars = counted
                        .iter()
                        .zip(values)
                        .map(|(slot, value)| Bar {
                            value: Some(value),
                            count: sums[*slot].count,
                            works: sums[*slot].works,
                        })
                        .collect::<Vec<_>>();
                    Chart::Categorical {
                        key: dimension.key.clone(),
                        bars,
                        distinct,
                        none,
                    }
                }
                DimensionKind::Range { min, max } => Chart::Range {
                    key: dimension.key.clone(),
                    columns: columns(*min, *max, &self.numbers_of(&dimension.key)?, sums),
                    none,
                },
            });
        }
        Ok(Ok(Profile {
            group: group.clone(),
            count: total.0,
            works: total.1,
            charts,
            command: self.lines_command(pattern, group, &clause),
            clause,
        }))
    }

    /// The `ling matches` command whose records, filtered by the `jq`
    /// program, are the entry's lines.
    fn lines_command(&self, pattern: &str, group: &serde_json::Value, clause: &str) -> String {
        let quoted = |value: &serde_json::Value| value.to_string();
        let test = match group["kind"].as_str() {
            Some("key") => {
                let fields = group["fields"]
                    .as_array()
                    .map(|fields| {
                        fields
                            .iter()
                            .map(|field| match field["kind"].as_str() {
                                Some("list") => serde_json::json!({
                                    "kind": "list", "name": field["name"], "values": field["values"],
                                }),
                                _ => serde_json::json!({
                                    "kind": "scalar", "name": field["name"], "value": field["value"],
                                }),
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                format!(".data.key == {}", serde_json::Value::Array(fields))
            }
            Some("unit") => format!(".data.unit == {}", quoted(&group["value"])),
            Some("term") => format!(
                ".data.unit == null and .data.lexical.term == {}",
                quoted(&group["value"])
            ),
            _ => format!(
                ".data.unit == null and .data.lexical == null and .data.surface == {}",
                quoted(&group["value"])
            ),
        };
        let Some((corpus, catalog)) = self.command_inputs() else {
            return String::new();
        };
        let mut command = String::from("ling --json matches");
        if !clause.is_empty() {
            command.push_str(" --where ");
            command.push_str(&shell_word(clause));
        }
        command.push_str(&format!(
            " --corpus {} --patterns {} | jq -c {}",
            shell_word(corpus),
            shell_word(catalog),
            shell_word(&format!(
                "select(.type == \"match\" and .data.pattern == {} and {test})",
                serde_json::Value::from(pattern)
            ))
        ));
        command
    }

    /// The entry's occurrences under `conditions` from `offset`, in corpus
    /// order, matched from the sentences the table records for the entry in
    /// the admitted documents.
    ///
    /// # Errors
    ///
    /// An unknown pattern, a table that fails to read, or a failed match.
    pub fn lines(
        &self,
        cancel: &CancellationFlag,
        dimensions: &[Dimension],
        pattern: &str,
        group: &serde_json::Value,
        conditions: &BTreeMap<String, Condition>,
        offset: usize,
        limit: usize,
    ) -> Result<Result<Lines, Unsupported>, String> {
        let Some((_, entry)) = self
            .find_entry(pattern, group)
            .map_err(|error| error.to_string())?
        else {
            return Ok(Err(Unsupported::UnknownEntry));
        };
        let (_, parsed) = match self.conditions_clause(dimensions, conditions) {
            Ok(found) => found,
            Err(unsupported) => return Ok(Err(unsupported)),
        };
        let mask = self.mask(
            &self
                .admitted(parsed.as_ref())
                .map_err(|error| error.to_string())?,
        );
        let limit = if limit == 0 {
            LINE_PAGE
        } else {
            limit.min(MAX_LINE_PAGE)
        };
        let (documents, counts) = self
            .table
            .entry_documents(entry)
            .map_err(|error| error.to_string())?;
        let mut admitted = documents
            .iter()
            .zip(counts)
            .filter(|(document, _)| mask.admits(**document as usize))
            .map(|(document, count)| {
                let document = *document as usize;
                let sentences = self
                    .sentences(document)
                    .map_err(|error| error.to_string())?;
                Ok((document, u64::from(*count), sentences))
            })
            .collect::<Result<Vec<_>, String>>()?;
        admitted.sort_by_key(|(_, _, sentences)| sentences.start);
        let total = admitted.iter().map(|(_, count, _)| count).sum::<u64>();
        let table_group = self.entry_group(entry).map_err(|error| error.to_string())?;
        let matcher = self
            .matcher
            .as_ref()
            .ok_or("the corpus has no bound catalog")?;
        let catalog = matcher
            .patterns
            .get(pattern)
            .ok_or_else(|| format!("pattern {pattern} is not bound to the corpus"))?;
        let policy = catalog.bound.catalog_policy();
        let (sentences, sentence_counts) = self
            .table
            .entry_sentences(entry)
            .map_err(|error| error.to_string())?;
        let ids = self.table.document_ids();
        let (listed, skip) = page_sentences(
            admitted
                .into_iter()
                .map(|(document, count, range)| (ids[document], count, range)),
            sentences,
            sentence_counts,
            offset as u64..offset.saturating_add(limit) as u64,
        )?;
        let mut sink = EntrySink {
            pattern,
            entry: &table_group,
            policy,
            text: AttributeKey::new("text").expect("text is a canonical attribute key"),
            lines: Vec::new(),
        };
        matcher
            .run(
                catalog,
                cancel,
                listed.iter().map(|(sentence, _)| *sentence).collect(),
                &mut sink,
            )
            .map_err(|error| error.to_string())?;
        let mut found = sink.lines.into_iter().peekable();
        let mut lines = Vec::new();
        for (sentence, count) in listed {
            let mut held = 0;
            while let Some(line) = found.next_if(|line| line.sentence == sentence) {
                held += 1;
                lines.push(line);
            }
            if held != count {
                return Err(format!(
                    "sentence {sentence} holds {held} occurrences of the entry, but the table counts {count}"
                ));
            }
        }
        let lines = lines
            .into_iter()
            .skip(usize::try_from(skip).unwrap_or(usize::MAX))
            .take(limit)
            .collect();
        Ok(Ok(Lines {
            total,
            offset,
            lines,
        }))
    }
}

/// The sentences, with the entry's count in each, that hold the entry's
/// occurrences numbered `page` in the order of `documents`, and how many of
/// the first sentence's occurrences precede the page. `documents` are a
/// document ID, its count and its corpus sentences, in corpus order;
/// `sentences` and `counts` are the entry's recorded sentences.
fn page_sentences(
    documents: impl IntoIterator<Item = (u64, u64, std::ops::Range<u64>)>,
    sentences: &[u32],
    counts: &[u32],
    page: std::ops::Range<u64>,
) -> Result<(Vec<(u64, u64)>, u64), String> {
    let mut before = 0u64;
    let mut skip = 0u64;
    let mut listed = Vec::new();
    for (id, count, range) in documents {
        if before.saturating_add(count) <= page.start {
            before += count;
            continue;
        }
        if before >= page.end {
            break;
        }
        let first = sentences.partition_point(|sentence| u64::from(*sentence) < range.start);
        let last = sentences.partition_point(|sentence| u64::from(*sentence) < range.end);
        let recorded = counts[first..last]
            .iter()
            .map(|count| u64::from(*count))
            .sum::<u64>();
        if recorded != count {
            return Err(format!(
                "document {id} records {recorded} occurrences of the entry in its sentences, but the table counts {count}"
            ));
        }
        for (sentence, count) in sentences[first..last].iter().zip(&counts[first..last]) {
            let count = u64::from(*count);
            if before + count <= page.start {
                before += count;
                continue;
            }
            if before >= page.end {
                break;
            }
            if listed.is_empty() {
                skip = page.start - before;
            }
            listed.push((u64::from(*sentence), count));
            before += count;
        }
    }
    Ok((listed, skip))
}

/// Columns over `[min, max]` with the entry's sums per value slot, given
/// each slot's number: one per integer when the values are integers within
/// `INTEGER_COLUMNS`, otherwise `RANGE_BINS` of equal width.
fn columns(min: f64, max: f64, numbers: &[f64], sums: &[ValueCount]) -> Vec<Column> {
    let valued = || {
        numbers
            .iter()
            .zip(sums)
            .filter(|(number, _)| number.is_finite())
    };
    let integers = valued().all(|(number, _)| number.fract() == 0.0);
    let span = max - min;
    let (count, width) = if integers && span + 1.0 <= INTEGER_COLUMNS {
        (span as usize + 1, 1.0)
    } else {
        (
            RANGE_BINS,
            (span / RANGE_BINS as f64).max(f64::MIN_POSITIVE),
        )
    };
    let mut ends = vec![None::<(f64, f64)>; count];
    let mut columns = vec![
        Column {
            lo: None,
            hi: None,
            count: 0,
            works: 0,
        };
        count
    ];
    for (number, sum) in valued() {
        let at = (((number - min) / width) as usize).min(count - 1);
        let (lo, hi) = ends[at].get_or_insert((*number, *number));
        *lo = lo.min(*number);
        *hi = hi.max(*number);
        columns[at].count += sum.count;
        columns[at].works += sum.works;
    }
    for (column, ends) in columns.iter_mut().zip(ends) {
        if let Some((lo, hi)) = ends {
            column.lo = Some(json_number(lo));
            column.hi = Some(json_number(hi));
        }
    }
    columns
}

/// `number` as JSON, an integer when it has no fraction.
fn json_number(number: f64) -> serde_json::Number {
    if number.fract() == 0.0 && number.abs() < 2f64.powi(53) {
        serde_json::Number::from(number as i64)
    } else {
        serde_json::Number::from_f64(number).expect("a finite number")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::generators;

    /// Every finite value falls in exactly one column, in value order, and
    /// a column's `lo` and `hi` are its least and greatest values.
    #[hegel::test(test_cases = 300, derandomize = true)]
    fn range_columns_partition_the_values_in_order(tc: hegel::TestCase) {
        let integers = tc.draw(generators::booleans());
        let numbers = tc.draw(
            generators::vecs(generators::integers::<i32>().min_value(-500).max_value(500))
                .min_size(1)
                .max_size(30),
        );
        let mut numbers = numbers
            .into_iter()
            .map(|number| {
                if integers {
                    f64::from(number)
                } else {
                    f64::from(number) / 4.0
                }
            })
            .collect::<Vec<_>>();
        if tc.draw(generators::booleans()) {
            numbers.push(f64::NAN);
        }
        let sums = (0..numbers.len())
            .map(|_| ValueCount {
                admission: lingrep_corpus::Admission {
                    documents_admitted: 0,
                    sentences_admitted: 0,
                    tokens_admitted: 0,
                },
                count: tc.draw(generators::integers::<u64>().max_value(9)),
                works: tc.draw(generators::integers::<u64>().max_value(3)),
            })
            .collect::<Vec<_>>();
        let finite = || numbers.iter().copied().filter(|number| number.is_finite());
        let min = finite().fold(f64::INFINITY, f64::min);
        let max = finite().fold(f64::NEG_INFINITY, f64::max);

        let columns = columns(min, max, &numbers, &sums);

        let whole = finite().all(|number| number.fract() == 0.0);
        let expected = if whole && max - min < INTEGER_COLUMNS {
            (max - min) as usize + 1
        } else {
            RANGE_BINS
        };
        assert_eq!(columns.len(), expected);
        let bound =
            |number: &Option<serde_json::Number>| number.as_ref().map(|n| n.as_f64().unwrap());
        let mut previous = f64::NEG_INFINITY;
        let (mut count, mut works) = (0, 0);
        for column in &columns {
            let (lo, hi) = (bound(&column.lo), bound(&column.hi));
            assert_eq!(lo.is_some(), hi.is_some(), "{column:?}");
            let Some((lo, hi)) = lo.zip(hi) else {
                assert_eq!((column.count, column.works), (0, 0));
                continue;
            };
            assert!(previous < lo && lo <= hi, "{columns:?}");
            let inside = numbers
                .iter()
                .zip(&sums)
                .filter(|(number, _)| (lo..=hi).contains(*number))
                .collect::<Vec<_>>();
            assert!(inside.iter().any(|(number, _)| **number == lo));
            assert!(inside.iter().any(|(number, _)| **number == hi));
            assert_eq!(
                column.count,
                inside.iter().map(|(_, sum)| sum.count).sum::<u64>()
            );
            assert_eq!(
                column.works,
                inside.iter().map(|(_, sum)| sum.works).sum::<u64>()
            );
            count += column.count;
            works += column.works;
            previous = hi;
        }
        let valued = numbers
            .iter()
            .zip(&sums)
            .filter(|(number, _)| number.is_finite());
        assert_eq!(count, valued.clone().map(|(_, sum)| sum.count).sum::<u64>());
        assert_eq!(works, valued.map(|(_, sum)| sum.works).sum::<u64>());
        if whole {
            assert!(
                columns
                    .iter()
                    .flat_map(|column| [&column.lo, &column.hi])
                    .flatten()
                    .all(serde_json::Number::is_i64)
            );
        }
    }

    #[hegel::test(test_cases = 300, derandomize = true)]
    fn a_page_lists_exactly_the_sentences_holding_its_occurrences(tc: hegel::TestCase) {
        let shapes = tc.draw(
            generators::vecs(
                generators::vecs(generators::integers::<u32>().min_value(1).max_value(3))
                    .min_size(1)
                    .max_size(4),
            )
            .max_size(5),
        );
        let (mut sentences, mut counts, mut documents, mut occurrences) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (index, shape) in shapes.iter().enumerate() {
            let start = 10 * index as u64;
            for (offset, count) in shape.iter().enumerate() {
                let sentence = start + offset as u64;
                sentences.push(sentence as u32);
                counts.push(*count);
                occurrences.extend(std::iter::repeat_n(sentence, *count as usize));
            }
            let count = shape.iter().map(|count| u64::from(*count)).sum();
            documents.push((index as u64, count, start..start + 10));
        }
        let start = tc.draw(generators::integers::<u64>().max_value(occurrences.len() as u64 + 2));
        let length = tc.draw(generators::integers::<u64>().min_value(1).max_value(6));
        let (listed, skip) = page_sentences(
            documents.clone(),
            &sentences,
            &counts,
            start..start + length,
        )
        .expect("consistent counts");
        let shown = occurrences
            .iter()
            .skip(start as usize)
            .take(length as usize)
            .copied()
            .collect::<Vec<_>>();
        let mut expected = shown.clone();
        expected.dedup();
        assert_eq!(
            listed
                .iter()
                .map(|(sentence, _)| *sentence)
                .collect::<Vec<_>>(),
            expected
        );
        for (sentence, count) in &listed {
            assert_eq!(
                *count,
                occurrences.iter().filter(|held| *held == sentence).count() as u64
            );
        }
        let held = listed
            .iter()
            .flat_map(|(sentence, count)| std::iter::repeat_n(*sentence, *count as usize))
            .skip(skip as usize)
            .take(length as usize)
            .collect::<Vec<_>>();
        assert_eq!(held, shown);
    }

    #[test]
    fn each_pattern_gets_the_first_catalog_that_publishes_it() {
        // A catalog compiled whole publishes every pattern; one compiled for
        // `a` publishes `a`, the lexicon `w`, and `b` when `a` and `b` share
        // an output name, but never `c`.
        let cases: [(&[&str], usize); 3] =
            [(&["a", "b", "c", "w"], 1), (&["w"], 3), (&["b", "w"], 2)];
        for (also, compiles_expected) in cases {
            let mut compiles = Vec::new();
            let compiled = compile_each(["a", "b", "c", "w"], |pattern| {
                compiles.push(pattern.to_owned());
                let mut published = vec![pattern.to_owned()];
                if pattern == "a" {
                    published.extend(also.iter().map(|name| (*name).to_owned()));
                }
                Ok((pattern.to_owned(), published))
            })
            .expect("compiled");
            assert_eq!(compiles.len(), compiles_expected, "{also:?}: {compiles:?}");
            for (pattern, catalog) in &compiled {
                let publisher = if also.contains(&pattern.as_str()) {
                    "a"
                } else {
                    pattern
                };
                assert_eq!(catalog.as_str(), publisher, "{also:?}: {pattern}");
            }
            assert_eq!(compiled.len(), 4);
        }
    }

    #[test]
    fn a_document_whose_sentences_disagree_with_its_count_is_an_error() {
        let error = page_sentences([(7, 3, 0..10)], &[1, 2], &[1, 1], 0..50).expect_err("mismatch");
        assert_eq!(
            error,
            "document 7 records 2 occurrences of the entry in its sentences, but the table counts 3"
        );
    }
}
