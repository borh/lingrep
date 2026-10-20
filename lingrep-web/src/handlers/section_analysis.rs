use axum::{
    Router,
    extract::{Form, Path, State},
    response::{Html, IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;

use crate::revision::{RevisionId, WorkbenchId};
use crate::state::RequestStore;

#[derive(Deserialize)]
struct SectionSelection {
    submitted: Option<String>,
    #[serde(default)]
    section: Vec<String>,
    distance: Option<String>,
    selection_action: Option<String>,
}

pub fn routes() -> Router<RequestStore> {
    Router::new().route(
        "/section-analysis/{workbench_id}/{revision}",
        post(section_analysis),
    )
}

async fn section_analysis(
    State(store): State<RequestStore>,
    Path((workbench_id, revision)): Path<(String, u64)>,
    Form(selection): Form<SectionSelection>,
) -> Response {
    let workbench = WorkbenchId::new(&workbench_id);
    let snapshot = match store
        .coordinators
        .displayed_snapshot(&workbench, RevisionId(revision))
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => return super::inspection_response(error.into()),
    };
    let submitted = selection.submitted.as_deref() == Some("1");
    let selected = if submitted {
        match selection.selection_action.as_deref() {
            Some("imrad") => Some(crate::section_analysis::imrad_section_labels(&snapshot)),
            Some("all") => Some(crate::section_analysis::available_section_labels(&snapshot)),
            Some("none") => Some(Vec::new()),
            _ => Some(selection.section),
        }
    } else {
        None
    };
    let distance = submitted.then(|| {
        crate::section_dendrogram::DistanceMetric::from_form_value(selection.distance.as_deref())
    });
    let analysis =
        crate::section_analysis::render(&snapshot, &workbench_id, selected.as_deref(), distance)
            .map_or_else(
                || "<div id=\"section-analysis\"></div>".to_owned(),
                |fragment| fragment.to_string(),
            );
    let scope_control = crate::section_analysis::render_scope_control(
        &snapshot,
        &workbench_id,
        selected.as_deref(),
        distance,
        true,
    )
    .map_or_else(
        || "<div id=\"corpus-analysis-scope\" hx-swap-oob=\"outerHTML\"></div>".to_owned(),
        |fragment| fragment.to_string(),
    );
    Html(format!("{analysis}{scope_control}")).into_response()
}
