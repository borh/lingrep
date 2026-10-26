use askama::Template;
use std::collections::HashSet;

use crate::section_comparison::SectionComparison;
use crate::section_contrast::{ContrastSelection, SectionContrast};
use crate::section_dendrogram::{DistanceMetric, SectionDendrogram};
use crate::section_tally::{SectionAvailability, SectionSlot};
use crate::snapshot::CompletedSnapshot;
use crate::templates::{SectionAnalysis, SectionOption, SectionScopeControl, TrustedFragmentHtml};

pub(crate) const DEFAULT_SECTION_LABELS: [&str; 9] = [
    "background",
    "introduction",
    "methods",
    "data",
    "results",
    "discussion",
    "conclusion",
    "future_work",
    "literature_review",
];

fn default_labels(labels: &[String]) -> Vec<String> {
    let preferred = labels
        .iter()
        .filter(|label| {
            DEFAULT_SECTION_LABELS
                .iter()
                .any(|name| label.eq_ignore_ascii_case(name))
        })
        .cloned()
        .collect::<Vec<_>>();
    if preferred.is_empty() {
        labels.to_vec()
    } else {
        preferred
    }
}

fn is_preferred_label(label: &str) -> bool {
    DEFAULT_SECTION_LABELS
        .iter()
        .any(|name| label.eq_ignore_ascii_case(name))
}

fn available_options(
    snapshot: &CompletedSnapshot,
    selected_labels: &[String],
) -> Vec<SectionOption> {
    let SectionAvailability::Available(tally) = &snapshot.sections else {
        return Vec::new();
    };
    let mut options = tally
        .slots()
        .iter()
        .enumerate()
        .filter_map(|(slot, descriptor)| match descriptor {
            SectionSlot::Label(label) if tally.universe(slot) > 0 => Some(SectionOption {
                label: label.clone(),
                count_label: crate::templates::format_count(tally.universe(slot)),
                selected: false,
            }),
            SectionSlot::Label(_) | SectionSlot::None => None,
        })
        .collect::<Vec<_>>();
    options.sort_by(|left, right| left.label.cmp(&right.label));
    let selected = selected_labels
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    for option in &mut options {
        option.selected = selected.contains(option.label.as_str());
    }
    options
}

pub(crate) fn available_section_labels(snapshot: &CompletedSnapshot) -> Vec<String> {
    let SectionAvailability::Available(tally) = &snapshot.sections else {
        return Vec::new();
    };
    let mut labels = tally
        .slots()
        .iter()
        .enumerate()
        .filter_map(|(slot, descriptor)| match descriptor {
            SectionSlot::Label(label) if tally.universe(slot) > 0 => Some(label.clone()),
            SectionSlot::Label(_) | SectionSlot::None => None,
        })
        .collect::<Vec<_>>();
    labels.sort();
    labels
}

fn selected_section_labels(labels: Vec<String>, selected_labels: Option<&[String]>) -> Vec<String> {
    if let Some(selected) = selected_labels {
        let selected = selected.iter().map(String::as_str).collect::<HashSet<_>>();
        labels
            .into_iter()
            .filter(|label| selected.contains(label.as_str()))
            .collect()
    } else {
        default_labels(&labels)
    }
}

pub(crate) fn imrad_section_labels(snapshot: &CompletedSnapshot) -> Vec<String> {
    let labels = available_section_labels(snapshot);
    labels
        .into_iter()
        .filter(|label| is_preferred_label(label))
        .collect()
}

pub(crate) fn render_scope_control(
    snapshot: &CompletedSnapshot,
    workbench_id: &str,
    selected_labels: Option<&[String]>,
    selected_distance: Option<DistanceMetric>,
    contrast: &ContrastSelection,
    oob: bool,
) -> Option<TrustedFragmentHtml> {
    let selected = selected_section_labels(available_section_labels(snapshot), selected_labels);
    let options = available_options(snapshot, &selected);
    if options.is_empty() {
        return None;
    }
    let selected = options
        .iter()
        .filter(|option| option.selected)
        .map(|option| option.label.as_str())
        .collect::<Vec<_>>();
    let summary = match selected.as_slice() {
        [] => "None selected".to_owned(),
        [one] => (*one).to_owned(),
        [first, second] => format!("{first}, {second}"),
        [first, second, rest @ ..] => format!("{first}, {second} +{}", rest.len()),
    };
    SectionScopeControl {
        workbench_id,
        revision: snapshot.revision.0,
        options: &options,
        selected_count: selected.len(),
        total_count: options.len(),
        summary,
        summary_empty: selected.is_empty(),
        distance_value: selected_distance.unwrap_or_default().form_value(),
        contrast,
        oob,
    }
    .render()
    .ok()
    .map(TrustedFragmentHtml::from_rendered)
}

pub fn render(
    snapshot: &CompletedSnapshot,
    workbench_id: &str,
    selected_labels: Option<&[String]>,
    selected_distance: Option<DistanceMetric>,
    contrast_selection: &ContrastSelection,
) -> Option<TrustedFragmentHtml> {
    let labels = available_section_labels(snapshot);
    if labels.is_empty() {
        return None;
    }
    let selected = selected_section_labels(labels, selected_labels);
    let has_selection = !selected.is_empty();
    let comparison = has_selection
        .then(|| {
            SectionComparison::selected(&snapshot.sections, &snapshot.patterns, Some(&selected))
        })
        .flatten();
    let distance = selected_distance.unwrap_or_default();
    let dendrogram = comparison
        .as_ref()
        .and_then(|comparison| SectionDendrogram::with_distance(comparison, distance));
    let section_dendrogram = comparison.as_ref().and_then(|comparison| {
        SectionDendrogram::by_pattern_profile_with_distance(comparison, distance)
    });
    let contrast = comparison
        .as_ref()
        .and_then(|comparison| SectionContrast::new(comparison, contrast_selection));
    SectionAnalysis {
        comparison_control: render_scope_control(
            snapshot,
            workbench_id,
            selected_labels,
            selected_distance,
            contrast_selection,
            false,
        ),
        workbench_id,
        revision: snapshot.revision.0,
        selected_labels: &selected,
        has_selection,
        has_comparison: comparison.is_some(),
        distance_jensen_shannon: distance == DistanceMetric::JensenShannon,
        distance_cosine: distance == DistanceMetric::Cosine,
        comparison: comparison.as_ref(),
        contrast_selection,
        contrast: contrast.as_ref(),
        dendrogram: dendrogram.as_ref(),
        section_dendrogram: section_dendrogram.as_ref(),
    }
    .render()
    .ok()
    .map(TrustedFragmentHtml::from_rendered)
}

#[cfg(test)]
mod tests {
    use super::{default_labels, render, render_scope_control};
    use crate::corpus_runtime::CorpusIdentity;
    use crate::revision::RevisionId;
    use crate::section_contrast::{ContrastOrder, ContrastSelection};
    use crate::section_dendrogram::DistanceMetric;
    use crate::section_tally::{SectionAvailability, SpanEvent, SpanEvents, build};
    use crate::snapshot::{
        CompletedSnapshot, CompoundCategory, PatternDescriptor, PatternKey, PatternOrigin,
    };

    fn no_contrast() -> ContrastSelection {
        ContrastSelection::default()
    }

    fn snapshot_with_sections() -> CompletedSnapshot {
        let labels = ["methods", "results", "data"].map(|label| lingrep_corpus::SectionCoverage {
            label: label.into(),
            coverage: 1,
        });
        let events = SpanEvents::from_events(
            (0..3)
                .map(|slot| SpanEvent {
                    slot,
                    first: u64::from(slot),
                    end: u64::from(slot) + 1,
                })
                .collect(),
        );
        let mut builder = build(&labels, 3, events).expect("fixture vocabulary fits");
        for index in 0..3 {
            builder
                .observe(
                    u64::try_from(index).unwrap(),
                    &[PatternKey::new(index).unwrap()],
                    &[],
                )
                .unwrap();
        }
        let sections = SectionAvailability::Available(builder.finish().unwrap());
        CompletedSnapshot {
            search_scope: Default::default(),
            excluded_sentences: 0,
            revision: RevisionId(1),
            corpus_identity: CorpusIdentity::new("fixture"),
            source: std::sync::Arc::from("fixture := []."),
            derive_compounds: false,
            compound_category: CompoundCategory::Compound,
            patterns: (0..3)
                .map(|index| PatternDescriptor {
                    key: PatternKey::new(index).unwrap(),
                    name: format!("pattern-{index}"),
                    exact_total: 1,
                    metadata: Box::default(),
                    origin: PatternOrigin::Authored { chained_total: 0 },
                })
                .collect(),
            match_refs: Box::default(),
            capture_catalog: Box::default(),
            sections,
            observed_sentences: 3,
            matched_sentences: 0,
            documents: None,
            sentences_matched: 3,
            oversized_omissions: None,
        }
    }

    #[test]
    fn selector_remains_visible_for_26_labels_and_296_patterns() {
        let mut snapshot = snapshot_with_sections();
        let labels = (0..26)
            .map(|index| lingrep_corpus::SectionCoverage {
                label: format!("section-{index}"),
                coverage: 1,
            })
            .collect::<Vec<_>>();
        let events = SpanEvents::from_events(
            (0..26)
                .map(|slot| SpanEvent {
                    slot,
                    first: 0,
                    end: 1,
                })
                .collect(),
        );
        let mut tally = build(&labels, 296, events).expect("ordinary section matrix fits");
        tally
            .observe(0, &[PatternKey::new(295).unwrap()], &[])
            .unwrap();
        snapshot.sections = SectionAvailability::Available(tally.finish().unwrap());
        let scope = render_scope_control(&snapshot, "wb", None, None, &no_contrast(), true)
            .expect("section selector is visible")
            .to_string();
        assert_eq!(scope.matches("name=\"section\"").count(), 26);
        assert!(scope.contains("26 / 26"));
        assert!(scope.contains("hx-post=\"api/section-analysis/wb/1\""));
    }

    #[test]
    fn markdown_defaults_include_imrad_extensions() {
        let labels = [
            "introduction",
            "methods",
            "data",
            "results",
            "discussion",
            "conclusion",
            "future_work",
            "literature_review",
            "subtitle",
            "abstract",
            "caption",
        ]
        .map(str::to_owned);

        assert_eq!(
            default_labels(&labels),
            [
                "introduction",
                "methods",
                "data",
                "results",
                "discussion",
                "conclusion",
                "future_work",
                "literature_review",
            ]
            .map(str::to_owned)
        );
    }

    #[test]
    fn defaults_fall_back_to_all_labels_when_no_preferred_labels_exist() {
        let labels = ["subtitle", "abstract", "caption"].map(str::to_owned);
        assert_eq!(default_labels(&labels), labels);
    }

    #[test]
    fn a_two_section_contrast_renders_its_columns_in_difference_order() {
        let snapshot = snapshot_with_sections();
        let selected = ["methods", "results", "data"].map(str::to_owned);
        let contrast = ContrastSelection {
            reference: Some("methods".into()),
            comparison: Some("results".into()),
            order: ContrastOrder::DifferenceDescending,
        };
        let html = render(&snapshot, "wb", Some(&selected), None, &contrast)
            .expect("selected labels produce an analysis")
            .to_string();

        assert!(
            html.contains(
                "data-contrast-column=\"difference\" data-i18n=\"results.contrast_col_difference\""
            ),
            "{html}"
        );
        assert!(
            html.contains(
                "data-contrast-column=\"ratio\" data-i18n=\"results.contrast_col_ratio\""
            ),
            "{html}"
        );
        assert!(
            html.contains(
                "data-contrast-column=\"reference\"><span class=\"comparison-label\">methods</span>"
            ),
            "{html}"
        );
        assert!(
            html.contains("data-contrast-column=\"comparison\"><span class=\"comparison-label\">results</span>"),
            "{html}"
        );
        // Each section holds one sentence and pattern-i matched only in section
        // i, so results-only pattern-1 leads, pattern-2 has no occurrence in
        // either, and methods-only pattern-0 trails with a ratio of zero.
        let (_, table) = html
            .split_once("section-contrast\"")
            .expect("the contrast table renders");
        let position = |needle: &str| {
            table
                .find(needle)
                .unwrap_or_else(|| panic!("{needle} in {table}"))
        };
        assert!(position(">pattern-1<") < position(">pattern-2<"));
        assert!(position(">pattern-2<") < position(">pattern-0<"));
        assert!(
            table.contains("data-difference-ppm=\"1000000\">+1,000,000<"),
            "{table}"
        );
        assert!(
            table.contains("data-i18n=\"results.contrast_ratio_unavailable\""),
            "pattern-1 has a zero reference rate: {table}"
        );
        assert!(
            table.contains("data-difference-ppm=\"-1000000\">\u{2212}1,000,000<"),
            "{table}"
        );
        assert!(
            table.contains("<td class=\"comparison-cell\">0.00</td>"),
            "{table}"
        );
        assert!(
            html.contains("<option value=\"methods\" selected>methods</option>"),
            "the reference select keeps its choice: {html}"
        );
        assert!(
            html.contains("name=\"contrast_comparison\" value=\"results\""),
            "the clustering form carries the contrast: {html}"
        );
        let scope = render_scope_control(&snapshot, "wb", Some(&selected), None, &contrast, true)
            .expect("the scope control renders")
            .to_string();
        assert!(
            scope.contains("name=\"contrast_reference\" value=\"methods\""),
            "the scope form carries the contrast: {scope}"
        );
    }

    #[test]
    fn explicit_section_and_distance_choices_reach_the_rendered_analysis() {
        let snapshot = snapshot_with_sections();
        let selected = ["results".to_owned(), "data".to_owned()];
        let html = render(
            &snapshot,
            "wb",
            Some(&selected),
            Some(DistanceMetric::Cosine),
            &no_contrast(),
        )
        .expect("selected labels produce an analysis")
        .to_string();

        assert!(html.contains("name=\"section\" value=\"data\""), "{html}");
        assert!(
            html.contains("name=\"section\" value=\"results\""),
            "{html}"
        );
        assert!(
            !html.contains("type=\"hidden\" name=\"section\" value=\"methods\""),
            "the clustering request carries only selected sections: {html}"
        );
        let scope = render_scope_control(
            &snapshot,
            "wb",
            Some(&selected),
            Some(DistanceMetric::Cosine),
            &no_contrast(),
            true,
        )
        .expect("selected labels produce the scope control")
        .to_string();
        assert!(
            !scope.contains("hx-preserve"),
            "OOB updates must replace the selector's selected state: {scope}"
        );
        assert!(scope.contains("hx-swap-oob=\"outerHTML\""), "{scope}");
        assert!(scope.contains("value=\"data\" checked"), "{scope}");
        assert!(scope.contains("value=\"results\" checked"), "{scope}");
        assert!(scope.contains("2 / 3"), "{scope}");
        assert!(scope.contains("value=\"imrad\""), "{scope}");
        assert!(
            scope.contains("name=\"distance\" value=\"cosine\""),
            "{scope}"
        );
        assert!(scope.contains("hx-target=\"#section-analysis\""), "{scope}");
        assert!(
            html.contains("value=\"cosine\" selected"),
            "the requested distance remains selected: {html}"
        );
        assert!(
            html.contains("data-i18n=\"results.cluster_cosine_distance\""),
            "the clustering caption reflects cosine: {html}"
        );
        assert!(html.contains(">data</span>"), "{html}");
        assert!(html.contains(">results</span>"), "{html}");
        assert!(
            !html.contains("<span class=\"comparison-label\">methods</span>"),
            "the excluded section is absent from the comparison columns: {html}"
        );
    }
}
