//! Independent test-only interpreter for validated bound lingrep patterns programs.

use super::model::{
    BoundExpr, BoundProgram, CaptureId, CaptureProjection, ExecutionDefinition, ExecutionExpr,
    ExecutionRelation, LookDirection, LookPolarity, RelationLabels, RelationPolarity,
    RelationTarget,
};
use crate::fsm::{AttrValueType, Capture, Match, PositionAssertion};
use crate::token_match::{AttributeKey, TokenInfoTrait};
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
struct OraclePath {
    position: usize,
    slots: BTreeMap<CaptureId, (usize, usize)>,
    decisions: Vec<OracleDecision>,
    any_order_branches: Vec<Box<[usize]>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OracleDecision {
    Alternative(usize),
    AnyOrderOperand(usize),
    Repetition(usize),
}

#[derive(Clone, Debug)]
struct OracleCapture {
    id: CaptureId,
    name: compact_str::CompactString,
    path: compact_str::CompactString,
}

fn execution_oracle_matches<T: TokenInfoTrait>(
    definition: &ExecutionDefinition,
    tokens: &[T],
) -> Vec<Match> {
    let mut descriptors = Vec::new();
    collect_execution_captures(&definition.expr, &mut descriptors);
    descriptors.sort_by_key(|capture| capture.id);

    let mut matches = Vec::new();
    for start in 0..tokens.len() {
        let initial = OraclePath {
            position: start,
            slots: BTreeMap::new(),
            decisions: Vec::new(),
            any_order_branches: Vec::new(),
        };
        let mut paths = evaluate_execution(&definition.expr, tokens, initial);
        paths.retain(|path| path.position > start);
        paths.retain(|path| execution_relations_hold(&definition.relations, path, tokens));
        paths.sort_by(compare_oracle_paths);
        paths.dedup();
        let mut seen_branches = BTreeSet::new();
        for selected in paths
            .into_iter()
            .filter(|path| seen_branches.insert(path.any_order_branches.clone()))
        {
            let mut captures = descriptors
                .iter()
                .filter_map(|descriptor| {
                    selected
                        .slots
                        .get(&descriptor.id)
                        .map(|&(capture_start, capture_end)| Capture {
                            name: descriptor.name.clone(),
                            path: descriptor.path.clone(),
                            start: capture_start,
                            end: capture_end,
                        })
                })
                .collect::<Vec<_>>();
            captures.sort_by_key(|capture| (capture.start, capture.end));
            matches.push(Match {
                pattern: definition.name.clone(),
                start,
                end: selected.position,
                captures: captures.into_boxed_slice(),
                graph_bindings: Box::default(),
                lexical: None,
            });
        }
    }
    matches
}

fn execution_relations_hold<T: TokenInfoTrait>(
    relations: &[ExecutionRelation],
    path: &OraclePath,
    tokens: &[T],
) -> bool {
    relations.iter().all(|relation| {
        let Some(&(head_start, _)) = path.slots.get(&relation.head) else {
            return false;
        };
        match &relation.target {
            RelationTarget::Captured { capture, .. } => {
                let Some(&(dependent_start, _)) = path.slots.get(capture) else {
                    return false;
                };
                execution_relation_matches_token(relation, tokens, dependent_start, head_start)
            }
            RelationTarget::Anonymous { polarity, .. } => {
                if !execution_dependency_observation_is_usable(tokens) {
                    return false;
                }
                let exists = (0..tokens.len()).any(|dependent_start| {
                    execution_relation_matches_token(relation, tokens, dependent_start, head_start)
                });
                match polarity {
                    RelationPolarity::Positive => exists,
                    RelationPolarity::Negative => !exists,
                }
            }
            RelationTarget::Predicate {
                polarity,
                predicates,
                ..
            } => {
                if !execution_dependency_observation_is_usable(tokens) {
                    return false;
                }
                let required_attributes = predicates
                    .iter()
                    .flat_map(|predicate| predicate.iter())
                    .map(|atom| &atom.attr)
                    .collect::<BTreeSet<_>>();
                for dependent_start in 0..tokens.len() {
                    if !execution_relation_matches_token(
                        relation,
                        tokens,
                        dependent_start,
                        head_start,
                    ) {
                        continue;
                    }
                    if *polarity == RelationPolarity::Negative
                        && required_attributes
                            .iter()
                            .any(|attribute| tokens[dependent_start].value_key(attribute).is_none())
                    {
                        return false;
                    }
                    let witness = predicates
                        .iter()
                        .any(|predicate| predicate_matches(&tokens[dependent_start], predicate));
                    if witness {
                        return *polarity == RelationPolarity::Positive;
                    }
                }
                *polarity == RelationPolarity::Negative
            }
        }
    })
}

fn execution_dependency_observation_is_usable<T: TokenInfoTrait>(tokens: &[T]) -> bool {
    use crate::token_match::AttributeValueRef;

    let head_key = AttributeKey::new("dep.head").expect("canonical oracle key");
    let relation_key = AttributeKey::new("dep.rel").expect("canonical oracle key");
    tokens.iter().all(|token| {
        let Some(AttributeValueRef::Scalar(label)) = token.value_key(&relation_key) else {
            return false;
        };
        if label.is_empty() || label == "_" {
            return false;
        }
        let Some(AttributeValueRef::Scalar(head)) = token.value_key(&head_key) else {
            return false;
        };
        if head == "_" {
            return false;
        }
        head.parse::<usize>().is_ok_and(|head| head <= tokens.len())
    })
}

fn execution_relation_matches_token<T: TokenInfoTrait>(
    relation: &ExecutionRelation,
    tokens: &[T],
    dependent_start: usize,
    head_start: usize,
) -> bool {
    use crate::token_match::AttributeValueRef;

    let relation_key = AttributeKey::new("dep.rel").expect("canonical oracle key");
    let head_key = AttributeKey::new("dep.head").expect("canonical oracle key");
    let Some(AttributeValueRef::Scalar(label)) = tokens[dependent_start].value_key(&relation_key)
    else {
        return false;
    };
    let label_matches = match &relation.labels {
        RelationLabels::Any => true,
        RelationLabels::Listed(labels) => labels.iter().any(|candidate| candidate.matches(label)),
    };
    if !label_matches {
        return false;
    }
    let Some(AttributeValueRef::Scalar(head)) = tokens[dependent_start].value_key(&head_key) else {
        return false;
    };
    head.parse::<usize>()
        .is_ok_and(|head| head == head_start + 1)
}

fn evaluate_execution<T: TokenInfoTrait>(
    expr: &ExecutionExpr,
    tokens: &[T],
    path: OraclePath,
) -> Vec<OraclePath> {
    match expr {
        ExecutionExpr::Token(predicate) => tokens
            .get(path.position)
            .filter(|token| predicate_matches(*token, predicate))
            .map(|_| OraclePath {
                position: path.position + 1,
                ..path
            })
            .into_iter()
            .collect(),
        ExecutionExpr::Assertion(assertion) => {
            execution_assertion_matches(assertion, tokens, path.position)
                .then_some(path)
                .into_iter()
                .collect()
        }
        ExecutionExpr::Sequence(expressions) => {
            let mut paths = vec![path];
            for expression in expressions {
                paths = paths
                    .into_iter()
                    .flat_map(|path| evaluate_execution(expression, tokens, path))
                    .collect();
                if paths.is_empty() {
                    break;
                }
            }
            paths
        }
        ExecutionExpr::Alternation(expressions) => expressions
            .iter()
            .enumerate()
            .flat_map(|(index, expression)| {
                let mut branch = path.clone();
                branch.decisions.push(OracleDecision::Alternative(index));
                evaluate_execution(expression, tokens, branch)
            })
            .collect(),
        // Written from the definition rather than from `program.rs`: an end
        // position is in `A & B` at this start exactly when it is in the set of
        // ends of *every* operand. Computed as a fold over those sets, and the
        // surviving paths are then the first operand's that land in it — which
        // is well defined only because a capture inside `&` is refused, so
        // every operand hands back the same slots it was given.
        ExecutionExpr::Intersection { operands, .. } => {
            let agreed = operands
                .iter()
                .map(|operand| {
                    evaluate_execution(operand, tokens, path.clone())
                        .into_iter()
                        .map(|reached| reached.position)
                        .collect::<BTreeSet<_>>()
                })
                .reduce(|held, next| held.intersection(&next).copied().collect())
                .unwrap_or_default();
            operands
                .first()
                .map(|first| evaluate_execution(first, tokens, path))
                .unwrap_or_default()
                .into_iter()
                .filter(|reached| agreed.contains(&reached.position))
                .collect()
        }
        ExecutionExpr::AnyOrder { operands, .. } => {
            let mut found = Vec::new();
            for first in 0..operands.len() {
                let mut branch = path.clone();
                branch
                    .decisions
                    .push(OracleDecision::AnyOrderOperand(first));
                let remaining = (0..operands.len())
                    .filter(|index| *index != first)
                    .collect::<Vec<_>>();
                for next in evaluate_execution(&operands[first], tokens, branch) {
                    let order = vec![first];
                    found.extend(evaluate_execution_any_order_tail(
                        operands, &remaining, &order, tokens, next,
                    ));
                }
            }
            found
        }
        ExecutionExpr::Repeat { expr, min, max } => {
            evaluate_execution_repeat(expr, *min, *max, tokens, path)
        }
        ExecutionExpr::Capture { id, expr, .. } => {
            let start = path.position;
            evaluate_execution(expr, tokens, path)
                .into_iter()
                .map(|mut path| {
                    path.slots.insert(*id, (start, path.position));
                    path
                })
                .collect()
        }
        ExecutionExpr::SameAs {
            capture,
            projection,
            ..
        } => {
            let Some(&(capture_start, capture_end)) = path.slots.get(capture) else {
                return Vec::new();
            };
            let Some(width) = capture_end.checked_sub(capture_start) else {
                return Vec::new();
            };
            let Some(end) = path.position.checked_add(width) else {
                return Vec::new();
            };
            if end <= tokens.len()
                && execution_spans_equal(
                    tokens,
                    capture_start,
                    path.position,
                    width,
                    projection.clone(),
                )
            {
                vec![OraclePath {
                    position: end,
                    ..path
                }]
            } else {
                Vec::new()
            }
        }
    }
}

fn evaluate_execution_any_order_tail<T: TokenInfoTrait>(
    operands: &[ExecutionExpr],
    remaining: &[usize],
    order: &[usize],
    tokens: &[T],
    path: OraclePath,
) -> Vec<OraclePath> {
    if remaining.is_empty() {
        let mut path = path;
        path.any_order_branches.push(order.into());
        return vec![path];
    }
    let mut found = Vec::new();
    for (choice, operand_index) in remaining.iter().copied().enumerate() {
        let mut next_remaining = remaining.to_vec();
        next_remaining.remove(choice);
        let mut next_order = order.to_vec();
        next_order.push(operand_index);
        for position in (path.position..tokens.len()).rev() {
            let mut placed = path.clone();
            placed.position = position;
            placed
                .decisions
                .push(OracleDecision::AnyOrderOperand(operand_index));
            for next in evaluate_execution(&operands[operand_index], tokens, placed) {
                found.extend(evaluate_execution_any_order_tail(
                    operands,
                    &next_remaining,
                    &next_order,
                    tokens,
                    next,
                ));
            }
        }
    }
    found
}

fn evaluate_execution_repeat<T: TokenInfoTrait>(
    expr: &ExecutionExpr,
    min: usize,
    max: Option<usize>,
    tokens: &[T],
    initial: OraclePath,
) -> Vec<OraclePath> {
    let decision_index = initial.decisions.len();
    let ceiling = max.unwrap_or_else(|| tokens.len().saturating_add(1));
    let mut levels = vec![vec![initial]];
    for _ in 0..ceiling {
        let next = levels
            .last()
            .expect("the zero-repetition level exists")
            .iter()
            .cloned()
            .flat_map(|path| {
                let before = path.position;
                evaluate_execution(expr, tokens, path)
                    .into_iter()
                    .filter(move |next| next.position > before)
            })
            .collect::<Vec<_>>();
        if next.is_empty() {
            break;
        }
        levels.push(next);
    }

    levels
        .into_iter()
        .enumerate()
        .rev()
        .filter(|(count, _)| *count >= min)
        .flat_map(|(count, paths)| {
            paths.into_iter().map(move |mut path| {
                path.decisions
                    .insert(decision_index, OracleDecision::Repetition(count));
                path
            })
        })
        .collect()
}

fn compare_oracle_paths(left: &OraclePath, right: &OraclePath) -> Ordering {
    // Public selection prefers the longest accepted span. Ties use the
    // authored decisions below instead of depending on iterator order.
    right
        .position
        .cmp(&left.position)
        .then_with(|| compare_oracle_decisions(&left.decisions, &right.decisions))
        .then_with(|| left.slots.cmp(&right.slots))
}

fn compare_oracle_decisions(left: &[OracleDecision], right: &[OracleDecision]) -> Ordering {
    for (left, right) in left.iter().zip(right) {
        let ordering = match (left, right) {
            (OracleDecision::Alternative(left), OracleDecision::Alternative(right)) => {
                left.cmp(right)
            }
            (OracleDecision::Repetition(left), OracleDecision::Repetition(right)) => {
                right.cmp(left)
            }
            (OracleDecision::AnyOrderOperand(left), OracleDecision::AnyOrderOperand(right)) => {
                left.cmp(right)
            }
            (OracleDecision::Alternative(_), _) => Ordering::Less,
            (_, OracleDecision::Alternative(_)) => Ordering::Greater,
            (OracleDecision::AnyOrderOperand(_), OracleDecision::Repetition(_)) => Ordering::Less,
            (OracleDecision::Repetition(_), OracleDecision::AnyOrderOperand(_)) => {
                Ordering::Greater
            }
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

fn execution_assertion_matches<T: TokenInfoTrait>(
    assertion: &PositionAssertion,
    tokens: &[T],
    position: usize,
) -> bool {
    match assertion {
        PositionAssertion::SentenceStart => position == 0,
        PositionAssertion::SentenceEnd => position == tokens.len(),
        PositionAssertion::PreviousToken(predicate) => position
            .checked_sub(1)
            .and_then(|index| tokens.get(index))
            .is_some_and(|token| predicate_matches(token, predicate)),
        PositionAssertion::Lookaround(_) => {
            panic!("execution oracle input cannot resolve an opaque lookaround occurrence")
        }
    }
}

fn execution_spans_equal<T: TokenInfoTrait>(
    tokens: &[T],
    left: usize,
    right: usize,
    width: usize,
    projection: CaptureProjection,
) -> bool {
    let attribute = projection.attribute();
    (0..width).all(|offset| {
        match (
            tokens[left + offset].value_key(attribute),
            tokens[right + offset].value_key(attribute),
        ) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    })
}

fn collect_execution_captures(expr: &ExecutionExpr, captures: &mut Vec<OracleCapture>) {
    match expr {
        ExecutionExpr::Capture {
            id,
            name,
            path,
            expr,
            ..
        } => {
            captures.push(OracleCapture {
                id: *id,
                name: name.clone(),
                path: path.clone(),
            });
            collect_execution_captures(expr, captures);
        }
        ExecutionExpr::Sequence(children) | ExecutionExpr::Alternation(children) => {
            for child in children {
                collect_execution_captures(child, captures);
            }
        }
        ExecutionExpr::Intersection { operands, .. } => {
            for operand in operands {
                collect_execution_captures(operand, captures);
            }
        }
        ExecutionExpr::AnyOrder { operands, .. } => {
            for operand in operands {
                collect_execution_captures(operand, captures);
            }
        }
        ExecutionExpr::Repeat { expr, .. } => collect_execution_captures(expr, captures),
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => {}
    }
}

pub(crate) fn oracle_matches<T: TokenInfoTrait>(
    program: &BoundProgram,
    tokens: &[T],
) -> Vec<Match> {
    let mut matches = Vec::new();

    for definition in &program.definitions {
        if !definition.is_ordinary_pattern() {
            continue;
        }
        let expr = definition
            .expr()
            .expect("ordinary patterns have expressions");
        for start in 0..tokens.len() {
            for end in expression_ends(program, expr, tokens, start) {
                if end <= start || end > tokens.len() {
                    continue;
                }
                matches.push(Match {
                    pattern: definition.name.clone(),
                    start,
                    end,
                    captures: Box::default(),
                    graph_bindings: Box::default(),
                    lexical: None,
                });
            }
        }
    }

    let mut by_pattern = BTreeMap::<_, Vec<_>>::new();
    for matched in matches {
        by_pattern
            .entry(matched.pattern.clone())
            .or_default()
            .push(matched);
    }
    let mut matches = Vec::new();
    for candidates in by_pattern.into_values() {
        let mut longest_by_start = BTreeMap::<usize, Match>::new();
        for candidate in candidates {
            longest_by_start
                .entry(candidate.start)
                .and_modify(|current| {
                    if candidate.end > current.end {
                        *current = candidate.clone();
                    }
                })
                .or_insert(candidate);
        }
        let mut longest_earlier_end: Option<usize> = None;
        for candidate in longest_by_start.into_values() {
            if longest_earlier_end.is_none_or(|end| end <= candidate.end) {
                longest_earlier_end =
                    Some(longest_earlier_end.map_or(candidate.end, |end| end.max(candidate.end)));
                matches.push(candidate);
            }
        }
    }

    matches.sort_by(|left, right| {
        (left.start, left.end, left.pattern.as_str()).cmp(&(
            right.start,
            right.end,
            right.pattern.as_str(),
        ))
    });
    matches.dedup();
    matches
}

pub(crate) fn expression_ends<T: TokenInfoTrait>(
    program: &BoundProgram,
    expr: &BoundExpr,
    tokens: &[T],
    start: usize,
) -> BTreeSet<usize> {
    match expr {
        BoundExpr::Token(predicate) => {
            let mut ends = BTreeSet::new();
            if let Some(token) = tokens.get(start)
                && predicate_matches(token, predicate)
            {
                ends.insert(start + 1);
            }
            ends
        }
        BoundExpr::Assertion(assertion) => {
            let matched = match assertion {
                crate::fsm::PositionAssertion::SentenceStart => start == 0,
                crate::fsm::PositionAssertion::SentenceEnd => start == tokens.len(),
                crate::fsm::PositionAssertion::PreviousToken(predicate) => start
                    .checked_sub(1)
                    .and_then(|index| tokens.get(index))
                    .is_some_and(|token| predicate_matches(token, predicate)),
                crate::fsm::PositionAssertion::Lookaround(_) => false,
            };
            if matched {
                BTreeSet::from([start])
            } else {
                BTreeSet::new()
            }
        }
        BoundExpr::Lookaround(lookaround) => {
            let matched = assertion_holds_oracle(
                program,
                &lookaround.body,
                lookaround.direction,
                tokens,
                start,
            );
            let matched = match lookaround.polarity {
                LookPolarity::Positive => matched,
                LookPolarity::Negative => !matched,
            };
            if matched {
                BTreeSet::from([start])
            } else {
                BTreeSet::new()
            }
        }
        BoundExpr::Sequence(expressions) => {
            let mut positions = BTreeSet::from([start]);
            for expression in expressions {
                let mut next = BTreeSet::new();
                for position in positions {
                    next.extend(expression_ends(program, expression, tokens, position));
                }
                positions = next;
                if positions.is_empty() {
                    break;
                }
            }
            positions
        }
        BoundExpr::Alternation(expressions) => expressions
            .iter()
            .flat_map(|expression| expression_ends(program, expression, tokens, start))
            .collect(),
        // The whole operator, at this level of description: `|` is the union of
        // the operands' end sets and `&` is their intersection. That this reads
        // as one word is the point of the set-of-ends formulation — it is what
        // makes this an independent statement of the semantics rather than a
        // transcription of the candidate-filtering evaluator.
        BoundExpr::Intersection { operands, .. } => operands
            .iter()
            .map(|operand| expression_ends(program, operand, tokens, start))
            .reduce(|held, next| held.intersection(&next).copied().collect())
            .unwrap_or_default(),
        BoundExpr::AnyOrder { operands, .. } => {
            let mut ends = BTreeSet::new();
            for first in 0..operands.len() {
                let remaining = (0..operands.len())
                    .filter(|index| *index != first)
                    .collect::<Vec<_>>();
                for position in expression_ends(program, &operands[first], tokens, start) {
                    ends.extend(any_order_tail_ends(
                        program, operands, &remaining, tokens, position,
                    ));
                }
            }
            ends
        }
        BoundExpr::Repeat { expr, min, max } => {
            repeat_ends(program, expr, *min, *max, tokens, start)
        }
        BoundExpr::Capture { expr, .. } => expression_ends(program, expr, tokens, start),
        BoundExpr::SameAs { .. } => BTreeSet::new(),
        BoundExpr::Definition { id, .. } => {
            program
                .definitions
                .get(id.index())
                .map_or_else(BTreeSet::new, |definition| {
                    expression_ends(
                        program,
                        definition.expr().expect("lexicon references are refused"),
                        tokens,
                        start,
                    )
                })
        }
    }
}

fn any_order_tail_ends<T: TokenInfoTrait>(
    program: &BoundProgram,
    operands: &[BoundExpr],
    remaining: &[usize],
    tokens: &[T],
    position: usize,
) -> BTreeSet<usize> {
    if remaining.is_empty() {
        return BTreeSet::from([position]);
    }
    let mut ends = BTreeSet::new();
    for (choice, operand_index) in remaining.iter().copied().enumerate() {
        let mut next_remaining = remaining.to_vec();
        next_remaining.remove(choice);
        for start in (position..tokens.len()).rev() {
            for next in expression_ends(program, &operands[operand_index], tokens, start) {
                ends.extend(any_order_tail_ends(
                    program,
                    operands,
                    &next_remaining,
                    tokens,
                    next,
                ));
            }
        }
    }
    ends
}

pub(crate) fn assertion_holds_oracle<T: TokenInfoTrait>(
    program: &BoundProgram,
    expr: &BoundExpr,
    direction: LookDirection,
    tokens: &[T],
    boundary: usize,
) -> bool {
    if boundary > tokens.len() {
        return false;
    }
    match direction {
        LookDirection::FollowedBy => !expression_ends(program, expr, tokens, boundary).is_empty(),
        LookDirection::PrecededBy => (0..=boundary)
            .any(|start| expression_ends(program, expr, tokens, start).contains(&boundary)),
    }
}

fn predicate_matches<T: TokenInfoTrait>(
    token: &T,
    predicate: &[crate::fsm::SymbolicAttrValue],
) -> bool {
    use crate::token_match::AttributeValueRef;
    predicate.iter().all(|attribute| {
        let Some(actual) = token.value_key(&attribute.attr) else {
            return false;
        };
        let held = match (&attribute.value, actual) {
            (AttrValueType::Single(expected), AttributeValueRef::Scalar(actual)) => {
                actual == expected
            }
            (AttrValueType::OneOf(expected), AttributeValueRef::Scalar(actual)) => {
                expected.iter().any(|candidate| candidate == actual)
            }
            (AttrValueType::NotIn(excluded), AttributeValueRef::Scalar(actual)) => {
                excluded.iter().all(|candidate| candidate != actual)
            }
            (AttrValueType::Contains(needle), AttributeValueRef::Scalar(actual)) => {
                actual.contains(needle.as_str())
            }
            (AttrValueType::NotContains(needle), AttributeValueRef::Scalar(actual)) => {
                !actual.contains(needle.as_str())
            }
            (AttrValueType::Regex(regex), AttributeValueRef::Scalar(actual)) => {
                regex.is_match(actual)
            }
            (AttrValueType::NotRegex(regex), AttributeValueRef::Scalar(actual)) => {
                !regex.is_match(actual)
            }
            (AttrValueType::SetHas(expected), AttributeValueRef::StringSet(actual)) => {
                actual.iter().any(|value| value == expected)
            }
            (AttrValueType::SetHasAny(expected), AttributeValueRef::StringSet(actual)) => {
                actual.iter().any(|value| expected.contains(value))
            }
            (AttrValueType::SetNotHas(excluded), AttributeValueRef::StringSet(actual)) => {
                actual.iter().all(|value| value != excluded)
            }
            (AttrValueType::SetNotHasAny(excluded), AttributeValueRef::StringSet(actual)) => {
                actual.iter().all(|value| !excluded.contains(value))
            }
            // `~` and `!~` are the two kind-polymorphic operators: existential
            // over a string set, direct over a scalar.
            (AttrValueType::Contains(needle), AttributeValueRef::StringSet(actual)) => {
                actual.iter().any(|value| value.contains(needle.as_str()))
            }
            (AttrValueType::NotContains(needle), AttributeValueRef::StringSet(actual)) => {
                actual.iter().all(|value| !value.contains(needle.as_str()))
            }
            (AttrValueType::Regex(regex), AttributeValueRef::StringSet(actual)) => {
                actual.iter().any(|value| regex.is_match(value))
            }
            (AttrValueType::NotRegex(regex), AttributeValueRef::StringSet(actual)) => {
                actual.iter().all(|value| !regex.is_match(value))
            }
            _ => false,
        };
        held != attribute.complement
    })
}

fn repeat_ends<T: TokenInfoTrait>(
    program: &BoundProgram,
    expr: &BoundExpr,
    min: usize,
    max: Option<usize>,
    tokens: &[T],
    start: usize,
) -> BTreeSet<usize> {
    let mut accepted = BTreeSet::new();
    let mut frontier = BTreeSet::from([start]);
    if min == 0 {
        accepted.insert(start);
    }

    let maximum = max.unwrap_or_else(|| tokens.len().saturating_sub(start).saturating_add(1));
    for count in 1..=maximum {
        let mut next = BTreeSet::new();
        for position in frontier {
            for end in expression_ends(program, expr, tokens, position) {
                if end != position {
                    next.insert(end);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        if count >= min {
            accepted.extend(next.iter().copied());
        }
        frontier = next;
    }
    accepted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsm::compiler::model::{
        BoundDefinition, BoundDefinitionBody, BoundExpr, BoundProgram, DefinitionId,
        DefinitionVisibility, SourceId, SourceRegistry, SourceSpan,
    };
    use crate::fsm::compiler::nfa::lower_to_nfa;
    use crate::fsm::{AttrValue, AttrValueType, Match};
    use crate::fsm::{CompileLimits, PatternSource, compile_catalog};
    use crate::token_match::{SequenceToken, TokenAttributeIndex, TokenInfo};
    use compact_str::CompactString;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn token_expr(value: &str) -> BoundExpr {
        BoundExpr::Token(
            vec![AttrValue {
                attr: TokenAttributeIndex::Orth.into(),
                value: AttrValueType::Single(CompactString::from(value)),
                complement: false,
            }]
            .into_boxed_slice(),
        )
    }

    fn token(value: &str) -> TokenInfo {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, CompactString::from(value));
        token
    }

    fn orth_pos_token(orth: &str, pos1: &str) -> TokenInfo {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, CompactString::from(orth));
        token.set(TokenAttributeIndex::Pos1, CompactString::from(pos1));
        token
    }

    fn execution_definition(source: &str) -> super::super::model::ExecutionDefinition {
        let limits = CompileLimits::default();
        let parsed = super::super::lower::lower_document(
            &PatternSource::named("execution-oracle.lx", source),
            &limits,
        )
        .expect("execution-oracle source must lower");
        let bound =
            super::super::bind::bind(parsed, &limits).expect("execution-oracle source must bind");
        // No anchors: the oracle takes a bare source, with no catalog policy to
        // declare one in.
        let assertions =
            super::super::assertion::compile_assertions(&bound, &limits, &BTreeMap::new())
                .expect("execution-oracle assertions must compile");
        let mut definitions = super::super::materialize::materialize_program(
            &bound,
            &limits,
            &assertions.occurrences,
        )
        .expect("execution-oracle program must materialize");
        super::super::analyze::analyze_program(&mut definitions, &bound.sources)
            .expect("execution-oracle program must analyze");
        definitions
            .into_vec()
            .into_iter()
            .next()
            .expect("fixture has one pattern")
    }

    fn bound_program(source: &str) -> super::super::model::BoundProgram {
        let limits = CompileLimits::default();
        let parsed = super::super::lower::lower_document(
            &PatternSource::named("lookaround-oracle.lx", source),
            &limits,
        )
        .expect("lookaround-oracle source must lower");
        super::super::bind::bind(parsed, &limits).expect("lookaround-oracle source must bind")
    }

    fn assert_bound_oracle(source: &str, tokens: &[TokenInfo]) {
        let program = bound_program(source);
        assert_eq!(
            nfa_matches(&program, tokens),
            oracle_matches(&program, tokens),
            "source: {source}",
        );
    }

    #[test]
    fn every_lookaround_direction_and_polarity_matches_the_bound_oracle() {
        // The four lookaround combinations: two directions (preceded/followed)
        // x two polarities (positive/negative), each guarding a consuming
        // pattern. Proves all four actually execute against the oracle, so a
        // generator regression that stops emitting one fails here.
        let tokens = [token("a"), token("x"), token("z")];
        for source in [
            "pattern p := preceded_by([text=a]) [text=x].",
            "pattern p := not_preceded_by([text=z]) [text=x].",
            "pattern p := [text=x] followed_by([text=z]).",
            "pattern p := [text=x] not_followed_by([text=a]).",
        ] {
            assert_bound_oracle(source, &tokens);
        }
    }

    #[hegel::test(test_cases = 300, derandomize = true)]
    fn generated_lookaround_matches_bound_oracle(tc: hegel::TestCase) {
        use hegel::generators;

        // Consuming, non-nullable inner expressions E (no captures / same_as
        // inside lookaround per the lingrep patterns contract), so every draw compiles.
        let inner = tc.draw(generators::sampled_from(vec![
            "[text=a]",
            "[text=a] [text=b]",
            "[text=a]+",
            "[text in {a, b}]",
        ]));
        let text = tc.draw(generators::integers::<u8>().min_value(0).max_value(3));
        let source = match text {
            0 => format!("pattern p := preceded_by({inner}) [text=x]."),
            1 => format!("pattern p := not_preceded_by({inner}) [text=x]."),
            2 => format!("pattern p := [text=x] followed_by({inner})."),
            _ => format!("pattern p := [text=x] not_followed_by({inner})."),
        };

        let symbols = tc.draw(
            generators::vecs(generators::integers::<u8>().min_value(0).max_value(2)).max_size(14),
        );
        let tokens = symbols
            .into_iter()
            .map(|symbol| match symbol {
                0 => token("a"),
                1 => token("b"),
                _ => token("x"),
            })
            .collect::<Vec<_>>();

        assert_bound_oracle(&source, &tokens);
    }

    fn optional_token(value: Option<&str>) -> TokenInfo {
        value.map_or_else(TokenInfo::default, token)
    }

    fn assert_execution_oracle<T: TokenInfoTrait>(source: &str, tokens: &[T]) {
        let definition = execution_definition(source);
        let expected = execution_oracle_matches(&definition, tokens);
        let actual = compile_catalog(
            PatternSource::named("execution-oracle.lx", source),
            CompileLimits::default(),
        )
        .expect("fixture compiles")
        .matcher
        .find_matches(tokens);
        assert_eq!(actual, expected, "source: {source}");
    }

    #[test]
    fn anonymous_relations_match_the_independent_execution_oracle() {
        for source in [
            "pattern p := (?<h> []) where $h -[obj]-> _.",
            "pattern p := (?<h> []) where not $h -[obj]-> _.",
            "pattern p := (?<h> []) where $h -[obj]-> [upos=NOUN].",
            "pattern p := (?<h> []) where not $h -[obj]-> [upos=NOUN].",
        ] {
            let definition = execution_definition(source);
            let compiled = compile_catalog(
                PatternSource::named("execution-oracle.lx", source),
                CompileLimits::default(),
            )
            .expect("fixture compiles")
            .matcher;
            let bound = compiled
                .bind(&crate::corpus::conllu::token_schema())
                .expect("CoNLL-U schema binds");

            for observations in [
                [(Some("NOUN"), "2", "obj"), (Some("VERB"), "0", "root")],
                [(Some("NOUN"), "2", "nsubj"), (Some("VERB"), "0", "root")],
                [(None, "2", "obj"), (Some("VERB"), "0", "root")],
            ] {
                let values = observations
                    .iter()
                    .flat_map(|(upos, head, relation)| {
                        bound
                            .projection()
                            .keys()
                            .iter()
                            .map(move |key| match key.as_str() {
                                "upos" => upos.map(Into::into),
                                "dep.head" => Some((*head).into()),
                                "dep.rel" => Some((*relation).into()),
                                other => panic!("unexpected oracle projection key {other}"),
                            })
                    })
                    .collect();
                let sentence = crate::token_match::BoundTokenSentence::from_projected_values(
                    bound.projection(),
                    observations.len(),
                    values,
                )
                .expect("oracle sentence projects");
                let tokens = (0..sentence.len())
                    .map(|index| SequenceToken::new(&sentence, index).expect("index is in range"))
                    .collect::<Vec<_>>();
                assert!(execution_dependency_observation_is_usable(&tokens));
                assert_eq!(
                    bound.find_matches(&sentence).expect("schema matches"),
                    execution_oracle_matches(&definition, &tokens),
                    "source: {source}; observations: {observations:?}",
                );
            }
        }
    }

    #[test]
    fn execution_oracle_reports_source_ordered_capture() {
        let source = "pattern p := ((?<left> a) | (?<right> a)).";
        let definition = execution_definition(source);
        let tokens = [token("a")];
        let actual = compile_catalog(
            PatternSource::named("execution-oracle.lx", source),
            CompileLimits::default(),
        )
        .expect("fixture compiles")
        .matcher
        .find_matches(&tokens);

        assert_eq!(execution_oracle_matches(&definition, &tokens), actual);
    }

    #[test]
    fn execution_oracle_exhausts_small_capture_and_same_as_inputs() {
        let sources = [
            "pattern p := ((?<left> a) | (?<right> a)).",
            "pattern p := (?<x> a{1,3}) a.",
            "pattern p := (?<outer> (?<inner> a) b?).",
            "pattern p := ((?<x> a) b?){1,2}.",
            "pattern p := (?<x> a{1,2}) same_as($x) same_as($x).",
        ];
        let cases = sources
            .iter()
            .map(|source| {
                let definition = execution_definition(source);
                let matcher = compile_catalog(
                    PatternSource::named("execution-oracle.lx", source),
                    CompileLimits::default(),
                )
                .expect("fixed matrix compiles")
                .matcher;
                (*source, definition, matcher)
            })
            .collect::<Vec<_>>();

        for len in 0..=6 {
            let case_count = 3_usize.pow(u32::try_from(len).expect("six fits u32"));
            for encoded in 0..case_count {
                let mut value = encoded;
                let tokens = (0..len)
                    .map(|_| {
                        let symbol = value % 3;
                        value /= 3;
                        optional_token(match symbol {
                            0 => Some("a"),
                            1 => Some("b"),
                            _ => None,
                        })
                    })
                    .collect::<Vec<_>>();
                for (source, definition, matcher) in &cases {
                    assert_eq!(
                        matcher.find_matches(&tokens),
                        execution_oracle_matches(definition, &tokens),
                        "source: {source}, encoded input: {encoded}, length: {len}"
                    );
                }
            }
        }
    }

    #[hegel::test(test_cases = 300, derandomize = true)]
    fn generated_bounded_execution_matches_independent_oracle(tc: hegel::TestCase) {
        use hegel::generators;

        let shape = tc.draw(generators::integers::<u8>().min_value(0).max_value(4));
        let left = tc.draw(generators::sampled_from(vec!["a", "b"]));
        let right = tc.draw(generators::sampled_from(vec!["a", "b"]));
        let min = tc.draw(generators::integers::<usize>().min_value(1).max_value(3));
        let extra = tc.draw(generators::integers::<usize>().min_value(0).max_value(2));
        let max = min.saturating_add(extra);
        let width = tc.draw(generators::integers::<usize>().min_value(1).max_value(8));
        tc.target(width as f64);
        let source = match shape {
            0 => format!("pattern p := (?<x> {left}{{{min},{max}}}) {right}."),
            1 => format!("pattern p := ((?<left> {left}) | (?<right> {right})) {left}?."),
            2 => format!("pattern p := ((?<x> {left}) {right}?){{{min},{max}}}."),
            3 => format!("pattern p := (?<outer> (?<inner> {left}{{1,{max}}}) {right}?)."),
            _ => format!("pattern p := (?<x> {left}{{{width}}}) same_as($x)."),
        };
        let symbols = tc.draw(
            generators::vecs(generators::integers::<u8>().min_value(0).max_value(2)).max_size(18),
        );
        let tokens = symbols
            .into_iter()
            .map(|symbol| match symbol {
                0 => optional_token(Some("a")),
                1 => optional_token(Some("b")),
                _ => optional_token(None),
            })
            .collect::<Vec<_>>();

        assert_execution_oracle(&source, &tokens);
    }

    #[hegel::test(test_cases = 300, derandomize = true)]
    fn generated_predicate_and_boundary_matches_independent_oracle(tc: hegel::TestCase) {
        use hegel::generators;

        // Predicate atom families the execution oracle scores (single, in-set,
        // not-in-set, regex verified against the oracle at oracle.rs:885-887;
        // &-conjunction via predicate_matches slice iteration at :477-486). All
        // consuming, non-nullable, valid by construction.
        let atom = tc.draw(generators::integers::<u8>().min_value(0).max_value(4));
        let predicate = match atom {
            0 => "[text=a]".to_string(),
            1 => "[text in {a, b}]".to_string(),
            2 => "[text not in {z}]".to_string(),
            3 => "[text~/^a+$/]".to_string(),
            _ => "[text in {a, b} & unidic.pos1=X]".to_string(),
        };
        // Boundary framing: none, start-anchored, end-anchored, or both.
        let framing = tc.draw(generators::integers::<u8>().min_value(0).max_value(3));
        let source = match framing {
            0 => format!("pattern p := {predicate}."),
            1 => format!("pattern p := ^ {predicate}."),
            2 => format!("pattern p := {predicate} $."),
            _ => format!("pattern p := ^ {predicate} $."),
        };

        // Tokens carry both orth and pos1 so the &-conjunction is meaningfully
        // exercised (some (orth, pos1) pairs satisfy it, some do not).
        let symbols = tc.draw(
            generators::vecs(generators::integers::<u8>().min_value(0).max_value(5)).max_size(12),
        );
        let tokens = symbols
            .into_iter()
            .map(|symbol| match symbol {
                0 => orth_pos_token("a", "X"),
                1 => orth_pos_token("a", "Y"),
                2 => orth_pos_token("b", "X"),
                3 => orth_pos_token("b", "Y"),
                4 => orth_pos_token("z", "X"),
                _ => orth_pos_token("z", "Y"),
            })
            .collect::<Vec<_>>();

        assert_execution_oracle(&source, &tokens);
    }

    fn program(expr: BoundExpr) -> BoundProgram {
        BoundProgram {
            definitions: vec![BoundDefinition {
                id: DefinitionId::new(0),
                source: SourceId::new(0),
                name: CompactString::from("p"),
                body: BoundDefinitionBody::Pattern {
                    visibility: DefinitionVisibility::Pattern,
                    expr,
                    relations: Box::default(),
                    enhanced_relations: Box::default(),
                },
                metadata: Arc::new(HashMap::new()),
                key: None,
                span: fixture_span(),
            }],
            sources: SourceRegistry::single("fixture.lx", "")
                .expect("empty fixture source is valid"),
            sets: Vec::new(),
            set_uses: BTreeMap::new(),
            tests: Vec::new(),
            test_uses: BTreeMap::new(),
        }
    }

    fn fixture_span() -> SourceSpan {
        SourceSpan::new(SourceId::new(0), 0..0).expect("fixture span")
    }

    fn repeat_program(value: &str, min: usize, max: Option<usize>) -> BoundProgram {
        program(BoundExpr::Repeat {
            expr: Box::new(token_expr(value)),
            min,
            max,
        })
    }

    fn nfa_matches(program: &BoundProgram, tokens: &[TokenInfo]) -> Vec<Match> {
        let limits = CompileLimits::default();
        // General (non-specialized) lookaround must be compiled into an
        // assertion catalog and installed on the matcher before NFA
        // lowering; lower_to_nfa panics on a general Lookaround occurrence
        // it cannot resolve. Programs without lookaround compile an empty
        // catalog here, so this is a no-op for every prior nfa_matches use.
        // No anchors: these are bound programs built directly, without the
        // catalog policy an `anchor:` declaration is read into.
        let assertions =
            super::super::assertion::compile_assertions(program, &limits, &BTreeMap::new())
                .expect("bounded test program must compile assertions");
        let mut lowered = lower_to_nfa(program, &limits, &[], &assertions.occurrences)
            .expect("bounded test program must lower");
        lowered.matcher.install_assertions(assertions.catalog);
        lowered.matcher.find_matches(tokens)
    }

    fn spans(program: &BoundProgram, values: &[&str]) -> Vec<(usize, usize)> {
        let tokens = values.iter().map(|value| token(value)).collect::<Vec<_>>();
        oracle_matches(program, &tokens)
            .into_iter()
            .map(|matched| (matched.start, matched.end))
            .collect()
    }

    #[test]
    fn sequence_returns_exclusive_end_and_full_context() {
        let program = BoundProgram {
            definitions: vec![BoundDefinition {
                id: DefinitionId::new(0),
                source: SourceId::new(0),
                name: CompactString::from("pair"),
                body: BoundDefinitionBody::Pattern {
                    visibility: DefinitionVisibility::Pattern,
                    expr: BoundExpr::Sequence(
                        vec![token_expr("a"), token_expr("b")].into_boxed_slice(),
                    ),
                    relations: Box::default(),
                    enhanced_relations: Box::default(),
                },
                metadata: Arc::new(HashMap::new()),
                key: None,
                span: fixture_span(),
            }],
            sources: SourceRegistry::single("fixture.lx", "")
                .expect("empty fixture source is valid"),
            sets: Vec::new(),
            set_uses: BTreeMap::new(),
            tests: Vec::new(),
            test_uses: BTreeMap::new(),
        };
        let tokens = [token("a"), token("b"), token("x")];

        assert_eq!(
            oracle_matches(&program, &tokens),
            vec![Match {
                pattern: CompactString::from("pair"),
                start: 0,
                end: 2,
                captures: Box::default(),
                graph_bindings: Box::default(),
                lexical: None,
            }]
        );
    }

    #[test]
    fn alternation_unions_child_ends() {
        let program = program(BoundExpr::Alternation(
            vec![token_expr("a"), token_expr("b")].into_boxed_slice(),
        ));
        assert_eq!(spans(&program, &["a", "b", "x"]), [(0, 1), (1, 2)]);
    }

    #[test]
    fn exact_one_repeat_equals_the_child() {
        let plain = program(token_expr("a"));
        let repeated = program(BoundExpr::Repeat {
            expr: Box::new(token_expr("a")),
            min: 1,
            max: Some(1),
        });
        assert_eq!(
            spans(&plain, &["a", "x", "a"]),
            spans(&repeated, &["a", "x", "a"])
        );
    }

    #[test]
    fn optional_filters_zero_length_only_at_top_level() {
        let program = program(BoundExpr::Sequence(
            vec![
                BoundExpr::Repeat {
                    expr: Box::new(token_expr("a")),
                    min: 0,
                    max: Some(1),
                },
                token_expr("b"),
            ]
            .into_boxed_slice(),
        ));
        assert_eq!(spans(&program, &["b", "a", "b"]), [(0, 1), (1, 3), (2, 3)]);
    }

    #[test]
    fn one_or_more_stops_before_nonmatching_token() {
        let program = program(BoundExpr::Repeat {
            expr: Box::new(token_expr("a")),
            min: 1,
            max: None,
        });
        assert_eq!(spans(&program, &["a", "a", "x"]), [(0, 2), (1, 2)]);
        assert_eq!(
            expression_ends(
                &program,
                program.definitions[0].expr().expect("pattern"),
                &[token("a"), token("a"), token("x")],
                0
            ),
            BTreeSet::from([1, 2]),
            "expression semantics retain intermediate ends before the public longest-match filter"
        );
    }

    #[test]
    fn bounded_optional_repeat_keeps_the_longest_public_match_per_start() {
        let program = repeat_program("a", 0, Some(3));
        assert_eq!(spans(&program, &["a", "a"]), [(0, 2), (1, 2)]);
    }

    #[test]
    fn bounded_repeat_accepts_its_minimum_before_a_terminator() {
        let program = repeat_program("a", 1, Some(3));
        let tokens = [token("a"), token("b")];
        assert_eq!(
            nfa_matches(&program, &tokens),
            oracle_matches(&program, &tokens)
        );
    }

    #[test]
    fn grouped_repeat_repeats_the_expression_not_each_child() {
        let program = program(BoundExpr::Repeat {
            expr: Box::new(BoundExpr::Sequence(
                vec![token_expr("a"), token_expr("b")].into_boxed_slice(),
            )),
            min: 2,
            max: Some(2),
        });
        let tokens = [token("a"), token("b"), token("a"), token("b")];
        assert_eq!(
            nfa_matches(&program, &tokens),
            oracle_matches(&program, &tokens)
        );
    }

    #[test]
    fn definition_edges_resolve_by_id() {
        let mut program = program(token_expr("a"));
        program.definitions.push(BoundDefinition {
            id: DefinitionId::new(1),
            source: SourceId::new(0),
            name: CompactString::from("ref"),
            body: BoundDefinitionBody::Pattern {
                visibility: DefinitionVisibility::Pattern,
                expr: BoundExpr::Definition {
                    id: DefinitionId::new(0),
                    span: fixture_span(),
                },
                relations: Box::default(),
                enhanced_relations: Box::default(),
            },
            metadata: Arc::new(HashMap::new()),
            key: None,
            span: fixture_span(),
        });
        let matches = oracle_matches(&program, &[token("a")]);
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].pattern.as_str(), "p");
        assert_eq!(matches[1].pattern.as_str(), "ref");
    }

    #[test]
    fn predicate_variants_are_interpreted_independently() {
        let cases = [
            AttrValueType::OneOf(Arc::from(vec!["a".into(), "b".into()])),
            AttrValueType::NotIn(Arc::from(vec!["x".into()])),
            AttrValueType::Regex(regex::Regex::new("^[ab]$").expect("valid regex")),
        ];
        for value in cases {
            let program = program(BoundExpr::Token(
                vec![AttrValue {
                    attr: TokenAttributeIndex::Orth.into(),
                    value,
                    complement: false,
                }]
                .into_boxed_slice(),
            ));
            assert_eq!(spans(&program, &["a", "x"]), [(0, 1)]);
        }
    }

    #[derive(Clone, Debug)]
    enum GeneratedExpr {
        Lit(char),
        Seq(Box<GeneratedExpr>, Box<GeneratedExpr>),
        Alt(Box<GeneratedExpr>, Box<GeneratedExpr>),
        Optional(Box<GeneratedExpr>),
        Plus(Box<GeneratedExpr>),
        Group(Box<GeneratedExpr>),
    }

    impl GeneratedExpr {
        fn render(&self, parent_precedence: u8) -> String {
            let (precedence, text) = match self {
                Self::Lit(value) => (4, value.to_string()),
                Self::Group(expr) => (4, format!("({})", expr.render(0))),
                Self::Optional(expr) => (3, format!("{}?", expr.render(3))),
                Self::Plus(expr) => (3, format!("{}+", expr.render(3))),
                Self::Seq(left, right) => (2, format!("{} {}", left.render(2), right.render(2))),
                Self::Alt(left, right) => (1, format!("{} | {}", left.render(1), right.render(1))),
            };
            if precedence < parent_precedence {
                format!("({text})")
            } else {
                text
            }
        }

        fn bound(&self) -> BoundExpr {
            match self {
                Self::Lit(value) => token_expr(&value.to_string()),
                Self::Seq(left, right) => {
                    BoundExpr::Sequence(vec![left.bound(), right.bound()].into_boxed_slice())
                }
                Self::Alt(left, right) => {
                    BoundExpr::Alternation(vec![left.bound(), right.bound()].into_boxed_slice())
                }
                Self::Optional(expr) => BoundExpr::Repeat {
                    expr: Box::new(expr.bound()),
                    min: 0,
                    max: Some(1),
                },
                Self::Plus(expr) => BoundExpr::Repeat {
                    expr: Box::new(expr.bound()),
                    min: 1,
                    max: None,
                },
                Self::Group(expr) => expr.bound(),
            }
        }
    }

    fn generated_atom(tc: &hegel::TestCase) -> GeneratedExpr {
        let value = tc.draw(hegel::generators::sampled_from(vec!['a', 'b', 'c']));
        if tc.draw(
            hegel::generators::integers::<u8>()
                .min_value(0)
                .max_value(1),
        ) == 0
        {
            GeneratedExpr::Lit(value)
        } else {
            GeneratedExpr::Group(Box::new(GeneratedExpr::Lit(value)))
        }
    }

    fn generated_expr(tc: &hegel::TestCase, depth: usize) -> GeneratedExpr {
        if depth == 0 {
            return generated_atom(tc);
        }
        match tc.draw(
            hegel::generators::integers::<u8>()
                .min_value(0)
                .max_value(4),
        ) {
            0 => generated_atom(tc),
            1 => GeneratedExpr::Seq(
                Box::new(generated_expr(tc, depth - 1)),
                Box::new(generated_expr(tc, depth - 1)),
            ),
            2 => GeneratedExpr::Alt(
                Box::new(generated_expr(tc, depth - 1)),
                Box::new(generated_expr(tc, depth - 1)),
            ),
            3 => GeneratedExpr::Optional(Box::new(generated_atom(tc))),
            _ => GeneratedExpr::Plus(Box::new(generated_atom(tc))),
        }
    }

    #[hegel::test(test_cases = 500, derandomize = true)]
    fn normalized_source_matches_independent_oracle(tc: hegel::TestCase) {
        let expr = generated_expr(&tc, 3);
        let symbols = tc.draw(
            hegel::generators::vecs(hegel::generators::sampled_from(vec!["a", "b", "c", "z"]))
                .max_size(6),
        );
        let tokens = symbols.iter().map(|value| token(value)).collect::<Vec<_>>();
        let source = format!("p := ({}) z.", expr.render(0));
        let actual = compile_catalog(
            PatternSource::named("generated.lx", &source),
            CompileLimits::default(),
        )
        .expect("bounded generated source must compile")
        .matcher
        .find_matches(&tokens);
        let expected = oracle_matches(
            &program(BoundExpr::Sequence(
                vec![expr.bound(), token_expr("z")].into_boxed_slice(),
            )),
            &tokens,
        );
        assert_eq!(actual, expected, "source: {source}");
    }

    #[hegel::test(test_cases = 300, derandomize = true)]
    fn oracle_and_nfa_agree_without_repetition(tc: hegel::TestCase) {
        use hegel::generators;

        let definition_count = tc.draw(generators::integers::<usize>().min_value(1).max_value(4));
        let mut definitions = Vec::with_capacity(definition_count);
        for id in 0..definition_count {
            let left = tc.draw(generators::sampled_from(vec!["a", "b", "x"]));
            let right = tc.draw(generators::sampled_from(vec!["a", "b", "x"]));
            let shape = tc.draw(generators::integers::<u8>().min_value(0).max_value(3));
            let expr = match (shape, id) {
                (0, _) => token_expr(left),
                (1, _) => BoundExpr::Sequence(
                    vec![token_expr(left), token_expr(right)].into_boxed_slice(),
                ),
                (2, _) => BoundExpr::Alternation(
                    vec![token_expr(left), token_expr(right)].into_boxed_slice(),
                ),
                (_, 0) => token_expr(left),
                (_, _) => BoundExpr::Definition {
                    id: DefinitionId::new(u32::try_from(id - 1).expect("four definitions fit u32")),
                    span: fixture_span(),
                },
            };
            definitions.push(BoundDefinition {
                id: DefinitionId::new(u32::try_from(id).expect("four definitions fit u32")),
                source: SourceId::new(0),
                name: CompactString::from(format!("p{id}")),
                body: BoundDefinitionBody::Pattern {
                    visibility: DefinitionVisibility::Pattern,
                    expr,
                    relations: Box::default(),
                    enhanced_relations: Box::default(),
                },
                metadata: Arc::new(HashMap::new()),
                key: None,
                span: fixture_span(),
            });
        }
        let symbols =
            tc.draw(generators::vecs(generators::sampled_from(vec!["a", "b", "x"])).max_size(8));
        let tokens = symbols
            .iter()
            .map(|symbol| token(symbol))
            .collect::<Vec<_>>();
        let program = BoundProgram {
            definitions,
            sources: SourceRegistry::single("generated.lx", "")
                .expect("generated source registry is valid"),
            sets: Vec::new(),
            set_uses: BTreeMap::new(),
            tests: Vec::new(),
            test_uses: BTreeMap::new(),
        };

        let expected = oracle_matches(&program, &tokens);
        let actual = lower_to_nfa(
            &program,
            &CompileLimits::default(),
            &[],
            &Default::default(),
        )
        .expect("bounded generated program must lower")
        .matcher
        .find_matches(&tokens);

        assert_eq!(actual, expected);
    }

    #[hegel::test(test_cases = 300, derandomize = true)]
    fn oracle_and_nfa_agree_with_repetition(tc: hegel::TestCase) {
        use hegel::generators;

        let repeated = tc.draw(generators::sampled_from(vec!["a", "b"]));
        let (min, max) = tc.draw(generators::sampled_from(vec![
            (0, Some(1)),
            (1, Some(1)),
            (0, Some(3)),
            (1, Some(3)),
            (1, None),
        ]));
        let symbols =
            tc.draw(generators::vecs(generators::sampled_from(vec!["a", "b", "x"])).max_size(8));
        tc.target(symbols.len() as f64);
        tc.target(max.unwrap_or(9) as f64);
        let tokens = symbols
            .iter()
            .map(|symbol| token(symbol))
            .collect::<Vec<_>>();
        let program = repeat_program(repeated, min, max);

        assert_eq!(
            nfa_matches(&program, &tokens),
            oracle_matches(&program, &tokens)
        );
    }

    #[hegel::test(test_cases = 200, derandomize = true)]
    fn alternation_matches_the_union_of_its_branches(tc: hegel::TestCase) {
        use hegel::generators;

        let left = tc.draw(generators::sampled_from(vec!["a", "b", "x"]));
        let right = tc.draw(generators::sampled_from(vec!["a", "b", "x"]));
        let symbols =
            tc.draw(generators::vecs(generators::sampled_from(vec!["a", "b", "x"])).max_size(8));
        let tokens = symbols
            .iter()
            .map(|symbol| token(symbol))
            .collect::<Vec<_>>();
        let alternation = program(BoundExpr::Alternation(
            vec![token_expr(left), token_expr(right)].into_boxed_slice(),
        ));
        let mut union = nfa_matches(&program(token_expr(left)), &tokens);
        union.extend(nfa_matches(&program(token_expr(right)), &tokens));
        for matched in &mut union {
            matched.pattern = "p".into();
        }
        union.sort_by_key(|matched| (matched.start, matched.end, matched.pattern.clone()));
        union.dedup();

        assert_eq!(nfa_matches(&alternation, &tokens), union);
    }

    /// Protocol check 6 for `&`: complete canonical `Match` values from the
    /// production pipeline agree with the independent interpreter.
    ///
    /// The two are genuinely independent here, which is what makes the check
    /// worth running. `program.rs` evaluates operands left to right and retains
    /// the first's candidates by position; `evaluate_execution` folds the
    /// operands' end-position *sets* and filters afterwards. Same operator, two
    /// formulations, and only one of them was written while looking at the
    /// other.
    #[hegel::test(test_cases = 300, derandomize = true)]
    fn intersection_matches_the_intersection_of_its_operands(tc: hegel::TestCase) {
        use hegel::generators;

        let shape = tc.draw(generators::integers::<u8>().min_value(0).max_value(4));
        let left = tc.draw(generators::sampled_from(vec!["a", "b"]));
        let right = tc.draw(generators::sampled_from(vec!["a", "b"]));
        let width = tc.draw(generators::integers::<usize>().min_value(1).max_value(4));
        tc.target(width as f64);
        let source = match shape {
            // Widths that overlap without coinciding.
            0 => format!("pattern p := [text={left}]+ & []{{{width}}}."),
            // The double-wo shape: unbounded segments met by a length bound.
            1 => format!(
                "pattern p := ([text={left}]* [text={right}] [text in {{a, b}}]*) & []{{1,{width}}}."
            ),
            // Nested beneath a quantifier.
            2 => format!("pattern p := ([text in {{a, b}}] & [text={right}]){{1,{width}}}."),
            // An operand that is itself an alternation.
            3 => {
                format!("pattern p := ([text={left}] | [text={left}] [text={right}]) & []{{1,2}}.")
            }
            // Reached through a fragment, which is how a catalog writes it.
            _ => format!(
                "fragment inner := [text in {{a, b}}]+ & []{{{width}}}.\npattern p := @inner."
            ),
        };
        let symbols = tc.draw(
            generators::vecs(generators::integers::<u8>().min_value(0).max_value(2)).max_size(12),
        );
        let tokens = symbols
            .into_iter()
            .map(|symbol| match symbol {
                0 => token("a"),
                1 => token("b"),
                _ => token("z"),
            })
            .collect::<Vec<_>>();

        assert_execution_oracle(&source, &tokens);
    }

    #[hegel::test(test_cases = 200, derandomize = true)]
    fn exact_one_repetition_equals_its_child(tc: hegel::TestCase) {
        use hegel::generators;

        let value = tc.draw(generators::sampled_from(vec!["a", "b", "x"]));
        let symbols =
            tc.draw(generators::vecs(generators::sampled_from(vec!["a", "b", "x"])).max_size(8));
        let tokens = symbols
            .iter()
            .map(|symbol| token(symbol))
            .collect::<Vec<_>>();

        assert_eq!(
            nfa_matches(&program(token_expr(value)), &tokens),
            nfa_matches(&repeat_program(value, 1, Some(1)), &tokens)
        );
    }

    #[hegel::test(test_cases = 200, derandomize = true)]
    fn repetition_spans_stay_within_nonempty_token_bounds(tc: hegel::TestCase) {
        use hegel::generators;

        let symbols =
            tc.draw(generators::vecs(generators::sampled_from(vec!["a", "x"])).max_size(8));
        let tokens = symbols
            .iter()
            .map(|symbol| token(symbol))
            .collect::<Vec<_>>();
        for matched in nfa_matches(&repeat_program("a", 0, Some(3)), &tokens) {
            assert!(matched.start < matched.end);
            assert!(matched.end <= tokens.len());
        }
    }

    #[hegel::test(test_cases = 200, derandomize = true)]
    fn failing_terminator_does_not_extend_repetition(tc: hegel::TestCase) {
        use hegel::generators;

        let count = tc.draw(generators::integers::<usize>().min_value(1).max_value(8));
        let program = repeat_program("a", 1, None);
        let prefix = (0..count).map(|_| token("a")).collect::<Vec<_>>();
        let mut terminated = prefix.clone();
        terminated.push(token("x"));
        let prefix_ends = nfa_matches(&program, &prefix)
            .into_iter()
            .map(|matched| (matched.start, matched.end))
            .collect::<Vec<_>>();
        let terminated_ends = nfa_matches(&program, &terminated)
            .into_iter()
            .filter(|matched| matched.end <= count)
            .map(|matched| (matched.start, matched.end))
            .collect::<Vec<_>>();

        assert_eq!(terminated_ends, prefix_ends);
        assert!(
            nfa_matches(&program, &terminated)
                .iter()
                .all(|matched| matched.end <= count)
        );
    }

    #[test]
    fn any_token_predicate_agrees_between_matcher_and_oracle() {
        // `predicate_matches` is `.all()` over an empty attribute slice, which
        // is vacuously true -- the same answer `CompiledPredicate::Empty` gives
        // the matcher. This pins that agreement instead of inferring it.
        let program = bound_program("p := [] [].");
        let tokens = [token("a"), token("b"), token("c")];

        let expected = oracle_matches(&program, &tokens);
        assert_eq!(nfa_matches(&program, &tokens), expected);
        // Pin the spans too. Parity alone is satisfied when BOTH sides return
        // nothing, which is exactly the failure this test exists to catch.
        assert_eq!(
            expected
                .iter()
                .map(|matched| (matched.start, matched.end))
                .collect::<Vec<_>>(),
            vec![(0, 2), (1, 3)],
        );
    }
}
