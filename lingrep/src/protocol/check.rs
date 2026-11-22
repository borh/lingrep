use serde::{Deserialize, Deserializer, Serialize, de::Error as _};

use super::query::SourceSpan;

pub const SCHEMA: &str = super::ContractId::Check.as_str();

pub(crate) fn contract() -> super::discovery::ContractSpec {
    use super::discovery::{Channel, StreamShape, json_contract};
    json_contract::<CheckEnvelope>(
        super::ContractId::Check,
        &["check"],
        "One compiled-catalog report with warnings and drift evidence.",
        StreamShape::Single { events: &["check"] },
        Channel::Stdout,
        "Authored catalog order.",
        "The complete catalog report.",
        "A closed pipe is a machine-output failure.",
        "0 valid catalog and checked claims hold, 1 violated claims or runtime/output failure, 2 invalid invocation or catalog.",
        &["check/inline-pattern"],
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckEnvelope {
    #[serde(deserialize_with = "deserialize_schema")]
    pub schema: String,
    #[serde(flatten)]
    pub event: CheckEvent,
}
fn deserialize_schema<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let schema = String::deserialize(deserializer)?;
    if schema == SCHEMA {
        Ok(schema)
    } else {
        Err(D::Error::custom("wrong check schema"))
    }
}
impl CheckEnvelope {
    pub fn new(event: CheckEvent) -> Self {
        Self {
            schema: SCHEMA.into(),
            event,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum CheckEvent {
    Check(CheckData),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DefinitionDescriptor {
    pub name: String,
    pub catalog_ordinal: u64,
    pub body: super::query::DefinitionBody,
    pub metadata: serde_json::Value,
    pub source_file: String,
    pub source_span: SourceSpan,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptureKeyEdit {
    pub definition: String,
    pub published_name: String,
    /// Zero-based position of the field in the definition's `key:` list.
    pub field: u64,
    pub capture: String,
    pub attribute: String,
    pub attributes: Vec<String>,
    pub source_name: String,
    pub byte_range: SourceSpan,
}

impl From<lingrep_core::fsm::CaptureKeyEdit> for CaptureKeyEdit {
    fn from(edit: lingrep_core::fsm::CaptureKeyEdit) -> Self {
        Self {
            definition: edit.definition,
            published_name: edit.published_name,
            field: edit.field as u64,
            capture: edit.capture,
            attribute: edit.attribute,
            attributes: edit.attributes.into_iter().map(str::to_owned).collect(),
            source_name: edit.source_name.to_string(),
            byte_range: SourceSpan {
                start: edit.byte_range.start as u64,
                end: edit.byte_range.end as u64,
            },
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompileStats {
    pub source_bytes: u64,
    pub definitions: u64,
    pub lexicon_members: u64,
    pub expression_nodes: u64,
    pub max_depth: u64,
    pub metadata_bytes: u64,
    pub regex_bytes: u64,
    pub reference_edges: u64,
    pub expansion_work: u64,
    pub generated_owned_bytes: u64,
    pub nfa_states: u64,
    pub transitions: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CompileWarning {
    pub code: String,
    pub message: String,
    pub location: Option<SourceLocation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SourceLocation {
    pub source_file: String,
    pub source_span: SourceSpan,
}

/// One catalog carrying one body in a drift report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DriftCarrierData {
    pub catalog: String,
    /// `"pattern"` or `"fragment"`. Reported, never compared.
    pub kind: String,
}

/// One distinct body.
///
/// An object rather than a bare `Vec<DriftCarrierData>`: the
/// additive-fields-keep-`/v1` rule only holds where every wire node has
/// somewhere to put a field. A nested bare list would be the one node in this
/// protocol that cannot absorb one, so adding a body fingerprint or structural
/// diff would mean turning a JSON array into an object
/// — a breaking change, and exactly the schema bump the rule exists to avoid.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DriftBodyData {
    pub carriers: Vec<DriftCarrierData>,
}

/// One definition name whose reference-closed canonical bodies differ across
/// the compared catalogs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DriftData {
    pub name: String,
    pub bodies: Vec<DriftBodyData>,
}

/// One referenced attribute a named schema cannot supply, with the definition
/// sites that reference it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UnavailableAttributeData {
    pub attribute: String,
    /// Empty for attributes referenced only by catalog configuration.
    pub sites: Vec<BindingSiteData>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BindingSiteData {
    pub definition: String,
    pub source_file: String,
    /// 1-indexed line of the definition's own declaration.
    pub line: u64,
}

/// The `--schema` finding: how this catalog binds against a named schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BindingData {
    /// The requested schema name, e.g. `"unidic-morphology"`.
    pub schema: String,
    /// Referenced attributes the schema does not carry. The catalog still
    /// binds; positive tests over them match nothing there.
    pub unavailable: Vec<UnavailableAttributeData>,
    /// Present when the catalog cannot bind at all — dependency `where`
    /// clauses without their edge attributes, or a value-kind conflict — and
    /// the check then fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CheckData {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capture_key_edits: Vec<CaptureKeyEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comparison: Vec<ComparedDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub examples: Option<ExampleVerification>,
    pub catalog_fingerprint: String,
    pub definitions: Vec<DefinitionDescriptor>,
    pub compile_stats: CompileStats,
    pub warnings: Vec<CompileWarning>,
    /// Cross-catalog drift, present only when `--compare-with` found any.
    ///
    /// Additive fields keep the `lingrep.check/v1` schema string: nothing in
    /// this protocol sets `deny_unknown_fields`, and `serde(default)` keeps
    /// previously-written records readable. The string bumps only on a
    /// breaking change — a removed field, a changed type, or a changed
    /// meaning for an existing field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub drift: Vec<DriftData>,
    /// The `--schema` binding finding, present only when one was requested.
    /// Additive under the rule above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<BindingData>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExampleVerification {
    pub analyzer: super::query::AnalyzerRun,
    pub resource_identity: String,
    pub checked: u64,
    pub failures: Vec<ExampleFailure>,
    pub post_policy_checked: u64,
    pub post_policy_failures: Vec<ExampleFailure>,
    /// Counterexamples that hold and that no single relaxation of their
    /// definition matches. Additive under the `lingrep.check/v1` rule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vacuous_counterexamples: Vec<VacuousCounterexample>,
    /// Holding counterexamples whose relaxations were not all evaluated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unchecked_counterexamples: Vec<UncheckedCounterexample>,
    /// The `--propose-counterexamples` scan, when one was requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterexample_candidates: Option<CandidateScan>,
}

/// One constraint of a definition that a relaxation removes.
#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct Relaxation {
    /// `attribute_test`: one attribute test of a token element holds for
    /// every token. `element`: a token element may match no token.
    /// `lookaround`: a lookaround holds everywhere.
    pub kind: String,
    /// The token element or lookaround that carries the constraint.
    pub element: ElementLocation,
    /// For `attribute_test`: the attribute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute: Option<String>,
    /// For `attribute_test`: the operator and operand, such as `=猫`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct VacuousCounterexample {
    pub definition: String,
    pub example: String,
    /// Every relaxation tried: each attribute test, token element and
    /// lookaround of the definition body, references expanded, one at a time.
    pub relaxations_tried: Vec<Relaxation>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UncheckedCounterexample {
    pub definition: String,
    pub example: String,
    pub reason: String,
}

/// Sentences of one file that a definition does not match and one relaxation
/// of it does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CandidateScan {
    pub source: String,
    /// Non-empty lines of `source` that were analyzed, each as one sentence.
    pub sentences_scanned: u64,
    /// Pattern definitions each sentence was evaluated against.
    pub definitions_evaluated: u64,
    /// Definitions the explanation pass cannot evaluate, which have no candidates.
    pub definitions_skipped: Vec<String>,
    /// At most `candidates_per_relaxation` sentences per definition and relaxation.
    pub candidates_per_relaxation: u64,
    pub candidates: Vec<CounterexampleCandidate>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CounterexampleCandidate {
    pub definition: String,
    pub sentence: String,
    /// The constraint whose removal makes the definition match `sentence`.
    pub relaxation: Relaxation,
    /// The `[start, end)` token span the relaxed definition matches.
    pub span: [u64; 2],
}

impl From<lingrep_core::fsm::Relaxation> for Relaxation {
    fn from(relaxation: lingrep_core::fsm::Relaxation) -> Self {
        use lingrep_core::fsm::RelaxedConstraint;
        let (kind, attribute, test) = match relaxation.constraint {
            RelaxedConstraint::AttributeTest { attribute, test } => {
                ("attribute_test", Some(attribute), Some(test))
            }
            RelaxedConstraint::Element => ("element", None, None),
            RelaxedConstraint::Lookaround => ("lookaround", None, None),
        };
        Self {
            kind: kind.to_owned(),
            element: relaxation.element.into(),
            attribute,
            test,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ExampleFailure {
    pub definition: String,
    pub example: String,
    pub kind: String,
    /// For an `expected_match` failure, why the definition did not publish a
    /// match. Additive under the `lingrep.check/v1` rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<NoMatchExplanation>,
}

/// Why one definition did not publish a match for one sentence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NoMatchExplanation {
    /// `stopped`, `not_published`, `where_rejected` or `unexplained`.
    pub outcome: String,
    /// `[start, end)` token spans the definition (`not_published`) or its body
    /// (`where_rejected`) matched.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<[u64; 2]>,
    /// For `stopped`: the attempt that consumed the most tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<StoppedAttempt>,
    /// For `unexplained`: why no explanation is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Start positions the explanation pass attempted. The pass evaluates the
    /// definition body separately from the matcher.
    pub starts_tried: u64,
    /// The explanation pass reached its step limit before trying every start.
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StoppedAttempt {
    /// Token index the attempt started at.
    pub start: u64,
    /// Token index the failing element was tested at; the sentence length when
    /// the sentence had ended.
    pub position: u64,
    /// The surface text of the token at `position`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// `token`, `sentence_ended`, `not_sentence_start`, `not_sentence_end`,
    /// `followed_by`, `not_followed_by`, `preceded_by` or `not_preceded_by`.
    pub stop: String,
    /// The failing element in the catalog source; absent for an anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element: Option<ElementLocation>,
    /// For `token`: each attribute test of the element against the token.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tests: Vec<AttributeTest>,
}

#[derive(
    Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct ElementLocation {
    pub source: String,
    pub line: u64,
    /// One-based, in bytes.
    pub column: u64,
    pub byte_start: u64,
    pub byte_end: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AttributeTest {
    pub attribute: String,
    /// The operator and operand, such as `=猫`.
    pub test: String,
    /// The token's value; absent when the token does not carry the attribute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    pub held: bool,
}

impl NoMatchExplanation {
    /// `surface` gives the surface text of the token at an index.
    pub fn new(
        explanation: lingrep_core::fsm::DefinitionExplanation,
        surface: impl Fn(usize) -> Option<String>,
    ) -> Self {
        use lingrep_core::fsm::{DefinitionOutcome, StopReason};
        let spans = |spans: Vec<(usize, usize)>| {
            spans
                .into_iter()
                .map(|(start, end)| [start as u64, end as u64])
                .collect()
        };
        let mut wire = Self {
            outcome: String::new(),
            spans: Vec::new(),
            attempt: None,
            detail: None,
            starts_tried: explanation.starts_tried as u64,
            truncated: explanation.truncated,
        };
        match explanation.outcome {
            DefinitionOutcome::Published { spans: found } => {
                wire.outcome = "published".into();
                wire.spans = spans(found);
            }
            DefinitionOutcome::NotPublished { spans: found } => {
                wire.outcome = "not_published".into();
                wire.spans = spans(found);
            }
            DefinitionOutcome::WhereRejected { spans: found } => {
                wire.outcome = "where_rejected".into();
                wire.spans = spans(found);
            }
            DefinitionOutcome::Unexplained { reason } => {
                wire.outcome = "unexplained".into();
                wire.detail = Some(reason);
            }
            DefinitionOutcome::Stopped(attempt) => {
                wire.outcome = "stopped".into();
                let (stop, tests) = match attempt.reason {
                    StopReason::Token { tests } => ("token", tests),
                    StopReason::SentenceEnded => ("sentence_ended", Vec::new()),
                    StopReason::NotSentenceStart => ("not_sentence_start", Vec::new()),
                    StopReason::NotSentenceEnd => ("not_sentence_end", Vec::new()),
                    StopReason::Lookaround {
                        preceded_by,
                        negative,
                    } => (
                        match (preceded_by, negative) {
                            (false, false) => "followed_by",
                            (false, true) => "not_followed_by",
                            (true, false) => "preceded_by",
                            (true, true) => "not_preceded_by",
                        },
                        Vec::new(),
                    ),
                };
                wire.attempt = Some(StoppedAttempt {
                    start: attempt.start as u64,
                    position: attempt.position as u64,
                    token: surface(attempt.position),
                    stop: stop.into(),
                    element: attempt.element.map(ElementLocation::from),
                    tests: tests
                        .into_iter()
                        .map(|test| AttributeTest {
                            attribute: test.attribute,
                            test: test.test,
                            actual: test.actual,
                            held: test.held,
                        })
                        .collect(),
                });
            }
        }
        wire
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ComparedDefinition {
    pub name: String,
    pub revisions: Vec<DefinitionRevision>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DefinitionRevision {
    pub catalog: String,
    pub kind: String,
    pub body_group: u64,
    pub expanded: String,
}

impl From<lingrep_core::fsm::ElementLocation> for ElementLocation {
    fn from(element: lingrep_core::fsm::ElementLocation) -> Self {
        Self {
            source: element.source.to_string(),
            line: element.line as u64,
            column: element.column as u64,
            byte_start: element.byte_range.start as u64,
            byte_end: element.byte_range.end as u64,
        }
    }
}
