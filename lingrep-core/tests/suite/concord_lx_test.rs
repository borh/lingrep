//! The 呼応 correlative-concord catalog's two-spelling measurement design.
//!
//! `catalog_examples.rs` discovers `patterns/concord.lx` and runs its
//! `examples:` claims across the whole inventory. What survives here is the
//! one claim an annotation cannot make — the `_gap` / `_look` pairing — so
//! that design is pinned as structure rather than remembered as prose.

use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};

fn compile_fixture() -> lingrep_core::fsm::CompileOutput {
    compile_catalog(
        PatternSource::named(
            "patterns/concord.lx",
            include_str!("../../../patterns/concord.lx"),
        ),
        CompileLimits::default(),
    )
    .expect("concord lingrep patterns must compile strictly")
}

// `every_documented_concord_claim_holds_against_the_dictionary` lived here,
// checking this catalog and no other. `catalog_examples.rs` now checks every
// tracked catalog through `verify_examples`; two implementations of one check
// drift.

/// The pairing is the measurement design: every construction exists as a
/// `_gap` spelling (emits the adverb-to-predicate span) and a `_look` spelling
/// (emits the adverb alone, asserts the predicate). A construction that loses
/// one half silently stops being comparable, so the mechanical suffix pairing
/// is asserted rather than described.
#[test]
fn every_construction_carries_both_spellings() {
    let matcher = compile_fixture().matcher;
    let names: Vec<&str> = matcher
        .patterns()
        .keys()
        .map(compact_str::CompactString::as_str)
        .collect();
    assert_eq!(names.len(), 12, "six constructions, two spellings each");

    for name in &names {
        let (stem, spelling) = name
            .rsplit_once('_')
            .expect("every concord pattern name carries a spelling suffix");
        assert!(
            matches!(spelling, "gap" | "look"),
            "{name} must end in _gap or _look"
        );
        let sibling = match spelling {
            "gap" => format!("{stem}_look"),
            _ => format!("{stem}_gap"),
        };
        assert!(
            names.contains(&sibling.as_str()),
            "{name} has no paired spelling {sibling}"
        );
    }
}
