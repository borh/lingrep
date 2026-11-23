//! Direct bounded `matches` orchestration and publication.

use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Write};
use std::ops::Range;
use std::time::{Duration, Instant};

use lingrep_core::execution::{
    ExecutionError, MatchSink, PolicySelection, Publication, SinkControl, cached_run_attributes,
};
use lingrep_core::fsm::{Match, PublishedRecord, RecordView};
use lingrep_core::token_match::{AttributeValueRef, SentenceSpan, TokenSequence};
use serde::{Serialize, Serializer};

use super::CommandOutcome;
use super::query_completion::FilesystemCompletion;
use super::query_resolution::{
    MatchingQuerySession, MatchingQuerySpec, MatchingRunOutcome, QueryProjection,
};
use super::query_scope::parse_scope;
use super::query_support::{
    execution_output_error, execution_output_io, oversized_omissions_error,
    protocol_execution_limits, publish_query_jsonl, publish_run_and_definitions,
    write_vectors_notice,
};
use crate::catalog_input::load_and_compile_with_vectors;
use crate::cli::{ExecutionCliConfig, MatchesArgs, ProgressMode};
use crate::direct_source::OversizedSentenceOmissionSummary;
use crate::direct_source::{matched_orth_tokens, matched_surface, matched_surface_into};
use crate::error::CliError;
use crate::output::{
    ColorPolicy, ContextColorPalette, EvidenceTsvRow, HumanKwicRow, HumanMatchLayout,
    HumanRenderer, MIN_HUMAN_MATCH_WIDTH, MatchTsvRow, TerminalColorCapability, write_evidence_tsv,
    write_match_tsv,
};
use crate::presentation::{
    ColorMode, OutputMode, ResultDelivery, resolve_match_context, resolve_match_limit,
};
use crate::protocol::query::{
    CharSpan as ProtocolCharSpan, CommittedPrefix, Coverage, EvidenceWord,
    GraphNodeId as ProtocolGraphNodeId, MatchEvidence, MatchOrder, PublishedProjection,
    QueryComplete, QueryEnvelope, QueryEvent, QueryMode, QueryRun, SourceSpan, SpanConvention,
    TemporaryStorageLimits, Termination,
};
use crate::terminal::{PresentationProgress, TerminalArbiter};

#[derive(Debug, PartialEq, Eq)]
enum ValidatedCompletion<T> {
    Cancelled,
    Publish(T),
}

fn protocol_graph_node(node: lingrep_core::corpus::conllu::EnhancedNodeId) -> ProtocolGraphNodeId {
    match node {
        lingrep_core::corpus::conllu::EnhancedNodeId::Word { token_index } => {
            ProtocolGraphNodeId::Word {
                token_index: token_index as u64,
            }
        }
        lingrep_core::corpus::conllu::EnhancedNodeId::Empty { boundary, subindex } => {
            ProtocolGraphNodeId::Empty {
                boundary: boundary as u64,
                subindex: subindex as u64,
            }
        }
    }
}

/// `published` narrows a word node's scalars to the run's attributes before
/// `--evidence` widened the projection, so that `--evidence` adds a field and
/// alters none. An empty node's scalars come from the graph, not the
/// projection, and are never narrowed.
fn graph_node_attribute_published(
    node: lingrep_core::corpus::conllu::EnhancedNodeId,
    attribute: &lingrep_core::fsm::GraphNodeAttribute,
    published: Option<&[lingrep_core::token_match::AttributeKey]>,
) -> bool {
    !matches!(
        node,
        lingrep_core::corpus::conllu::EnhancedNodeId::Word { .. }
    ) || attribute.name.as_str().starts_with("node.")
        || published.is_none_or(|keys| keys.contains(&attribute.name))
}

#[derive(Serialize)]
struct BorrowedMatchEnvelope<'a> {
    schema: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    data: BorrowedMatchData<'a>,
}

#[derive(Serialize)]
struct BorrowedMatchData<'a> {
    sequence: u64,
    pattern: &'a str,
    span: SourceSpan,
    #[serde(skip_serializing_if = "BorrowedSpanIds::is_empty")]
    span_ids: BorrowedSpanIds<'a>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    whole_span_ids: BTreeMap<&'a str, Vec<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    char_span: Option<ProtocolCharSpan>,
    captures: BorrowedCaptures<'a>,
    #[serde(skip_serializing_if = "BorrowedGraphBindings::is_empty")]
    graph_bindings: BorrowedGraphBindings<'a>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    interpretations: &'a [lingrep_core::fsm::Interpretation],
    corpus_sentence_ordinal: u64,
    source_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_metadata: Option<&'a serde_json::Map<String, serde_json::Value>>,
    document_ordinal: u64,
    document_sentence_ordinal: u64,
    surface: &'a str,
    tokens: BorrowedTokenSlice<'a>,
    left_context: Option<BorrowedTokenSlice<'a>>,
    right_context: Option<BorrowedTokenSlice<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unit: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    key: Option<BorrowedKeyFields<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lexical: Option<BorrowedLexicalMatch<'a>>,
    #[serde(skip_serializing_if = "BorrowedTags::is_empty")]
    tags: BorrowedTags<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence: Option<MatchEvidence>,
}

struct BorrowedSpanIds<'a> {
    spans: &'a [SentenceSpan],
    start: usize,
    end: usize,
    has_any: bool,
}

impl BorrowedSpanIds<'_> {
    fn is_empty(&self) -> bool {
        !self.has_any
    }
}

impl Serialize for BorrowedSpanIds<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(None)?;
        for (id, span) in self.spans.iter().enumerate() {
            if span.start <= self.start && self.end <= span.end {
                seq.serialize_element(&(id as u64))?;
            }
        }
        seq.end()
    }
}

struct BorrowedCaptures<'a> {
    captures: &'a [lingrep_core::fsm::Capture],
    sentence: &'a dyn TokenSequence,
}

#[derive(Serialize)]
struct BorrowedCapture<'a> {
    name: &'a str,
    hygienic_path: &'a str,
    span: SourceSpan,
    #[serde(skip_serializing_if = "Option::is_none")]
    char_span: Option<ProtocolCharSpan>,
}

impl Serialize for BorrowedCaptures<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.captures.len()))?;
        for capture in self.captures {
            seq.serialize_element(&BorrowedCapture {
                name: capture.name.as_str(),
                hygienic_path: capture.path.as_str(),
                span: SourceSpan {
                    start: capture.start as u64,
                    end: capture.end as u64,
                },
                char_span: span_char_range(self.sentence, capture.start..capture.end),
            })?;
        }
        seq.end()
    }
}

struct BorrowedGraphBindings<'a> {
    bindings: &'a [lingrep_core::fsm::GraphBinding],
    published: Option<&'a [lingrep_core::token_match::AttributeKey]>,
}

impl BorrowedGraphBindings<'_> {
    fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

struct BorrowedGraphNodeAttributes<'a> {
    node: lingrep_core::corpus::conllu::EnhancedNodeId,
    attributes: &'a [lingrep_core::fsm::GraphNodeAttribute],
    published: Option<&'a [lingrep_core::token_match::AttributeKey]>,
}

#[derive(Serialize)]
struct BorrowedGraphNodeAttribute<'a> {
    name: &'a str,
    value: &'a str,
}

impl Serialize for BorrowedGraphNodeAttributes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(None)?;
        for attribute in self.attributes {
            if graph_node_attribute_published(self.node, attribute, self.published) {
                seq.serialize_element(&BorrowedGraphNodeAttribute {
                    name: attribute.name.pattern_key(),
                    value: attribute.value.as_str(),
                })?;
            }
        }
        seq.end()
    }
}

#[derive(Serialize)]
#[serde(tag = "binding_kind", rename_all = "snake_case")]
enum BorrowedGraphBinding<'a> {
    Measure {
        name: &'a str,
        value: Option<serde_json::Number>,
        status: &'static str,
    },
    Count {
        name: &'a str,
        value: Option<usize>,
    },
    Node {
        name: &'a str,
        node: ProtocolGraphNodeId,
        attributes: BorrowedGraphNodeAttributes<'a>,
    },
    Edge {
        name: &'a str,
        head: ProtocolGraphNodeId,
        dependent: ProtocolGraphNodeId,
        relation: &'a str,
    },
}

impl Serialize for BorrowedGraphBindings<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.bindings.len()))?;
        for binding in self.bindings {
            let borrowed = match binding {
                lingrep_core::fsm::GraphBinding::Measure { name, value } => {
                    BorrowedGraphBinding::Measure {
                        name: name.as_str(),
                        value: value.number().and_then(serde_json::Number::from_f64),
                        status: value.status(),
                    }
                }
                lingrep_core::fsm::GraphBinding::Count { name, value } => {
                    BorrowedGraphBinding::Count {
                        name: name.as_str(),
                        value: *value,
                    }
                }
                lingrep_core::fsm::GraphBinding::Node {
                    name,
                    node,
                    attributes,
                } => BorrowedGraphBinding::Node {
                    name: name.as_str(),
                    node: protocol_graph_node(*node),
                    attributes: BorrowedGraphNodeAttributes {
                        node: *node,
                        attributes,
                        published: self.published,
                    },
                },
                lingrep_core::fsm::GraphBinding::Edge {
                    name,
                    head,
                    dependent,
                    relation,
                } => BorrowedGraphBinding::Edge {
                    name: name.as_str(),
                    head: protocol_graph_node(*head),
                    dependent: protocol_graph_node(*dependent),
                    relation: relation.as_str(),
                },
            };
            seq.serialize_element(&borrowed)?;
        }
        seq.end()
    }
}

struct BorrowedTokenSlice<'a> {
    sentence: &'a dyn TokenSequence,
    range: Range<usize>,
    projection: &'a PublishedProjection,
}

#[derive(Serialize)]
struct BorrowedTokenEvidence<'a> {
    index: u64,
    attributes: BorrowedTokenAttributes<'a>,
}

struct BorrowedTokenAttributes<'a> {
    sentence: &'a dyn TokenSequence,
    index: usize,
    projection: &'a PublishedProjection,
}

impl Serialize for BorrowedTokenAttributes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        for key in self.projection.attributes() {
            if let Some(value) = self.sentence.value_key(self.index, key) {
                match value {
                    AttributeValueRef::Scalar(value) => {
                        map.serialize_entry(key.as_str(), value)?;
                    }
                    AttributeValueRef::StringSet(values) => {
                        map.serialize_entry(key.as_str(), values)?;
                    }
                }
            }
        }
        map.end()
    }
}

impl Serialize for BorrowedTokenSlice<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.range.len()))?;
        for index in self.range.clone() {
            seq.serialize_element(&BorrowedTokenEvidence {
                index: index as u64,
                attributes: BorrowedTokenAttributes {
                    sentence: self.sentence,
                    index,
                    projection: self.projection,
                },
            })?;
        }
        seq.end()
    }
}

enum BorrowedKeyFields<'a> {
    Scalar(&'a [lingrep_core::fsm::ProjectedKeyField]),
    Lexical(&'a [lingrep_core::fsm::LexicalProjectedKeyField]),
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum BorrowedKeyField<'a> {
    Scalar { name: &'a str, value: &'a str },
    List { name: &'a str, values: &'a [String] },
}

impl Serialize for BorrowedKeyFields<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        match self {
            Self::Scalar(fields) => {
                let mut seq = serializer.serialize_seq(Some(fields.len()))?;
                for field in *fields {
                    seq.serialize_element(&BorrowedKeyField::Scalar {
                        name: field.name.as_str(),
                        value: field.value.as_str(),
                    })?;
                }
                seq.end()
            }
            Self::Lexical(fields) => {
                let mut seq = serializer.serialize_seq(Some(fields.len()))?;
                for field in *fields {
                    let borrowed = match &field.value {
                        lingrep_core::fsm::LexicalProjectedKeyValue::Scalar(value) => {
                            BorrowedKeyField::Scalar {
                                name: field.name.as_str(),
                                value: value.as_str(),
                            }
                        }
                        lingrep_core::fsm::LexicalProjectedKeyValue::Segments(values) => {
                            BorrowedKeyField::List {
                                name: field.name.as_str(),
                                values: values.as_slice(),
                            }
                        }
                    };
                    seq.serialize_element(&borrowed)?;
                }
                seq.end()
            }
        }
    }
}

#[derive(Serialize)]
struct BorrowedLexicalMatch<'a> {
    term: &'a str,
    matched_by: BorrowedMatchedBy<'a>,
}

struct BorrowedMatchedBy<'a>(&'a [lingrep_core::token_match::AttributeKey]);

impl Serialize for BorrowedMatchedBy<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for attribute in self.0 {
            seq.serialize_element(attribute.pattern_key())?;
        }
        seq.end()
    }
}

struct BorrowedTags<'a>(&'a [(compact_str::CompactString, String)]);

impl BorrowedTags<'_> {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Serialize)]
struct BorrowedTagField<'a> {
    name: &'a str,
    value: &'a str,
}

impl Serialize for BorrowedTags<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for (name, value) in self.0 {
            seq.serialize_element(&BorrowedTagField {
                name: name.as_str(),
                value: value.as_str(),
            })?;
        }
        seq.end()
    }
}

#[allow(clippy::too_many_arguments)]
fn borrowed_match_envelope<'a>(
    sequence: u64,
    record: &'a RecordView<'a>,
    location: &'a lingrep_core::execution::SentenceLocation,
    sentence: &'a dyn TokenSequence,
    start: usize,
    end: usize,
    requested_context: usize,
    projection: &'a PublishedProjection,
    surface: &'a str,
    char_span: Option<ProtocolCharSpan>,
    published_graph_keys: Option<&'a [lingrep_core::token_match::AttributeKey]>,
    evidence: Option<MatchEvidence>,
) -> BorrowedMatchEnvelope<'a> {
    let spans = sentence.spans();
    let mut has_span_ids = false;
    let mut whole_span_ids = BTreeMap::<&'a str, Vec<u64>>::new();
    for (id, span) in spans.iter().enumerate() {
        if span.start <= record.start && record.end <= span.end {
            has_span_ids = true;
            if (span.start, span.end) == (record.start, record.end) {
                whole_span_ids
                    .entry(span.kind.as_str())
                    .or_default()
                    .push(id as u64);
            }
        }
    }
    let left = start.saturating_sub(requested_context)..start;
    let right = end..end.saturating_add(requested_context).min(sentence.len());
    BorrowedMatchEnvelope {
        schema: crate::protocol::query::SCHEMA,
        kind: "match",
        data: BorrowedMatchData {
            sequence,
            pattern: record.pattern,
            span: SourceSpan {
                start: record.start as u64,
                end: record.end as u64,
            },
            span_ids: BorrowedSpanIds {
                spans,
                start: record.start,
                end: record.end,
                has_any: has_span_ids,
            },
            whole_span_ids,
            char_span,
            captures: BorrowedCaptures {
                captures: record.captures,
                sentence,
            },
            graph_bindings: BorrowedGraphBindings {
                bindings: record.graph_bindings,
                published: published_graph_keys,
            },
            interpretations: record.interpretations,
            corpus_sentence_ordinal: location.corpus_sentence_ordinal,
            source_name: location.source_name.as_ref(),
            source_metadata: location.source_metadata.as_deref(),
            document_ordinal: location.document_ordinal,
            document_sentence_ordinal: location.document_sentence_ordinal,
            surface,
            tokens: BorrowedTokenSlice {
                sentence,
                range: start..end,
                projection,
            },
            left_context: (requested_context != 0).then_some(BorrowedTokenSlice {
                sentence,
                range: left,
                projection,
            }),
            right_context: (requested_context != 0).then_some(BorrowedTokenSlice {
                sentence,
                range: right,
                projection,
            }),
            unit: record.unit,
            key: record
                .key
                .map(BorrowedKeyFields::Scalar)
                .or_else(|| record.lexical_key.map(BorrowedKeyFields::Lexical)),
            lexical: record.lexical.map(|lexical| BorrowedLexicalMatch {
                term: lexical.term.as_str(),
                matched_by: BorrowedMatchedBy(&lexical.matched_by),
            }),
            tags: BorrowedTags(record.tags),
            evidence,
        },
    }
}

fn resolve_matches_completion<T>(
    terminal: &TerminalArbiter,
    validate: impl FnOnce() -> Result<T, CliError>,
    observe_omissions: impl FnOnce() -> Result<Option<OversizedSentenceOmissionSummary>, ExecutionError>,
) -> Result<ValidatedCompletion<T>, CliError> {
    let validated = validate()?;
    if !terminal.try_claim_completion() {
        return Ok(ValidatedCompletion::Cancelled);
    }
    if let Some(summary) = observe_omissions()? {
        return Err(oversized_omissions_error(summary));
    }
    Ok(ValidatedCompletion::Publish(validated))
}

pub fn run(
    args: &MatchesArgs,
    config: &ExecutionCliConfig,
    terminal: &TerminalArbiter,
    output: OutputMode,
    color_mode: ColorMode,
) -> Result<CommandOutcome, CliError> {
    if args.retain.is_some() && output != OutputMode::Jsonl {
        return Err(CliError::InvalidArgs("--retain requires --json".into()));
    }
    if (args.publish_all_attributes || !args.publish_attribute.is_empty())
        && output != OutputMode::Jsonl
    {
        return Err(CliError::InvalidArgs(
            "--publish-attribute and --publish-all-attributes require --json".into(),
        ));
    }
    let context = resolve_match_context(output, args.context);
    let limit = if args.ending_report.is_some() {
        None
    } else {
        resolve_match_limit(output, args.limit)
    };
    if let Some(path) = &args.ending_report
        && path.try_exists()?
    {
        return Err(CliError::InvalidArgs(format!(
            "ending report already exists: {}",
            path.display()
        )));
    }
    let sink_output = match output {
        OutputMode::Jsonl => MatchSinkOutput::Jsonl,
        OutputMode::Tsv => MatchSinkOutput::Tsv,
        OutputMode::Human => {
            let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
            let color = ColorPolicy::resolve(color_mode, no_color, io::stdout().is_terminal());
            let colorfgbg = std::env::var("COLORFGBG").ok();
            let colorterm = std::env::var("COLORTERM").ok();
            let term = std::env::var("TERM").ok();
            let theme = args.context_color_theme.resolve(colorfgbg.as_deref());
            let capability =
                TerminalColorCapability::resolve(colorterm.as_deref(), term.as_deref());
            let layout = if args.align_hits {
                HumanMatchLayout::AlignHits
            } else {
                HumanMatchLayout::Flowing
            };
            let width = terminal_size::terminal_size()
                .map_or(120, |(width, _)| usize::from(width.0))
                .max(MIN_HUMAN_MATCH_WIDTH);
            let palette = ContextColorPalette::new(args.context_color_depth, theme, capability);
            MatchSinkOutput::Human(HumanRenderer::new(
                palette,
                color,
                width,
                layout,
                args.morpheme_separator.clone(),
            ))
        }
    };
    if args.evidence && output == OutputMode::Human {
        return Err(CliError::InvalidArgs(
            "--evidence requires --json or --tsv matches output".to_owned(),
        ));
    }
    if !args.source_metadata.is_empty() && output != OutputMode::Jsonl {
        return Err(CliError::InvalidArgs(
            "--source-metadata requires --json matches output".to_owned(),
        ));
    }
    let metadata_projection = if args.source_metadata.is_empty() {
        None
    } else {
        Some(
            lingrep_core::corpus::SourceMetadataProjection::new(args.source_metadata.clone())
                .map_err(|error| CliError::InvalidArgs(error.to_string()))?,
        )
    };
    let started = Instant::now();
    let catalog =
        load_and_compile_with_vectors(&args.catalog.catalog_input()?, args.vectors.as_deref())?;
    write_vectors_notice(&catalog, config)?;
    let word_vectors = super::query_support::word_vectors_report(&catalog);
    if output == OutputMode::Tsv && !catalog.output.matcher.lexicons().is_empty() {
        return Err(CliError::InvalidArgs(
            "matches --tsv does not accept lexicon definitions; use --json or human output".into(),
        ));
    }
    let descriptors = catalog.authored_definitions()?;
    let catalog_fingerprint = catalog.fingerprint.clone();
    let mut compiled = catalog.output;
    compiled.matcher = compiled.matcher.with_overlaps(args.overlaps.into());

    // Parsed before any input is opened: a clause is authored text, so a bad
    // one is a usage error rather than a run that reads a corpus and then
    // fails.
    let scope_clause = parse_scope(args.selection.clause.as_deref())?;
    let sample = super::query_support::resolve_query_sample(
        &args.sample,
        args.cache.cache,
        args.tsv,
        output,
        QueryMode::Matches,
    )?;

    // Before query execution: direct execution is the default; `--cache`
    // observes ROOT, then reuses or creates a compatible artifact beside it.
    let resolved_corpus = crate::corpus_mode::resolve(
        &args.input,
        &args.cache,
        &args.input.markdown,
        config,
        terminal,
        output,
    )?;
    let selected_corpus = resolved_corpus.corpus.as_deref();
    let char_offsets_available = output != OutputMode::Tsv;
    let mut projected_attributes = cached_run_attributes(&compiled.matcher);
    if output == OutputMode::Human {
        projected_attributes.push(
            lingrep_core::token_match::AttributeKey::new("conllu.misc")
                .expect("conllu.misc is a canonical attribute"),
        );
        projected_attributes.sort_unstable();
        projected_attributes.dedup();
    }
    if args.ending_report.is_some() {
        projected_attributes
            .push(lingrep_core::token_match::AttributeKey::new("upos").expect("canonical key"));
        projected_attributes.sort_unstable();
        projected_attributes.dedup();
    }
    let mut published_attributes = projected_attributes.clone();
    // Read by the rendered surface and context; a schema without it drops it.
    projected_attributes.push(
        lingrep_core::token_match::AttributeKey::new("surface")
            .expect("surface is a canonical attribute"),
    );
    projected_attributes.sort_unstable();
    projected_attributes.dedup();
    if args.evidence {
        projected_attributes.extend(EVIDENCE_ATTRIBUTES.iter().map(|key| {
            lingrep_core::token_match::AttributeKey::new(key).expect("canonical evidence key")
        }));
        projected_attributes.sort_unstable();
        projected_attributes.dedup();
    }
    let session = MatchingQuerySession::open(
        MatchingQuerySpec {
            input: &args.input,
            selected_corpus,
            compiled: &compiled,
            projection: QueryProjection {
                selected: &projected_attributes,
                additional: &args.publish_attribute,
                all: args.publish_all_attributes,
            },
            scope_clause: scope_clause.as_ref(),
            mode: QueryMode::Matches,
            char_offsets: true,
            metadata_projection: metadata_projection.clone(),
            block_sample: sample.filter(|_| args.sample.blocks),
            allow_selective: args.ending_report.is_none()
                && limit.is_none()
                && sample.is_none()
                && scope_clause.is_none(),
            needs_provenance: false,
        },
        config,
        terminal,
    )?;
    if args.publish_all_attributes {
        published_attributes.extend_from_slice(session.bound.projection().keys());
    } else {
        published_attributes.extend(
            args.publish_attribute
                .iter()
                .filter(|key| session.bound.projection().keys().contains(key))
                .cloned(),
        );
    }
    let published_projection = PublishedProjection::new(&published_attributes);
    let graph_scalars = args
        .evidence
        .then(|| published_projection.attributes().to_vec());
    let run_corpus = session.lifecycle.run_corpus();
    let run_inventory = session.lifecycle.run_inventory();
    let run_id = session.run_id.clone();
    let conllu_spacing = session
        .bound
        .projection()
        .ordinal_key("conllu.misc")
        .is_some();
    session.scope.warn_before_run(
        scope_clause.as_ref(),
        &args.input.markdown.self_label_no_ops(),
        config,
        &run_id,
    )?;
    let run = QueryEnvelope::new(QueryEvent::Run(Box::new(QueryRun {
        pattern_metadata: None,
        matcher_semantics_identity: lingrep_core::fsm::MATCHER_SEMANTICS_IDENTITY.to_owned(),
        run_id: run_id.clone(),
        mode: QueryMode::Matches,
        catalog_fingerprint,
        definition_count: descriptors.len() as u64,
        analyzer: session.run.analyzer.clone(),
        corpus: run_corpus,
        dictionary_identity: session.run.dictionary_identity.clone(),
        reader_identity: session.run.reader_identity.clone(),
        token_schema_identity: session.run.token_schema_identity.clone(),
        published_attributes: published_projection.names().to_vec(),
        published_source_metadata: metadata_projection
            .as_ref()
            .map_or_else(Vec::new, |projection| projection.keys().to_vec()),
        sample: session
            .run
            .block_sample_report
            .clone()
            .or_else(|| sample.map(crate::protocol::query::SampleReport::of)),
        overlaps: crate::protocol::query::OverlapsReport::of(args.overlaps),
        word_vectors,
        ordering: MatchOrder::CorpusSentenceStartEndPatternCapturesGraphBindingsTerm,
        span_convention: SpanConvention::HalfOpenToken,
        char_offsets: char_offsets_available,
        requested_context: context as u64,
        requested_limit: limit,
        requested_top: None,
        by_span: None,
        execution_limits: protocol_execution_limits(config),
        temporary_storage_limits: TemporaryStorageLimits {
            max_temp_bytes: config.max_temp_bytes,
        },
        traversal_limits: session.run.traversal_limits.clone(),
        inventory: run_inventory,
        worker_count: config.workers as u64,
    })));

    let post_run = (|| -> Result<CommandOutcome, CliError> {
        let stdout = io::stdout();
        let mut writer = io::BufWriter::with_capacity(256 * 1024, stdout.lock());
        if output == OutputMode::Jsonl {
            publish_run_and_definitions(&mut writer, &run, &descriptors)?;
        }

        let mut sink = DirectMatchSink {
            last_human_flush: None,
            writer: &mut writer,
            output: sink_output,
            requested_context: context,
            limit,
            projection: published_projection.clone(),
            char_offsets: char_offsets_available,
            conllu_spacing,
            offsets_omitted: 0,
            sequence: 0,
            emitted: 0,
            last_committed: None,
            scratch: Vec::new(),
            surface_scratch: String::new(),
            evidence: graph_scalars,
            endings: args
                .ending_report
                .as_ref()
                .map(|_| super::ending_report::EndingReport::default()),
            retention: args
                .retain
                .map(|capacity| MatchRetention::new(capacity, args.retain_seed)),
        };
        // The seam itself is `execute`'s; this surface only chooses.
        let selection = if args.raw_matches {
            PolicySelection::Raw
        } else {
            PolicySelection::Apply
        };
        // `--trace-policy 0` prints only the run totals, which every run counts.
        let publication = match args.trace_policy {
            Some(capacity) if capacity > 0 => Publication::from(selection).with_trace(capacity),
            _ => Publication::from(selection),
        };
        let progress_mode = config.progress;
        let presentation_progress = PresentationProgress::stderr(
            &config.stderr,
            progress_mode,
            output,
            ResultDelivery::Streaming,
        );
        if progress_mode == ProgressMode::Always {
            presentation_progress.write_line(&format!(
                "publishing token attributes: {}",
                published_projection.names().join(", ")
            ));
        }
        let progress_writer = presentation_progress.clone();
        let mut progress =
            move |progress: lingrep_core::execution::ExecutionProgress,
                  _sink: &DirectMatchSink<'_, io::BufWriter<io::StdoutLock<'_>>>| {
                let (sentences, matches) = (progress.sentences_matched, progress.matches_found);
                progress_writer.snapshot(move || {
                    format!("matched {sentences} sentences, found {matches} matches")
                });
            };
        let completed = match session.execute(
            sample.filter(|_| !args.sample.blocks),
            publication,
            config,
            terminal,
            &mut sink,
            &mut progress,
        )? {
            MatchingRunOutcome::Completed(completed) => *completed,
            MatchingRunOutcome::Cancelled => return Ok(CommandOutcome::Cancelled),
        };
        presentation_progress.finish();
        let limited = limit.is_some_and(|limit| sink.emitted == limit);
        completed.metrics.write_post_run_notices(
            &compiled,
            config,
            limited,
            None,
            args.trace_policy.is_some(),
        )?;
        let scope_report = completed.metrics.scope.report_after_run(
            scope_clause.as_ref(),
            limited,
            config,
            &run_id,
        )?;
        let coverage = if limited {
            Coverage::Prefix
        } else {
            Coverage::Full
        };
        let termination = if limited {
            Termination::ResultLimit
        } else {
            Termination::Eof
        };
        let completion = resolve_matches_completion(
            terminal,
            || {
                completed.lifecycle.complete(if limited {
                    FilesystemCompletion::PrefixUnavailable
                } else {
                    FilesystemCompletion::Full
                })
            },
            || {
                completed
                    .metrics
                    .source_stats
                    .oversized_sentence_omissions()
            },
        )?;
        let (complete_corpus, inventory) = match completion {
            ValidatedCompletion::Cancelled => return Ok(CommandOutcome::Cancelled),
            ValidatedCompletion::Publish(completed) => completed,
        };
        let ending_corpus = complete_corpus.clone();
        if output == OutputMode::Jsonl {
            let retained_matches = sink.publish_retained()?;
            // `matches` drops nothing, but it does publish the `key` field, so a
            // record whose declared key could not be projected is published without
            // one. That is the same unkeyable-record disposition `summarize` counts,
            // and reporting a hardcoded zero here would have made the absent field
            // look like "no key was declared".
            // `offsets_omitted` is only meaningful on a run that declares offsets:
            // the sink leaves it zero otherwise.
            // Computed after `publish_retained`, which updates `sink.emitted`
            // to the retained match count when `--retain` is active.
            let counters = completed.metrics.run_counters(
                sink.emitted,
                completed.metrics.report.policy_stats.records_unkeyed,
                sink.offsets_omitted,
            )?;
            let resources = completed.metrics.resource_high_water(0, config);
            publish_query_jsonl(
                sink.writer,
                &QueryEnvelope::new(QueryEvent::Complete(QueryComplete {
                    grouping_coverage: None,
                    retained_matches,
                    coverage,
                    corpus: complete_corpus,
                    counters,
                    elapsed_millis: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                    resources,
                    committed_prefix: (coverage == Coverage::Prefix)
                        .then(|| sink.last_committed.clone())
                        .flatten(),
                    inventory,
                    termination,
                    scope: scope_report.clone(),
                })),
            )?;
        }
        sink.writer.flush()?;
        if let (Some(path), Some(endings)) = (&args.ending_report, &sink.endings) {
            endings.write(path, &run, &ending_corpus, scope_report.as_ref())?;
        }
        Ok(CommandOutcome::Success)
    })();
    post_run.map_err(|error| error.with_run_id(run_id, None))
}

#[expect(
    clippy::large_enum_variant,
    reason = "One sink per query owns the fixed renderer; its size does not grow with matches."
)]
enum MatchSinkOutput {
    Jsonl,
    Tsv,
    Human(HumanRenderer),
}

fn human_tokens(
    sentence: &dyn lingrep_core::token_match::TokenSequence,
    span: std::ops::Range<usize>,
    location: &lingrep_core::execution::SentenceLocation,
    conllu_spacing: bool,
) -> Result<Vec<String>, crate::direct_source::MissingRequiredAttribute> {
    use lingrep_core::token_match::{AttributeKey, AttributeValueRef, source_space_before};
    let mut tokens = matched_orth_tokens(sentence, span.clone(), location)?;
    let boundaries = tokens.len().saturating_sub(1);
    let misc = AttributeKey::new("conllu.misc").expect("conllu.misc is canonical");
    for (offset, token) in tokens.iter_mut().take(boundaries).enumerate() {
        let index = span.start + offset;
        let spaced = source_space_before(sentence, index + 1).unwrap_or_else(|| {
            conllu_spacing
                && !matches!(
                    sentence.value_key(index, &misc),
                    Some(AttributeValueRef::StringSet(values))
                        if values.iter().any(|value| value == "SpaceAfter=No")
                )
        });
        if spaced {
            token.push(' ');
        }
    }
    Ok(tokens)
}

/// A token span's code-point range, or `None` when the sentence cannot place
/// either end.
///
/// All-or-nothing on purpose. A span whose first token has an offset and whose
/// last does not would otherwise publish a range ending where the match does
/// not, and a half-known position is the one answer worse than none — the same
/// rule that drops a capture leaving a rewritten span rather than clamping it.
fn span_char_range(
    sentence: &dyn lingrep_core::token_match::TokenSequence,
    span: std::ops::Range<usize>,
) -> Option<ProtocolCharSpan> {
    if span.start >= span.end {
        return None;
    }
    let first = sentence.char_span(span.start)?;
    let last = sentence.char_span(span.end.checked_sub(1)?)?;
    Some(first.join(last).into())
}

/// The token attributes an evidence word reports, in its field order.
const EVIDENCE_ATTRIBUTES: [&str; 7] = [
    "text", "lemma", "norm", "upos", "xpos", "dep.head", "dep.rel",
];

/// The sentence and the words a record rests on: the span, every capture, and
/// every word a graph binding names, in sentence order.
fn match_evidence(
    sentence: &dyn lingrep_core::token_match::TokenSequence,
    record: &RecordView<'_>,
    location: &lingrep_core::execution::SentenceLocation,
) -> Result<MatchEvidence, ExecutionError> {
    use lingrep_core::corpus::conllu::EnhancedNodeId;
    use lingrep_core::token_match::AttributeKey;
    use std::collections::BTreeMap;

    let mut roles: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for index in record.start..record.end {
        roles.entry(index).or_default();
    }
    for capture in record.captures {
        for index in capture.start..capture.end {
            roles
                .entry(index)
                .or_default()
                .push(capture.name.to_string());
        }
    }
    let mut bind = |node: EnhancedNodeId, role: String| {
        if let EnhancedNodeId::Word { token_index } = node {
            roles.entry(token_index).or_default().push(role);
        }
    };
    for binding in record.graph_bindings {
        match binding {
            lingrep_core::fsm::GraphBinding::Count { .. }
            | lingrep_core::fsm::GraphBinding::Measure { .. } => {}
            lingrep_core::fsm::GraphBinding::Node { name, node, .. } => {
                bind(*node, name.to_string())
            }
            lingrep_core::fsm::GraphBinding::Edge {
                name,
                head,
                dependent,
                ..
            } => {
                bind(*head, format!("{name}.head"));
                bind(*dependent, format!("{name}.dependent"));
            }
        }
    }
    let keys =
        EVIDENCE_ATTRIBUTES.map(|key| AttributeKey::new(key).expect("canonical evidence key"));
    let words = roles
        .into_iter()
        .filter(|(index, _)| *index < sentence.len())
        .map(|(index, mut roles)| {
            roles.sort_unstable();
            roles.dedup();
            let [text, lemma, norm, upos, xpos, head, rel] = keys
                .each_ref()
                .map(|key| sentence.scalar_key(index, key).map(ToOwned::to_owned));
            EvidenceWord {
                word: index as u64 + 1,
                roles,
                in_span: (record.start..record.end).contains(&index),
                text,
                lemma,
                norm,
                upos,
                xpos,
                head,
                rel,
            }
        })
        .collect();
    Ok(MatchEvidence {
        spans: sentence
            .spans()
            .iter()
            .map(|span| crate::protocol::query::EvidenceSpan {
                kind: span.kind.clone(),
                start: span.start as u64,
                end: span.end as u64,
                head: span.head.map(|head| head as u64),
                label: span.label.clone(),
                governor: span.governor.map(|id| id as u64),
            })
            .collect(),
        sentence: matched_surface(sentence, 0..sentence.len(), location)
            .map_err(ExecutionError::from)?,
        words,
    })
}

struct DirectMatchSink<'a, W> {
    last_human_flush: Option<Instant>,
    writer: &'a mut W,
    output: MatchSinkOutput,
    requested_context: usize,
    limit: Option<u64>,
    projection: PublishedProjection,
    sequence: u64,
    emitted: u64,
    /// Whether this run's source derives character offsets at all.
    ///
    /// Held so the sink can tell a record that lost its offsets from a run that
    /// never had any: only the first is worth counting, and counting the second
    /// would report every record of a Markdown run as an omission.
    char_offsets: bool,
    conllu_spacing: bool,
    offsets_omitted: u64,
    last_committed: Option<CommittedPrefix>,
    /// Reused serialization scratch for the JSONL record stream.
    ///
    /// One buffer for the whole run rather than one allocation per record;
    /// see [`crate::protocol::write_jsonl_buffered`].
    scratch: Vec<u8>,
    /// Reused surface text buffer for streaming JSONL publication.
    surface_scratch: String,
    /// Present under `--evidence`: the attributes the run projected before
    /// evidence widened it; see [`protocol_graph_binding`].
    evidence: Option<Vec<lingrep_core::token_match::AttributeKey>>,
    endings: Option<super::ending_report::EndingReport>,
    retention: Option<MatchRetention>,
}

struct MatchRetention {
    capacity: u32,
    seed: u64,
    seen: u64,
    bytes: usize,
    records: Vec<(u64, Vec<u8>, bool)>,
}

impl MatchRetention {
    const MAX_BYTES: usize = 64 * 1024 * 1024;

    fn new(capacity: u32, seed: u64) -> Self {
        Self {
            capacity,
            seed,
            seen: 0,
            bytes: 0,
            records: Vec::new(),
        }
    }

    fn draw_slot(&mut self) -> Result<Option<usize>, ExecutionError> {
        self.seen = self
            .seen
            .checked_add(1)
            .ok_or(ExecutionError::ResourceAccounting)?;
        let draw = lingrep_core::execution::reservoir_draw(self.seed, 0, self.seen) % self.seen;
        if self.records.len() < self.capacity as usize {
            Ok(Some(self.records.len()))
        } else if draw < u64::from(self.capacity) {
            Ok(Some(draw as usize))
        } else {
            Ok(None)
        }
    }

    fn store_slot<E: Serialize>(
        &mut self,
        slot: usize,
        sequence: u64,
        event: &E,
        offsets_missing: bool,
    ) -> Result<(), ExecutionError> {
        let bytes = serde_json::to_vec(event).map_err(|error| ExecutionError::InvalidLimit {
            field: "retain",
            message: error.to_string(),
        })?;
        let old = self
            .records
            .get(slot)
            .map_or(0, |(_, bytes, _)| bytes.len());
        let total = self
            .bytes
            .checked_sub(old)
            .and_then(|n| n.checked_add(bytes.len()))
            .ok_or(ExecutionError::ResourceAccounting)?;
        if total > Self::MAX_BYTES {
            return Err(ExecutionError::InvalidLimit {
                field: "retain",
                message: "retained matches exceed 64 MiB; reduce --retain or --context".into(),
            });
        }
        self.bytes = total;
        if slot == self.records.len() {
            self.records.push((sequence, bytes, offsets_missing));
        } else {
            self.records[slot] = (sequence, bytes, offsets_missing);
        }
        Ok(())
    }
}

impl<W: Write> DirectMatchSink<'_, W> {
    fn flush_human_row(&mut self, now: Instant) -> Result<(), ExecutionError> {
        if self
            .last_human_flush
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_millis(100))
        {
            self.writer.flush().map_err(execution_output_io)?;
            self.last_human_flush = Some(now);
        }
        Ok(())
    }

    fn flush_human_sentence(
        &mut self,
        now: impl FnOnce() -> Instant,
    ) -> Result<(), ExecutionError> {
        if matches!(self.output, MatchSinkOutput::Human(_)) {
            self.writer.flush().map_err(execution_output_io)?;
            self.last_human_flush = Some(now());
        }
        Ok(())
    }

    fn publish_retained(
        &mut self,
    ) -> Result<Option<crate::protocol::query::RetainedMatches>, CliError> {
        let Some(retention) = self.retention.as_mut() else {
            return Ok(None);
        };
        retention
            .records
            .sort_unstable_by_key(|(sequence, _, _)| *sequence);
        for (_, bytes, _) in &retention.records {
            self.writer.write_all(bytes)?;
            self.writer.write_all(b"\n")?;
        }
        self.emitted = retention.records.len() as u64;
        self.offsets_omitted = retention
            .records
            .iter()
            .filter(|(_, _, missing)| *missing)
            .count() as u64;
        Ok(Some(crate::protocol::query::RetainedMatches {
            algorithm: "algorithm_r_splitmix64_v1".into(),
            unit: "published_match".into(),
            seed: retention.seed,
            capacity: retention.capacity,
            max_bytes: MatchRetention::MAX_BYTES as u64,
            seen: retention.seen,
            retained: self.emitted,
        }))
    }

    /// The one emission path both publication modes drive.
    ///
    /// Taking [`RecordView`] rather than `&Match` or `&PublishedRecord` is what
    /// keeps it one path: the modes differ only in whether `unit`, `key`, and
    /// `tags` exist to write, and every other decision here — format, context,
    /// limit, sequence accounting — is identical. Two copies of this loop would
    /// be two places for the limit to be checked differently.
    fn publish<'r>(
        &mut self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        records: impl Iterator<Item = RecordView<'r>>,
        mut now: impl FnMut() -> Instant,
    ) -> Result<SinkControl, ExecutionError> {
        let mut records_emitted = 0usize;
        for record in records {
            if self.limit.is_some_and(|limit| self.emitted == limit) {
                return Ok(SinkControl::StopAfter { records_emitted });
            }
            let start = record.start.min(sentence.len());
            let end = record.end.min(sentence.len()).max(start);
            let left = start.saturating_sub(self.requested_context)..start;
            let right = end..end
                .saturating_add(self.requested_context)
                .min(sentence.len());
            match &self.output {
                MatchSinkOutput::Jsonl => {
                    matched_surface_into(
                        sentence,
                        record.start..record.end,
                        location,
                        &mut self.surface_scratch,
                    )
                    .map_err(ExecutionError::from)?;
                    let char_span = span_char_range(sentence, record.start..record.end);
                    let offsets_missing = self.char_offsets && char_span.is_none();
                    if offsets_missing {
                        self.offsets_omitted = self.offsets_omitted.saturating_add(1);
                    }
                    let evidence = self
                        .evidence
                        .is_some()
                        .then(|| match_evidence(sentence, &record, location))
                        .transpose()?;
                    if let Some(retention) = self.retention.as_mut() {
                        if let Some(slot) = retention.draw_slot()? {
                            let event = borrowed_match_envelope(
                                self.sequence,
                                &record,
                                location,
                                sentence,
                                start,
                                end,
                                self.requested_context,
                                &self.projection,
                                &self.surface_scratch,
                                char_span,
                                self.evidence.as_deref(),
                                evidence,
                            );
                            retention.store_slot(slot, self.sequence, &event, offsets_missing)?;
                        }
                    } else {
                        let event = borrowed_match_envelope(
                            self.sequence,
                            &record,
                            location,
                            sentence,
                            start,
                            end,
                            self.requested_context,
                            &self.projection,
                            &self.surface_scratch,
                            char_span,
                            self.evidence.as_deref(),
                            evidence,
                        );
                        crate::protocol::write_jsonl_buffered(
                            self.writer,
                            &mut self.scratch,
                            &event,
                        )
                        .map_err(execution_output_error)?;
                    }
                }
                MatchSinkOutput::Tsv if self.evidence.is_some() => {
                    let evidence = match_evidence(sentence, &record, location)?;
                    for word in &evidence.words {
                        write_evidence_tsv(
                            self.writer,
                            &EvidenceTsvRow {
                                sequence: self.sequence,
                                pattern: record.pattern,
                                source_name: &location.source_name,
                                document_ordinal: location.document_ordinal,
                                document_sentence_ordinal: location.document_sentence_ordinal,
                                corpus_sentence_ordinal: location.corpus_sentence_ordinal,
                                match_start: record.start as u64,
                                match_end: record.end as u64,
                                word,
                                sentence: &evidence.sentence,
                            },
                        )
                        .map_err(execution_output_error)?;
                    }
                }
                MatchSinkOutput::Tsv => {
                    let surface = matched_surface(sentence, record.start..record.end, location)
                        .map_err(ExecutionError::from)?;
                    write_match_tsv(
                        self.writer,
                        &MatchTsvRow {
                            sequence: self.sequence,
                            pattern: record.pattern.to_owned(),
                            source_name: location.source_name.to_string(),
                            document_ordinal: location.document_ordinal,
                            document_sentence_ordinal: location.document_sentence_ordinal,
                            corpus_sentence_ordinal: location.corpus_sentence_ordinal,
                            match_start: record.start as u64,
                            match_end: record.end as u64,
                            left_surface: (self.requested_context != 0)
                                .then(|| matched_surface(sentence, left.clone(), location))
                                .transpose()
                                .map_err(ExecutionError::from)?,
                            surface,
                            right_surface: (self.requested_context != 0)
                                .then(|| matched_surface(sentence, right.clone(), location))
                                .transpose()
                                .map_err(ExecutionError::from)?,
                        },
                    )
                    .map_err(execution_output_error)?;
                }
                MatchSinkOutput::Human(renderer) => {
                    if let Some(lexical) = record.lexical {
                        writeln!(
                            self.writer,
                            "term={} matched_by={}",
                            lexical.term,
                            lexical
                                .matched_by
                                .iter()
                                .map(lingrep_core::token_match::AttributeKey::pattern_key)
                                .collect::<Vec<_>>()
                                .join(",")
                        )
                        .map_err(execution_output_io)?;
                    }
                    let row = HumanKwicRow {
                        left: human_tokens(sentence, left.clone(), location, self.conllu_spacing)
                            .map_err(ExecutionError::from)?,
                        keyword: human_tokens(
                            sentence,
                            record.start..record.end,
                            location,
                            self.conllu_spacing,
                        )
                        .map_err(ExecutionError::from)?,
                        right: human_tokens(sentence, right.clone(), location, self.conllu_spacing)
                            .map_err(ExecutionError::from)?,
                    };
                    renderer
                        .match_row(&mut *self.writer, &row)
                        .map_err(execution_output_io)?;
                    self.flush_human_row(now())?;
                }
            }
            self.last_committed = Some(CommittedPrefix {
                corpus_sentence_ordinal: location.corpus_sentence_ordinal,
                sequence: self.sequence,
            });
            self.sequence = self
                .sequence
                .checked_add(1)
                .ok_or(ExecutionError::ResourceAccounting)?;
            self.emitted = self
                .emitted
                .checked_add(1)
                .ok_or(ExecutionError::ResourceAccounting)?;
            records_emitted = records_emitted
                .checked_add(1)
                .ok_or(ExecutionError::ResourceAccounting)?;
            if self.limit.is_some_and(|limit| self.emitted == limit) {
                self.flush_human_sentence(&mut now)?;
                return Ok(SinkControl::StopAfter { records_emitted });
            }
        }
        if records_emitted != 0 {
            self.flush_human_sentence(now)?;
        }
        Ok(SinkControl::Continue { records_emitted })
    }
}

impl<W: Write> MatchSink for DirectMatchSink<'_, W> {
    fn push(
        &mut self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        if let Some(endings) = &mut self.endings {
            endings.observe(location, sentence, matches)?;
        }
        self.publish(
            location,
            sentence,
            matches.iter().map(RecordView::raw),
            Instant::now,
        )
    }

    fn push_records(
        &mut self,
        location: &lingrep_core::execution::SentenceLocation,
        sentence: &dyn lingrep_core::token_match::TokenSequence,
        records: Vec<PublishedRecord>,
    ) -> Result<SinkControl, ExecutionError> {
        if let Some(endings) = &mut self.endings {
            let matches = records
                .iter()
                .cloned()
                .map(PublishedRecord::into_match)
                .collect::<Vec<_>>();
            endings.observe(location, sentence, &matches)?;
        }
        self.publish(
            location,
            sentence,
            records.iter().map(RecordView::published),
            Instant::now,
        )
    }

    fn finish_batch(&mut self) -> Result<(), ExecutionError> {
        self.writer.flush().map_err(execution_output_io)
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::query_support::token_schema_mismatch;
    use lingrep_core::corpus::CorpusReadConfig;
    use lingrep_corpus::ReaderIdentity;
    use std::cell::Cell;
    use std::sync::Arc;

    use super::*;
    use lingrep_core::token_match::TokenInfo;

    /// A counting writer tracking bytes written, write count, and flush count.
    #[derive(Default)]
    struct CountingWriter {
        bytes: Vec<u8>,
        writes: usize,
        flushes: usize,
        flush_sizes: Vec<usize>,
        fail_flush: Option<(usize, io::ErrorKind)>,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            self.flush_sizes.push(self.bytes.len());
            if let Some((at, kind)) = self.fail_flush
                && self.flushes == at
            {
                return Err(kind.into());
            }
            Ok(())
        }
    }

    fn sink_test_location() -> lingrep_core::execution::SentenceLocation {
        lingrep_core::execution::SentenceLocation {
            lookaround_divisions: None,
            source_metadata: None,
            corpus_sentence_ordinal: 3,
            document_id: lingrep_core::corpus::DocumentId::new(7),
            document_ordinal: 11,
            document_sentence_ordinal: 1,
            source_name: Arc::from("doc.md"),
        }
    }

    fn sink_test_sentence() -> Vec<TokenInfo> {
        ["形態", "素", "解析"]
            .into_iter()
            .map(|orth| {
                let mut token = TokenInfo::default();
                token.set(
                    lingrep_core::token_match::TokenAttributeIndex::Orth,
                    orth.into(),
                );
                token
            })
            .collect()
    }

    fn sink_test_matches(count: usize) -> Vec<Match> {
        (0..count)
            .map(|index| Match {
                pattern: "p".into(),
                start: index % 3,
                end: index % 3 + 1,
                captures: Box::new([]),
                graph_bindings: Box::default(),
                interpretations: Box::default(),
                lexical: None,
            })
            .collect()
    }

    fn sink_over<'a, W: Write>(
        writer: &'a mut W,
        output: MatchSinkOutput,
    ) -> DirectMatchSink<'a, W> {
        DirectMatchSink {
            last_human_flush: None,
            writer,
            output,
            requested_context: 0,
            limit: None,
            projection: PublishedProjection::everything(),
            char_offsets: false,
            conllu_spacing: false,
            offsets_omitted: 0,
            sequence: 0,
            emitted: 0,
            last_committed: None,
            scratch: Vec::new(),
            surface_scratch: String::new(),
            evidence: None,
            endings: None,
            retention: None,
        }
    }

    fn sink_test_human_output() -> MatchSinkOutput {
        MatchSinkOutput::Human(HumanRenderer::new(
            ContextColorPalette::new(
                5,
                crate::output::ResolvedContextColorTheme::Dark,
                TerminalColorCapability::Ansi16,
            ),
            ColorPolicy::resolve(ColorMode::Never, true, false),
            80,
            HumanMatchLayout::Flowing,
            "".parse().unwrap(),
        ))
    }

    /// The record stream owes durability at the **logical batch seam**, not at
    /// each record, and that is where the publication win came from: a
    /// per-record flush turns every record into its own `write(2)` and defeats
    /// the writer's buffer. Measured on `patterns/distributions.lx` over the
    /// tracked corpus, per-record flushing cost 5.6s of the run's 6.0s of
    /// system time and 38% of its wall clock.
    ///
    /// Byte-for-byte, the stream is the one `write_jsonl` produced; only the
    /// flush cadence differs. `matches_jsonl_bytes_are_unchanged_by_buffering`
    /// below pins the bytes.
    #[test]
    fn matches_jsonl_records_flush_only_at_the_batch_seam() {
        let location = sink_test_location();
        let sentence = sink_test_sentence();
        let matches = sink_test_matches(8);
        let mut writer = CountingWriter::default();
        {
            let mut sink = sink_over(&mut writer, MatchSinkOutput::Jsonl);
            sink.push(&location, &sentence, &matches)
                .expect("published");
            assert_eq!(
                sink.writer.flushes, 0,
                "eight records must not ask for eight flushes"
            );
            sink.finish_batch().expect("batch finished");
        }
        assert_eq!(writer.flushes, 1, "the batch seam is the durability point");
        assert_eq!(
            writer.bytes.iter().filter(|byte| **byte == b'\n').count(),
            8,
            "one line per record still reaches the destination"
        );
    }

    #[test]
    fn human_rows_flush_first_result_and_complete_sentences_including_limits() {
        let location = sink_test_location();
        let sentence = sink_test_sentence();
        let matches = sink_test_matches(5);
        for limit in [None, Some(0), Some(2)] {
            let mut writer = io::BufWriter::new(CountingWriter::default());
            let mut sink = sink_over(&mut writer, sink_test_human_output());
            sink.limit = limit;
            let now = Instant::now();
            sink.publish(
                &location,
                &sentence,
                matches.iter().map(RecordView::raw),
                || now,
            )
            .expect("published");
            let output = sink.writer.get_ref();
            if limit == Some(0) {
                assert_eq!(output.flushes, 0);
                assert!(output.bytes.is_empty());
                continue;
            }
            assert_eq!(output.flushes, 2);
            assert_eq!(
                output.bytes[..output.flush_sizes[0]]
                    .iter()
                    .filter(|&&b| b == b'\n')
                    .count(),
                1
            );
            assert_eq!(
                output.bytes.iter().filter(|&&b| b == b'\n').count(),
                limit.unwrap_or(5) as usize
            );
            assert!(sink.writer.buffer().is_empty());
        }
    }

    #[test]
    fn human_rows_flush_at_the_deadline_and_empty_sentences_do_not_flush() {
        let location = sink_test_location();
        let sentence = sink_test_sentence();
        let matches = sink_test_matches(5);
        let mut writer = CountingWriter::default();
        let mut sink = sink_over(&mut writer, sink_test_human_output());
        let start = Instant::now();
        let mut times = [0, 99, 100, 199, 200]
            .into_iter()
            .chain(std::iter::repeat(200));
        sink.publish(
            &location,
            &sentence,
            matches.iter().map(RecordView::raw),
            || {
                start
                    .checked_add(Duration::from_millis(times.next().unwrap()))
                    .unwrap()
            },
        )
        .unwrap();
        let rows_at_flush = sink
            .writer
            .flush_sizes
            .iter()
            .map(|&end| {
                sink.writer.bytes[..end]
                    .iter()
                    .filter(|&&byte| byte == b'\n')
                    .count()
            })
            .collect::<Vec<_>>();
        assert_eq!(rows_at_flush, [1, 3, 5, 5]);
        sink.publish(&location, &sentence, std::iter::empty(), || {
            panic!("empty sentence clock read")
        })
        .unwrap();
        assert_eq!(sink.writer.flushes, 4);
    }

    #[test]
    fn human_flush_failures_preserve_io_error_kinds() {
        let location = sink_test_location();
        let sentence = sink_test_sentence();
        let matches = sink_test_matches(5);
        for at in [1, 2] {
            for kind in [
                io::ErrorKind::BrokenPipe,
                io::ErrorKind::WouldBlock,
                io::ErrorKind::Other,
            ] {
                let mut writer = CountingWriter {
                    fail_flush: Some((at, kind)),
                    ..Default::default()
                };
                let mut sink = sink_over(&mut writer, sink_test_human_output());
                let now = Instant::now();
                let error = sink
                    .publish(
                        &location,
                        &sentence,
                        matches.iter().map(RecordView::raw),
                        || now,
                    )
                    .unwrap_err();
                assert!(
                    matches!(error, ExecutionError::Output { kind: actual, .. } if actual == kind)
                );
                assert_eq!(sink.emitted, if at == 1 { 0 } else { 5 });
            }
        }
    }

    /// The buffered record path and the flushing one produce the same bytes.
    /// Two spellings of the same wire format is exactly how a format silently
    /// drifts, so this compares them directly rather than trusting review.
    #[test]
    fn matches_jsonl_bytes_are_unchanged_by_buffering() {
        let location = sink_test_location();
        let sentence = sink_test_sentence();
        let matches = sink_test_matches(4);

        let mut buffered = CountingWriter::default();
        {
            let mut sink = sink_over(&mut buffered, MatchSinkOutput::Jsonl);
            sink.push(&location, &sentence, &matches)
                .expect("published");
        }

        let mut per_record = CountingWriter::default();
        {
            let mut sink = sink_over(&mut per_record, MatchSinkOutput::Jsonl);
            for one in &matches {
                sink.publish(
                    &location,
                    &sentence,
                    std::iter::once(RecordView::raw(one)),
                    Instant::now,
                )
                .expect("published");
            }
        }
        assert_eq!(
            String::from_utf8(buffered.bytes).expect("utf-8"),
            String::from_utf8(per_record.bytes).expect("utf-8"),
        );
    }

    #[test]
    fn retained_offset_omissions_count_only_emitted_records() {
        let mut writer = CountingWriter::default();
        let mut sink = sink_over(&mut writer, MatchSinkOutput::Jsonl);
        sink.char_offsets = true;
        sink.retention = Some(MatchRetention::new(5, 7));
        sink.push(
            &sink_test_location(),
            &sink_test_sentence(),
            &sink_test_matches(20),
        )
        .unwrap();
        let summary = sink.publish_retained().unwrap().unwrap();
        assert_eq!(summary.seen, 20);
        assert_eq!(sink.emitted, 5);
        assert_eq!(sink.offsets_omitted, 5);
    }

    #[test]
    fn retention_byte_budget_fails_without_publishing_partial_evidence() {
        let location = sink_test_location();
        let mut token = TokenInfo::default();
        token.set(
            lingrep_core::token_match::TokenAttributeIndex::Orth,
            "a".repeat(1024 * 1024).into(),
        );
        let sentence = vec![token];
        let matches = vec![
            Match {
                pattern: "p".into(),
                start: 0,
                end: 1,
                captures: Box::new([]),
                graph_bindings: Box::default(),
                interpretations: Box::default(),
                lexical: None
            };
            64
        ];
        let mut writer = CountingWriter::default();
        let mut sink = sink_over(&mut writer, MatchSinkOutput::Jsonl);
        sink.retention = Some(MatchRetention::new(100_000, 7));
        let error = sink.push(&location, &sentence, &matches).unwrap_err();
        assert!(matches!(
            error,
            ExecutionError::InvalidLimit {
                field: "retain",
                ..
            }
        ));
        assert!(sink.writer.bytes.is_empty());
        assert!(sink.retention.as_ref().unwrap().bytes <= MatchRetention::MAX_BYTES);
    }

    #[test]
    fn integrity_failure_prevents_cancellation_and_omission_resolution() {
        let terminal = TerminalArbiter::new();
        let omission_read = Cell::new(false);

        let error = resolve_matches_completion(
            &terminal,
            || Err::<(), CliError>(CliError::Execution(ExecutionError::InventoryMismatch)),
            || {
                omission_read.set(true);
                Ok(None)
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CliError::Execution(ExecutionError::InventoryMismatch)
        ));
        assert!(!omission_read.get());
    }

    #[test]
    fn cancellation_prevents_the_omission_read() {
        let terminal = TerminalArbiter::new();
        assert!(terminal.try_cancel(1));

        let outcome = resolve_matches_completion(
            &terminal,
            || Ok("validated"),
            || panic!("cancelled completion must not read omission stats"),
        )
        .unwrap();

        assert_eq!(outcome, ValidatedCompletion::Cancelled);
    }

    #[test]
    fn omission_prevents_publication_after_validation_and_completion_claim() {
        let terminal = TerminalArbiter::new();
        let error = resolve_matches_completion(
            &terminal,
            || Ok("validated"),
            || {
                Ok(Some(OversizedSentenceOmissionSummary {
                    count: 2,
                    first_source_name: "00027.md".into(),
                    first_ordinal: 7,
                    first_actual: 187_965,
                    limit: 65_536,
                }))
            },
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CliError::OversizedSentencesSkipped { count: 2, .. }
        ));
    }

    #[test]
    fn omission_observation_failure_prevents_publication() {
        let terminal = TerminalArbiter::new();
        let error = resolve_matches_completion(
            &terminal,
            || Ok("validated"),
            || Err(ExecutionError::ResourceAccounting),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            CliError::Execution(ExecutionError::ResourceAccounting)
        ));
    }

    fn analyzer_record(family: &str, profile: &str) -> lingrep_corpus::AnalyzerRecord {
        let mut record = lingrep_corpus::AnalyzerRecord::vibrato_fixture();
        record.family = family.to_owned();
        record.profile = profile.to_owned();
        record
    }

    fn declared_digest(family: &str, profile: &str) -> [u8; 32] {
        *analyzer_record(family, profile)
            .declared_token_schema()
            .expect("a native family declares a schema")
            .digest()
            .as_bytes()
    }

    /// The round trip: an artifact this build could have produced is silent.
    ///
    /// Without this the notice could be unconditional and the mismatch test
    /// below would still pass, which would make every prepared run noisy.
    #[test]
    fn an_artifact_built_under_this_token_schema_is_not_reported() {
        let record = analyzer_record("vibrato", "morphology");
        assert_eq!(
            token_schema_mismatch(&record, &declared_digest("vibrato", "morphology")),
            None
        );
    }

    /// The defect this comparison was carrying: a Sudachi artifact was
    /// measured against the *Vibrato* schema, so every one of them was
    /// reported as stale and told to rebuild — which would have produced the
    /// identical artifact and left the notice firing.
    #[test]
    fn a_sudachi_artifact_is_measured_against_the_sudachi_schema() {
        let sudachi = declared_digest("sudachi", "morphology");
        assert_ne!(
            sudachi,
            declared_digest("vibrato", "morphology"),
            "the two schemas must differ, or this test proves nothing"
        );
        assert_eq!(
            token_schema_mismatch(&analyzer_record("sudachi", "morphology"), &sudachi),
            None
        );
    }

    /// An adapter-hosted analyzer declares its schema at handshake time, so
    /// this build has nothing to compare and must claim nothing.
    #[test]
    fn an_adapter_hosted_analyzer_draws_no_claim() {
        assert_eq!(
            token_schema_mismatch(&analyzer_record("external", "morphology"), &[0; 32]),
            None
        );
    }

    /// A differing schema is reported, and the message names the rebuild.
    ///
    /// The recovery is the whole value of the notice: knowing the identities
    /// differ is useless to a researcher who is not told that `corpus build`
    /// is what reconciles them.
    #[test]
    fn an_artifact_built_under_another_token_schema_names_the_rebuild() {
        let message = token_schema_mismatch(&analyzer_record("vibrato", "morphology"), &[0; 32])
            .expect("a differing token schema must be reported");
        assert!(
            message.contains("corpus build"),
            "the notice must name the rebuild, got {message}"
        );
    }

    #[test]
    fn bounded_markdown_identity_does_not_alias_the_historical_direct_or_prepared_fixture() {
        let bounded = ReaderIdentity::derive(
            &CorpusReadConfig::Markdown(lingrep_core::corpus::MarkdownReaderConfig::new(
                lingrep_core::corpus::SectionPolicy::default(),
            )),
            lingrep_core::analysis::NormalizationPolicy::Nfc,
        );
        let mut historical = blake3::Hasher::new();
        historical.update(b"lingrep/corpus-reader/v1");
        historical.update(&0_u64.to_le_bytes());
        historical.update(&[3]);
        let historical =
            lingrep_corpus::ReaderIdentity::from_bytes(*historical.finalize().as_bytes());

        assert_ne!(bounded, historical);
    }

    #[test]
    fn borrowed_match_envelope_is_byte_identical_to_owned_match_data() {
        use crate::commands::query_support::token_evidence;
        use crate::protocol::query::{
            Capture, GraphBinding as ProtocolGraphBinding,
            GraphNodeAttribute as ProtocolGraphNodeAttribute, LexicalMatchData, MatchData,
            ProtocolKeyField, TagField,
        };
        use lingrep_core::corpus::conllu::EnhancedNodeId;
        use lingrep_core::fsm::{
            GraphBinding, GraphNodeAttribute, LexicalMatch, LexicalProjectedKeyField,
            LexicalProjectedKeyValue, ProjectedKeyField,
        };
        use lingrep_core::token_match::{
            AttributeKey, BoundTokenSentence, CharSpan, SentenceSpan, TokenSchema,
        };

        let schema = TokenSchema::from_keys(["text", "surface", "lemma", "conllu.misc"]).unwrap();
        let schema_projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let bound = BoundTokenSentence::from_projected_values(
            &schema_projection,
            4,
            vec![
                Some("猫".into()),
                Some("猫".into()),
                Some("猫".into()),
                Some("SpaceAfter=No".into()),
                Some("が".into()),
                Some("が".into()),
                Some("が".into()),
                None,
                Some("走っ".into()),
                Some("走っ".into()),
                Some("走る".into()),
                None,
                Some("た".into()),
                Some("た".into()),
                Some("た".into()),
                None,
            ],
        )
        .unwrap()
        .with_char_offsets(Some(Arc::new(vec![
            CharSpan { start: 0, end: 1 },
            CharSpan { start: 1, end: 2 },
            CharSpan { start: 2, end: 4 },
            CharSpan { start: 4, end: 5 },
        ])))
        .with_spans(vec![
            SentenceSpan {
                kind: "clause".into(),
                start: 0,
                end: 4,
                head: Some(2),
                label: Some("main".into()),
                governor: None,
            },
            SentenceSpan {
                kind: "chunk".into(),
                start: 1,
                end: 3,
                head: Some(2),
                label: None,
                governor: None,
            },
        ])
        .unwrap();

        let projection = PublishedProjection::new(schema.keys());
        let mut metadata = serde_json::Map::new();
        metadata.insert("genre".into(), serde_json::json!("fiction"));
        let mut location = sink_test_location();
        location.source_metadata = Some(Arc::new(metadata));

        let captures = [lingrep_core::fsm::Capture {
            name: "verb".into(),
            path: "p.verb".into(),
            start: 2,
            end: 3,
        }];
        let graph_bindings = [
            GraphBinding::Measure {
                name: "dist".into(),
                value: lingrep_core::fsm::Measurement::Number {
                    sum: 3,
                    count: std::num::NonZeroU64::new(2).unwrap(),
                },
            },
            GraphBinding::Count {
                name: "deps".into(),
                value: Some(2),
            },
            GraphBinding::Node {
                name: "head".into(),
                node: EnhancedNodeId::Word { token_index: 2 },
                attributes: vec![
                    GraphNodeAttribute {
                        name: AttributeKey::new("lemma").unwrap(),
                        value: "走る".into(),
                    },
                    GraphNodeAttribute {
                        name: AttributeKey::new("upos").unwrap(),
                        value: "VERB".into(),
                    },
                ]
                .into(),
            },
            GraphBinding::Edge {
                name: "rel".into(),
                head: EnhancedNodeId::Word { token_index: 2 },
                dependent: EnhancedNodeId::Empty {
                    boundary: 1,
                    subindex: 1,
                },
                relation: "nsubj".into(),
            },
        ];
        let scalar_key = [ProjectedKeyField {
            name: "k".into(),
            value: "走る".into(),
        }];
        let lexical_key = [
            LexicalProjectedKeyField {
                name: "k1".into(),
                value: LexicalProjectedKeyValue::Scalar("走る".into()),
            },
            LexicalProjectedKeyField {
                name: "k2".into(),
                value: LexicalProjectedKeyValue::Segments(vec!["が".into(), "走っ".into()]),
            },
        ];
        let lexical = LexicalMatch {
            term: "が走る".into(),
            matched_by: vec![AttributeKey::new("lemma").unwrap()].into(),
        };
        let tags = [(compact_str::CompactString::new("t"), "v".to_owned())];
        let published_graph_keys = vec![AttributeKey::new("lemma").unwrap()];

        let check_case = |record: RecordView<'_>,
                          requested_context: usize,
                          published_keys: Option<&[AttributeKey]>| {
            let start = record.start.min(bound.len());
            let end = record.end.min(bound.len()).max(start);
            let left = start.saturating_sub(requested_context)..start;
            let right = end..end.saturating_add(requested_context).min(bound.len());
            let surface = matched_surface(&bound, record.start..record.end, &location).unwrap();
            let char_span = span_char_range(&bound, record.start..record.end);
            let evidence_owned = published_keys
                .is_some()
                .then(|| match_evidence(&bound, &record, &location).unwrap());
            let evidence_borrowed = published_keys
                .is_some()
                .then(|| match_evidence(&bound, &record, &location).unwrap());

            let mut span_ids = Vec::new();
            let mut whole_span_ids = BTreeMap::<String, Vec<u64>>::new();
            for (id, span) in bound.spans().iter().enumerate() {
                if span.start <= record.start && record.end <= span.end {
                    span_ids.push(id as u64);
                    if (span.start, span.end) == (record.start, record.end) {
                        whole_span_ids
                            .entry(span.kind.clone())
                            .or_default()
                            .push(id as u64);
                    }
                }
            }
            let owned = QueryEnvelope::new(QueryEvent::Match(MatchData {
                sequence: 7,
                pattern: record.pattern.to_owned(),
                span: SourceSpan {
                    start: record.start as u64,
                    end: record.end as u64,
                },
                span_ids,
                whole_span_ids,
                char_span,
                captures: record
                    .captures
                    .iter()
                    .map(|capture| Capture {
                        name: capture.name.to_string(),
                        hygienic_path: capture.path.to_string(),
                        span: SourceSpan {
                            start: capture.start as u64,
                            end: capture.end as u64,
                        },
                        char_span: span_char_range(&bound, capture.start..capture.end),
                    })
                    .collect(),
                graph_bindings: record
                    .graph_bindings
                    .iter()
                    .map(|binding| match binding {
                        GraphBinding::Measure { name, value } => ProtocolGraphBinding::Measure {
                            name: name.to_string(),
                            value: value.number().and_then(serde_json::Number::from_f64),
                            status: value.status().into(),
                        },
                        GraphBinding::Count { name, value } => ProtocolGraphBinding::Count {
                            name: name.to_string(),
                            value: *value,
                        },
                        GraphBinding::Node {
                            name,
                            node,
                            attributes,
                        } => ProtocolGraphBinding::Node {
                            name: name.to_string(),
                            node: protocol_graph_node(*node),
                            attributes: attributes
                                .iter()
                                .filter(|attribute| {
                                    graph_node_attribute_published(*node, attribute, published_keys)
                                })
                                .map(|attribute| ProtocolGraphNodeAttribute {
                                    name: attribute.name.pattern_key().to_owned(),
                                    value: attribute.value.to_string(),
                                })
                                .collect(),
                        },
                        GraphBinding::Edge {
                            name,
                            head,
                            dependent,
                            relation,
                        } => ProtocolGraphBinding::Edge {
                            name: name.to_string(),
                            head: protocol_graph_node(*head),
                            dependent: protocol_graph_node(*dependent),
                            relation: relation.to_string(),
                        },
                    })
                    .collect(),
                interpretations: record.interpretations.to_vec(),
                corpus_sentence_ordinal: location.corpus_sentence_ordinal,
                source_name: location.source_name.to_string(),
                source_metadata: location.source_metadata.clone(),
                document_ordinal: location.document_ordinal,
                document_sentence_ordinal: location.document_sentence_ordinal,
                surface: surface.clone(),
                tokens: token_evidence(&bound, start..end, &projection),
                left_context: (requested_context != 0)
                    .then(|| token_evidence(&bound, left, &projection)),
                right_context: (requested_context != 0)
                    .then(|| token_evidence(&bound, right, &projection)),
                unit: record.unit.map(ToOwned::to_owned),
                key: record
                    .key
                    .map(|fields| {
                        fields
                            .iter()
                            .map(|field| ProtocolKeyField::Scalar {
                                name: field.name.to_string(),
                                value: field.value.clone(),
                            })
                            .collect()
                    })
                    .or_else(|| {
                        record.lexical_key.map(|fields| {
                            fields
                                .iter()
                                .map(|field| match &field.value {
                                    LexicalProjectedKeyValue::Scalar(value) => {
                                        ProtocolKeyField::Scalar {
                                            name: field.name.to_string(),
                                            value: value.clone(),
                                        }
                                    }
                                    LexicalProjectedKeyValue::Segments(values) => {
                                        ProtocolKeyField::List {
                                            name: field.name.to_string(),
                                            values: values.clone(),
                                        }
                                    }
                                })
                                .collect()
                        })
                    }),
                lexical: record.lexical.map(|lexical| LexicalMatchData {
                    term: lexical.term.to_string(),
                    matched_by: lexical
                        .matched_by
                        .iter()
                        .map(|attribute| attribute.pattern_key().to_owned())
                        .collect(),
                }),
                tags: record
                    .tags
                    .iter()
                    .map(|(name, value)| TagField {
                        name: name.to_string(),
                        value: value.clone(),
                    })
                    .collect(),
                evidence: evidence_owned,
            }));

            let borrowed = borrowed_match_envelope(
                7,
                &record,
                &location,
                &bound,
                start,
                end,
                requested_context,
                &projection,
                &surface,
                char_span,
                published_keys,
                evidence_borrowed,
            );
            let owned_json = serde_json::to_string(&owned).unwrap();
            let borrowed_json = serde_json::to_string(&borrowed).unwrap();
            assert_eq!(borrowed_json, owned_json);
            let round_tripped: QueryEnvelope = serde_json::from_str(&borrowed_json).unwrap();
            assert_eq!(round_tripped, owned);
        };

        // Case 1: fully populated with scalar key, context, and evidence narrowing.
        check_case(
            RecordView {
                pattern: "p",
                definition: "p",
                start: 1,
                end: 3,
                captures: &captures,
                graph_bindings: &graph_bindings,
                interpretations: &[],
                unit: Some("が走っ"),
                key: Some(&scalar_key),
                lexical_key: None,
                lexical: Some(&lexical),
                tags: &tags,
            },
            2,
            Some(&published_graph_keys),
        );

        // Case 2: lexical key (scalar + segments), no context, no evidence.
        check_case(
            RecordView {
                pattern: "lex",
                definition: "lex",
                start: 0,
                end: 2,
                captures: &[],
                graph_bindings: &[],
                interpretations: &[],
                unit: None,
                key: None,
                lexical_key: Some(&lexical_key),
                lexical: Some(&lexical),
                tags: &[],
            },
            0,
            None,
        );
    }
}
