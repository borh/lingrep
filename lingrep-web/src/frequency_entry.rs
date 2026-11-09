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
    BoundCatalog, CatalogPolicy, CompileLimits, LexicalProjectedKeyValue, Match, PatternSource,
    PublishedRecord, RecordView, compile_catalog_pattern,
};
use lingrep_core::token_match::{AttributeKey, TokenSequence};
use lingrep_corpus::{FieldValue, Group};
use serde::Serialize;

use crate::corpus_runtime::ReadyCorpus;
use crate::frequency::{
    Condition, Dimension, DimensionKind, FrequencyPair, QueryError, Unsupported, ValueSum,
    document_clause, group_value, predicates, shell_word,
};

/// Lines a request returns when it names no limit, and the most it may name.
pub const LINE_PAGE: usize = 50;
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
    patterns: HashMap<String, PatternCatalog>,
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
    /// token schema. Must be called inside a Tokio runtime, which later
    /// answers lookaround division queries.
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
        let patterns = patterns
            .into_iter()
            .map(|pattern| {
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
                    pattern.to_owned(),
                    PatternCatalog {
                        division_kinds: output.matcher.lookaround_division_kinds(),
                        bound,
                    },
                ))
            })
            .collect::<Result<_, String>>()?;
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
        let source = source.with_ordinals(sentences);
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
            &CancellationFlag::new(),
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

/// What a published record groups by, as `ling summarize` groups it: a
/// declared key, else a rendered unit, else a lexicon term, else the matched
/// surface. `None` for a record whose declared key could not be projected.
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
    if policy
        .get(record.definition)
        .is_some_and(|entry| !entry.key.is_empty())
    {
        return record
            .key
            .map(RecordGroup::Key)
            .or_else(|| record.lexical_key.map(RecordGroup::LexicalKey));
    }
    if let Some(unit) = record.unit {
        return Some(RecordGroup::Unit(unit));
    }
    if let Some(lexical) = record.lexical {
        return Some(RecordGroup::Term(lexical.term.as_str()));
    }
    let end = record.end.min(sentence.len());
    let mut surface = String::new();
    for index in record.start.min(end)..end {
        surface.push_str(sentence.get_key(index, text)?);
    }
    Some(RecordGroup::Surface(surface))
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
    /// Values with a nonzero count by descending count, then first
    /// occurrence: every one up to `LISTED_VALUES`, otherwise the most
    /// frequent of them and the chosen ones. `none` counts the documents
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
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Lines {
    pub total: u64,
    pub offset: usize,
    pub lines: Vec<Line>,
}

impl FrequencyPair {
    /// The entry of `pattern` whose group is `group`, in the JSON form of a
    /// frequency row.
    fn find_entry(
        &self,
        pattern: &str,
        group: &serde_json::Value,
    ) -> Result<Option<(usize, u32)>, QueryError> {
        self.admit_query()?;
        let pattern = self.pattern_index(pattern)?;
        for entry in self.table.pattern_range(pattern)? {
            let entry = u32::try_from(entry).expect("a table numbers entries in u32");
            if group_value(&self.entry_group(entry)?) == *group {
                return Ok(Some((pattern, entry)));
            }
        }
        Ok(None)
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
            let (sums, none) = self.value_sums(&dimension.key, &mask, pairs())?;
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
                DimensionKind::Range { min, max } => {
                    let spans = columns(*min, *max, &self.numbers_of(&dimension.key)?, &sums);
                    let ends = spans
                        .iter()
                        .flat_map(|span| [span.lo, span.hi])
                        .flatten()
                        .collect::<Vec<_>>();
                    let ends = ends
                        .iter()
                        .copied()
                        .zip(self.values_of(&dimension.key, &ends)?)
                        .collect::<HashMap<_, _>>();
                    let number = |slot: Option<usize>| {
                        slot.and_then(|slot| ends.get(&slot))
                            .and_then(|text| text.parse::<serde_json::Number>().ok())
                    };
                    Chart::Range {
                        key: dimension.key.clone(),
                        columns: spans
                            .into_iter()
                            .map(|span| Column {
                                lo: number(span.lo),
                                hi: number(span.hi),
                                count: span.sum.count,
                                works: span.sum.works,
                            })
                            .collect(),
                        none,
                    }
                }
            });
        }
        Ok(Ok(Profile {
            group: group.clone(),
            count: total.0,
            works: total.1,
            charts,
            command: self.lines_command(pattern, group, &clause),
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
        matcher: &EntryMatcher,
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
        let catalog = matcher
            .patterns
            .get(pattern)
            .ok_or_else(|| format!("pattern {pattern} is not bound to the corpus"))?;
        let policy = catalog.bound.catalog_policy();
        let end = offset.saturating_add(limit);
        let mut before = 0usize;
        let mut skip = 0usize;
        let mut page = Vec::new();
        for (document, count, sentences) in admitted {
            let count = usize::try_from(count).unwrap_or(usize::MAX);
            if before.saturating_add(count) <= offset {
                before += count;
                continue;
            }
            if before >= end {
                break;
            }
            if page.is_empty() {
                skip = offset - before;
            }
            page.push((document, count, sentences));
            before += count;
        }
        let mut sink = EntrySink {
            pattern,
            entry: &table_group,
            policy,
            text: AttributeKey::new("text").expect("text is a canonical attribute key"),
            lines: Vec::new(),
        };
        let sentences = self
            .table
            .entry_sentences(entry)
            .map_err(|error| error.to_string())?;
        let mut listed = Vec::new();
        for (_, _, range) in &page {
            let first = sentences.partition_point(|sentence| u64::from(*sentence) < range.start);
            let last = sentences.partition_point(|sentence| u64::from(*sentence) < range.end);
            listed.extend(
                sentences[first..last]
                    .iter()
                    .map(|sentence| u64::from(*sentence)),
            );
        }
        matcher
            .run(catalog, listed, &mut sink)
            .map_err(|error| error.to_string())?;
        let ids = self.table.document_ids();
        let mut found = sink.lines.into_iter().peekable();
        let mut lines = Vec::new();
        for (document, count, _) in page {
            let id = ids[document];
            let mut held = 0;
            while let Some(line) = found.next_if(|line| line.document == id) {
                held += 1;
                lines.push(line);
            }
            if held != count {
                return Err(format!(
                    "document {id} holds {held} occurrences of the entry, but the table counts {count}"
                ));
            }
        }
        let lines = lines.into_iter().skip(skip).take(limit).collect();
        Ok(Ok(Lines {
            total,
            offset,
            lines,
        }))
    }
}

/// The slots of a column's least and greatest values, and its sum.
struct Span {
    lo: Option<usize>,
    hi: Option<usize>,
    sum: ValueSum,
}

/// Columns over `[min, max]` with the entry's sums per value slot, given
/// each slot's number: one per integer when the values are integers within
/// `INTEGER_COLUMNS`, otherwise `RANGE_BINS` of equal width.
fn columns(min: f64, max: f64, numbers: &[f64], sums: &[ValueSum]) -> Vec<Span> {
    let valued = || {
        numbers
            .iter()
            .enumerate()
            .filter(|(_, number)| number.is_finite())
    };
    let integers = valued().all(|(_, number)| number.fract() == 0.0);
    let span = max - min;
    let (count, width) = if integers && span + 1.0 <= INTEGER_COLUMNS {
        (span as usize + 1, 1.0)
    } else {
        (
            RANGE_BINS,
            (span / RANGE_BINS as f64).max(f64::MIN_POSITIVE),
        )
    };
    let mut columns = (0..count)
        .map(|_| Span {
            lo: None,
            hi: None,
            sum: ValueSum::default(),
        })
        .collect::<Vec<_>>();
    for (slot, number) in valued() {
        let column = &mut columns[(((number - min) / width) as usize).min(count - 1)];
        if column.lo.is_none_or(|lo| *number < numbers[lo]) {
            column.lo = Some(slot);
        }
        if column.hi.is_none_or(|hi| *number > numbers[hi]) {
            column.hi = Some(slot);
        }
        column.sum.count += sums[slot].count;
        column.sum.works += sums[slot].works;
    }
    columns
}
