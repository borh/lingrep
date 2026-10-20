use std::collections::BTreeMap;

use compact_str::CompactString;
use lingrep_core::scope::{ExclusionReason, Verdict};
use lingrep_core::token_match::{
    AttributeKey, AttributeKind, AttributeValueRef, TokenAttributeIndex, TokenInfo, TokenSchema,
    TokenSequence,
};
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

pub const SCHEMA: &str = super::ContractId::Query.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamPhase, StreamShape, json_contract};
    json_contract::<QueryEnvelope>(
        super::ContractId::Query,
        &["matches", "summarize"],
        "Canonical query run, definition, result, and completion records.",
        StreamShape::Phased {
            phases: &[
                StreamPhase {
                    name: "run",
                    events: &["run"],
                    min: 1,
                    max: Some(1),
                },
                StreamPhase {
                    name: "definitions",
                    events: &["definition"],
                    min: 0,
                    max: None,
                },
                StreamPhase {
                    name: "results",
                    events: &["match", "summary"],
                    min: 0,
                    max: None,
                },
                StreamPhase {
                    name: "complete",
                    events: &["complete"],
                    min: 1,
                    max: Some(1),
                },
            ],
        },
        Channel::Stdout,
        "Canonical corpus and authored-definition order.",
        "Complete scan counts with full, retained, or prefix evidence as declared.",
        "A closed pipe is a machine-output failure.",
        "0 complete, 1 runtime/output failure, 2 invocation failure, 128+signal on cancellation.",
        &[
            "matches/catalog-file-direct-root",
            "summarize/catalog-file-direct-root",
        ],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueryEnvelope {
    #[serde(deserialize_with = "deserialize_schema")]
    pub schema: String,
    #[serde(flatten)]
    pub event: QueryEvent,
}

fn deserialize_schema<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let schema = String::deserialize(deserializer)?;
    if schema == SCHEMA {
        Ok(schema)
    } else {
        Err(D::Error::custom("wrong query schema"))
    }
}

impl QueryEnvelope {
    pub fn new(event: QueryEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum QueryEvent {
    Run(Box<QueryRun>),
    Definition(DefinitionData),
    Match(MatchData),
    Summary(SummaryData),
    Complete(QueryComplete),
}

impl QueryEvent {
    pub(crate) const fn is_complete(&self) -> bool {
        matches!(self, Self::Complete(_))
    }
    pub(crate) const fn type_name(&self) -> &'static str {
        match self {
            Self::Run(_) => "run",
            Self::Definition(_) => "definition",
            Self::Match(_) => "match",
            Self::Summary(_) => "summary",
            Self::Complete(_) => "complete",
        }
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum QueryMode {
    #[default]
    Matches,
    Summarize,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    #[default]
    Full,
    Prefix,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Termination {
    #[default]
    Eof,
    ResultLimit,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MatchOrder {
    #[default]
    CorpusSentenceStartEndPatternCapturesGraphBindingsTerm,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SpanConvention {
    #[default]
    HalfOpenToken,
}

/// Corpus information available when a run starts. Direct scans deliberately
/// have no durable identity yet; stdin has only its run-local stream identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "corpus_kind", rename_all = "snake_case")]
pub enum RunCorpus {
    Prepared {
        source_identity: String,
        analysis_identity: String,
    },
    Direct {
        state: DirectRunState,
    },
    Stdin {
        stream_id: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DirectRunState {
    Pending,
}

/// Corpus information available when a stream completes. This is separate
/// from `RunCorpus` so impossible availability combinations are unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "corpus_kind", rename_all = "snake_case")]
pub enum CompleteCorpus {
    Prepared {
        source_identity: String,
        analysis_identity: String,
    },
    Direct {
        #[serde(flatten)]
        completion: DirectCompletion,
    },
    Stdin {
        stream_id: String,
        observed_content_digest: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DirectCompletion {
    Known {
        source_identity: String,
        analysis_identity: String,
    },
    PrefixUnavailable,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExecutionLimits {
    pub max_sentence_bytes: u64,
    pub max_stage_sentences: u64,
    pub max_stage_input_bytes: u64,
    pub max_tokenized_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TemporaryStorageLimits {
    pub max_temp_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TraversalLimits {
    pub max_depth: u64,
    pub max_source_entries: u64,
    pub max_directory_entries: u64,
    pub max_documents: u64,
    pub max_index_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InventoryStats {
    pub documents: u64,
    pub distinct_targets: u64,
    pub alias_documents: u64,
    pub max_target_multiplicity: u64,
    pub external_targets: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SchemaField {
    pub key: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AnalyzerRun {
    pub family: String,
    pub profile: String,
    pub segmentation: String,
    pub normalization: String,
    pub trust: String,
    pub language_policy: String,
    pub boundary_policy: String,
    pub semantic_identity: String,
    pub schema: Vec<SchemaField>,
}

impl AnalyzerRun {
    pub fn new(record: &lingrep_corpus::AnalyzerRecord, trust: &str, schema: &TokenSchema) -> Self {
        Self {
            family: record.family.clone(),
            profile: record.profile.clone(),
            segmentation: record.segmentation.clone(),
            normalization: record.normalization.clone(),
            trust: trust.to_owned(),
            language_policy: record.language_policy.clone(),
            boundary_policy: record.boundary_policy.clone(),
            semantic_identity: record.semantic_identity.clone(),
            schema: schema
                .attributes()
                .iter()
                .map(|attribute| SchemaField {
                    key: attribute.key().as_str().to_owned(),
                    kind: match attribute.kind() {
                        AttributeKind::Scalar => "scalar",
                        AttributeKind::StringSet => "string_set",
                    }
                    .to_owned(),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueryRun {
    pub run_id: String,
    pub mode: QueryMode,
    pub catalog_fingerprint: String,
    pub definition_count: u64,
    pub analyzer: AnalyzerRun,
    #[serde(flatten)]
    pub corpus: RunCorpus,
    pub dictionary_identity: String,
    pub reader_identity: String,
    pub token_schema_identity: String,
    /// The token attributes this run publishes, sorted by schema key.
    ///
    /// A consumer reading a file later distinguishes "this run did not publish
    /// `kana`" from "these tokens had no `kana`" by this list and nothing else.
    pub published_attributes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub published_source_metadata: Vec<String>,
    pub ordering: MatchOrder,
    pub span_convention: SpanConvention,
    /// Whether this run publishes character offsets.
    ///
    /// The `published_attributes` distinction one level up: a consumer reading
    /// a file later tells "this run never had offsets" from "this record's
    /// sentence withdrew them" by this flag and `counters.offsets_omitted`, and
    /// by nothing else. Offsets address normalized analysis text; words sharing
    /// a surface token carry the enclosing surface range.
    pub char_offsets: bool,
    pub requested_context: u64,
    pub requested_limit: Option<u64>,
    pub requested_top: Option<u64>,
    pub execution_limits: ExecutionLimits,
    pub temporary_storage_limits: TemporaryStorageLimits,
    pub traversal_limits: TraversalLimits,
    pub inventory: Option<InventoryStats>,
    pub worker_count: u64,
    /// Present only on a sampled run, so an unsampled run stays
    /// byte-identical under `lingrep.query/v2` — the same rule
    /// [`ScopeReport`] follows.
    ///
    /// Every count a sampled run publishes is a count over the sample. A
    /// consumer that cannot see this field cannot tell an estimate from a
    /// census, which is why the declaration travels with the results rather
    /// than in whatever invoked them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<SampleReport>,
}

/// Counts describe the selected sample; token-block draws also identify their physical layout.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "unit", rename_all = "snake_case")]
pub enum SampleReport {
    Sentence {
        rate_per_million: u64,
        seed: u64,
    },
    TokenBlock {
        rate_per_million: u64,
        seed: u64,
        token_component_blake3: String,
    },
}

impl SampleReport {
    #[must_use]
    pub const fn of(sample: lingrep_core::execution::CorpusSample) -> Self {
        Self::Sentence {
            rate_per_million: sample.rate_per_million() as u64,
            seed: sample.seed(),
        }
    }

    #[must_use]
    pub fn token_block(
        sample: lingrep_core::execution::CorpusSample,
        token_component_blake3: String,
    ) -> Self {
        Self::TokenBlock {
            rate_per_million: sample.rate_per_million() as u64,
            seed: sample.seed(),
            token_component_blake3,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SourceSpan {
    pub start: u64,
    pub end: u64,
}

/// A half-open range of Unicode code points within the sentence's own text.
///
/// Deliberately not a [`SourceSpan`]. That type already carries two different
/// quantities — half-open token ordinals on a match, and `.lx` byte offsets on
/// a definition — and a third meaning on one type is how a consumer ends up
/// assuming the unit. The name carries the unit instead; the origin is declared
/// once on the `run` record, beside `span_convention`.
///
/// Code points rather than UTF-8 bytes because every consumer this exists for
/// counts characters. On an all-BMP corpus the two are indistinguishable from
/// UTF-16 units as well, which is why the conversion is pinned by a non-BMP
/// fixture rather than a Japanese one.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct CharSpan {
    pub start: u64,
    pub end: u64,
}

impl From<lingrep_core::token_match::CharSpan> for CharSpan {
    fn from(span: lingrep_core::token_match::CharSpan) -> Self {
        Self {
            start: u64::from(span.start),
            end: u64::from(span.end),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DefinitionData {
    pub name: String,
    pub catalog_ordinal: u64,
    pub body: DefinitionBody,
    pub metadata: serde_json::Value,
    pub source_file: String,
    pub source_span: SourceSpan,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DefinitionBody {
    Pattern { expression: String },
    Lexicon { by: Vec<String>, member_count: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TokenEvidence {
    pub index: u64,
    pub attributes: TokenAttributes,
}

/// The token attributes a run publishes.
///
/// One value carries both obligations: it is what the `run` record declares in
/// `published_attributes`, and it is what publication filters each token by. A
/// single value is the point — two independently computed "what this run
/// publishes" sets that drifted apart would produce records the declaration
/// cannot explain.
///
/// It is also what keeps direct and prepared runs publishing **equal**
/// evidence. A prepared run's tokens already carry only the projection, so the
/// filter is a no-op; a direct run tokenizes everything, so the filter is what
/// narrows it to the same set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedProjection {
    attributes: Vec<AttributeKey>,
    names: Vec<String>,
}

impl PublishedProjection {
    /// Builds from the attributes a run projects, in any order.
    #[must_use]
    pub fn new<K>(attributes: &[K]) -> Self
    where
        K: Clone + Into<AttributeKey>,
    {
        let mut attributes: Vec<_> = attributes.iter().cloned().map(Into::into).collect();
        attributes.sort();
        attributes.dedup();
        let mut names: Vec<String> = attributes
            .iter()
            .map(|attribute| attribute.as_str().to_owned())
            .collect();
        names.sort();
        Self { attributes, names }
    }

    /// Every attribute, for a run that projects nothing away.
    #[must_use]
    pub fn everything() -> Self {
        Self::new(&TokenAttributeIndex::ALL)
    }

    /// The published attribute names, sorted by schema key.
    #[must_use]
    pub fn names(&self) -> &[String] {
        &self.names
    }

    #[must_use]
    pub fn attributes(&self) -> &[AttributeKey] {
        &self.attributes
    }
}

/// A token's attributes, in exactly the JSON shape a `BTreeMap<String, String>`
/// produced.
///
/// Keys are the schema's own `&'static str`, and values are the
/// [`CompactString`] the token already holds, so publishing a token allocates
/// nothing for a key and nothing for a value that fits inline. The invariant is
/// that entries are **sorted by key**, which construction gets from a schema
/// order sorted once per process rather than by sorting every token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ProtocolAttributeValue {
    Scalar(String),
    StringSet(Vec<String>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct TokenAttributes(BTreeMap<String, ProtocolAttributeValue>);

impl<'de> Deserialize<'de> for TokenAttributes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let values = BTreeMap::<String, ProtocolAttributeValue>::deserialize(deserializer)?;
        for key in values.keys() {
            AttributeKey::new(key).map_err(|_| D::Error::custom("invalid token attribute key"))?;
        }
        Ok(Self(values))
    }
}

impl TokenAttributes {
    /// Collects the attributes a token carries, in key order.
    #[must_use]
    pub fn from_token(token: &TokenInfo) -> Self {
        Self::from_token_projected(token, &PublishedProjection::everything())
    }

    /// Collects the attributes a token carries **and** the run publishes, in
    /// key order.
    ///
    /// Both filters are load-bearing and neither implies the other. A prepared
    /// run's token carries only the projection, so `projection` admits
    /// everything present; a direct run's token carries the whole schema, so
    /// `projection` is what narrows it. Applying the same projection to both is
    /// what makes their published evidence equal.
    #[must_use]
    pub fn from_token_projected(token: &TokenInfo, projection: &PublishedProjection) -> Self {
        Self::from_sequence_projected(std::slice::from_ref(token), 0, projection)
    }

    #[must_use]
    pub fn from_sequence_projected(
        tokens: &(impl TokenSequence + ?Sized),
        index: usize,
        projection: &PublishedProjection,
    ) -> Self {
        Self(
            projection
                .attributes()
                .iter()
                .filter_map(|key| {
                    tokens.value_key(index, key).map(|value| {
                        let value = match value {
                            AttributeValueRef::Scalar(value) => {
                                ProtocolAttributeValue::Scalar(value.to_owned())
                            }
                            AttributeValueRef::StringSet(values) => {
                                ProtocolAttributeValue::StringSet(
                                    values.iter().map(ToString::to_string).collect(),
                                )
                            }
                        };
                        (key.as_str().to_owned(), value)
                    })
                })
                .collect(),
        )
    }

    /// Builds from explicit pairs, for tests and non-token callers.
    ///
    /// Sorts, because a caller's order is not the schema's.
    #[must_use]
    pub fn from_pairs<I, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (&'static str, V)>,
        V: Into<CompactString>,
    {
        let entries = pairs
            .into_iter()
            .map(|(key, value)| {
                let value: CompactString = value.into();
                (
                    key.to_owned(),
                    ProtocolAttributeValue::Scalar(value.to_string()),
                )
            })
            .collect();
        Self(entries)
    }

    /// The attributes, in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ProtocolAttributeValue)> {
        self.0.iter().map(|(key, value)| (key.as_str(), value))
    }

    /// The value for a schema key, if the token carries it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        match self.0.get(key)? {
            ProtocolAttributeValue::Scalar(value) => Some(value.as_str()),
            ProtocolAttributeValue::StringSet(_) => None,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Capture {
    pub name: String,
    pub hygienic_path: String,
    pub span: SourceSpan,
    /// This capture's code-point span in the sentence text.
    ///
    /// Per capture and not only per record because that is the granularity a
    /// standoff consumer addresses: a collocation's noun, particle and verb are
    /// three separate spans of the same record, and a record-level span alone
    /// cannot express them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_span: Option<CharSpan>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "node_kind", rename_all = "snake_case")]
pub enum GraphNodeId {
    Word { token_index: u64 },
    Empty { boundary: u64, subindex: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphNodeAttribute {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "binding_kind", rename_all = "snake_case")]
pub enum GraphBinding {
    Node {
        name: String,
        node: GraphNodeId,
        attributes: Vec<GraphNodeAttribute>,
    },
    Edge {
        name: String,
        head: GraphNodeId,
        dependent: GraphNodeId,
        relation: String,
    },
}

/// One typed field of a record's ordered projected key.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProtocolKeyField {
    Scalar { name: String, value: String },
    List { name: String, values: Vec<String> },
}

/// One derived per-instance annotation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TagField {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MatchData {
    pub sequence: u64,
    pub pattern: String,
    pub span: SourceSpan,
    /// The published span's code-point range in the sentence text.
    ///
    /// Absent when the run cannot supply offsets at all — see the `run`
    /// record's `char_offsets` declaration — and when this record's sentence
    /// withdrew them, which `counters.offsets_omitted` counts. The two are
    /// different facts and a consumer distinguishes them by those two fields.
    ///
    /// It tracks whatever span the record publishes, so a `detach`, a
    /// `drop_edge` or a `truncate_before` moves it: offsets are a projection of
    /// the span, not an independent record of where the match once was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub char_span: Option<CharSpan>,
    /// The record's captures — **whose meaning depends on the publication
    /// mode**.
    ///
    /// By default these are the *post-rewrite survivors*: a `policy` block's
    /// `detach`, and the `drop_edge` and `truncate_before` transforms, all
    /// rewrite the published span, and a capture that leaves it is dropped,
    /// never clamped. A capture present in the raw match can therefore
    /// be absent here.
    /// Under `--raw-matches` these are the raw match's captures verbatim.
    ///
    /// A consumer that reads a default run's captures as raw match coordinates
    /// will mis-parse transformed records. Raw captures are obtained by asking
    /// for the raw stream; `counters.captures_dropped` is what makes the
    /// difference countable rather than silent.
    pub captures: Vec<Capture>,
    /// Named enhanced graph assignments in canonical binding-kind/name order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graph_bindings: Vec<GraphBinding>,
    pub corpus_sentence_ordinal: u64,
    pub source_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "source_metadata_schema")]
    pub source_metadata: Option<std::sync::Arc<serde_json::Map<String, serde_json::Value>>>,
    pub document_ordinal: u64,
    pub document_sentence_ordinal: u64,
    pub surface: String,
    pub tokens: Vec<TokenEvidence>,
    pub left_context: Option<Vec<TokenEvidence>>,
    pub right_context: Option<Vec<TokenEvidence>>,
    /// The rendered unit, present only when the pattern's transform ran a
    /// `join`. Distinct from `surface`, which stays the published span's
    /// own text: a substitution changes the unit and not the span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// The projected key, in declared order.
    ///
    /// Absent both when the pattern declares no `key:` and when it declares one
    /// this record could not satisfy — the second is counted in
    /// `counters.records_unkeyed`. Absence is never published as an empty
    /// value, which would silently co-group unrelated records downstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Vec<ProtocolKeyField>>,
    /// Authored lexicon member and the projections that recognized it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical: Option<LexicalMatchData>,
    /// Derived annotations, in catalog order. A tag whose source was
    /// absent is omitted rather than emitted empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<TagField>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LexicalMatchData {
    pub term: String,
    pub matched_by: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SummaryData {
    pub pattern: String,
    pub group: SummaryGroupData,
    pub count: u64,
    pub rank: u64,
}

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SummaryGroupData {
    Surface { value: String },
    Unit { value: String },
    Term { value: String },
    Key { fields: Vec<ProtocolKeyField> },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RunCounters {
    pub documents_admitted: u64,
    pub sentences_admitted: u64,
    /// Admitted sentences excluded by an exact necessary-condition proof.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sentences_proven_absent: u64,
    pub sentences_matched: u64,
    pub tokens_matched: u64,
    pub matches_found: u64,
    pub records_emitted: u64,
    /// Records dropped from keyed output because a declared `key:` capture did
    /// not participate in the match.
    ///
    /// Additive and skipped when zero, so a catalog declaring no key is
    /// byte-identical to before. Present because a dropped row must be
    /// countable: silently omitting it would make a partial result look
    /// complete.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub records_unkeyed: u64,
    /// Captures dropped because a span rewrite removed the tokens they covered
    ///.
    ///
    /// Additive and skipped when zero. Present because the published `captures`
    /// field is mode-dependent: without a count, a consumer comparing a default
    /// run against a raw one sees captures vanish with no evidence that policy,
    /// rather than matching, is what removed them.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub captures_dropped: u64,
    /// Records published without character offsets on a run that otherwise
    /// supplies them.
    ///
    /// Additive and skipped when zero. A run that declares `char_offsets:
    /// false` never increments it — that absence is the declaration's, not a
    /// per-record fact — so a nonzero count means specifically that some
    /// sentences withdrew offsets while others kept them, which for CoNLL-U is
    /// a `# text` comment its own rows do not reconstruct.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offsets_omitted: u64,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if requires a by-reference predicate"
)]
fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ResourceHighWater {
    pub staged_raw_bytes: u64,
    pub tokenized_bytes: u64,
    pub temporary_storage_bytes: u64,
    pub execution_limits: ExecutionLimits,
    pub temporary_storage_limits: TemporaryStorageLimits,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CommittedPrefix {
    pub corpus_sentence_ordinal: u64,
    pub sequence: u64,
}

/// What a scope report's counts cover.
///
/// A `--limit`ed run stops mid-stream, and by then the reader has run ahead of
/// the matcher: a direct source crosses document boundaries to fill a batch
/// while the runner can stop inside an earlier partition. Carrying the basis
/// lets a consumer decide whether it holds a corpus denominator without
/// knowing which path produced the record or reasoning about batch sizes.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ScopeBasis {
    /// The complete corpus. Every prepared run, whose universe is precomputed
    /// from the artifact, and any direct run that reached the end of its
    /// input.
    #[default]
    Corpus,
    /// The prefix the reader observed before the run stopped. Neither the
    /// whole corpus nor the matched prefix, and its exact extent depends on
    /// the batching limits in force.
    ObservedPrefix,
}

/// Accounting that would not be exact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScopeOverflow;

/// A scoped run's observation universe at one grain.
///
/// Four buckets, not three. A predicate over a value whose *shape* does not
/// fit it — `year >= 2010` against `pages: "47-56"` — is false in both
/// polarities like a missing field, but it is a different fact: the field is
/// present and the query could not use it. Folding the two would report a data
/// problem as an absence.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScopeUniverse {
    pub admitted: u64,
    pub excluded_by_predicate: u64,
    pub excluded_missing_field: u64,
    pub excluded_shape_mismatch: u64,
}

impl ScopeUniverse {
    /// Adds `weight` to the bucket a verdict names.
    ///
    /// Checked, not saturating. The published claim about these fields is that
    /// they sum *exactly* to the universe; a saturating add would keep
    /// publishing that claim while making it false, and a false denominator is
    /// the failure this whole report exists to prevent. Overflow is
    /// unreachable for any corpus whose sentence count fits the `u64` the
    /// artifact already stores, which is why refusing it costs nothing.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeOverflow`] if the bucket would not fit.
    pub fn record(&mut self, verdict: Verdict, weight: u64) -> Result<(), ScopeOverflow> {
        let bucket = match verdict {
            Verdict::Admitted => &mut self.admitted,
            Verdict::Excluded(ExclusionReason::Predicate) => &mut self.excluded_by_predicate,
            Verdict::Excluded(ExclusionReason::MissingField) => &mut self.excluded_missing_field,
            Verdict::Excluded(ExclusionReason::ShapeMismatch) => &mut self.excluded_shape_mismatch,
        };
        *bucket = bucket.checked_add(weight).ok_or(ScopeOverflow)?;
        Ok(())
    }

    /// The whole universe.
    ///
    /// A scope and its apparent complement do not sum to this, which is the
    /// point of reporting it.
    ///
    /// # Errors
    ///
    /// Returns [`ScopeOverflow`] if the four buckets would not sum.
    pub fn total(&self) -> Result<u64, ScopeOverflow> {
        self.admitted
            .checked_add(self.excluded_by_predicate)
            .and_then(|sum| sum.checked_add(self.excluded_missing_field))
            .and_then(|sum| sum.checked_add(self.excluded_shape_mismatch))
            .ok_or(ScopeOverflow)
    }
}

/// The scope clause a run applied, and the universe it observed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ScopeReport {
    /// The clause as parsed. A frequency under scope is a claim about the
    /// scoped universe, so the output names it.
    pub clause: String,
    /// What `documents` and `sentences` are counts *of*. Only
    /// [`ScopeBasis::Corpus`] licenses using them as a corpus denominator.
    pub basis: ScopeBasis,
    pub documents: ScopeUniverse,
    pub sentences: ScopeUniverse,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RetainedMatches {
    pub algorithm: String,
    pub unit: String,
    pub seed: u64,
    pub capacity: u32,
    pub max_bytes: u64,
    pub seen: u64,
    pub retained: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueryComplete {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_matches: Option<RetainedMatches>,
    pub coverage: Coverage,
    #[serde(flatten)]
    pub corpus: CompleteCorpus,
    pub counters: RunCounters,
    pub elapsed_millis: u64,
    pub resources: ResourceHighWater,
    pub committed_prefix: Option<CommittedPrefix>,
    pub inventory: Option<InventoryStats>,
    pub termination: Termination,
    /// Present only on a scoped run, so an unscoped run stays byte-identical
    /// under `lingrep.query/v2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ScopeReport>,
}

/// The `lingrep.query/v2` fields whose value legitimately differs between two
/// runs of the same catalog over the same input.
///
/// Establishing that a change publishes the same records is how every
/// performance campaign discharges the output-parity requirement of
/// `docs/PERFORMANCE_REQUIREMENTS.md`, and a stream cannot be compared
/// byte-for-byte until these are neutralized. The list belongs beside the schema
/// rather than in whatever script is doing the comparing: it is a property of
/// what the current contract publishes, so a field added to a later contract is noticed here
/// instead of silently defeating a parity check.
///
/// Three entries, each earned: `run_id` identifies the run and appears on
/// `run` and on every diagnostic; `elapsed_millis` is wall-clock and appears
/// on `complete`; `temporary_storage_bytes` is a resource high-water mark
/// whose exact value can vary with parallel spill scheduling. None changes the
/// records a run publishes.
pub const RUN_VARIANT_FIELDS: [&str; 3] = ["run_id", "elapsed_millis", "temporary_storage_bytes"];

/// Replaces every [`RUN_VARIANT_FIELDS`] value in `event` with null, in place.
///
/// By key name at any depth, because `run_id` is nested one level under `data`
/// on a `run` record and two under a diagnostic's. The bluntness is deliberate
/// and is why the list is short: a key that shares one of these names and does
/// not vary per run would be neutralized too, so a new entry needs the same
/// justification the first two have.
pub fn normalize_run_variance(event: &mut serde_json::Value) {
    match event {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields.iter_mut() {
                if RUN_VARIANT_FIELDS.contains(&key.as_str()) {
                    *value = serde_json::Value::Null;
                } else {
                    normalize_run_variance(value);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                normalize_run_variance(item);
            }
        }
        _ => {}
    }
}

fn source_metadata_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type":["object","null"],
        "additionalProperties":false,
        "properties":{
            "document":{"type":"object"},
            "sentence":{"type":"object"},
            "divisions":{"type":"array","maxItems":256,"items":{
                "type":"object","additionalProperties":false,"required":["id","kind","parent_id","source_id","metadata"],
                "properties":{"source_id":{"type":["string","null"],"maxLength":256},"id":{"type":"integer","minimum":0},"kind":{"type":"string","minLength":1,"maxLength":256},"parent_id":{"type":["integer","null"],"minimum":0},"metadata":{"type":"object"}}
            }}
        }
    })
}

#[cfg(test)]
mod run_variance_tests {
    use super::{RUN_VARIANT_FIELDS, normalize_run_variance};

    #[test]
    fn every_named_field_is_neutralized_at_any_depth() {
        let mut event = serde_json::json!({
            "type": "run",
            "data": {"run_id": "abc", "nested": {"elapsed_millis": 12}},
            "list": [{"run_id": "def"}],
        });
        normalize_run_variance(&mut event);
        assert_eq!(
            event,
            serde_json::json!({
                "type": "run",
                "data": {"run_id": null, "nested": {"elapsed_millis": null}},
                "list": [{"run_id": null}],
            })
        );
    }

    /// Neutralizing is not blanking the record: a stream normalized this way
    /// must still report a genuine difference, or the parity check it serves
    /// would pass on anything.
    #[test]
    fn a_difference_outside_the_named_fields_survives() {
        let render = |sequence: u64| {
            let mut event = serde_json::json!({
                "data": {"run_id": "abc", "sequence": sequence},
            });
            normalize_run_variance(&mut event);
            event
        };
        assert_ne!(render(0), render(1));
    }

    /// Parallel aggregation can reach the same exact rows through different
    /// spill schedules, so its temporary-storage high-water mark is a run
    /// observation rather than semantic output. Performance parity must not
    /// reject two otherwise identical streams on that value alone.
    #[test]
    fn temporary_storage_high_water_is_neutralized() {
        let render = |temporary_storage_bytes: u64| {
            let mut event = serde_json::json!({
                "type": "complete",
                "data": {
                    "resources": {"temporary_storage_bytes": temporary_storage_bytes},
                    "counters": {"records_emitted": 42}
                }
            });
            normalize_run_variance(&mut event);
            event
        };
        assert_eq!(render(10), render(20));
    }

    #[test]
    fn the_list_is_the_three_fields_the_contract_names() {
        assert_eq!(
            RUN_VARIANT_FIELDS,
            ["run_id", "elapsed_millis", "temporary_storage_bytes"]
        );
    }
}
