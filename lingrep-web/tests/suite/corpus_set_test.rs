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

use std::sync::Arc;

use indexmap::IndexMap;
use lingrep_core::testing::MiniDictionary;
use lingrep_web::corpus_runtime::CorpusRuntime;
use lingrep_web::corpus_set::{CorpusName, CorpusSet, CorpusSlot, NameError};
use lingrep_web::server_context::ServerContext;

#[test]
fn a_name_accepts_the_url_safe_charset() {
    assert_eq!(
        CorpusName::parse("science-2026").unwrap().as_str(),
        "science-2026"
    );
    assert_eq!(CorpusName::parse("a_B9").unwrap().as_str(), "a_B9");
}

#[test]
fn a_name_refuses_what_the_permalink_parser_would_not_carry() {
    assert_eq!(CorpusName::parse(""), Err(NameError::Empty));
    assert_eq!(CorpusName::parse(&"x".repeat(65)), Err(NameError::TooLong));
    assert_eq!(
        CorpusName::parse("has space"),
        Err(NameError::InvalidChar(' '))
    );
    assert_eq!(
        CorpusName::parse("日本語"),
        Err(NameError::InvalidChar('日'))
    );
    assert_eq!(CorpusName::parse("a/b"), Err(NameError::InvalidChar('/')));
}

/// `default` is a legal *protocol* key — a v2 link for the default corpus
/// carries `k=default`, and the parser that reads `k` must admit it.
#[test]
fn the_default_key_is_a_valid_protocol_key() {
    assert_eq!(CorpusName::parse("default").unwrap().as_str(), "default");
}

/// It is refused only where an *operator* supplies it, because `--input`
/// already keys as `default` and two corpora cannot contend for one key.
/// Splitting these is what keeps a default-corpus permalink readable.
#[test]
fn the_default_key_is_reserved_against_declaration() {
    assert_eq!(
        CorpusName::parse_declared("default"),
        Err(NameError::Reserved)
    );
    // Every other rule is shared.
    assert_eq!(
        CorpusName::parse_declared("has space"),
        Err(NameError::InvalidChar(' '))
    );
    assert_eq!(CorpusName::parse_declared("sci").unwrap().as_str(), "sci");
}

/// The compatibility case that forced CorpusName and display_name apart: this
/// stem cannot be a URL key, and it is an ordinary corpus filename here.
#[test]
fn a_non_ascii_stem_is_a_display_name_not_a_key() {
    assert!(CorpusName::parse("日本語コーパス").is_err());
    assert_eq!(CorpusName::DEFAULT, "default");
}

fn test_context() -> Arc<ServerContext> {
    let dictionary = MiniDictionary::new();
    Arc::new(ServerContext::new(dictionary.path()).unwrap())
}

fn slot(context: &Arc<ServerContext>, key: &str, corpus_path: &str) -> (CorpusName, CorpusSlot) {
    let name = CorpusName::parse_declared(key).unwrap();
    let runtime = Arc::new(CorpusRuntime::new(Arc::clone(context), corpus_path).unwrap());
    (name.clone(), CorpusSlot::new(name, runtime, key.to_owned()))
}

#[test]
fn resolve_finds_the_declared_default_and_named_slots_and_misses_the_rest() {
    let context = test_context();
    let mut slots = IndexMap::new();
    let (first_name, first_slot) = slot(&context, "first", "/tmp/first.jsonl");
    slots.insert(first_name.clone(), first_slot);
    let (second_name, second_slot) = slot(&context, "second", "/tmp/second.jsonl");
    slots.insert(second_name, second_slot);
    let set = CorpusSet::new(slots);

    assert_eq!(set.iter().count(), 2);
    // First-declared is the default, which `resolve(None)` is the only way to
    // observe: `CorpusSet` exposes no separate default-name accessor.
    assert_eq!(set.resolve(None).unwrap().name, first_name);
    assert_eq!(set.resolve(Some("second")).unwrap().name.as_str(), "second");
    assert!(set.resolve(Some("absent")).is_none());
    assert!(set.resolve(Some("has space")).is_none());
}
