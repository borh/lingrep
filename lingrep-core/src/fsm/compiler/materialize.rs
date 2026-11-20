use compact_str::CompactString;
use std::collections::{BTreeMap, BTreeSet};

use super::diagnostic::{CompileError, Diagnostic, DiagnosticCode};
use super::limits::CompileLimits;
use super::model::{
    BoundExpr, BoundProgram, CaptureDeclId, CaptureId, DefinitionId, ExecutionCapability,
    ExecutionDefinition, ExecutionExpr, RelationLabels, RelationTarget, SourceSpan, SpanConstraint,
    SpanOperand, SpanPosition,
};
use crate::fsm::AssertionOccurrenceId;

pub(crate) fn materialize_program(
    bound: &BoundProgram,
    limits: &CompileLimits,
    assertion_occurrences: &BTreeMap<SourceSpan, AssertionOccurrenceId>,
) -> Result<Box<[ExecutionDefinition]>, CompileError> {
    let mut output = Vec::new();
    for definition in &bound.definitions {
        let mut state = Materializer {
            bound,
            limits,
            visits: 0,
            owned_bytes: 0,
            next_capture: 0,
            captures: BTreeMap::new(),
            assertion_occurrences,
        };
        if !definition.is_ordinary_pattern() {
            if let Some(entry) = ["within_span", "span_dependency"]
                .iter()
                .find_map(|key| definition.metadata.get(*key))
            {
                return Err(state.at(
                    entry.span,
                    DiagnosticCode::InvalidPolicyValue,
                    "span constraints require an emitting pattern",
                ));
            }
            continue;
        }
        let path = definition.name.clone();
        let expr = state.expand(
            definition.id,
            definition
                .expr()
                .expect("ordinary patterns have expressions"),
            &path,
            &mut BTreeMap::new(),
        )?;
        let relations = state.resolve_relations(definition.relations(), &expr)?;
        let (enhanced_relations, orders, graph_bindings) = state.resolve_enhanced_relations(
            definition.enhanced_relations(),
            definition.orders(),
            &expr,
        )?;
        let constrained = !relations.is_empty()
            || !enhanced_relations.is_empty()
            || !orders.is_empty()
            || !definition.span_paths().is_empty()
            || definition.metadata.contains_key("within_span")
            || definition.metadata.contains_key("span_dependency");
        output.push(ExecutionDefinition {
            interpretation: state.interpretation(
                definition.interpretation.as_ref(),
                &expr,
                constrained,
            )?,
            span_constraints: state.span_constraints(
                &definition.metadata,
                definition.span_paths(),
                &expr,
            )?,
            name: definition.name.clone(),
            expr,
            capability: ExecutionCapability::Plain,
            relations,
            enhanced_relations,
            orders,
            graph_bindings,
            metadata: definition.metadata.clone(),
            key: definition.key.clone(),
            source: definition.source,
            span: definition.span,
        });
    }
    Ok(output.into_boxed_slice())
}

fn parse_span_path(
    path: &str,
    resolve: &impl Fn(&str) -> Result<CaptureId, CompileError>,
    invalid: &impl Fn(&str) -> CompileError,
) -> Result<SpanConstraint, CompileError> {
    let (kind, chain) = path
        .split_once(':')
        .ok_or_else(|| invalid("expected 'kind: $capture -> $capture'"))?;
    let kind = kind.trim();
    if kind.is_empty() {
        return Err(invalid("kind must be nonempty"));
    }
    let mut nodes = Vec::new();
    for operand in chain.split("->") {
        let operand = operand.trim();
        let (selector, label) = match operand.split_once('@') {
            Some((_, "")) => return Err(invalid("expected a label after @")),
            Some((selector, label)) => (selector, Some(label.to_owned())),
            None => (operand, None),
        };
        let (name, position) = selector.split_once('.').unwrap_or((selector, "inside"));
        let position = match position {
            "inside" => SpanPosition::Inside,
            "whole" => SpanPosition::Whole,
            "head" => SpanPosition::Head,
            _ => return Err(invalid("expected .inside, .whole or .head")),
        };
        let name = name
            .strip_prefix('$')
            .filter(|name| !name.is_empty())
            .ok_or_else(|| invalid("each path operand must name a $capture"))?;
        nodes.push(SpanOperand {
            source: None,
            capture: Some(resolve(name)?),
            position,
            label,
        });
    }
    nodes.reverse();
    Ok(SpanConstraint {
        kind: kind.to_owned(),
        nodes: nodes.into_boxed_slice(),
    })
}

struct Materializer<'a> {
    bound: &'a BoundProgram,
    limits: &'a CompileLimits,
    visits: usize,
    owned_bytes: usize,
    next_capture: u32,
    captures: BTreeMap<(DefinitionId, CaptureDeclId), CaptureId>,
    assertion_occurrences: &'a BTreeMap<SourceSpan, AssertionOccurrenceId>,
}

impl Materializer<'_> {
    fn expand(
        &mut self,
        owner: DefinitionId,
        expr: &BoundExpr,
        path: &CompactString,
        references: &mut BTreeMap<DefinitionId, usize>,
    ) -> Result<ExecutionExpr, CompileError> {
        self.visits = self
            .visits
            .checked_add(1)
            .ok_or_else(|| self.limit("expansion work overflowed"))?;
        if self.visits > self.limits.max_expansion_work() {
            return Err(self.limit("lingrep patterns program exceeds the expansion work limit"));
        }
        Ok(match expr {
            BoundExpr::Token(predicate) => ExecutionExpr::Token(predicate.clone()),
            BoundExpr::Assertion(assertion) => ExecutionExpr::Assertion(assertion.clone()),
            BoundExpr::Lookaround(lookaround) => {
                if let Some(predicate) = lookaround.specialized_previous_token() {
                    ExecutionExpr::Assertion(crate::fsm::PositionAssertion::PreviousToken(
                        predicate.clone(),
                    ))
                } else {
                    let occurrence = *self
                        .assertion_occurrences
                        .get(&lookaround.span)
                        .expect("general lookaround occurrence is compiled before materialization");
                    ExecutionExpr::Assertion(crate::fsm::PositionAssertion::Lookaround(occurrence))
                }
            }
            BoundExpr::Sequence(expressions) => ExecutionExpr::Sequence(
                expressions
                    .iter()
                    .map(|expr| self.expand(owner, expr, path, references))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
            ),
            BoundExpr::Alternation(expressions) => ExecutionExpr::Alternation(
                expressions
                    .iter()
                    .map(|expr| self.expand(owner, expr, path, references))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
            ),
            BoundExpr::Intersection { operands, span } => ExecutionExpr::Intersection {
                operands: operands
                    .iter()
                    .map(|expr| self.expand(owner, expr, path, references))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
                span: *span,
            },
            BoundExpr::AnyOrder { operands, span } => ExecutionExpr::AnyOrder {
                operands: operands
                    .iter()
                    .map(|expr| self.expand(owner, expr, path, references))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_boxed_slice(),
                span: *span,
            },
            BoundExpr::Repeat { expr, min, max } => ExecutionExpr::Repeat {
                expr: Box::new(self.expand(owner, expr, path, references)?),
                min: *min,
                max: *max,
            },
            BoundExpr::Capture {
                declaration,
                name,
                expr,
                span,
            } => {
                let id = CaptureId::new(self.next_capture);
                self.next_capture = self.next_capture.checked_add(1).ok_or_else(|| {
                    self.at(
                        *span,
                        DiagnosticCode::LimitExceeded,
                        "capture identity overflowed",
                    )
                })?;
                self.captures.insert((owner, *declaration), id);
                let display = CompactString::from(format!("{path}/{name}"));
                self.owned_bytes = self
                    .owned_bytes
                    .checked_add(display.len())
                    .ok_or_else(|| self.limit("generated owned bytes overflowed"))?;
                if self.owned_bytes > self.limits.max_generated_owned_bytes() {
                    return Err(self.at(
                        *span,
                        DiagnosticCode::LimitExceeded,
                        "lingrep patterns program exceeds the generated-owned-byte limit",
                    ));
                }
                ExecutionExpr::Capture {
                    id,
                    name: name.clone(),
                    path: display,
                    span: *span,
                    expr: Box::new(self.expand(owner, expr, path, references)?),
                }
            }
            BoundExpr::SameAs {
                declaration,
                projection,
                span,
            } => ExecutionExpr::SameAs {
                capture: *self
                    .captures
                    .get(&(owner, *declaration))
                    .expect("definite assignment guarantees a materialized capture"),
                projection: projection.clone(),
                span: *span,
            },
            BoundExpr::Definition { id, span } => {
                let count = references.entry(*id).or_default();
                *count = count.checked_add(1).ok_or_else(|| {
                    self.at(
                        *span,
                        DiagnosticCode::LimitExceeded,
                        "fragment occurrence overflowed",
                    )
                })?;
                let definition = &self.bound.definitions[id.index()];
                let nested_path =
                    CompactString::from(format!("{path}/{}#{}", definition.name, *count));
                self.owned_bytes = self
                    .owned_bytes
                    .checked_add(nested_path.len())
                    .ok_or_else(|| self.limit("generated owned bytes overflowed"))?;
                let mut nested_references = BTreeMap::new();
                self.expand(
                    *id,
                    definition.expr().expect("lexicon references are refused"),
                    &nested_path,
                    &mut nested_references,
                )?
            }
        })
    }

    fn interpretation(
        &mut self,
        parsed: Option<&super::model::ParsedInterpretation>,
        expr: &ExecutionExpr,
        constrained: bool,
    ) -> Result<Option<Box<super::model::InterpretationRecipe>>, CompileError> {
        use super::model::InterpretationRecipe;
        let Some(parsed) = parsed else {
            return Ok(None);
        };
        if let Some(first) = parsed.captures.values().next()
            && (constrained || !supports_interpretation(expr))
        {
            return Err(self.at(first.span, DiagnosticCode::InvalidInterpretation, "interpret supports consuming tokens, captures, alternatives, sequences and repetitions; remove the unsupported assertion, intersection, comparison, any_order or where constraint"));
        }
        let mut names = BTreeMap::new();
        collect_capture_names(expr, &mut names);
        let mut captures = BTreeMap::new();
        // Public role identity survives reordering and helper extraction.
        let mut templates = BTreeMap::new();
        for (capture, assignment) in &parsed.captures {
            let ids = names.get(capture.as_str()).ok_or_else(|| {
                self.at(
                    assignment.span,
                    DiagnosticCode::InvalidInterpretation,
                    &format!("interpret names unknown capture `${capture}`"),
                )
            })?;
            let mut bytes = ids
                .len()
                .saturating_mul(size_of::<(CaptureId, usize)>())
                .saturating_add(assignment.role.len());
            for (key, value) in &assignment.labels {
                bytes = bytes.saturating_add(key.len() + size_of::<crate::fsm::AnalyticalValue>());
                if let crate::fsm::AnalyticalValue::String(text) = value {
                    bytes = bytes.saturating_add(text.len());
                }
            }
            self.owned_bytes = self.owned_bytes.saturating_add(bytes);
            if self.owned_bytes > self.limits.max_generated_owned_bytes() {
                return Err(self.at(
                    assignment.span,
                    DiagnosticCode::LimitExceeded,
                    "analytical roles exceed the generated owned byte limit",
                ));
            }
            templates
                .entry((&assignment.role, &assignment.labels))
                .or_insert_with(Vec::new)
                .extend_from_slice(ids);
        }
        let mut roles = Vec::with_capacity(templates.len());
        for ((role, labels), ids) in templates {
            let index = roles.len();
            roles.push((role.clone(), labels.clone()));
            for id in ids {
                captures.insert(id, index);
            }
        }
        let classify_by = parsed
            .classify_by
            .as_ref()
            .map(|(name, span)| {
                roles
                    .iter()
                    .any(|(role, _)| role == name)
                    .then(|| name.clone())
                    .ok_or_else(|| {
                        self.at(
                            *span,
                            DiagnosticCode::InvalidInterpretation,
                            "classify by must name a role declared by interpret",
                        )
                    })
            })
            .transpose()?;
        Ok(Some(Box::new(InterpretationRecipe {
            captures,
            roles,
            classify_by,
        })))
    }

    fn span_constraints(
        &self,
        metadata: &super::model::Metadata,
        paths: &[super::model::ParsedSpanPath],
        expr: &ExecutionExpr,
    ) -> Result<Box<[super::model::SpanConstraint]>, CompileError> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Within {
            kind: String,
            label: Option<String>,
            capture: Option<String>,
            #[serde(default)]
            position: SpanPosition,
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Dependency {
            kind: String,
            governor: String,
            dependent: String,
        }

        let mut constraints = Vec::new();
        for key in ["within_span", "span_dependency"] {
            let Some(entry) = metadata.get(key) else {
                continue;
            };
            let invalid = |message: &str| {
                self.at(
                    entry.span,
                    DiagnosticCode::InvalidPolicyValue,
                    &format!("invalid `{key}`: {message}"),
                )
            };
            let mut captures = BTreeMap::new();
            collect_capture_names(expr, &mut captures);
            let resolve = |name: &str| self.resolve_relation_operand(&captures, name, entry.span);
            if key == "span_dependency" && !entry.value.is_object() {
                let paths = match entry.value.as_array() {
                    Some(paths) if paths.is_empty() => {
                        return Err(invalid("expected at least one path"));
                    }
                    Some(paths) => paths.as_slice(),
                    None => std::slice::from_ref(&entry.value),
                };
                for path in paths {
                    let path = path
                        .as_str()
                        .ok_or_else(|| invalid("expected a quoted span path"))?;
                    constraints.push(parse_span_path(path, &resolve, &invalid)?);
                }
                continue;
            }
            let constraint = if key == "within_span" {
                let within = if let Some(kind) = entry.value.as_str() {
                    Within {
                        kind: kind.into(),
                        label: None,
                        capture: None,
                        position: Default::default(),
                    }
                } else {
                    serde_json::from_value(entry.value.clone())
                        .map_err(|e| invalid(&e.to_string()))?
                };
                if within.kind.is_empty() {
                    return Err(invalid("kind must be nonempty"));
                }
                SpanConstraint {
                    kind: within.kind,
                    nodes: Box::new([SpanOperand {
                        source: None,
                        label: within.label,
                        position: within.position,
                        capture: within.capture.as_deref().map(resolve).transpose()?,
                    }]),
                }
            } else {
                let edge: Dependency = serde_json::from_value(entry.value.clone())
                    .map_err(|e| invalid(&e.to_string()))?;
                if edge.kind.is_empty() || edge.governor == edge.dependent {
                    return Err(invalid(
                        "a dependency needs a kind and two distinct captures",
                    ));
                }
                SpanConstraint {
                    kind: edge.kind,
                    nodes: Box::new([
                        SpanOperand {
                            capture: Some(resolve(&edge.dependent)?),
                            ..Default::default()
                        },
                        SpanOperand {
                            capture: Some(resolve(&edge.governor)?),
                            ..Default::default()
                        },
                    ]),
                }
            };
            constraints.push(constraint);
        }
        if !paths.is_empty() {
            let mut captures = BTreeMap::new();
            collect_capture_names(expr, &mut captures);
            for path in paths {
                let nodes = path
                    .operands
                    .iter()
                    .rev()
                    .map(|operand| {
                        Ok(SpanOperand {
                            capture: Some(self.resolve_relation_operand(
                                &captures,
                                &operand.capture,
                                operand.source,
                            )?),
                            position: operand.position,
                            label: operand.label.clone(),
                            source: Some(operand.source),
                        })
                    })
                    .collect::<Result<Box<[_]>, CompileError>>()?;
                constraints.push(SpanConstraint {
                    kind: path.kind.clone(),
                    nodes,
                });
            }
        }
        Ok(constraints.into_boxed_slice())
    }

    fn resolve_relations(
        &mut self,
        relations: &[super::model::BoundRelation],
        expr: &ExecutionExpr,
    ) -> Result<Box<[super::model::ExecutionRelation]>, CompileError> {
        if relations.is_empty() {
            return Ok(Box::default());
        }
        let mut by_name: BTreeMap<&str, Vec<CaptureId>> = BTreeMap::new();
        collect_capture_names(expr, &mut by_name);
        let mut resolved = Vec::with_capacity(relations.len());
        for relation in relations {
            let head =
                self.resolve_relation_operand(&by_name, &relation.head, relation.head_span)?;
            let target = match &relation.target {
                RelationTarget::Captured {
                    polarity,
                    capture,
                    span,
                } => RelationTarget::Captured {
                    polarity: *polarity,
                    capture: self.resolve_relation_operand(&by_name, capture, *span)?,
                    span: *span,
                },
                RelationTarget::Anonymous { polarity, span } => RelationTarget::Anonymous {
                    polarity: *polarity,
                    span: *span,
                },
                RelationTarget::Predicate {
                    polarity,
                    predicates,
                    span,
                } => RelationTarget::Predicate {
                    polarity: *polarity,
                    predicates: predicates.clone(),
                    span: *span,
                },
            };
            // Charged over the *resolved* set, so a `@name` term costs what
            // its members cost rather than what its reference spells. That is
            // the same accounting rule `[x in @S]` follows, and it is what
            // keeps a large set behind a short reference inside the budget.
            let label_bytes = match &relation.labels {
                RelationLabels::Any => 0,
                RelationLabels::Listed(labels) => labels
                    .iter()
                    .try_fold(0usize, |total, label| {
                        total.checked_add(label.authored().len())
                    })
                    .ok_or_else(|| self.limit("generated owned bytes overflowed"))?,
            };
            self.owned_bytes = self
                .owned_bytes
                .checked_add(label_bytes)
                .ok_or_else(|| self.limit("generated owned bytes overflowed"))?;
            if self.owned_bytes > self.limits.max_generated_owned_bytes() {
                return Err(self.at(
                    relation.head_span,
                    DiagnosticCode::LimitExceeded,
                    "lingrep patterns program exceeds the generated owned byte limit",
                ));
            }
            resolved.push(super::model::ExecutionRelation {
                head,
                head_span: relation.head_span,
                labels: relation.labels.clone(),
                label_span: relation.label_span,
                target,
            });
        }
        Ok(resolved.into_boxed_slice())
    }

    fn resolve_enhanced_relations(
        &mut self,
        relations: &[super::model::BoundEnhancedRelation],
        orders: &[super::model::OrderConstraint],
        expr: &ExecutionExpr,
    ) -> Result<
        (
            Box<[super::model::ExecutionEnhancedRelation]>,
            Box<[super::model::ExecutionOrder]>,
            Box<[super::model::ExecutionGraphBindingDecl]>,
        ),
        CompileError,
    > {
        use super::model::{
            BoundEnhancedEndpoint, ExecutionEnhancedEndpoint, ExecutionEnhancedRelation,
            ExecutionGraphBindingDecl,
        };
        if relations.is_empty() && orders.is_empty() {
            return Ok((Box::default(), Box::default(), Box::default()));
        }
        let mut captures: BTreeMap<&str, Vec<CaptureId>> = BTreeMap::new();
        collect_capture_names(expr, &mut captures);
        let mut graph_bindings = BTreeMap::new();
        let mut declarations = Vec::new();
        for endpoint in relations
            .iter()
            .filter(|relation| relation.polarity == super::model::RelationPolarity::Positive)
            .flat_map(|relation| [&relation.head, &relation.dependent])
        {
            let BoundEnhancedEndpoint::Binding {
                name,
                predicates,
                span,
            } = endpoint
            else {
                continue;
            };
            if captures.contains_key(name.as_str()) || graph_bindings.contains_key(name.as_str()) {
                return Err(self.at(
                    *span,
                    DiagnosticCode::EnhancedGraphBindingDuplicate,
                    &format!("graph binding `${name}` duplicates another surface or graph binding"),
                ));
            }
            graph_bindings.insert(name.as_str(), (predicates.clone(), *span));
            declarations.push((*span, ExecutionGraphBindingDecl::Node(name.clone())));
        }
        let mut all_names = graph_bindings.keys().copied().collect::<BTreeSet<_>>();
        for relation in relations {
            if relation.polarity == super::model::RelationPolarity::Negative {
                continue;
            }
            let Some((name, span)) = &relation.edge else {
                continue;
            };
            if captures.contains_key(name.as_str()) || !all_names.insert(name.as_str()) {
                return Err(self.at(
                    *span,
                    DiagnosticCode::EnhancedGraphBindingDuplicate,
                    &format!(
                        "enhanced edge binding `${name}` duplicates another surface or graph binding"
                    ),
                ));
            }
            declarations.push((*span, ExecutionGraphBindingDecl::Edge(name.clone())));
        }
        let mut declared_graph_nodes = BTreeSet::new();
        for relation in relations {
            for endpoint in [&relation.head, &relation.dependent] {
                if relation.polarity == super::model::RelationPolarity::Negative {
                    let Some((name, span)) = (match endpoint {
                        BoundEnhancedEndpoint::Reference { name, span } => Some((name, span)),
                        BoundEnhancedEndpoint::Binding { span, .. } => {
                            return Err(self.at(
                                *span,
                                DiagnosticCode::EnhancedGraphBindingUndefined,
                                "a negative enhanced edge cannot introduce a graph binding",
                            ));
                        }
                        BoundEnhancedEndpoint::BasicHead { .. } => None,
                    }) else {
                        continue;
                    };
                    if !captures.contains_key(name.as_str())
                        && !graph_bindings.contains_key(name.as_str())
                    {
                        return Err(self.at(
                            *span,
                            DiagnosticCode::EnhancedGraphBindingUndefined,
                            &format!("undefined enhanced graph or surface binding `${name}`"),
                        ));
                    }
                    continue;
                }
                match endpoint {
                    BoundEnhancedEndpoint::Binding { name, .. } => {
                        declared_graph_nodes.insert(name.as_str());
                    }
                    BoundEnhancedEndpoint::Reference { name, span }
                        if !captures.contains_key(name.as_str())
                            && !declared_graph_nodes.contains(name.as_str()) =>
                    {
                        return Err(self.at(
                            *span,
                            DiagnosticCode::EnhancedGraphBindingUndefined,
                            &format!("graph binding `${name}` is referenced before it is declared"),
                        ));
                    }
                    BoundEnhancedEndpoint::Reference { .. }
                    | BoundEnhancedEndpoint::BasicHead { .. } => {}
                }
            }
        }
        let resolve = |endpoint: &BoundEnhancedEndpoint,
                       this: &Self|
         -> Result<ExecutionEnhancedEndpoint, CompileError> {
            let (name, span) = match endpoint {
                BoundEnhancedEndpoint::Reference { name, span }
                | BoundEnhancedEndpoint::Binding { name, span, .. } => (name, *span),
                BoundEnhancedEndpoint::BasicHead { dependent, span } => {
                    return Ok(ExecutionEnhancedEndpoint::BasicHead(
                        this.resolve_relation_operand(&captures, dependent, *span)?,
                    ));
                }
            };
            if let Some(ids) = captures.get(name.as_str()) {
                return match ids.as_slice() {
                    [id] => Ok(ExecutionEnhancedEndpoint::Surface(*id)),
                    _ => Err(this.at(
                        span,
                        DiagnosticCode::EnhancedGraphBindingUndefined,
                        &format!(
                            "enhanced endpoint `${name}` resolves to more than one materialized capture"
                        ),
                    )),
                };
            }
            graph_bindings
                .get(name.as_str())
                .map(|(predicates, _)| ExecutionEnhancedEndpoint::Graph {
                    name: name.clone(),
                    predicates: predicates.clone(),
                })
                .ok_or_else(|| {
                    this.at(
                        span,
                        DiagnosticCode::EnhancedGraphBindingUndefined,
                        &format!("undefined enhanced graph or surface binding `${name}`"),
                    )
                })
        };
        let mut resolved = Vec::with_capacity(relations.len());
        for relation in relations {
            resolved.push(ExecutionEnhancedRelation {
                layer: relation.layer,
                polarity: relation.polarity,
                head: resolve(&relation.head, self)?,
                labels: relation.labels.clone(),
                edge: relation.edge.as_ref().map(|(name, _)| name.clone()),
                dependent: resolve(&relation.dependent, self)?,
            });
        }
        let orders = orders
            .iter()
            .map(|order| {
                order
                    .operands
                    .iter()
                    .map(|(name, span)| {
                        resolve(
                            &BoundEnhancedEndpoint::Reference {
                                name: name.clone(),
                                span: *span,
                            },
                            self,
                        )
                    })
                    .collect()
            })
            .collect::<Result<_, _>>()?;

        let mut reachable = BTreeSet::new();
        for relation in resolved
            .iter()
            .filter(|relation| relation.polarity == super::model::RelationPolarity::Positive)
        {
            if endpoint_is_surface_anchor(&relation.head)
                && let ExecutionEnhancedEndpoint::Graph { name, .. } = &relation.dependent
            {
                reachable.insert(name.clone());
            }
            if endpoint_is_surface_anchor(&relation.dependent)
                && let ExecutionEnhancedEndpoint::Graph { name, .. } = &relation.head
            {
                reachable.insert(name.clone());
            }
        }
        loop {
            let before = reachable.len();
            for relation in resolved
                .iter()
                .filter(|relation| relation.polarity == super::model::RelationPolarity::Positive)
            {
                let (
                    ExecutionEnhancedEndpoint::Graph { name: left, .. },
                    ExecutionEnhancedEndpoint::Graph { name: right, .. },
                ) = (&relation.head, &relation.dependent)
                else {
                    continue;
                };
                if reachable.contains(left) {
                    reachable.insert(right.clone());
                }
                if reachable.contains(right) {
                    reachable.insert(left.clone());
                }
            }
            if reachable.len() == before {
                break;
            }
        }
        if let Some((name, (_, span))) = graph_bindings
            .iter()
            .find(|(name, _)| !reachable.contains(**name))
        {
            return Err(self.at(
                *span,
                DiagnosticCode::EnhancedGraphComponentUnanchored,
                &format!(
                    "enhanced graph component containing `${name}` has no surface capture anchor"
                ),
            ));
        }
        // A `where` clause is a conjunction, so authored constraint order is
        // not semantic. Keep the observable binding vector canonical too:
        // nodes first, then edges, with names ordering each namespace.
        declarations.sort_by(|(_, left), (_, right)| match (left, right) {
            (ExecutionGraphBindingDecl::Node(left), ExecutionGraphBindingDecl::Node(right))
            | (ExecutionGraphBindingDecl::Edge(left), ExecutionGraphBindingDecl::Edge(right)) => {
                left.cmp(right)
            }
            (ExecutionGraphBindingDecl::Node(_), ExecutionGraphBindingDecl::Edge(_)) => {
                std::cmp::Ordering::Less
            }
            (ExecutionGraphBindingDecl::Edge(_), ExecutionGraphBindingDecl::Node(_)) => {
                std::cmp::Ordering::Greater
            }
        });
        Ok((
            resolved.into_boxed_slice(),
            orders,
            declarations
                .into_iter()
                .map(|(_, declaration)| declaration)
                .collect(),
        ))
    }

    fn resolve_relation_operand(
        &self,
        by_name: &BTreeMap<&str, Vec<CaptureId>>,
        name: &str,
        span: SourceSpan,
    ) -> Result<CaptureId, CompileError> {
        match by_name.get(name).map(Vec::as_slice) {
            Some([id]) => Ok(*id),
            Some(_) => Err(self.at(
                span,
                DiagnosticCode::WhereCaptureUndefined,
                &format!(
                    "`where` operand `${name}` resolves to more than one \
                     materialized capture; a constraint needs exactly one"
                ),
            )),
            None => Err(self.at(
                span,
                DiagnosticCode::WhereCaptureUndefined,
                &format!(
                    "`where` operand `${name}` names no capture in this \
                     definition's expression"
                ),
            )),
        }
    }

    fn limit(&self, message: &str) -> CompileError {
        CompileError::from_diagnostics(vec![Diagnostic::error_with_source_label(
            DiagnosticCode::LimitExceeded,
            self.bound
                .sources
                .source_label(super::model::SourceId::new(0)),
            0..0,
            message,
            1,
            1,
        )])
    }

    fn at(&self, span: SourceSpan, code: DiagnosticCode, message: &str) -> CompileError {
        let range = span.byte_range();
        let (line, column) = self.bound.sources.line_column(span);
        CompileError::from_diagnostics(vec![Diagnostic::error_with_source_label(
            code,
            self.bound.sources.source_label(span.source()),
            usize::try_from(range.start).expect("source offset fits usize")
                ..usize::try_from(range.end).expect("source offset fits usize"),
            message,
            line,
            column,
        )])
    }
}

fn endpoint_is_surface_anchor(endpoint: &super::model::ExecutionEnhancedEndpoint) -> bool {
    matches!(
        endpoint,
        super::model::ExecutionEnhancedEndpoint::Surface(_)
            | super::model::ExecutionEnhancedEndpoint::BasicHead(_)
    )
}

fn collect_capture_names<'expr>(
    expr: &'expr ExecutionExpr,
    by_name: &mut BTreeMap<&'expr str, Vec<CaptureId>>,
) {
    match expr {
        ExecutionExpr::Token(_) | ExecutionExpr::Assertion(_) | ExecutionExpr::SameAs { .. } => {}
        ExecutionExpr::Sequence(children) | ExecutionExpr::Alternation(children) => {
            for child in children {
                collect_capture_names(child, by_name);
            }
        }
        ExecutionExpr::Intersection { operands, .. } | ExecutionExpr::AnyOrder { operands, .. } => {
            for operand in operands {
                collect_capture_names(operand, by_name);
            }
        }
        ExecutionExpr::Repeat { expr, .. } => collect_capture_names(expr, by_name),
        ExecutionExpr::Capture { id, name, expr, .. } => {
            by_name.entry(name.as_str()).or_default().push(*id);
            collect_capture_names(expr, by_name);
        }
    }
}

fn supports_interpretation(expr: &ExecutionExpr) -> bool {
    match expr {
        ExecutionExpr::Token(_) => true,
        ExecutionExpr::Sequence(parts) | ExecutionExpr::Alternation(parts) => {
            parts.iter().all(supports_interpretation)
        }
        ExecutionExpr::Capture { expr, .. } | ExecutionExpr::Repeat { expr, .. } => {
            supports_interpretation(expr)
        }
        ExecutionExpr::Assertion(_)
        | ExecutionExpr::AnyOrder { .. }
        | ExecutionExpr::SameAs { .. }
        | ExecutionExpr::Intersection { .. } => false,
    }
}
