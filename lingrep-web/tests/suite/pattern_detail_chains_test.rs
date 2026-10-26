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
//! The Chains tab: compound items containing the selected base pattern.

use askama::Template;
use lingrep_web::compound_chains::{chain_panel, chain_share};
use lingrep_web::corpus_runtime::CorpusIdentity;
use lingrep_web::pattern_detail::{PatternDetailProjection, RetainedHitText};
use lingrep_web::revision::RevisionId;
use lingrep_web::section_tally::{SectionAvailability, SectionRefusal};
use lingrep_web::snapshot::{
    CompletedSnapshot, CompoundCategory, PatternDescriptor, PatternKey, PatternOrigin,
};
use lingrep_web::templates::PatternDetailFragment;

fn key(index: usize) -> PatternKey {
    PatternKey::new(index).unwrap()
}

fn authored(index: usize, name: &str, total: u64, chained_total: u64) -> PatternDescriptor {
    PatternDescriptor {
        key: key(index),
        name: name.into(),
        exact_total: total,
        metadata: Box::default(),
        origin: PatternOrigin::Authored { chained_total },
    }
}

fn compound(index: usize, name: &str, components: &[usize], total: u64) -> PatternDescriptor {
    PatternDescriptor {
        key: key(index),
        name: name.into(),
        exact_total: total,
        metadata: Box::default(),
        origin: PatternOrigin::Compound {
            components: components.iter().map(|index| key(*index)).collect(),
            coincident: Box::default(),
        },
    }
}

/// A B C adjacent derives A+B, B+C and A+B+C.
fn snapshot(derive_compounds: bool) -> CompletedSnapshot {
    CompletedSnapshot {
        search_scope: Default::default(),
        excluded_sentences: 0,
        revision: RevisionId(1),
        corpus_identity: CorpusIdentity::new(""),
        source: "".into(),
        derive_compounds,
        compound_category: CompoundCategory::Compound,
        patterns: vec![
            authored(0, "A", 10, 4),
            authored(1, "B", 20, 5),
            authored(2, "C", 5, 0),
            compound(3, "複合項目〔A ＋ B〕", &[0, 1], 4),
            compound(4, "複合項目〔B ＋ C〕", &[1, 2], 1_200),
            compound(5, "複合項目〔A ＋ B ＋ C〕", &[0, 1, 2], 3),
        ]
        .into_boxed_slice(),
        match_refs: Box::default(),
        capture_catalog: Box::default(),
        sections: SectionAvailability::VocabularyTooLarge(SectionRefusal {
            labels: 0,
            patterns: 0,
        }),
        observed_sentences: 0,
        matched_sentences: 0,
        documents: Some(lingrep_web::snapshot::DocumentSummary {
            documents_scanned: 61,
            by_pattern: (0..6)
                .map(|index| lingrep_web::snapshot::DocumentReach {
                    documents: if index == 1 { 12 } else { 0 },
                    top_counts: if index == 1 {
                        Box::from([5, 4])
                    } else {
                        Box::default()
                    },
                })
                .collect(),
        }),
        sentences_matched: 0,
        oversized_omissions: None,
    }
}

fn render(snapshot: &CompletedSnapshot, pattern: usize) -> String {
    let hits = [false, true]
        .into_iter()
        .enumerate()
        .map(|(ordinal, in_chain)| RetainedHitText {
            reference_ordinal: ordinal,
            left: "左".into(),
            key: "鍵".into(),
            right: "右".into(),
            section_labels: Box::from([]),
            in_chain,
        })
        .collect::<Vec<_>>();
    let sections = lingrep_web::pattern_detail::sample_sections(&hits);
    let projection = PatternDetailProjection::new(hits, &sections, key(pattern));
    let descriptor = snapshot.pattern(key(pattern)).unwrap();
    PatternDetailFragment {
        definition: None,
        chains: chain_panel(snapshot, key(pattern)),
        chained: chain_share(snapshot, key(pattern)),
        documents: lingrep_web::document_reach::document_share(snapshot, key(pattern)),
        pattern_name: &descriptor.name,
        exact_total_label: descriptor.exact_total.to_string(),
        exact_total: descriptor.exact_total,
        retained_count: 2,
        projection: &projection,
        workbench_id: "wb",
        revision: 1,
        pattern_key: pattern,
    }
    .render()
    .unwrap()
}

#[test]
fn the_middle_pattern_lists_its_neighbours_and_the_longer_chain_with_its_position() {
    let html = render(&snapshot(true), 1);

    assert!(html.contains("data-detail-view=\"chains\""), "{html}");
    let before = html.find("id=\"chains-before-heading\"").unwrap();
    let after = html.find("id=\"chains-after-heading\"").unwrap();
    let longer = html.find("id=\"chains-longer-heading\"").unwrap();
    assert!(before < after && after < longer, "{html}");

    // Before B: A, selecting the A+B compound; after B: C, selecting B+C.
    assert!(
        html[before..after]
            .contains("data-select-pattern=\"3\" title=\"複合項目〔A ＋ B〕\">A</button>"),
        "{html}"
    );
    assert!(html[before..after].contains(">4</td>"), "{html}");
    assert!(
        html[after..longer]
            .contains("data-select-pattern=\"4\" title=\"複合項目〔B ＋ C〕\">C</button>"),
        "{html}"
    );
    assert!(html[after..longer].contains(">1,200</td>"), "{html}");
    // The three-member chain names the compound, its length and B's position.
    assert!(
        html[longer..].contains("data-select-pattern=\"5\">複合項目〔A ＋ B ＋ C〕</button>"),
        "{html}"
    );
    assert!(
        html[longer..].contains("<td class=\"pattern-count\">3</td><td class=\"pattern-count\">2</td><td class=\"pattern-count\">3</td>"),
        "length 3, position 2, count 3: {html}"
    );
}

#[test]
fn the_summary_states_how_many_occurrences_are_chained() {
    let html = render(&snapshot(true), 1);
    assert!(
        html.contains("data-i18n=\"detail.chain_share\" data-i18n-args='{\"chained\": 5, \"exact\": 20, \"pct\": \"25.0\"}'"),
        "{html}"
    );
    assert!(
        html.contains("5 of 20 (25.0%) adjacent to another published match"),
        "{html}"
    );
    assert!(!render(&snapshot(false), 1).contains("detail.chain_share"));
    assert!(
        !render(&snapshot(true), 5).contains("detail.chain_share"),
        "a compound"
    );
}

#[test]
fn the_concordance_opens_on_all_occurrences_and_flags_each_row() {
    let html = render(&snapshot(true), 1);
    assert!(
        html.contains(r#"<button type="button" class="occurrence-filter" data-occurrence="standalone" aria-pressed="false">"#),
        "{html}"
    );
    assert!(
        html.contains(r#"data-occurrence="chained" aria-pressed="false""#)
            && html.contains(r#"data-occurrence="all" aria-pressed="true""#),
        "{html}"
    );
    // B: 20 occurrences, 5 chained, so 15 standalone.
    let selector_start = html.find("data-occurrence-filter").unwrap();
    let selector =
        &html[selector_start..html[selector_start..].find("</div>").unwrap() + selector_start];
    let counts = selector
        .match_indices(r#"<span class="ranked-count">"#)
        .map(|(at, tag)| {
            let rest = &selector[at + tag.len()..];
            &rest[..rest.find('<').unwrap()]
        })
        .collect::<Vec<_>>();
    assert_eq!(counts, ["15", "5", "20"]);
    assert!(
        html.contains(
            r#"data-hit-ordinal="0" data-variation="0" data-section-ids="" data-in-chain="false""#
        ),
        "{html}"
    );
    assert!(
        html.contains(
            r#"data-hit-ordinal="1" data-variation="0" data-section-ids="" data-in-chain="true""#
        ),
        "{html}"
    );
    assert!(html.contains("data-kwic-occurrence-empty"), "{html}");
    assert!(html.contains("data-occurrence-scope-note"), "{html}");
    for html in [render(&snapshot(true), 5), render(&snapshot(false), 1)] {
        assert!(!html.contains("data-occurrence-filter"), "{html}");
        assert!(!html.contains("data-occurrence-scope-note"), "{html}");
        assert!(
            html.contains(r#"data-in-chain="false""#),
            "rows carry the flag regardless: {html}"
        );
    }
}

#[test]
fn the_summary_states_document_reach_when_the_run_counted_it() {
    let html = render(&snapshot(true), 1);
    assert!(
        html.contains(r#"data-document-share data-i18n="detail.document_share" data-i18n-args='{"documents": 12, "scanned": 61, "top": "25.0", "top_five": "45.0"}'"#),
        "{html}"
    );
    assert!(
        html.contains("In 12 of 61 documents · top document 25.0%, top five 45.0% of occurrences"),
        "{html}"
    );
}

#[test]
fn the_panel_is_absent_for_compounds_and_without_derivation() {
    assert!(!render(&snapshot(true), 5).contains("data-detail-view=\"chains\""));
    assert!(!render(&snapshot(false), 1).contains("data-detail-view=\"chains\""));
}
