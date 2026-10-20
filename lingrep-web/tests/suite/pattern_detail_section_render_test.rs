// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
use askama::Template;
use lingrep_web::{
    pattern_detail::{PatternDetailProjection, RetainedHitText},
    section_tally::{SectionAvailability, SectionRefusal, SpanEvent, SpanEvents, build},
    templates::PatternDetailFragment,
};
use proptest::prelude::*;

use crate::render_security_support::{
    HTML_SECURITY_DELIMITERS, expected_html_text, safe_affix, security_payload,
};

fn hit(ordinal: usize, key: &str, sections: &[&str]) -> RetainedHitText {
    RetainedHitText {
        reference_ordinal: ordinal,
        left: "私は".into(),
        key: key.into(),
        right: "。".into(),
        section_labels: sections.iter().map(|s| (*s).to_string()).collect(),
    }
}

/// Renders against a tally that agrees with the retained sample, which is what
/// every test written before exact counting asserts about.
fn render(hits: Vec<RetainedHitText>) -> String {
    let sections = lingrep_web::pattern_detail::sample_sections(&hits);
    render_with(hits, &sections)
}

fn render_with(hits: Vec<RetainedHitText>, sections: &SectionAvailability) -> String {
    let retained_count = hits.len();
    let projection = PatternDetailProjection::new(
        hits,
        sections,
        lingrep_web::snapshot::PatternKey::new(0).unwrap(),
    );
    PatternDetailFragment {
        definition: None,
        pattern_name: "sections",
        exact_total: retained_count as u64,
        exact_total_label: format!("{retained_count}"),
        retained_count,
        projection: &projection,
        workbench_id: "wb",
        revision: 1,
        pattern_key: 0,
    }
    .render()
    .unwrap()
}

/// A tally built from a corpus vocabulary rather than from the retained hits.
///
/// Every other case in this file goes through `sample_sections`, which derives
/// the tally FROM the hits — so the two agree by construction and the states
/// where they disagree are unreachable. Those are exactly the states that made
/// the panel read as broken (`vibrato-pipe-mlo`), so they need a tally the
/// hits did not produce.
///
/// `spans` are `(slot, sentence_count)` in slot order; sentences outside every
/// span land in the trailing `None` slot.
fn corpus_tally(labels: &[&str], spans: &[(u32, u64)], sentences: u64) -> SectionAvailability {
    let coverage = labels
        .iter()
        .map(|label| lingrep_corpus::SectionCoverage {
            label: (*label).to_string(),
            coverage: 1,
        })
        .collect::<Vec<_>>();
    let mut events = Vec::new();
    let mut cursor = 0u64;
    for (slot, count) in spans {
        events.push(SpanEvent {
            slot: *slot,
            first: cursor,
            end: cursor + count,
        });
        cursor += count;
    }
    let mut builder =
        build(&coverage, 1, SpanEvents::from_events(events)).expect("the fixture vocabulary fits");
    for ordinal in 0..sentences {
        builder
            .observe(
                ordinal,
                &[lingrep_web::snapshot::PatternKey::new(0).unwrap()],
                &[],
            )
            .unwrap();
    }
    SectionAvailability::Available(builder.finish().expect("the fixture stream ends cleanly"))
}

/// A tally whose surface vocabulary comes from the corpus rather than from the
/// retained hits, so a surface can carry a positive corpus count and no
/// retained row — the divergence `vibrato-pipe-5f4` reports.
fn surface_corpus_tally() -> SectionAvailability {
    let mut builder = build(
        &[lingrep_corpus::SectionCoverage {
            label: "methods".to_string(),
            coverage: 4,
        }],
        1,
        SpanEvents::from_events(vec![SpanEvent {
            slot: 0,
            first: 0,
            end: 4,
        }]),
    )
    .expect("one label fits");
    let pattern = lingrep_web::snapshot::PatternKey::new(0).unwrap();
    // Three corpus sentences matched as `α`, one as `β`; only `β` is retained.
    for (ordinal, surface) in [(0u64, "α"), (1, "α"), (2, "α"), (3, "β")] {
        builder
            .observe(ordinal, &[pattern], &[(pattern, surface.to_string())])
            .unwrap();
    }
    SectionAvailability::Available(builder.finish().expect("the fixture stream ends cleanly"))
}

/// The `data-variation` of each rendered concordance row, in document order.
fn kwic_row_variations(html: &str) -> Vec<String> {
    html.split("<tr class=\"kwic-row\"")
        .skip(1)
        .filter_map(|row| {
            let start = row.find("data-variation=\"")? + "data-variation=\"".len();
            let rest = &row[start..];
            Some(rest[..rest.find('"')?].to_string())
        })
        .collect()
}

/// The variation list is corpus-exact and the concordance is the bounded
/// retained sample, so a surface can have a positive corpus count and no
/// retained row. Pressing it empties the table, and the pane must state which
/// population each figure describes rather than leaving a blank grid
/// (`vibrato-pipe-5f4`).
#[test]
fn a_surface_with_no_retained_row_states_why_the_table_is_empty() {
    let sections = surface_corpus_tally();
    let html = render_with(vec![hit(0, "β", &["methods"])], &sections);
    // The reason ships hidden and is unhidden by the client on that selection.
    assert!(html.contains("data-kwic-filter-empty"), "{html}");
    assert!(
        html.contains("data-kwic-filter-empty hidden"),
        "the note qualifies a selection and must not show unfiltered: {html}"
    );
    assert!(
        html.contains("No retained hits carry this surface"),
        "the note states why the table is empty: {html}"
    );
    // The corpus-exact count survives beside the zero-row surface.
    assert!(
        html.contains("data-search-value=\"α\"") && html.contains("data-count=\"3\""),
        "the unretained surface keeps its exact corpus count: {html}"
    );
    // The retained table holds only the surface the sample kept.
    assert_eq!(
        kwic_row_variations(&html),
        vec![String::from("1")],
        "only β's retained row is in the concordance: {html}"
    );
    assert!(
        html.contains("data-variation=\"0\""),
        "the unretained surface still carries its filter button: {html}"
    );
}

/// The panel now narrows to the selected surface, and the note that says which
/// population it is describing ships hidden; `applyRowFilters` unhides it while
/// a surface filter is pressed (`vibrato-pipe-9wp`).
#[test]
fn the_section_panel_carries_a_scope_note_that_starts_hidden() {
    let html = render(vec![hit(0, "α", &["methods"]), hit(1, "β", &["methods"])]);
    assert!(html.contains("data-section-scope-note"), "{html}");
    assert!(
        html.contains("data-section-scope-note hidden"),
        "the note qualifies a filtered view and must not show unfiltered: {html}"
    );
    assert!(
        html.contains("These counts and rates are for the selected surface"),
        "{html}"
    );
    assert!(
        !html.contains("The surface filter does not narrow them"),
        "the refusal note belongs to the refused branch alone: {html}"
    );
}

/// The counts the client narrows with. Positionally aligned with the rendered
/// rows, so a row's index into the JSON is its index in the list — which is
/// what lets the client re-render without matching labels back up.
#[test]
fn the_panel_ships_the_surface_cross_tabulation_it_narrows_with() {
    let html = render(vec![
        hit(0, "α", &["methods"]),
        hit(1, "β", &["methods"]),
        hit(2, "α", &[]),
    ]);
    assert!(html.contains("data-surface-sections"), "{html}");
    let blob = html
        .split("data-surface-sections>")
        .nth(1)
        .and_then(|rest| rest.split('<').next())
        .expect("the blob is rendered");
    // Two variations, and each row is one cell per rendered section plus the
    // unlabelled row. `methods` holds two sentences and one is unlabelled.
    assert!(blob.starts_with('{') && blob.ends_with('}'), "{blob}");
    assert!(blob.contains("\"0\":["), "{blob}");
    assert!(blob.contains("\"1\":["), "{blob}");
    assert!(
        html.contains("data-full-count="),
        "the invariant count survives a filter: {html}"
    );
}

/// The rate is the panel's measure, so it is drawn and stated; the count stays
/// beside it as the evidence.
#[test]
fn every_section_row_publishes_its_rate_and_its_denominator() {
    // `methods` covers 4 of 10 sentences and the pattern matched in 2 of them.
    let sections = corpus_tally(&["methods"], &[(0, 4)], 10);
    let html = render_with(vec![hit(0, "α", &["methods"])], &sections);
    assert!(html.contains("data-universe=\"4\""), "{html}");
    assert!(html.contains("data-rate-tenths="), "{html}");
    assert!(html.contains("ppm</span>"), "{html}");
    assert!(
        html.contains("Ordered by occurrences per million sentences"),
        "the panel says what it ranks on: {html}"
    );
    // Every sentence under `methods` matched, so the rate is the ceiling.
    assert!(html.contains("1000000.0 ppm"), "{html}");
    // The unlabelled slot holds the other six sentences, all matching.
    assert!(html.contains("data-universe=\"6\""), "{html}");
}

#[test]
fn a_corpus_recording_no_section_labels_says_so_rather_than_rendering_nothing() {
    let sections = corpus_tally(&[], &[], 3);
    let html = render_with(
        vec![hit(0, "\u{3b1}", &[]), hit(1, "\u{3b2}", &[])],
        &sections,
    );
    assert!(html.contains("data-section-empty"), "{html}");
    assert!(
        html.contains("This corpus records no section labels"),
        "{html}"
    );
    assert!(
        !html.contains("matched in none of the"),
        "the corpus has no labels, so the per-pattern wording is wrong here: {html}"
    );
}

#[test]
fn a_pattern_under_no_labelled_section_names_the_corpus_labels_it_missed() {
    // Two labels in the corpus; every observed sentence falls outside both, so
    // the pattern's own list is empty while the corpus vocabulary is not.
    let sections = corpus_tally(&["methods", "results"], &[], 3);
    let html = render_with(vec![hit(0, "\u{3b1}", &[])], &sections);
    assert!(html.contains("data-section-empty"), "{html}");
    assert!(html.contains("matched in none of the 2 sections"), "{html}");
    assert!(
        !html.contains("This corpus records no section labels"),
        "the corpus does record labels: {html}"
    );
}

#[test]
fn renders_the_section_block_with_labels_counts_and_membership() {
    let html = render(vec![
        hit(0, "α", &["methods", "results"]),
        hit(1, "β", &["methods"]),
        hit(2, "γ", &[]),
    ]);
    assert!(html.contains("class=\"detail-view section-distribution\""));
    assert!(html.contains("Section distribution — corpus"));
    // section labels sit in the dedicated hook
    assert!(html.contains("class=\"section-label\">methods<"));
    assert!(html.contains("class=\"section-label\">results<"));
    // a "(no section)" button appears because one hit has no section
    assert!(html.contains("data-section=\"none\""));
    assert!(html.contains("class=\"section-label\" data-i18n=\"detail.no_section\">(no section)<"));
    // rows carry their membership as integer ids (methods is index 0 by count)
    assert!(
        html.contains("data-section-ids=\"0 1 \"") || html.contains("data-section-ids=\"1 0 \"")
    );
}

#[test]
fn states_why_the_section_block_is_empty_when_no_hit_has_a_section() {
    let html = render(vec![hit(0, "α", &[]), hit(1, "β", &[])]);
    // The block is not omitted: an absent panel and a corpus without labels
    // are indistinguishable on screen, which is the defect this states away.
    assert!(html.contains("data-section-empty"), "{html}");
    assert!(
        html.contains("This corpus records no section labels"),
        "{html}"
    );
    // ...but it carries no ranked list, and no "(no section)" noise.
    assert!(!html.contains("data-ranked-list=\"section\""), "{html}");
    assert!(!html.contains("data-section=\"none\""));
    // the initial status stays section-less
    assert!(html.contains("All surfaces"));
    assert!(!html.contains("all sections"));
}

#[test]
fn initial_status_names_both_scopes_when_sections_exist() {
    let html = render(vec![hit(0, "α", &["methods"])]);
    assert!(!html.contains("data-variation=\"all\""));
    assert!(!html.contains("data-section=\"all\""));
    assert!(html.contains("class=\"kwic-filter-bar\""));
    assert!(html.contains("data-clear-surface-filter"));
    assert!(html.contains("data-clear-section-filter"));
    assert!(html.contains("data-clear-kwic-search"));
    assert!(html.contains("data-kwic-search"));
    assert!(html.contains("class=\"kwic-filter-status\" role=\"status\" aria-live=\"polite\""));
    assert!(html.contains("All surfaces · All sections"));
    let status_start = html.find("class=\"kwic-filter-status\"").unwrap();
    let status_end = status_start + html[status_start..].find("</p>").unwrap() + "</p>".len();
    let clear_surface = html.find("data-clear-surface-filter").unwrap();
    let clear_section = html.find("data-clear-section-filter").unwrap();
    assert!(clear_surface > status_end);
    assert!(clear_section > status_end);
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn section_labels_escape_every_html_delimiter_at_every_render_site(
        prefix in safe_affix(),
        suffix in safe_affix(),
    ) {
        for delimiter in HTML_SECURITY_DELIMITERS {
            let payload = security_payload(&prefix, delimiter, &suffix);
            let expected = expected_html_text(&payload);
            let html = render(vec![hit(0, "α", &[&payload])]);

            prop_assert!(
                !html.contains(&payload),
                "raw section label survived for {delimiter:?}",
            );
            prop_assert_eq!(
                html.matches(&expected).count(),
                5,
                "section label must be escaped in the distribution value and label, row title, row label, and compact metadata",
            );
            for site in [
                format!("data-search-value=\"{expected}\""),
                format!("class=\"section-label\">{expected}</span>"),
                format!("title=\"{expected}\""),
                format!("class=\"kwic-section-label\">{expected}</span>"),
                format!("data-section-meta><span>{expected}</span>"),
            ] {
                prop_assert!(
                    html.contains(&site),
                    "escaped section label missing from render site {site}",
                );
            }
        }
    }
}

#[test]
fn an_unavailable_section_projection_says_why() {
    let html = render_with(
        vec![hit(0, "α", &["methods"])],
        &SectionAvailability::VocabularyTooLarge(SectionRefusal {
            labels: 1_200,
            patterns: 40,
        }),
    );
    assert!(html.contains("data-section-unavailable"));
    assert!(
        html.contains("1200") && html.contains("40"),
        "the reason names the vocabulary and the catalog that produced it"
    );
    assert!(
        !html.contains("class=\"section-overview\""),
        "no empty list is rendered in place of the refused one"
    );
    assert!(
        !html.contains("data-section=\"none\""),
        "and no (no section) bucket stands in for it either"
    );
}

#[test]
fn a_section_row_prints_the_exact_count_not_the_retained_one() {
    // One retained hit; 12,431 corpus sentences under `methods`. The divergence
    // this feature creates, in the one place it is visible.
    let mut builder = lingrep_web::section_tally::build(
        &[lingrep_corpus::SectionCoverage {
            label: "methods".to_string(),
            coverage: 12_431,
        }],
        1,
        SpanEvents::from_events(vec![SpanEvent {
            slot: 0,
            first: 0,
            end: 12_431,
        }]),
    )
    .expect("one label fits");
    for ordinal in 0..12_431 {
        builder
            .observe(
                ordinal,
                &[lingrep_web::snapshot::PatternKey::new(0).unwrap()],
                &[],
            )
            .unwrap();
    }
    let html = render_with(
        vec![hit(0, "α", &["methods"])],
        &SectionAvailability::Available(builder.finish().unwrap()),
    );
    assert!(
        html.contains("data-count=\"12431\""),
        "the row carries the corpus-exact count"
    );
    assert!(
        html.contains(">12431</span>"),
        "and prints it beside the bar"
    );
    // The KWIC table still holds the one retained row the filter can select.
    assert_eq!(html.matches("<tr class=\"kwic-row\"").count(), 1);
}
