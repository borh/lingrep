use compact_str::CompactString;
use std::collections::{BTreeMap, BTreeSet};

use super::diagnostic::{CompileError, Diagnostic, DiagnosticCode};
use super::limits::CompileLimits;
use super::model::{
    BoundExpr, BoundProgram, CaptureDeclId, CaptureId, DefinitionId, ExecutionCapability,
    ExecutionDefinition, ExecutionExpr, RelationLabels, RelationTarget, SourceSpan,
};
use crate::fsm::AssertionOccurrenceId;

pub(crate) fn materialize_program(
    bound: &BoundProgram,
    limits: &CompileLimits,
    assertion_occurrences: &BTreeMap<SourceSpan, AssertionOccurrenceId>,
) -> Result<Box<[ExecutionDefinition]>, CompileError> {
    let mut output = Vec::new();
    for definition in &bound.definitions {
        if !definition.is_ordinary_pattern() {
            continue;
        }
        let mut state = Materializer {
            bound,
            limits,
            visits: 0,
            owned_bytes: 0,
            next_capture: 0,
            captures: BTreeMap::new(),
            assertion_occurrences,
        };
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
        let (enhanced_relations, graph_bindings) =
            state.resolve_enhanced_relations(definition.enhanced_relations(), &expr)?;
        output.push(ExecutionDefinition {
            name: definition.name.clone(),
            expr,
            capability: ExecutionCapability::Plain,
            relations,
            enhanced_relations,
            graph_bindings,
            metadata: definition.metadata.clone(),
            key: definition.key.clone(),
            source: definition.source,
            span: definition.span,
        });
    }
    Ok(output.into_boxed_slice())
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

    /// Resolves a definition's `where` operands against the captures of its
    /// materialized expression.
    ///
    /// Resolution runs here, after fragment expansion, so a capture arriving
    /// through an inclusion is a legal operand exactly when it is a legal
    /// capture. A name that resolves to more than one materialized capture —
    /// one fragment included twice — is refused: the constraint would have no
    /// single edge to check.
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
                RelationTarget::Captured { capture, span } => RelationTarget::Captured {
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
        expr: &ExecutionExpr,
    ) -> Result<
        (
            Box<[super::model::ExecutionEnhancedRelation]>,
            Box<[super::model::ExecutionGraphBindingDecl]>,
        ),
        CompileError,
    > {
        use super::model::{
            BoundEnhancedEndpoint, ExecutionEnhancedEndpoint, ExecutionEnhancedRelation,
            ExecutionGraphBindingDecl,
        };
        if relations.is_empty() {
            return Ok((Box::default(), Box::default()));
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
                    &format!(
                        "enhanced graph binding `${name}` duplicates another surface or graph binding"
                    ),
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
                            &format!(
                                "enhanced graph binding `${name}` is referenced before it is declared"
                            ),
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
                polarity: relation.polarity,
                head: resolve(&relation.head, self)?,
                labels: relation.labels.clone(),
                edge: relation.edge.as_ref().map(|(name, _)| name.clone()),
                dependent: resolve(&relation.dependent, self)?,
            });
        }

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

/// Collects every materialized capture, grouped by authored name.
///
/// Grouped rather than flattened because one fragment included twice
/// materializes its capture once per inclusion: the caller refuses a `where`
/// operand whose name carries more than one identity.
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
