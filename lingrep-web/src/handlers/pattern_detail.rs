use askama::Template;
use axum::{
    Router,
    extract::{Path, State},
    response::Response,
    routing::get,
};

use crate::inspection::{InspectionError, MatchInspection, TokenRole, WHOLE_SENTENCE_CONTEXT};
use crate::pattern_detail::{PatternDetailProjection, RetainedHitText};
use crate::provenance_view::HitProvenance;
use crate::provenance_view::section_labels;
use crate::revision::{RevisionId, WorkbenchId};
use crate::snapshot::{CompletedSnapshot, PatternDescriptor, PatternKey};
use crate::state::RequestStore;
use crate::templates::{
    CaptureInspectionFragment, GraphBindingAttributeData, GraphBindingData, HitDetailFragment,
    KwicCaptureData, KwicCaptureLaneData, KwicTokenData, PatternDefinitionView,
    PatternDetailFragment, format_count,
};
use lingrep_core::corpus::SentenceId;
use lingrep_core::fsm::GraphBinding;

use super::EndpointInspectionError;

pub fn routes() -> Router<RequestStore> {
    Router::new()
        .merge(super::reading::routes())
        .route(
            "/pattern-detail/{workbench_id}/{revision}/{pattern_key}",
            get(pattern_detail),
        )
        .route(
            "/hit-detail/{workbench_id}/{revision}/{pattern_key}/{reference_ordinal}",
            get(hit_detail),
        )
}

async fn hit_detail(
    State(store): State<RequestStore>,
    Path((workbench_id, revision, pattern_key, reference_ordinal)): Path<(
        String,
        u64,
        usize,
        usize,
    )>,
) -> Response {
    let workbench = WorkbenchId::new(&workbench_id);
    let (snapshot, inspector, ready, runtime) =
        match super::bound_inspector(&store, &workbench, RevisionId(revision)).await {
            Ok(bound) => bound,
            Err(error) => return super::inspection_response(error),
        };
    let provenance = super::BoundProvenance::open_or_degrade(&ready).await;
    let Some(key) = PatternKey::new(pattern_key) else {
        return super::inspection_response(EndpointInspectionError::UnknownPattern);
    };
    let Some(reference) = snapshot.match_ref(key, reference_ordinal) else {
        return super::inspection_response(EndpointInspectionError::UnknownReference);
    };
    let refs = vec![reference];
    let sentences = match super::decode_refs(&runtime, &inspector, &refs).await {
        Ok(values) => values,
        Err(response) => return response,
    };
    let Some(Some(tokens)) = sentences.get(&reference.sentence_index) else {
        return super::invalid_provenance_response();
    };
    let view = match inspector.project_reference(tokens, reference, WHOLE_SENTENCE_CONTEXT) {
        Ok(view) => view,
        Err(InspectionError::InvalidSpan) => return super::invalid_provenance_response(),
        Err(_) => {
            // The workbench's own corpus, not the store's default.
            runtime.invalidator().invalidate_corrupt().await;
            return super::inspection_response(EndpointInspectionError::CacheCorruption);
        }
    };
    let provenance_view = super::read_provenance(
        provenance.as_ref(),
        &[SentenceId::new(reference.sentence_index)],
    )
    .await
    .into_iter()
    .next()
    .flatten()
    .map(|sentence| HitProvenance::from_sentence(&sentence));
    let overlaps = match super::overlaps::inspect(
        &store,
        std::sync::Arc::clone(&snapshot),
        key,
        &refs,
        sentences,
    )
    .await
    {
        Ok(mut overlaps) => overlaps.pop().unwrap_or_default(),
        Err(response) => return response,
    };
    render_hit_detail(
        &workbench_id,
        revision,
        pattern_key,
        reference_ordinal,
        &view,
        &reference.graph_bindings,
        provenance_view,
        overlaps,
    )
}

fn render_hit_detail(
    workbench_id: &str,
    revision: u64,
    pattern_key: usize,
    reference_ordinal: usize,
    inspection: &MatchInspection,
    graph_bindings: &[GraphBinding],
    provenance: Option<HitProvenance>,
    overlaps: Vec<crate::inspection::OverlapCandidate>,
) -> Response {
    let captures = inspection
        .captures
        .iter()
        .map(|capture| KwicCaptureData {
            ordinal: capture.ordinal.index(),
            name: capture.name.clone(),
            text: capture.text.clone(),
            start: capture.span.start,
            end: capture.span.end,
            grid_start: capture
                .span
                .start
                .saturating_sub(inspection.visible_range.start)
                + 1,
            grid_span: capture.span.end.saturating_sub(capture.span.start).max(1),
            detail_html: crate::templates::TrustedFragmentHtml::from_rendered(
                CaptureInspectionFragment { capture }
                    .render()
                    .unwrap_or_default(),
            ),
        })
        .collect::<Vec<_>>();
    let capture_lanes = inspection
        .capture_lanes
        .iter()
        .map(|lane| KwicCaptureLaneData {
            captures: lane
                .captures
                .iter()
                .filter_map(|ordinal| {
                    captures
                        .iter()
                        .find(|capture| capture.ordinal == ordinal.index())
                })
                .cloned()
                .collect(),
        })
        .collect();
    let fragment = HitDetailFragment {
        overlaps,
        reference_ordinal,
        workbench_id,
        revision,
        pattern_key,
        id_prefix: format!("hit-capture-{pattern_key}-{reference_ordinal}"),
        tokens: inspection.tokens.iter().map(token_data).collect(),
        capture_lanes,
        captures,
        graph_bindings: graph_bindings.iter().map(graph_binding_data).collect(),
        provenance,
    };
    super::render_or_500(fragment)
}

fn graph_binding_data(binding: &GraphBinding) -> GraphBindingData {
    match binding {
        GraphBinding::Node {
            name,
            node,
            attributes,
        } => GraphBindingData {
            name: name.to_string(),
            is_node: true,
            identity: node.spelling(),
            relation: None,
            attributes: attributes
                .iter()
                .map(|attribute| GraphBindingAttributeData {
                    name: attribute.name.as_str().to_owned(),
                    value: attribute.value.to_string(),
                })
                .collect(),
        },
        GraphBinding::Edge {
            name,
            head,
            dependent,
            relation,
        } => GraphBindingData {
            name: name.to_string(),
            is_node: false,
            identity: format!("{} → {}", head.spelling(), dependent.spelling()),
            relation: Some(relation.to_string()),
            attributes: Vec::new(),
        },
    }
}

fn token_data(token: &crate::inspection::InspectedToken) -> KwicTokenData {
    KwicTokenData {
        index: token.sentence_index,
        is_match: token.role == TokenRole::Match,
        orth: token.summary.orth.as_deref().unwrap_or_default().to_owned(),
        lemma: token
            .summary
            .lemma
            .as_deref()
            .unwrap_or_default()
            .to_owned(),
        pos: token.summary.pos.as_deref().unwrap_or_default().to_owned(),
        cform: token
            .summary
            .cform
            .as_deref()
            .unwrap_or_default()
            .to_owned(),
    }
}

async fn pattern_detail(
    State(store): State<RequestStore>,
    Path((workbench_id, revision, pattern_key)): Path<(String, u64, usize)>,
) -> Response {
    let workbench = WorkbenchId::new(&workbench_id);
    let (snapshot, inspector, ready, runtime) =
        match super::bound_inspector(&store, &workbench, RevisionId(revision)).await {
            Ok(bound) => bound,
            Err(error) => return super::inspection_response(error),
        };
    let provenance = super::BoundProvenance::open_or_degrade(&ready).await;
    let Some(key) = PatternKey::new(pattern_key) else {
        return super::inspection_response(EndpointInspectionError::UnknownPattern);
    };
    let Some(pattern) = snapshot.pattern(key) else {
        return super::inspection_response(EndpointInspectionError::UnknownPattern);
    };
    let refs = snapshot
        .match_refs
        .iter()
        .filter(|reference| reference.pattern_key == key)
        .collect::<Vec<_>>();
    let sentences = match super::decode_refs(&runtime, &inspector, &refs).await {
        Ok(values) => values,
        Err(response) => return response,
    };
    let sentence_ids = refs
        .iter()
        .map(|reference| SentenceId::new(reference.sentence_index))
        .collect::<Vec<_>>();
    let provenance_page = super::read_provenance(provenance.as_ref(), &sentence_ids).await;
    let labels_by_ref = provenance_page
        .into_iter()
        .map(|entry| {
            entry.map_or_else(
                || Box::<[String]>::from([]),
                |sentence| section_labels(&sentence.active_regions),
            )
        })
        .collect::<Vec<_>>();
    let mut hits = Vec::with_capacity(refs.len());
    for (reference_ordinal, reference) in refs.iter().enumerate() {
        let Some(Some(tokens)) = sentences.get(&reference.sentence_index) else {
            return super::invalid_provenance_response();
        };
        let view = match inspector.project_reference(tokens, reference, WHOLE_SENTENCE_CONTEXT) {
            Ok(view) => view,
            Err(InspectionError::InvalidSpan) => return super::invalid_provenance_response(),
            Err(_) => {
                // The workbench's own corpus, not the store's default.
                runtime.invalidator().invalidate_corrupt().await;
                return super::inspection_response(EndpointInspectionError::CacheCorruption);
            }
        };
        hits.push(retained_hit_text(
            reference_ordinal,
            &view,
            labels_by_ref[reference_ordinal].clone(),
            reference.in_chain,
        ));
    }
    let mut projection = PatternDetailProjection::new(hits, &snapshot.sections, key);
    let overlaps = match super::overlaps::inspect(
        &store,
        std::sync::Arc::clone(&snapshot),
        key,
        &refs,
        sentences,
    )
    .await
    {
        Ok(overlaps) => overlaps,
        Err(response) => return response,
    };
    let mut names = std::collections::BTreeSet::new();
    for (hit, overlaps) in projection.hits.iter_mut().zip(overlaps) {
        hit.has_overlaps = !overlaps.is_empty();
        projection.overlap_hits += usize::from(hit.has_overlaps);
        names.extend(overlaps.into_iter().map(|candidate| candidate.name));
    }
    projection.overlap_names = names.into_iter().collect();
    let fragment = PatternDetailFragment {
        pattern_name: &pattern.name,
        definition: definition_view(&snapshot, pattern),
        chains: crate::compound_chains::chain_panel(&snapshot, key),
        chained: crate::compound_chains::chain_share(&snapshot, key),
        documents: crate::document_reach::document_share(&snapshot, key),
        exact_total: pattern.exact_total,
        exact_total_label: format_count(pattern.exact_total),
        retained_count: refs.len(),
        projection: &projection,
        workbench_id: &workbench_id,
        revision,
        pattern_key,
    };
    super::render_or_500(fragment)
}

/// Resolve the selected pattern against the catalog *this revision ran*.
///
/// `CompletedSnapshot` keeps that source, which is what makes this honest: the
/// definition shown is the one the counts and hits beside it came from, not
/// whatever the editor holds now. An expansion failure yields no band at all —
/// the run compiled, so this can only fail on a defect here, and a reader of
/// finished results has nothing to do with the message either way.
fn definition_view(
    snapshot: &CompletedSnapshot,
    pattern: &PatternDescriptor,
) -> Option<PatternDefinitionView> {
    let expansion = match pattern.components() {
        None => crate::patterns::expand_web_pattern(&snapshot.source, &pattern.name),
        Some(components) => {
            let names = components
                .iter()
                .map(|key| {
                    snapshot
                        .pattern(*key)
                        .map(|component| component.name.as_str())
                })
                .collect::<Option<Vec<_>>>()?;
            crate::patterns::expand_compound_pattern(&snapshot.source, &pattern.name, &names)
        }
    }
    .ok()??;
    Some(PatternDefinitionView {
        declaration: expansion.declaration,
        expanded: expansion.expanded,
        references: expansion.references.into_vec(),
        diagram: expansion
            .diagram
            .map(crate::templates::TrustedFragmentHtml::from_rendered),
    })
}

fn retained_hit_text(
    reference_ordinal: usize,
    inspection: &MatchInspection,
    section_labels: Box<[String]>,
    in_chain: bool,
) -> RetainedHitText {
    let (left, key, right) = crate::inspection::kwic_orth_split(inspection);
    RetainedHitText {
        reference_ordinal,
        left,
        key,
        right,
        section_labels,
        in_chain,
    }
}
