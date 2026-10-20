//! The reduplication catalog's two claims that an `examples:` annotation
//! cannot state.
//!
//! `catalog_examples.rs` discovers `patterns/reduplication.lx` and runs its
//! claims across the whole inventory. What survives here is the one that is
//! dictionary-backed evidence about the compiler rather than about the
//! language: this catalog is `same_as`'s first tracked user, so its projection
//! attributes are the only real-tokenization witness that generalized
//! projection (`by=unidic.kana`, `by=unidic.orthbase`) reaches the catalog's
//! required set.

use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};
use lingrep_core::token_match::TokenAttributeIndex;

fn compile_fixture() -> lingrep_core::fsm::CompileOutput {
    compile_catalog(
        PatternSource::named(
            "patterns/reduplication.lx",
            include_str!("../../../patterns/reduplication.lx"),
        ),
        CompileLimits::default(),
    )
    .expect("reduplication lingrep patterns must compile strictly")
}

// `every_documented_reduplication_claim_holds_against_the_dictionary` lived
// here, checking this catalog and no other. `catalog_examples.rs` now checks
// every tracked catalog through `verify_examples`; two implementations of one
// check drift.

/// The projection attributes are required attributes: `kana` and `orthbase`
/// appear in this catalog only after `by=`, so a caller projecting columns
/// from `required_attributes()` must learn about them from the projection
/// alone. This is the tracked-catalog witness for the generalization; the
/// synthetic version lives in `same_as_projection_test.rs`.
#[test]
fn projection_attributes_reach_the_catalog_projection() {
    let matcher = compile_fixture().matcher;
    let attributes = matcher.required_attributes();
    for attribute in [TokenAttributeIndex::Kana, TokenAttributeIndex::OrthBase] {
        assert!(
            attributes.contains(&attribute.into()),
            "{attribute:?} is only mentioned by a projection and must still be required: \
             {attributes:?}"
        );
    }
}
