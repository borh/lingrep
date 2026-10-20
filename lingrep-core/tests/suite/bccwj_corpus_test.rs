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
//! The BCCWJ reader contract, driven through [`BccwEvents`] over a
//! synthetic distribution. All text is self-authored (§2): no BCCWJ content
//! is recorded in any tracked artifact.

use std::fs::{self, File};
use std::io::{self, Write};
use std::sync::{Arc, OnceLock};

use lingrep_core::corpus::TraversalLimits;
use lingrep_core::corpus::bccwj::{
    BccwEvents, BccwjInventory, Sampling, Subcorpus, basename_from_entry, zip_file_stat,
};
use lingrep_core::corpus::structured::{CorpusEvent, MetadataScalar, MetadataValue};

/// The synthetic `Joined_info` table (20 columns per the reference,
/// spec §3.2): row `one` (book, permission granted) and row `solo`
/// (book review).
const JOINED: &str = "a	b	c	d	e	f	g	h	i	j	k	l	m	n	o	p	q	r	s\none	book	題一		vol	resp	社一	2000	isbn	page							著一			LB\nsolo	book	題二		vol2	resp2	社二	2001	isbn2	page2							著二			PN\n";

/// Writes a synthetic distribution (all text self-authored) with the given
/// zips (logical name → entry name/XML pairs) and returns the root.
fn distribution(zips: &[(&str, &[(&str, &str)])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let doc = dir.path().join("DOC");
    fs::create_dir_all(&doc).expect("doc");
    fs::write(doc.join("Joined_info.txt"), JOINED).expect("joined");
    fs::write(doc.join("BCCWJ-NDC.txt"), "a\tb\n").expect("ndc");
    fs::write(doc.join("CopyRight_Annotation.txt"), "a\tb\n").expect("copyright");
    for &(logical, entries) in zips {
        let path = dir.path().join(logical);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("zip parent");
        }
        let mut writer = zip::ZipWriter::new(io::BufWriter::new(
            fs::File::create(&path).expect("zip file"),
        ));
        for (entry, content) in entries {
            writer
                .start_file(entry.to_owned(), zip::write::SimpleFileOptions::default())
                .expect("start entry");
            writer.write_all(content.as_bytes()).expect("entry");
        }
        writer.finish().expect("finish zip");
    }
    dir
}

fn open_events(dist: &std::path::Path, logical: &str) -> BccwEvents {
    let inventory = BccwjInventory::discover(dist, &TraversalLimits::default()).expect("discover");
    let zip = inventory
        .zips
        .iter()
        .find(|zip| zip.logical_name == logical)
        .expect("zip");
    let file: File = fs::File::open(&zip.path).expect("open");
    let integrity = zip_file_stat(&file).expect("stat");
    BccwEvents::new(
        file,
        zip,
        dist,
        Arc::new(OnceLock::new()),
        integrity,
        lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
    )
    .expect("events")
}

fn collect(events: &mut BccwEvents) -> Vec<CorpusEvent> {
    events
        .by_ref()
        .map(|event| event.expect("stream"))
        .collect()
}

/// The metadata the synthetic `one` row derives at `fixed` sampling — the
/// exact object shape `DocMetadata::to_json` emits for it.
fn row_metadata_one_fixed() -> Vec<(String, MetadataValue)> {
    let scalar = |value: &str| MetadataValue::Scalar(MetadataScalar::String(value.to_owned()));
    let mut values = vec![
        ("title".to_owned(), scalar("題一")),
        ("author".to_owned(), scalar("著一")),
        ("publisher".to_owned(), scalar("社一")),
        (
            "year".to_owned(),
            MetadataValue::Scalar(MetadataScalar::Number("2000".to_owned())),
        ),
        ("basename".to_owned(), scalar("one")),
        ("corpus".to_owned(), scalar("BCCWJ")),
        ("subcorpus".to_owned(), scalar("書籍")),
        (
            "category".to_owned(),
            MetadataValue::Scalars(Box::new([MetadataScalar::String("書籍".to_owned())])),
        ),
        (
            "genre".to_owned(),
            MetadataValue::Scalars(Box::new([MetadataScalar::String("書籍".to_owned())])),
        ),
        (
            "permission".to_owned(),
            MetadataValue::Scalar(MetadataScalar::Bool(true)),
        ),
        ("sampling".to_owned(), scalar("fixed")),
    ];
    values.sort_by(|left, right| left.0.cmp(&right.0));
    values
}

#[test]
fn a_two_zip_distribution_streams_documents_in_inventory_order() {
    // One FIXED and one VARIABLE zip; the entries mix coded and uncoded
    // wrapper tags to pin the D2 projection at the stream level.
    let speech = "<article><speech><paragraph><sentence><orth>引用文。</orth></sentence></paragraph></speech></article>";
    let plain =
        "<article><paragraph><sentence><orth>本文。</orth></sentence></paragraph></article>";
    let dist = distribution(&[
        (
            "C-XML/VARIABLE/LB/LB.zip",
            &[("LBa/one.xml", speech), ("LBa/solo.xml", plain)],
        ),
        ("C-XML/FIXED/PN/PN.zip", &[("PNb/one.xml", plain)]),
    ]);
    let inventory =
        BccwjInventory::discover(dist.path(), &TraversalLimits::default()).expect("discover");
    assert_eq!(
        inventory
            .zips
            .iter()
            .map(|zip| (zip.logical_name.as_str(), zip.sampling, zip.subcorpus))
            .collect::<Vec<_>>(),
        [
            ("C-XML/FIXED/PN/PN.zip", Sampling::Fixed, Subcorpus::PN),
            (
                "C-XML/VARIABLE/LB/LB.zip",
                Sampling::Variable,
                Subcorpus::LB
            ),
        ]
    );

    // Zip one (FIXED): plain paragraph, no coded tags → empty labels.
    let mut events = open_events(dist.path(), "C-XML/FIXED/PN/PN.zip");
    let fixed = collect(&mut events);
    assert_eq!(
        fixed,
        vec![
            CorpusEvent::DocumentStart {
                source_name: "PNb/one.xml".to_owned(),
                metadata: row_metadata_one_fixed().into_boxed_slice(),
            },
            CorpusEvent::DivisionObservation {
                kind: "paragraph".into(),
                observation: lingrep_core::corpus::DivisionObservation::Observed
            },
            CorpusEvent::ParagraphStart {
                metadata: Box::new([]),
            },
            CorpusEvent::Sentence {
                text: "本文。".to_owned(),
                words: None,
                enhanced_graph: None,
                active_labels: Box::new([]),
                metadata: Box::new([]),
            },
            CorpusEvent::ParagraphEnd,
            CorpusEvent::DocumentEnd,
        ]
    );

    // Zip two (VARIABLE): the coded `speech` survives, the uncoded
    // `article`/`paragraph` wrappers do not; paragraph metadata stays
    // empty.
    let mut events = open_events(dist.path(), "C-XML/VARIABLE/LB/LB.zip");
    let variable = collect(&mut events);
    let starts: Vec<_> = variable
        .iter()
        .filter_map(|event| match event {
            CorpusEvent::DocumentStart { source_name, .. } => Some(source_name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(starts, ["LBa/one.xml", "LBa/solo.xml"]);
    let labels: Vec<_> = variable
        .iter()
        .filter_map(|event| match event {
            CorpusEvent::Sentence { active_labels, .. } => Some(active_labels.as_ref().to_vec()),
            _ => None,
        })
        .collect();
    assert_eq!(labels[0], ["speech"].map(String::from));
    assert!(labels[1].is_empty());
}

#[test]
fn the_stream_is_deterministic_across_runs() {
    let xml = "<article><paragraph><sentence><orth>文。</orth></sentence></paragraph></article>";
    let dist = distribution(&[("C-XML/FIXED/LB/LB.zip", &[("LBa/one.xml", xml)])]);
    let mut first = open_events(dist.path(), "C-XML/FIXED/LB/LB.zip");
    let mut second = open_events(dist.path(), "C-XML/FIXED/LB/LB.zip");
    assert_eq!(collect(&mut first), collect(&mut second));
}

#[test]
fn a_missing_basename_fails_naming_zip_and_entry() {
    // The table rows are `one` and `solo`; the entry yields basename `two`.
    let xml = "<article><paragraph><sentence><orth>文。</orth></sentence></paragraph></article>";
    let dist = distribution(&[("C-XML/FIXED/LB/LB.zip", &[("LBa/two.xml", xml)])]);
    let mut events = open_events(dist.path(), "C-XML/FIXED/LB/LB.zip");
    let error: io::Error = events
        .next()
        .expect("an event")
        .expect_err("missing basename");
    let message = error.to_string();
    assert!(message.contains("C-XML/FIXED/LB/LB.zip"), "{message}");
    assert!(message.contains("LBa/two.xml"), "{message}");
    assert!(message.contains("two"), "{message}");
}

#[test]
fn the_basename_rule_cuts_at_the_first_dash() {
    assert_eq!(basename_from_entry("LBa/LBa0_00002.xml"), "LBa0_00002");
    assert_eq!(basename_from_entry("LB/LBa0_00002.xml"), "LBa0_00002");
    assert_eq!(basename_from_entry("a/b-1.xml"), "b");
    assert_eq!(basename_from_entry("x-1-2.xml"), "x");
}
