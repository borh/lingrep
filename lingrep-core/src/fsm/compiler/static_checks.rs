//! Checks that hold for every token sequence, computed from the bound program
//! without a corpus: token tests no token satisfies, and whether one
//! definition publishes only spans another also publishes.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use compact_str::CompactString;

use super::model::{
    BoundDefinition, BoundExpr, BoundProgram, DefinitionId, DefinitionVisibility, LookPolarity,
};
use super::no_match::{ElementLocation, NoMatchExplainer, render_test, value_test};
use crate::fsm::{AttrValueType, SymbolicAttrValue};
use crate::token_match::{AttributeKey, AttributeValueRef};

/// Substring needles or set members per attribute above which candidate values
/// are not enumerated completely.
const MAX_ENUMERATED_OPERANDS: usize = 5;
/// Candidate tokens per product state above which containment is not decided.
const MAX_TOKENS_PER_STATE: usize = 4_096;
/// Automaton states per definition above which containment is not decided.
const MAX_AUTOMATON_STATES: usize = 20_000;
/// Product states per pair above which containment is not decided.
const MAX_PRODUCT_STATES: usize = 50_000;
const MAX_REFERENCE_DEPTH: usize = 64;

/// A token test, or a whole definition, that no token sequence satisfies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unsatisfiable {
    pub definition: String,
    pub kind: UnsatisfiableKind,
    pub location: ElementLocation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnsatisfiableKind {
    /// One conjunction of attribute tests, rendered as `[a=x & a=y]`. A token
    /// test with several alternatives reports each failing alternative.
    TokenTest { test: String },
    /// The body matches no token sequence.
    Definition,
}

/// Whether every span one definition publishes is also published by another.
///
/// A definition publishes its longest match from each start, so the second
/// definition must match each such span and nothing longer from the same
/// start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Containment {
    Contained,
    /// A sentence on which the first definition publishes the leading `span`
    /// tokens and the second does not: it matches no such span, or a longer
    /// one. The sentence is a shortest one unless a test of either definition
    /// uses a regex or more operands than are enumerated.
    Distinguished {
        tokens: Vec<WitnessToken>,
        span: usize,
    },
    /// The bodies use a form the check does not model.
    NotDecided {
        construct: &'static str,
    },
    UnknownDefinition,
}

/// One token of a distinguishing sequence: the attributes the compared tests
/// read at this position. An attribute that is not listed is absent.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WitnessToken {
    pub attributes: Vec<WitnessAttribute>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WitnessAttribute {
    pub key: AttributeKey,
    pub value: WitnessValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WitnessValue {
    /// A value one of the compared tests names.
    Named(CompactString),
    /// A constructed value that no compared test names. It contains exactly
    /// the substrings the tests required of it.
    Unnamed(CompactString),
    Set(Box<[CompactString]>),
}

/// Two emitting definitions of which the first publishes only spans the second
/// also publishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainedPair {
    pub contained: String,
    pub containing: String,
    /// The second is also contained in the first. Such a pair is listed once,
    /// in name order.
    pub equivalent: bool,
}

/// The result of comparing every pair of emitting definitions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContainmentReport {
    pub pairs: Vec<ContainedPair>,
    /// Emitting definitions left out because their body uses a form the check
    /// does not model, with that form.
    pub not_decided: Vec<(String, &'static str)>,
}

impl NoMatchExplainer {
    /// Token tests and definitions that no token sequence satisfies.
    ///
    /// A test is reported only when every value of one attribute fails it. A
    /// regex is not analysed: it is assumed satisfiable by any value the other
    /// tests do not name.
    #[must_use]
    pub fn unsatisfiable(&self) -> Vec<Unsatisfiable> {
        let program = self.program();
        let mut empty = Emptiness {
            program,
            memo: HashMap::new(),
        };
        let mut findings = Vec::new();
        for definition in &program.definitions {
            if crate::fsm::policy::is_hidden(&definition.name) {
                continue;
            }
            let Some(expr) = definition.expr() else {
                continue;
            };
            let spans = program
                .token_spans
                .get(&definition.id)
                .map_or(&[][..], |spans| spans);
            let mut predicates = Vec::new();
            own_token_tests(expr, &mut predicates);
            // The spans are recorded in the same pre-order; a body whose spans
            // were not recorded reports its definition instead.
            let located = predicates.len() == spans.len();
            for (index, predicate) in predicates.iter().enumerate() {
                if !predicate_unsatisfiable(predicate) {
                    continue;
                }
                let span = if located {
                    spans[index]
                } else {
                    definition.span
                };
                findings.push(Unsatisfiable {
                    definition: definition.name.to_string(),
                    kind: UnsatisfiableKind::TokenTest {
                        test: render_predicate(predicate),
                    },
                    location: self.location(span),
                });
            }
            if empty.definition(definition, 0) {
                findings.push(Unsatisfiable {
                    definition: definition.name.to_string(),
                    kind: UnsatisfiableKind::Definition,
                    location: self.location(definition.span),
                });
            }
        }
        findings
    }

    /// Whether every span `contained` publishes is also published by
    /// `containing`.
    ///
    /// Spans are compared, not records: captures, keys and the result policy
    /// are not part of the comparison.
    #[must_use]
    pub fn containment(&self, contained: &str, containing: &str) -> Containment {
        let program = self.program();
        let find = |name: &str| {
            program
                .definitions
                .iter()
                .find(|candidate| candidate.name == name && candidate.expr().is_some())
        };
        let (Some(left), Some(right)) = (find(contained), find(containing)) else {
            return Containment::UnknownDefinition;
        };
        match (Automaton::of(program, left), Automaton::of(program, right)) {
            (Ok(left), Ok(right)) => compare(&left, &right),
            (Err(construct), _) | (_, Err(construct)) => Containment::NotDecided { construct },
        }
    }

    /// Compares every ordered pair of emitting definitions.
    #[must_use]
    pub fn containments(&self) -> ContainmentReport {
        let program = self.program();
        let mut report = ContainmentReport::default();
        let mut automata = Vec::new();
        for definition in &program.definitions {
            if definition.visibility() != Some(DefinitionVisibility::Pattern)
                || definition
                    .name
                    .starts_with(crate::fsm::policy::HIDDEN_PREFIX)
            {
                continue;
            }
            match Automaton::of(program, definition) {
                Ok(automaton) => automata.push((definition.name.as_str(), automaton)),
                Err(construct) => report
                    .not_decided
                    .push((definition.name.to_string(), construct)),
            }
        }
        let contained = |left: &Automaton<'_>, right: &Automaton<'_>| {
            compare(left, right) == Containment::Contained
        };
        for (index, (left_name, left)) in automata.iter().enumerate() {
            for (right_name, right) in &automata[index + 1..] {
                let forward = contained(left, right);
                let backward = contained(right, left);
                let pair = |contained: &str, containing: &str, equivalent| ContainedPair {
                    contained: contained.to_owned(),
                    containing: containing.to_owned(),
                    equivalent,
                };
                match (forward, backward) {
                    (true, true) => {
                        let (first, second) = if left_name <= right_name {
                            (left_name, right_name)
                        } else {
                            (right_name, left_name)
                        };
                        report.pairs.push(pair(first, second, true));
                    }
                    (true, false) => report.pairs.push(pair(left_name, right_name, false)),
                    (false, true) => report.pairs.push(pair(right_name, left_name, false)),
                    (false, false) => {}
                }
            }
        }
        report
            .pairs
            .sort_by(|a, b| (&a.contained, &a.containing).cmp(&(&b.contained, &b.containing)));
        report.not_decided.sort();
        report
    }
}

/// The token tests written in `expr`, in pre-order, without following
/// references: a referenced definition reports its own.
fn own_token_tests<'p>(expr: &'p BoundExpr, out: &mut Vec<&'p [SymbolicAttrValue]>) {
    match expr {
        BoundExpr::Token(predicate) => out.push(predicate),
        BoundExpr::Lookaround(lookaround) => own_token_tests(&lookaround.body, out),
        BoundExpr::Sequence(children)
        | BoundExpr::Alternation(children)
        | BoundExpr::Intersection {
            operands: children, ..
        }
        | BoundExpr::AnyOrder {
            operands: children, ..
        } => {
            for child in children {
                own_token_tests(child, out);
            }
        }
        BoundExpr::Repeat { expr, .. } | BoundExpr::Capture { expr, .. } => {
            own_token_tests(expr, out);
        }
        BoundExpr::Assertion(_) | BoundExpr::SameAs { .. } | BoundExpr::Definition { .. } => {}
    }
}

fn render_predicate(predicate: &[SymbolicAttrValue]) -> String {
    let atoms = predicate
        .iter()
        .map(|atom| {
            let test = render_test(&atom.value);
            let separator = if test.starts_with(|first: char| first.is_alphabetic()) {
                " "
            } else {
                ""
            };
            let test = format!("{}{separator}{test}", atom.attr);
            if atom.complement {
                format!("!({test})")
            } else {
                test
            }
        })
        .collect::<Vec<_>>();
    format!("[{}]", atoms.join(" & "))
}

struct Emptiness<'p> {
    program: &'p BoundProgram,
    memo: HashMap<DefinitionId, bool>,
}

impl<'p> Emptiness<'p> {
    fn definition(&mut self, definition: &'p BoundDefinition, depth: usize) -> bool {
        if let Some(known) = self.memo.get(&definition.id) {
            return *known;
        }
        let empty = definition
            .expr()
            .is_some_and(|expr| depth < MAX_REFERENCE_DEPTH && self.expr(expr, depth));
        self.memo.insert(definition.id, empty);
        empty
    }

    /// Whether `expr` certainly matches no token sequence.
    fn expr(&mut self, expr: &'p BoundExpr, depth: usize) -> bool {
        match expr {
            BoundExpr::Token(predicate) => predicate_unsatisfiable(predicate),
            BoundExpr::Sequence(children)
            | BoundExpr::Intersection {
                operands: children, ..
            }
            | BoundExpr::AnyOrder {
                operands: children, ..
            } => children.iter().any(|child| self.expr(child, depth)),
            BoundExpr::Alternation(children) => {
                !children.is_empty() && children.iter().all(|child| self.expr(child, depth))
            }
            BoundExpr::Repeat { expr, min, .. } => *min > 0 && self.expr(expr, depth),
            BoundExpr::Capture { expr, .. } => self.expr(expr, depth),
            BoundExpr::Lookaround(lookaround) => {
                lookaround.polarity == LookPolarity::Positive && self.expr(&lookaround.body, depth)
            }
            BoundExpr::Definition { id, .. } => self
                .program
                .definitions
                .iter()
                .find(|candidate| candidate.id == *id)
                .is_some_and(|definition| self.definition(definition, depth + 1)),
            BoundExpr::Assertion(_) | BoundExpr::SameAs { .. } => false,
        }
    }
}

/// One value an attribute can take on a candidate token.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Value {
    Absent,
    Named(CompactString),
    Unnamed(CompactString),
    Set(Box<[CompactString]>),
}

impl Value {
    fn as_ref(&self) -> Option<AttributeValueRef<'_>> {
        match self {
            Self::Absent => None,
            Self::Named(value) | Self::Unnamed(value) => Some(AttributeValueRef::Scalar(value)),
            Self::Set(values) => Some(AttributeValueRef::StringSet(values)),
        }
    }
}

fn is_set_test(value: &AttrValueType) -> bool {
    matches!(
        value,
        AttrValueType::SetHas(_)
            | AttrValueType::SetHasAny(_)
            | AttrValueType::SetNotHas(_)
            | AttrValueType::SetNotHasAny(_)
    )
}

fn is_regex_test(value: &AttrValueType) -> bool {
    matches!(value, AttrValueType::Regex(_) | AttrValueType::NotRegex(_))
}

/// The values of one attribute that the tests in `atoms` can tell apart.
struct Candidates {
    values: Vec<Value>,
    /// Every value the tests can tell apart is listed. False when an operand
    /// count exceeds [`MAX_ENUMERATED_OPERANDS`].
    complete: bool,
    /// `complete`, and every test has a definite result on every listed value
    /// that also holds for the unlisted values the listed one stands for.
    exact: bool,
}

/// Every subset of `items` when there are at most
/// [`MAX_ENUMERATED_OPERANDS`]; otherwise the empty set, each single item and
/// the whole set.
fn subsets<T: Clone>(items: &[T]) -> (Vec<Vec<T>>, bool) {
    if items.len() <= MAX_ENUMERATED_OPERANDS {
        let all = (0..1_usize << items.len())
            .map(|mask| {
                items
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| mask >> index & 1 == 1)
                    .map(|(_, item)| item.clone())
                    .collect()
            })
            .collect();
        return (all, true);
    }
    let mut some = vec![Vec::new(), items.to_vec()];
    some.extend(items.iter().map(|item| vec![item.clone()]));
    (some, false)
}

fn candidates(atoms: &[&SymbolicAttrValue]) -> Candidates {
    let mut literals = BTreeSet::new();
    let mut needles = BTreeSet::new();
    let mut members = BTreeSet::new();
    let mut regex = false;
    for atom in atoms {
        match &atom.value {
            AttrValueType::Single(value) => {
                literals.insert(value.clone());
            }
            AttrValueType::OneOf(values) | AttrValueType::NotIn(values) => {
                literals.extend(values.iter().cloned());
            }
            AttrValueType::Contains(value) | AttrValueType::NotContains(value) => {
                needles.insert(value.clone());
            }
            AttrValueType::SetHas(value) | AttrValueType::SetNotHas(value) => {
                members.insert(value.clone());
            }
            AttrValueType::SetHasAny(values) | AttrValueType::SetNotHasAny(values) => {
                members.extend(values.iter().cloned());
            }
            AttrValueType::Regex(_) | AttrValueType::NotRegex(_) => regex = true,
        }
    }
    let mut values = vec![Value::Absent];
    if atoms.iter().any(|atom| is_set_test(&atom.value)) {
        let members = members.into_iter().collect::<Vec<_>>();
        let (sets, complete) = subsets(&members);
        values.extend(sets.into_iter().map(|set| Value::Set(set.into())));
        let only_set_tests = atoms.iter().all(|atom| is_set_test(&atom.value));
        return Candidates {
            values,
            complete,
            exact: complete && only_set_tests,
        };
    }
    values.extend(literals.iter().cloned().map(Value::Named));
    // A separator that occurs in no operand, so that a constructed value
    // equals no literal and contains a needle only inside one of its parts.
    let separator = ('\u{E000}'..='\u{F8FF}')
        .find(|candidate| {
            literals
                .iter()
                .chain(&needles)
                .all(|operand| !operand.contains(*candidate))
        })
        .unwrap_or('\u{E000}');
    let needles = needles.into_iter().collect::<Vec<_>>();
    let (parts, complete) = subsets(&needles);
    for parts in parts {
        let mut value = String::from(separator);
        for part in parts {
            value.push_str(&part);
            value.push(separator);
        }
        values.push(Value::Unnamed(value.into()));
    }
    Candidates {
        values,
        complete,
        exact: complete && !regex,
    }
}

/// Whether `atom` holds on `value`, or `None` when the listed value does not
/// decide it for the values it stands for.
fn atom_holds(atom: &SymbolicAttrValue, value: &Value) -> Option<bool> {
    let Some(actual) = value.as_ref() else {
        return Some(atom.complement);
    };
    let undecided = match value {
        Value::Unnamed(_) => is_regex_test(&atom.value),
        Value::Set(_) => !is_set_test(&atom.value),
        Value::Absent | Value::Named(_) => false,
    };
    (!undecided).then(|| value_test(&atom.value, actual) != atom.complement)
}

/// Whether no token satisfies the conjunction `predicate`: some attribute has
/// no value on which all of its tests can hold.
fn predicate_unsatisfiable(predicate: &[SymbolicAttrValue]) -> bool {
    let mut by_attribute: BTreeMap<&AttributeKey, Vec<&SymbolicAttrValue>> = BTreeMap::new();
    for atom in predicate {
        by_attribute.entry(&atom.attr).or_default().push(atom);
    }
    by_attribute.values().any(|atoms| {
        let candidates = candidates(atoms);
        candidates.complete
            && candidates.values.iter().all(|value| {
                atoms
                    .iter()
                    .any(|atom| atom_holds(atom, value) == Some(false))
            })
    })
}

type Predicate<'p> = Vec<&'p SymbolicAttrValue>;

/// An ε-NFA over token predicates for one definition body.
struct Automaton<'p> {
    epsilon: Vec<Vec<usize>>,
    steps: Vec<Vec<(Predicate<'p>, usize)>>,
    start: usize,
    accept: usize,
}

struct Builder<'p> {
    program: &'p BoundProgram,
    epsilon: Vec<Vec<usize>>,
    steps: Vec<Vec<(Predicate<'p>, usize)>>,
}

impl<'p> Automaton<'p> {
    fn of(
        program: &'p BoundProgram,
        definition: &'p BoundDefinition,
    ) -> Result<Self, &'static str> {
        let mut builder = Builder {
            program,
            epsilon: Vec::new(),
            steps: Vec::new(),
        };
        let start = builder.state()?;
        let accept = builder.definition(definition, start, 0)?;
        Ok(Self {
            epsilon: builder.epsilon,
            steps: builder.steps,
            start,
            accept,
        })
    }

    fn closure(&self, states: impl IntoIterator<Item = usize>) -> BTreeSet<usize> {
        let mut closed = BTreeSet::new();
        let mut pending = states.into_iter().collect::<Vec<_>>();
        while let Some(state) = pending.pop() {
            if closed.insert(state) {
                pending.extend(&self.epsilon[state]);
            }
        }
        closed
    }
}

impl<'p> Builder<'p> {
    fn state(&mut self) -> Result<usize, &'static str> {
        if self.epsilon.len() >= MAX_AUTOMATON_STATES {
            return Err("a body too large to expand");
        }
        self.epsilon.push(Vec::new());
        self.steps.push(Vec::new());
        Ok(self.epsilon.len() - 1)
    }

    fn definition(
        &mut self,
        definition: &'p BoundDefinition,
        from: usize,
        depth: usize,
    ) -> Result<usize, &'static str> {
        if depth > MAX_REFERENCE_DEPTH {
            return Err("references nested too deeply");
        }
        let expr = definition.expr().ok_or("a reference to a lexicon")?;
        if !definition.relations().is_empty()
            || !definition.enhanced_relations().is_empty()
            || !definition.node_constraints().is_empty()
            || !definition.span_paths().is_empty()
        {
            return Err("a `where` clause");
        }
        self.expr(expr, from, depth)
    }

    /// Adds `expr` starting at `from` and returns the state it ends in.
    fn expr(
        &mut self,
        expr: &'p BoundExpr,
        from: usize,
        depth: usize,
    ) -> Result<usize, &'static str> {
        match expr {
            BoundExpr::Token(predicate) => {
                let to = self.state()?;
                self.steps[from].push((predicate.iter().collect(), to));
                Ok(to)
            }
            BoundExpr::Sequence(children) => children
                .iter()
                .try_fold(from, |at, child| self.expr(child, at, depth)),
            BoundExpr::Alternation(children) => {
                let end = self.state()?;
                for child in children {
                    let start = self.state()?;
                    self.epsilon[from].push(start);
                    let child_end = self.expr(child, start, depth)?;
                    self.epsilon[child_end].push(end);
                }
                Ok(end)
            }
            BoundExpr::Repeat { expr, min, max } => {
                let mut at = from;
                for _ in 0..*min {
                    at = self.expr(expr, at, depth)?;
                }
                let end = self.state()?;
                self.epsilon[at].push(end);
                match max {
                    None => {
                        let body_end = self.expr(expr, end, depth)?;
                        self.epsilon[body_end].push(end);
                    }
                    Some(max) => {
                        for _ in *min..*max {
                            at = self.expr(expr, at, depth)?;
                            self.epsilon[at].push(end);
                        }
                    }
                }
                Ok(end)
            }
            BoundExpr::Capture { expr, .. } => self.expr(expr, from, depth),
            BoundExpr::Definition { id, .. } => {
                let definition = self
                    .program
                    .definitions
                    .iter()
                    .find(|candidate| candidate.id == *id)
                    .ok_or("an unresolved reference")?;
                self.definition(definition, from, depth + 1)
            }
            BoundExpr::Intersection { operands, .. } => self.intersection(operands, from, depth),
            BoundExpr::Assertion(_) => Err("a position assertion"),
            BoundExpr::Lookaround(_) => Err("a lookaround"),
            BoundExpr::AnyOrder { .. } => Err("an any-order group"),
            BoundExpr::SameAs { .. } => Err("a `same_as` reference"),
        }
    }

    /// The synchronous product of the operands: a step is taken when every
    /// operand takes one, on a token that satisfies all of their predicates.
    fn intersection(
        &mut self,
        operands: &'p [BoundExpr],
        from: usize,
        depth: usize,
    ) -> Result<usize, &'static str> {
        let mut starts = Vec::new();
        let mut ends = Vec::new();
        for operand in operands {
            let start = self.state()?;
            ends.push(self.expr(operand, start, depth)?);
            starts.push(start);
        }
        let end = self.state()?;
        let first = self.state()?;
        self.epsilon[from].push(first);
        let mut product = HashMap::from([(starts.clone(), first)]);
        let mut pending = vec![starts];
        while let Some(tuple) = pending.pop() {
            let state = product[&tuple];
            if tuple == ends {
                self.epsilon[state].push(end);
            }
            let mut successors: Vec<(Option<Predicate<'p>>, Vec<usize>)> = Vec::new();
            for (index, component) in tuple.iter().enumerate() {
                for target in &self.epsilon[*component] {
                    let mut next = tuple.clone();
                    next[index] = *target;
                    successors.push((None, next));
                }
            }
            let mut joint: Vec<(Predicate<'p>, Vec<usize>)> = vec![(Vec::new(), Vec::new())];
            for component in &tuple {
                let mut extended = Vec::new();
                for (predicate, targets) in &joint {
                    for (step, target) in &self.steps[*component] {
                        let mut predicate = predicate.clone();
                        predicate.extend(step);
                        let mut targets = targets.clone();
                        targets.push(*target);
                        extended.push((predicate, targets));
                    }
                }
                joint = extended;
            }
            successors.extend(
                joint
                    .into_iter()
                    .map(|(predicate, targets)| (Some(predicate), targets)),
            );
            for (predicate, next) in successors {
                let target = if let Some(target) = product.get(&next) {
                    *target
                } else {
                    let target = self.state()?;
                    product.insert(next.clone(), target);
                    pending.push(next);
                    target
                };
                match predicate {
                    None => self.epsilon[state].push(target),
                    Some(predicate) => self.steps[state].push((predicate, target)),
                }
            }
        }
        Ok(end)
    }
}

/// Where a product state stands relative to the span the left definition
/// publishes from the first token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Phase {
    /// No token read. A body that accepts here matches a zero-width span,
    /// which is not published.
    Start,
    /// The left definition can still match a longer span.
    Reading,
    /// The left definition matched the first `span` tokens and has matched no
    /// longer prefix since.
    Past { span: usize },
}

type ProductState = (BTreeSet<usize>, BTreeSet<usize>, bool);

struct Reached {
    left: BTreeSet<usize>,
    right: BTreeSet<usize>,
    phase: Phase,
    length: usize,
    via: Option<(usize, WitnessToken)>,
}

/// Searches breadth-first for a sentence on which `left` publishes a span
/// from the first token that `right` does not, determinizing both on the
/// tokens their next steps can tell apart.
///
/// A definition publishes its longest match from each start. `left` therefore
/// publishes a span `right` does not in two cases: `left` matches a sequence
/// `right` does not match, or both match it and `right` alone matches a longer
/// one that extends it.
fn compare(left: &Automaton<'_>, right: &Automaton<'_>) -> Containment {
    let mut reached = vec![Reached {
        left: left.closure([left.start]),
        right: right.closure([right.start]),
        phase: Phase::Start,
        length: 0,
        via: None,
    }];
    let mut index_of: HashMap<ProductState, usize> = HashMap::new();
    let mut pending = VecDeque::from([0_usize]);
    let mut exact = true;
    while let Some(index) = pending.pop_front() {
        let node = &reached[index];
        let (left_states, right_states) = (node.left.clone(), node.right.clone());
        let (phase, length) = (node.phase, node.length);
        let left_accepts = left_states.contains(&left.accept);
        let right_accepts = right_states.contains(&right.accept);
        let found = match phase {
            Phase::Start => None,
            Phase::Reading => (left_accepts && !right_accepts).then_some(length),
            Phase::Past { span } => right_accepts.then_some(span),
        };
        if let Some(span) = found {
            let mut tokens = Vec::new();
            let mut at = index;
            while let Some((parent, token)) = &reached[at].via {
                tokens.push(token.clone());
                at = *parent;
            }
            tokens.reverse();
            return Containment::Distinguished { tokens, span };
        }
        let left_steps = left_states
            .iter()
            .flat_map(|state| &left.steps[*state])
            .collect::<Vec<_>>();
        let right_steps = right_states
            .iter()
            .flat_map(|state| &right.steps[*state])
            .collect::<Vec<_>>();
        let past = matches!(phase, Phase::Past { .. });
        // Once `left` has matched a span, `right` alone can extend it.
        let extendable = (past || left_accepts) && !right_steps.is_empty();
        if !extendable && (past || left_steps.is_empty()) {
            continue;
        }
        let mut by_attribute: BTreeMap<&AttributeKey, Vec<&SymbolicAttrValue>> = BTreeMap::new();
        for (predicate, _) in left_steps.iter().chain(&right_steps) {
            for atom in predicate {
                by_attribute.entry(&atom.attr).or_default().push(atom);
            }
        }
        let mut choices = Vec::new();
        let mut combinations = 1_usize;
        for (key, atoms) in &by_attribute {
            let candidates = candidates(atoms);
            exact &= candidates.exact;
            combinations = combinations.saturating_mul(candidates.values.len());
            choices.push((*key, candidates.values));
        }
        if combinations > MAX_TOKENS_PER_STATE {
            return Containment::NotDecided {
                construct: "token tests with too many value combinations",
            };
        }
        let mut selection = vec![0_usize; choices.len()];
        loop {
            let token = choices
                .iter()
                .zip(&selection)
                .map(|((key, values), chosen)| (*key, &values[*chosen]))
                .collect::<BTreeMap<_, _>>();
            let holds = |predicate: &Predicate<'_>| {
                predicate.iter().all(|atom| {
                    let value = token[&atom.attr];
                    match value.as_ref() {
                        None => atom.complement,
                        Some(actual) => value_test(&atom.value, actual) != atom.complement,
                    }
                })
            };
            let targets = |steps: &[&(Predicate<'_>, usize)]| {
                steps
                    .iter()
                    .filter(|(predicate, _)| holds(predicate))
                    .map(|(_, target)| *target)
                    .collect::<Vec<_>>()
            };
            let next_left = left.closure(targets(&left_steps));
            let next_right = right.closure(targets(&right_steps));
            let mut successors = Vec::new();
            if !past && !next_left.is_empty() {
                successors.push(Phase::Reading);
            }
            // Past a span `left` matched, only sentences on which `left`
            // matches nothing longer and `right` may still match are followed.
            let span = match phase {
                Phase::Past { span } => Some(span),
                Phase::Reading if left_accepts => Some(length),
                Phase::Start | Phase::Reading => None,
            };
            if let Some(span) = span
                && !next_left.contains(&left.accept)
                && !next_right.is_empty()
            {
                successors.push(Phase::Past { span });
            }
            for next_phase in successors {
                let key = (
                    next_left.clone(),
                    next_right.clone(),
                    matches!(next_phase, Phase::Past { .. }),
                );
                if index_of.contains_key(&key) {
                    continue;
                }
                if reached.len() >= MAX_PRODUCT_STATES {
                    return Containment::NotDecided {
                        construct: "bodies with too many states to compare",
                    };
                }
                let witness = WitnessToken {
                    attributes: token
                        .iter()
                        .filter_map(|(key, value)| {
                            let value = match value {
                                Value::Absent => return None,
                                Value::Named(value) => WitnessValue::Named(value.clone()),
                                Value::Unnamed(value) => WitnessValue::Unnamed(value.clone()),
                                Value::Set(values) => WitnessValue::Set(values.clone()),
                            };
                            Some(WitnessAttribute {
                                key: (*key).clone(),
                                value,
                            })
                        })
                        .collect(),
                };
                index_of.insert(key, reached.len());
                pending.push_back(reached.len());
                reached.push(Reached {
                    left: next_left.clone(),
                    right: next_right.clone(),
                    phase: next_phase,
                    length: length + 1,
                    via: Some((index, witness)),
                });
            }
            // Advance to the next combination of candidate values.
            let mut position = 0;
            while position < selection.len() {
                selection[position] += 1;
                if selection[position] < choices[position].1.len() {
                    break;
                }
                selection[position] = 0;
                position += 1;
            }
            if position == selection.len() {
                break;
            }
        }
    }
    if exact {
        Containment::Contained
    } else {
        Containment::NotDecided {
            construct: "a regex test, or a test with too many operands to enumerate",
        }
    }
}
