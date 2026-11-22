//! Where one definition's best attempt on one sentence stopped.
//!
//! A second evaluator over the bound program, separate from the recognizers in
//! `matcher`. It is run on request for a sentence the matcher did not match and
//! reports the element that failed; it never produces matches.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

use super::model::{
    BoundDefinition, BoundExpr, BoundProgram, DefinitionId, LookDirection, LookPolarity, SourceSpan,
};
use crate::fsm::{AttrValueType, PositionAssertion, SymbolicAttrValue};
use crate::token_match::{AttributeValueRef, TokenSequence};

/// Expression evaluations after which a report is returned as truncated.
const STEP_LIMIT: usize = 200_000;

/// The bound definitions of one compiled catalog, kept for
/// [`NoMatchExplainer::explain`].
#[derive(Clone, Debug)]
pub struct NoMatchExplainer {
    program: BoundProgram,
}

/// A position in pattern source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ElementLocation {
    pub source: Arc<str>,
    pub byte_range: Range<usize>,
    /// One-based.
    pub line: usize,
    /// One-based, in bytes.
    pub column: usize,
}

/// One attribute test of a token element, evaluated against one token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeTestOutcome {
    pub attribute: String,
    /// The operator and operand, such as `=猫` or `in {動詞, 助動詞}`.
    pub test: String,
    /// The token's value; `None` when the token does not carry the attribute.
    pub actual: Option<String>,
    pub held: bool,
}

/// Why the furthest attempt could not continue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The token at the stop position failed a token element.
    Token { tests: Vec<AttributeTestOutcome> },
    /// A token element was required and the sentence had ended.
    SentenceEnded,
    /// A sentence-start anchor did not hold at the stop position.
    NotSentenceStart,
    /// A sentence-end anchor did not hold at the stop position.
    NotSentenceEnd,
    /// A lookaround did not hold at the stop position.
    Lookaround { preceded_by: bool, negative: bool },
}

/// The attempt that consumed the most tokens before it failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoppedAttempt {
    /// Token index the attempt started at.
    pub start: usize,
    /// Token index the failing element was tested at; equal to the sentence
    /// length when the sentence had ended.
    pub position: usize,
    /// The failing element. `None` for an anchor.
    pub element: Option<ElementLocation>,
    pub reason: StopReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoMatchOutcome {
    /// No pattern definition has this name.
    UnknownDefinition,
    /// The definition uses a construct this pass does not evaluate.
    Unsupported { construct: &'static str },
    /// The body matches these `(start, end)` spans. `has_where` is whether a
    /// `where` clause or span constraint could still reject them.
    BodyMatches {
        spans: Vec<(usize, usize)>,
        has_where: bool,
    },
    /// No start position matched the body.
    Stopped(StoppedAttempt),
    /// The sentence has no tokens.
    EmptySentence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoMatchReport {
    pub outcome: NoMatchOutcome,
    /// Start positions attempted.
    pub starts_tried: usize,
    /// The step limit was reached, so a later start position could have
    /// progressed further or matched.
    pub truncated: bool,
}

impl NoMatchExplainer {
    pub(super) fn new(program: BoundProgram) -> Self {
        Self { program }
    }

    /// Evaluates `definition`'s body at every start position of `tokens`.
    pub fn explain<S: TokenSequence + ?Sized>(
        &self,
        definition: &str,
        tokens: &S,
    ) -> NoMatchReport {
        let report = |outcome, starts_tried, truncated| NoMatchReport {
            outcome,
            starts_tried,
            truncated,
        };
        let Some(target) = self
            .program
            .definitions
            .iter()
            .find(|candidate| candidate.name == definition && candidate.expr().is_some())
        else {
            return report(NoMatchOutcome::UnknownDefinition, 0, false);
        };
        let mut nodes = Nodes {
            program: &self.program,
            by_definition: HashMap::new(),
            has_where: false,
        };
        let root = match nodes.definition(target) {
            Ok(root) => root,
            Err(construct) => return report(NoMatchOutcome::Unsupported { construct }, 0, false),
        };
        if tokens.is_empty() {
            return report(NoMatchOutcome::EmptySentence, 0, false);
        }
        let mut run = Run {
            tokens,
            memo: HashMap::new(),
            steps: 0,
            truncated: false,
            start: 0,
            recording: true,
            best: None,
        };
        let mut spans = Vec::new();
        let mut starts_tried = 0;
        for start in 0..tokens.len() {
            if run.truncated {
                break;
            }
            starts_tried += 1;
            run.start = start;
            if let Some(end) = run
                .ends(&root, start)
                .iter()
                .copied()
                .rfind(|end| *end > start)
            {
                spans.push((start, end));
            }
        }
        let outcome = if !spans.is_empty() {
            NoMatchOutcome::BodyMatches {
                spans,
                has_where: nodes.has_where,
            }
        } else if let Some(best) = run.best {
            NoMatchOutcome::Stopped(StoppedAttempt {
                start: best.start,
                position: best.position,
                element: best.span.map(|span| self.location(span)),
                reason: best.reason,
            })
        } else {
            // A body with no failing element and no match is zero-width.
            NoMatchOutcome::Unsupported {
                construct: "a body that consumes no token",
            }
        };
        report(outcome, starts_tried, run.truncated)
    }

    fn location(&self, span: SourceSpan) -> ElementLocation {
        let (line, column) = self.program.sources.line_column(span);
        let range = span.byte_range();
        ElementLocation {
            source: self.program.sources.source_label(span.source()),
            byte_range: range.start as usize..range.end as usize,
            line,
            column,
        }
    }
}

enum Node<'p> {
    Token {
        predicate: &'p [SymbolicAttrValue],
        span: SourceSpan,
    },
    SentenceStart,
    SentenceEnd,
    Lookaround {
        direction: LookDirection,
        polarity: LookPolarity,
        body: Rc<Node<'p>>,
        span: SourceSpan,
    },
    Sequence(Vec<Rc<Node<'p>>>),
    Alternation(Vec<Rc<Node<'p>>>),
    Intersection(Vec<Rc<Node<'p>>>),
    AnyOrder(Vec<Rc<Node<'p>>>),
    Repeat {
        node: Rc<Node<'p>>,
        min: usize,
        max: Option<usize>,
    },
    /// A reference written at `span`, expanded to the referenced body.
    Reference {
        body: Rc<Node<'p>>,
        span: SourceSpan,
    },
}

/// Bound expressions joined with their token spans, references expanded.
struct Nodes<'p> {
    program: &'p BoundProgram,
    by_definition: HashMap<DefinitionId, Rc<Node<'p>>>,
    has_where: bool,
}

impl<'p> Nodes<'p> {
    fn definition(
        &mut self,
        definition: &'p BoundDefinition,
    ) -> Result<Rc<Node<'p>>, &'static str> {
        if let Some(node) = self.by_definition.get(&definition.id) {
            return Ok(Rc::clone(node));
        }
        let expr = definition.expr().ok_or("a reference to a lexicon")?;
        self.has_where |= !definition.relations().is_empty()
            || !definition.enhanced_relations().is_empty()
            || !definition.node_constraints().is_empty()
            || !definition.span_paths().is_empty();
        let spans = self
            .program
            .token_spans
            .get(&definition.id)
            .map_or(&[][..], |spans| spans);
        let mut spans = spans.iter().copied();
        let node = self.node(expr, &mut spans)?;
        if spans.next().is_some() {
            return Err("a body whose token spans were not recorded");
        }
        self.by_definition.insert(definition.id, Rc::clone(&node));
        Ok(node)
    }

    fn node(
        &mut self,
        expr: &'p BoundExpr,
        spans: &mut impl Iterator<Item = SourceSpan>,
    ) -> Result<Rc<Node<'p>>, &'static str> {
        let mut children = |nodes: &mut Self, exprs: &'p [BoundExpr]| {
            exprs
                .iter()
                .map(|child| nodes.node(child, spans))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(Rc::new(match expr {
            BoundExpr::Token(predicate) => Node::Token {
                predicate,
                span: spans
                    .next()
                    .ok_or("a body whose token spans were not recorded")?,
            },
            BoundExpr::Assertion(PositionAssertion::SentenceStart) => Node::SentenceStart,
            BoundExpr::Assertion(PositionAssertion::SentenceEnd) => Node::SentenceEnd,
            BoundExpr::Assertion(
                PositionAssertion::PreviousToken(_) | PositionAssertion::Lookaround(_),
            ) => return Err("a compiled position assertion"),
            BoundExpr::Lookaround(lookaround) => {
                if lookaround.scope.is_some() {
                    return Err("a `within=` lookaround");
                }
                if lookaround.skip_reach.is_some() {
                    return Err("a `skip:` declaration");
                }
                Node::Lookaround {
                    direction: lookaround.direction,
                    polarity: lookaround.polarity,
                    body: self.node(&lookaround.body, spans)?,
                    span: lookaround.span,
                }
            }
            BoundExpr::Sequence(exprs) => Node::Sequence(children(self, exprs)?),
            BoundExpr::Alternation(exprs) => Node::Alternation(children(self, exprs)?),
            BoundExpr::Intersection { operands, .. } => {
                Node::Intersection(children(self, operands)?)
            }
            BoundExpr::AnyOrder { operands, .. } => Node::AnyOrder(children(self, operands)?),
            BoundExpr::Repeat { expr, min, max } => Node::Repeat {
                node: self.node(expr, spans)?,
                min: *min,
                max: *max,
            },
            BoundExpr::Capture { expr, .. } => return self.node(expr, spans),
            BoundExpr::SameAs { .. } => return Err("a same-as reference to a capture"),
            BoundExpr::Definition { id, span } => {
                let definition = self
                    .program
                    .definitions
                    .get(id.index())
                    .ok_or("an unresolved reference")?;
                Node::Reference {
                    body: self.definition(definition)?,
                    span: *span,
                }
            }
        }))
    }
}

struct Failure {
    start: usize,
    position: usize,
    span: Option<SourceSpan>,
    reason: StopReason,
    /// Attribute tests of the failing token element that held.
    satisfied: usize,
}

struct Run<'t, S: ?Sized> {
    tokens: &'t S,
    /// End positions by node address, start position, and whether failures
    /// were being recorded.
    memo: HashMap<(usize, usize, bool), Rc<BTreeSet<usize>>>,
    steps: usize,
    truncated: bool,
    start: usize,
    /// False inside a lookaround body, whose failures are not progress of the
    /// attempt.
    recording: bool,
    best: Option<Failure>,
}

impl<S: TokenSequence + ?Sized> Run<'_, S> {
    fn fail(&mut self, position: usize, span: Option<SourceSpan>, reason: StopReason) {
        if !self.recording {
            return;
        }
        let satisfied = match &reason {
            StopReason::Token { tests } => tests.iter().filter(|test| test.held).count(),
            _ => 0,
        };
        let consumed = position.saturating_sub(self.start);
        let improves = self.best.as_ref().is_none_or(|best| {
            (consumed, satisfied) > (best.position.saturating_sub(best.start), best.satisfied)
        });
        if improves {
            self.best = Some(Failure {
                start: self.start,
                position,
                span,
                reason,
                satisfied,
            });
        }
    }

    fn ends(&mut self, node: &Rc<Node<'_>>, start: usize) -> Rc<BTreeSet<usize>> {
        let key = (Rc::as_ptr(node) as usize, start, self.recording);
        if let Some(known) = self.memo.get(&key) {
            return Rc::clone(known);
        }
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            self.truncated = true;
            return Rc::default();
        }
        let ends = Rc::new(self.evaluate(node, start));
        self.memo.insert(key, Rc::clone(&ends));
        ends
    }

    fn evaluate(&mut self, node: &Rc<Node<'_>>, start: usize) -> BTreeSet<usize> {
        let at = |held: bool| held.then_some(start).into_iter().collect::<BTreeSet<_>>();
        match node.as_ref() {
            Node::Token { predicate, span } => {
                if start >= self.tokens.len() {
                    self.fail(start, Some(*span), StopReason::SentenceEnded);
                    return BTreeSet::new();
                }
                let tests = predicate
                    .iter()
                    .map(|attribute| attribute_test(self.tokens, start, attribute))
                    .collect::<Vec<_>>();
                if tests.iter().all(|test| test.held) {
                    return BTreeSet::from([start + 1]);
                }
                self.fail(start, Some(*span), StopReason::Token { tests });
                BTreeSet::new()
            }
            Node::SentenceStart => {
                if start != 0 {
                    self.fail(start, None, StopReason::NotSentenceStart);
                }
                at(start == 0)
            }
            Node::SentenceEnd => {
                let held = start == self.tokens.len();
                if !held {
                    self.fail(start, None, StopReason::NotSentenceEnd);
                }
                at(held)
            }
            Node::Lookaround {
                direction,
                polarity,
                body,
                span,
            } => {
                let recording = std::mem::replace(&mut self.recording, false);
                let found = match direction {
                    LookDirection::FollowedBy => !self.ends(body, start).is_empty(),
                    LookDirection::PrecededBy => {
                        (0..=start).any(|from| self.ends(body, from).contains(&start))
                    }
                };
                self.recording = recording;
                let held = found == (*polarity == LookPolarity::Positive);
                if !held {
                    self.fail(
                        start,
                        Some(*span),
                        StopReason::Lookaround {
                            preceded_by: *direction == LookDirection::PrecededBy,
                            negative: *polarity == LookPolarity::Negative,
                        },
                    );
                }
                at(held)
            }
            Node::Reference { body, .. } => self.ends(body, start).as_ref().clone(),
            Node::Sequence(nodes) => {
                let mut positions = BTreeSet::from([start]);
                for node in nodes {
                    let mut next = BTreeSet::new();
                    for position in positions {
                        next.extend(self.ends(node, position).iter().copied());
                    }
                    positions = next;
                    if positions.is_empty() {
                        break;
                    }
                }
                positions
            }
            Node::Alternation(nodes) => {
                let mut ends = BTreeSet::new();
                for node in nodes {
                    ends.extend(self.ends(node, start).iter().copied());
                }
                ends
            }
            Node::Intersection(nodes) => {
                let mut agreed: Option<BTreeSet<usize>> = None;
                for node in nodes {
                    let ends = self.ends(node, start);
                    agreed = Some(match agreed {
                        Some(held) => held.intersection(&ends).copied().collect(),
                        None => ends.as_ref().clone(),
                    });
                }
                agreed.unwrap_or_default()
            }
            Node::AnyOrder(nodes) => {
                let remaining = (0..nodes.len()).collect::<Vec<_>>();
                let mut ends = BTreeSet::new();
                for (choice, first) in remaining.iter().copied().enumerate() {
                    let mut rest = remaining.clone();
                    rest.remove(choice);
                    for position in self.ends(&nodes[first], start).as_ref().clone() {
                        self.any_order_tail(nodes, &rest, position, &mut ends);
                    }
                }
                ends
            }
            Node::Repeat { node, min, max } => {
                let mut accepted = BTreeSet::new();
                if *min == 0 {
                    accepted.insert(start);
                }
                let mut frontier = BTreeSet::from([start]);
                let maximum = max.unwrap_or(self.tokens.len().saturating_sub(start) + 1);
                for count in 1..=maximum {
                    let mut next = BTreeSet::new();
                    for position in frontier {
                        next.extend(
                            self.ends(node, position)
                                .iter()
                                .copied()
                                .filter(|end| *end != position),
                        );
                    }
                    if next.is_empty() {
                        break;
                    }
                    if count >= *min {
                        accepted.extend(next.iter().copied());
                    }
                    frontier = next;
                }
                accepted
            }
        }
    }

    /// Records, for one derivation of `node` over `start..end`, each consumed
    /// token with the source spans of its element and of the references that
    /// enclose the element, innermost first. False when there is none.
    fn derive(
        &mut self,
        node: &Rc<Node<'_>>,
        start: usize,
        end: usize,
        enclosing: &mut Vec<SourceSpan>,
        consumed: &mut Vec<(usize, Vec<SourceSpan>)>,
    ) -> bool {
        if !self.ends(node, start).contains(&end) {
            return false;
        }
        match node.as_ref() {
            Node::Token { span, .. } => {
                let mut spans = vec![*span];
                spans.extend(enclosing.iter().rev().copied());
                consumed.push((start, spans));
                true
            }
            Node::SentenceStart | Node::SentenceEnd | Node::Lookaround { .. } => true,
            Node::Reference { body, span } => {
                enclosing.push(*span);
                let derived = self.derive(body, start, end, enclosing, consumed);
                enclosing.pop();
                derived
            }
            Node::Alternation(nodes) => nodes
                .iter()
                .any(|node| self.derive(node, start, end, enclosing, consumed)),
            Node::Intersection(nodes) => nodes
                .first()
                .is_some_and(|node| self.derive(node, start, end, enclosing, consumed)),
            Node::Sequence(nodes) => self.derive_sequence(nodes, start, end, enclosing, consumed),
            Node::Repeat { node, min, max } => {
                self.derive_repeat(node, (*min, *max), 0, start, end, enclosing, consumed)
            }
            // Operands may leave gaps, so the consumed tokens are not `start..end`.
            Node::AnyOrder(_) => false,
        }
    }

    fn derive_sequence(
        &mut self,
        nodes: &[Rc<Node<'_>>],
        start: usize,
        end: usize,
        enclosing: &mut Vec<SourceSpan>,
        consumed: &mut Vec<(usize, Vec<SourceSpan>)>,
    ) -> bool {
        let Some((first, rest)) = nodes.split_first() else {
            return start == end;
        };
        let kept = consumed.len();
        for middle in self.ends(first, start).as_ref().clone().into_iter().rev() {
            if middle <= end
                && self.derive(first, start, middle, enclosing, consumed)
                && self.derive_sequence(rest, middle, end, enclosing, consumed)
            {
                return true;
            }
            consumed.truncate(kept);
        }
        false
    }

    #[allow(clippy::too_many_arguments)]
    fn derive_repeat(
        &mut self,
        node: &Rc<Node<'_>>,
        (min, max): (usize, Option<usize>),
        count: usize,
        start: usize,
        end: usize,
        enclosing: &mut Vec<SourceSpan>,
        consumed: &mut Vec<(usize, Vec<SourceSpan>)>,
    ) -> bool {
        if start == end && count >= min {
            return true;
        }
        if max.is_some_and(|max| count >= max) {
            return false;
        }
        let kept = consumed.len();
        for middle in self.ends(node, start).as_ref().clone().into_iter().rev() {
            if middle > start
                && middle <= end
                && self.derive(node, start, middle, enclosing, consumed)
                && self.derive_repeat(
                    node,
                    (min, max),
                    count + 1,
                    middle,
                    end,
                    enclosing,
                    consumed,
                )
            {
                return true;
            }
            consumed.truncate(kept);
        }
        false
    }

    /// Places the remaining any-order operands, each at or after `position`.
    fn any_order_tail(
        &mut self,
        nodes: &[Rc<Node<'_>>],
        remaining: &[usize],
        position: usize,
        ends: &mut BTreeSet<usize>,
    ) {
        if remaining.is_empty() {
            ends.insert(position);
            return;
        }
        for (choice, operand) in remaining.iter().copied().enumerate() {
            let mut rest = remaining.to_vec();
            rest.remove(choice);
            for start in position..self.tokens.len() {
                for next in self.ends(&nodes[operand], start).as_ref().clone() {
                    self.any_order_tail(nodes, &rest, next, ends);
                }
            }
        }
    }
}

fn attribute_test<S: TokenSequence + ?Sized>(
    tokens: &S,
    token: usize,
    attribute: &SymbolicAttrValue,
) -> AttributeTestOutcome {
    let actual = tokens.value_key(token, &attribute.attr);
    let held = match actual {
        // As in the matcher: only a complemented test holds for a token
        // without the attribute.
        None => attribute.complement,
        Some(actual) => value_test(&attribute.value, actual) != attribute.complement,
    };
    let test = render_test(&attribute.value);
    AttributeTestOutcome {
        attribute: attribute.attr.to_string(),
        test: if attribute.complement {
            format!("!({test})")
        } else {
            test
        },
        actual: actual.map(|actual| match actual {
            AttributeValueRef::Scalar(value) => value.to_owned(),
            AttributeValueRef::StringSet(values) => format!("{{{}}}", values.join(", ")),
        }),
        held,
    }
}

fn value_test(expected: &AttrValueType, actual: AttributeValueRef<'_>) -> bool {
    use AttributeValueRef::{Scalar, StringSet};
    match (expected, actual) {
        (AttrValueType::Single(expected), Scalar(actual)) => actual == expected,
        (AttrValueType::OneOf(expected), Scalar(actual)) => {
            expected.iter().any(|candidate| candidate == actual)
        }
        (AttrValueType::NotIn(excluded), Scalar(actual)) => {
            excluded.iter().all(|candidate| candidate != actual)
        }
        (AttrValueType::Contains(needle), Scalar(actual)) => actual.contains(needle.as_str()),
        (AttrValueType::NotContains(needle), Scalar(actual)) => !actual.contains(needle.as_str()),
        (AttrValueType::Regex(regex), Scalar(actual)) => regex.is_match(actual),
        (AttrValueType::NotRegex(regex), Scalar(actual)) => !regex.is_match(actual),
        (AttrValueType::SetHas(expected), StringSet(actual)) => {
            actual.iter().any(|value| value == expected)
        }
        (AttrValueType::SetHasAny(expected), StringSet(actual)) => {
            actual.iter().any(|value| expected.contains(value))
        }
        (AttrValueType::SetNotHas(excluded), StringSet(actual)) => {
            actual.iter().all(|value| value != excluded)
        }
        (AttrValueType::SetNotHasAny(excluded), StringSet(actual)) => {
            actual.iter().all(|value| !excluded.contains(value))
        }
        (AttrValueType::Contains(needle), StringSet(actual)) => {
            actual.iter().any(|value| value.contains(needle.as_str()))
        }
        (AttrValueType::NotContains(needle), StringSet(actual)) => {
            actual.iter().all(|value| !value.contains(needle.as_str()))
        }
        (AttrValueType::Regex(regex), StringSet(actual)) => {
            actual.iter().any(|value| regex.is_match(value))
        }
        (AttrValueType::NotRegex(regex), StringSet(actual)) => {
            actual.iter().all(|value| !regex.is_match(value))
        }
        _ => false,
    }
}

fn render_test(value: &AttrValueType) -> String {
    let set = |values: &[compact_str::CompactString]| format!("{{{}}}", values.join(", "));
    match value {
        AttrValueType::Single(value) => format!("={value}"),
        AttrValueType::OneOf(values) => format!("in {}", set(values)),
        AttrValueType::NotIn(values) => format!("not in {}", set(values)),
        AttrValueType::SetHas(value) => format!("has {value}"),
        AttrValueType::SetHasAny(values) => format!("has any {}", set(values)),
        AttrValueType::SetNotHas(value) => format!("not has {value}"),
        AttrValueType::SetNotHasAny(values) => format!("not has any {}", set(values)),
        AttrValueType::Contains(needle) => format!("~{needle}"),
        AttrValueType::NotContains(needle) => format!("!~{needle}"),
        AttrValueType::Regex(regex) => format!("~/{regex}/"),
        AttrValueType::NotRegex(regex) => format!("!~/{regex}/"),
    }
}

/// What happened to one definition on one sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefinitionOutcome {
    /// The definition matched these `(start, end)` spans and result policy
    /// published them.
    Published { spans: Vec<(usize, usize)> },
    /// The definition matched these spans and result policy published none.
    NotPublished { spans: Vec<(usize, usize)> },
    /// The body matched these spans and the `where` clause or a span
    /// constraint rejected each.
    WhereRejected { spans: Vec<(usize, usize)> },
    /// No start position matched the body.
    Stopped(StoppedAttempt),
    /// The explanation pass cannot account for the missing match.
    Unexplained { reason: String },
}

/// [`DefinitionOutcome`] with the extent of the explanation pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefinitionExplanation {
    pub outcome: DefinitionOutcome,
    /// Start positions the explanation pass attempted; zero when the matcher
    /// answered.
    pub starts_tried: usize,
    /// The explanation pass reached its step limit.
    pub truncated: bool,
}

impl super::CompileOutput {
    /// Why `definition` does or does not produce a published match on `tokens`.
    ///
    /// # Errors
    ///
    /// As [`crate::fsm::CompiledCatalog::try_explain_sentence`].
    pub fn explain_definition<S: TokenSequence + ?Sized>(
        &self,
        definition: &str,
        tokens: &S,
    ) -> Result<DefinitionExplanation, crate::fsm::MatchPublicationError> {
        let explained = self.matcher.try_explain_sentence(tokens)?;
        Ok(self
            .no_match
            .explain_definition(definition, tokens, &explained.candidates))
    }
}

impl NoMatchExplainer {
    /// Why `definition` does or does not produce a published match on `tokens`.
    ///
    /// `candidates` is what the matcher found for the sentence, and decides
    /// whether the definition matched and what policy published. For a
    /// definition without a candidate, [`Self::explain`] evaluates the body
    /// separately.
    pub fn explain_definition<S: TokenSequence + ?Sized>(
        &self,
        definition: &str,
        tokens: &S,
        candidates: &[crate::fsm::ExplainedMatch],
    ) -> DefinitionExplanation {
        let candidates = candidates
            .iter()
            .filter(|candidate| candidate.raw.pattern == definition)
            .collect::<Vec<_>>();
        if !candidates.is_empty() {
            let spans = |published: bool| {
                candidates
                    .iter()
                    .filter(|candidate| candidate.published.is_some() == published)
                    .map(|candidate| (candidate.raw.start, candidate.raw.end))
                    .collect::<Vec<_>>()
            };
            let published = spans(true);
            let outcome = if published.is_empty() {
                DefinitionOutcome::NotPublished {
                    spans: spans(false),
                }
            } else {
                DefinitionOutcome::Published { spans: published }
            };
            return DefinitionExplanation {
                outcome,
                starts_tried: 0,
                truncated: false,
            };
        }
        let report = self.explain(definition, tokens);
        let outcome = match report.outcome {
            NoMatchOutcome::Stopped(attempt) => DefinitionOutcome::Stopped(attempt),
            NoMatchOutcome::BodyMatches {
                spans,
                has_where: true,
            } => DefinitionOutcome::WhereRejected { spans },
            NoMatchOutcome::BodyMatches {
                has_where: false, ..
            } => DefinitionOutcome::Unexplained {
                reason: "the explanation pass matched the body and the matcher did not".to_owned(),
            },
            NoMatchOutcome::UnknownDefinition => DefinitionOutcome::Unexplained {
                reason: format!("no pattern definition is named {definition}"),
            },
            NoMatchOutcome::Unsupported { construct } => DefinitionOutcome::Unexplained {
                reason: format!("the explanation pass does not evaluate {construct}"),
            },
            NoMatchOutcome::EmptySentence => DefinitionOutcome::Unexplained {
                reason: "the sentence has no tokens".to_owned(),
            },
        };
        DefinitionExplanation {
            outcome,
            starts_tried: report.starts_tried,
            truncated: report.truncated,
        }
    }
}

/// Where a definition's `examples` or `counterexamples` list is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimSite {
    /// The definition writes the key itself; the byte range is the key.
    Declared { key: Range<usize> },
    /// The key reaches the definition from `defaults` or a bundle.
    Inherited,
    /// The definition has no such key; the byte offset is where it starts.
    Absent { definition_start: usize },
}

impl super::CompileOutput {
    /// Where `key` is written for the pattern definition named `definition`.
    #[must_use]
    pub fn claim_site(&self, definition: &str, key: &str) -> Option<ClaimSite> {
        let definition = self
            .no_match
            .program
            .definitions
            .iter()
            .find(|candidate| candidate.name == definition && candidate.expr().is_some())?;
        Some(match definition.metadata.get(key) {
            None => ClaimSite::Absent {
                definition_start: definition.span.byte_range().start as usize,
            },
            Some(entry) if entry.inherited => ClaimSite::Inherited,
            Some(entry) => {
                let key = entry.span.byte_range();
                ClaimSite::Declared {
                    key: key.start as usize..key.end as usize,
                }
            }
        })
    }
}

/// One token of a match and the pattern source that consumed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsumedToken {
    /// Sentence-local token index.
    pub token: usize,
    /// The token element, then each reference the element was reached
    /// through, innermost first.
    pub elements: Vec<ElementLocation>,
}

impl super::CompileOutput {
    /// The elements of `definition` that consume the tokens `start..end`, in
    /// one derivation of its body.
    ///
    /// `None` when the body has no derivation over that span, the explanation
    /// pass reached its step limit, or the body uses a construct the pass does
    /// not evaluate, which includes an any-order group. A body with several
    /// derivations over the span reports one; it need not be the one whose
    /// captures the matcher published.
    #[must_use]
    pub fn consumed_tokens<S: TokenSequence + ?Sized>(
        &self,
        definition: &str,
        tokens: &S,
        start: usize,
        end: usize,
    ) -> Option<Vec<ConsumedToken>> {
        let explainer = &self.no_match;
        let target = explainer
            .program
            .definitions
            .iter()
            .find(|candidate| candidate.name == definition && candidate.expr().is_some())?;
        let mut nodes = Nodes {
            program: &explainer.program,
            by_definition: HashMap::new(),
            has_where: false,
        };
        let root = nodes.definition(target).ok()?;
        let mut run = Run {
            tokens,
            memo: HashMap::new(),
            steps: 0,
            truncated: false,
            start,
            recording: false,
            best: None,
        };
        let mut consumed = Vec::new();
        let derived = run.derive(&root, start, end, &mut Vec::new(), &mut consumed);
        (derived && !run.truncated).then(|| {
            consumed
                .into_iter()
                .map(|(token, spans)| ConsumedToken {
                    token,
                    elements: spans
                        .into_iter()
                        .map(|span| explainer.location(span))
                        .collect(),
                })
                .collect()
        })
    }
}
