//! Repository acceptance for cross-catalog drift.
//!
//! Pins selected divergences over the retained researcher catalogs. The full
//! drift set is deliberately not
//! pinned here: a name beyond ARU/NAI reporting distinct bodies is a *finding*
//! for the researcher to adjudicate, recorded in the baseline results
//! doc, not a test failure.

use lingrep_core::fsm::{CatalogDriftReport, PatternSource, report_catalog_drift};

/// The research surfaces. The fixture catalogs (`patterns/test-simple.lx`,
/// `tests/fixtures/*`, `lingrep/tests/fixtures/*`) are not research surfaces
/// and are deliberately excluded.
const RESEARCHER_CATALOGS: [&str; 3] = [
    "patterns/double-wo.lx",
    "patterns/sf-experimental.lx",
    "patterns/sfe.lx",
];

fn researcher_catalog_sources() -> Vec<(String, String)> {
    let sources: Vec<_> = lingrep_core::testing::catalogs::tracked_catalog_sources()
        .into_iter()
        .filter(|(path, _)| RESEARCHER_CATALOGS.contains(&path.as_str()))
        .collect();
    // A discovery bug that found nothing would make every assertion below
    // vacuously green — the `tracked_catalogs_test.rs` guard, applied here.
    assert_eq!(
        sources.len(),
        RESEARCHER_CATALOGS.len(),
        "researcher catalog set changed — update RESEARCHER_CATALOGS; found {:?}",
        sources.iter().map(|(path, _)| path).collect::<Vec<_>>()
    );
    sources
}

fn report(sources: &[(String, String)]) -> CatalogDriftReport {
    let borrowed: Vec<PatternSource<'_>> = sources
        .iter()
        .map(|(name, text)| PatternSource::named(name, text))
        .collect();
    report_catalog_drift(&borrowed).expect("tracked catalogs compile")
}

#[test]
fn tracked_catalogs_report_the_spec_enumerated_drift() {
    let sources = researcher_catalog_sources();
    let report = report(&sources);

    let entry = |name: &str| report.entries.iter().find(|entry| entry.name == name);

    // The retained catalog and sf-experimental still deliberately differ on
    // these shared helper definitions.
    assert_eq!(
        entry("ARU")
            .expect("ARU drifts across catalogs")
            .bodies
            .len(),
        2,
        "{report:#?}"
    );
    assert_eq!(
        entry("NAI")
            .expect("NAI drifts across catalogs")
            .bodies
            .len(),
        2,
        "{report:#?}"
    );

    // double-wo shares no name with any other researcher catalog, so nothing
    // it declares can drift.
    assert!(
        report
            .entries
            .iter()
            .all(|entry| !entry.name.starts_with("double_wo")),
        "{report:#?}"
    );
}

#[test]
fn a_catalog_against_itself_reports_nothing() {
    let sources = researcher_catalog_sources();
    let (name, text) = sources.first().expect("researcher catalogs are non-empty");
    let report = report_catalog_drift(&[
        PatternSource::named(name, text),
        PatternSource::named("copy.lx", text),
    ])
    .expect("catalog compiles");
    assert!(
        report.entries.is_empty(),
        "a catalog cannot drift against a byte-identical copy: {report:#?}"
    );
}

#[test]
fn every_reported_name_carries_at_least_two_distinct_bodies() {
    // The report's own invariant: a name with one body is not drift and must
    // never appear. Guards against a grouping regression that would flood the
    // researcher-facing notice with non-findings.
    let sources = researcher_catalog_sources();
    let report = report(&sources);
    for entry in &report.entries {
        assert!(
            entry.bodies.len() >= 2,
            "{} reported with {} body",
            entry.name,
            entry.bodies.len()
        );
        assert!(
            entry.bodies.iter().all(|body| !body.carriers.is_empty()),
            "{} has a body with no carrier",
            entry.name
        );
    }
}

#[test]
fn the_measured_drift_set_matches_the_recorded_baseline() {
    // Nine names, including the two helper definitions above. Pinned so the next catalog
    // edit that changes the set is visible as a diff against
    // `docs/results/2026-08-02-r5-drift-baseline.md` rather than sliding
    // silently. Adjudication of any entry stays with the researcher —
    // this asserts the measurement, not that the divergences are correct.
    let sources = researcher_catalog_sources();
    let report = report(&sources);
    let measured: Vec<(String, usize)> = report
        .entries
        .iter()
        .map(|entry| (entry.name.clone(), entry.bodies.len()))
        .collect();
    let expected: Vec<(String, usize)> = [
        ("ARU", 2),
        ("NAI", 2),
        ("かもしれない", 2),
        ("たい", 2),
        ("だ", 2),
        ("だろう", 2),
        ("であろう", 2),
        ("ようだ", 2),
        ("必要がある", 2),
    ]
    .into_iter()
    .map(|(name, bodies)| (name.to_owned(), bodies))
    .collect();
    assert_eq!(measured, expected, "{report:#?}");
}
