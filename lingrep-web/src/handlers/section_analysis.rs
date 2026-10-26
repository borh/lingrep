use axum::{
    Router,
    extract::{Path, State},
    response::{Html, IntoResponse, Response},
    routing::post,
};

use crate::revision::{RevisionId, WorkbenchId};
use crate::state::RequestStore;

#[derive(Debug, Default, PartialEq, Eq)]
struct SectionSelection {
    submitted: Option<String>,
    section: Vec<String>,
    distance: Option<String>,
    selection_action: Option<String>,
    contrast_reference: Option<String>,
    contrast_comparison: Option<String>,
    contrast_order: Option<String>,
}

impl SectionSelection {
    /// Reads the `application/x-www-form-urlencoded` body as the pair list it
    /// is. The scope form repeats `section` once per checked label, which a
    /// struct deserialization of the body rejects.
    fn parse(body: &str) -> Result<Self, serde_urlencoded::de::Error> {
        let pairs = serde_urlencoded::from_str::<Vec<(String, String)>>(body)?;
        let mut selection = Self::default();
        for (key, value) in pairs {
            match key.as_str() {
                "submitted" => selection.submitted = Some(value),
                "section" => selection.section.push(value),
                "distance" => selection.distance = Some(value),
                "selection_action" => selection.selection_action = Some(value),
                "contrast_reference" => selection.contrast_reference = Some(value),
                "contrast_comparison" => selection.contrast_comparison = Some(value),
                "contrast_order" => selection.contrast_order = Some(value),
                _ => {}
            }
        }
        Ok(selection)
    }
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
    body: String,
) -> Response {
    let Ok(selection) = SectionSelection::parse(&body) else {
        return axum::http::StatusCode::BAD_REQUEST.into_response();
    };
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
    let contrast = crate::section_contrast::ContrastSelection::from_form(
        selection.contrast_reference,
        selection.contrast_comparison,
        selection.contrast_order.as_deref(),
    );
    let analysis = crate::section_analysis::render(
        &snapshot,
        &workbench_id,
        selected.as_deref(),
        distance,
        &contrast,
    )
    .map_or_else(
        || "<div id=\"section-analysis\"></div>".to_owned(),
        |fragment| fragment.to_string(),
    );
    Html(analysis).into_response()
}

#[cfg(test)]
mod tests {
    use super::SectionSelection;

    #[test]
    fn a_repeated_section_field_is_read_as_the_checked_labels() {
        let selection = SectionSelection::parse(
            "submitted=1&section=methods&section=results&distance=cosine&contrast_reference=methods&contrast_comparison=results&contrast_order=name-asc",
        )
        .expect("a browser-encoded form body parses");
        assert_eq!(
            selection,
            SectionSelection {
                submitted: Some("1".into()),
                section: vec!["methods".into(), "results".into()],
                distance: Some("cosine".into()),
                selection_action: None,
                contrast_reference: Some("methods".into()),
                contrast_comparison: Some("results".into()),
                contrast_order: Some("name-asc".into()),
            }
        );
    }

    #[test]
    fn a_cleared_scope_sends_no_section_field_and_decodes_percent_escapes() {
        let selection = SectionSelection::parse(
            "submitted=1&selection_action=none&distance=jensen-shannon&section=%E6%96%B9%E6%B3%95",
        )
        .expect("parses");
        assert_eq!(selection.selection_action.as_deref(), Some("none"));
        assert_eq!(selection.section, ["方法"]);
    }
}
