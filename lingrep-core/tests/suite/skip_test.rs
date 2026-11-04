// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::panic_in_result_fn, clippy::unwrap_used)]

use lingrep_core::fsm::{CompileLimits, DiagnosticCode, PatternSource, compile_catalog};

const PUNCT: &str = "test punct := upos=PUNCT.\n";

/// A CoNLL-U sentence from `TEXT/UPOS` or `TEXT/UPOS/HEAD/DEPREL` words; the
/// lemma is the text, and a word without a head attaches to the root.
fn sentence(words: &str) -> String {
    words
        .split_whitespace()
        .enumerate()
        .map(|(index, word)| {
            let mut fields = word.split('/');
            let text = fields.next().unwrap();
            let upos = fields.next().unwrap();
            let head = fields.next().unwrap_or("0");
            let rel = fields.next().unwrap_or("root");
            format!(
                "{}\t{text}\t{text}\t{upos}\t_\t_\t{head}\t{rel}\t_\t_\n",
                index + 1
            )
        })
        .collect()
}

type Observed = Vec<(String, usize, usize, Vec<(String, usize, usize)>)>;

fn matches(catalog: &str, words: &str) -> Observed {
    let compiled = compile_catalog(
        PatternSource::named("skip.lx", catalog),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("compile {catalog}: {error}"))
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("the fixture binds to the CoNLL-U schema");
    let source = sentence(words);
    let mut reader = lingrep_core::corpus::conllu::Reader::new(
        std::io::BufReader::new(source.as_bytes()),
        "skip-test",
    );
    let tokens = reader
        .next()
        .expect("fixture holds one sentence")
        .expect("fixture parses")
        .project(bound.projection())
        .expect("fixture projects");
    bound
        .find_matches(&tokens)
        .expect("schema matches")
        .into_iter()
        .map(|matched| {
            (
                matched.pattern.to_string(),
                matched.start,
                matched.end,
                matched
                    .captures
                    .iter()
                    .map(|capture| (capture.name.to_string(), capture.start, capture.end))
                    .collect(),
            )
        })
        .collect()
}

fn spans(observed: &Observed) -> Vec<(usize, usize)> {
    observed
        .iter()
        .map(|(_, start, end, _)| (*start, *end))
        .collect()
}

#[test]
fn a_skip_pattern_matches_as_if_the_skipped_tokens_were_absent() {
    let rows: &[(&str, &str, &str, &[(usize, usize)])] = &[
        (
            "a sequence steps over skipped tokens between its elements",
            "pattern p := [text=well] [text=then].",
            "well/INTJ ,/PUNCT “/PUNCT then/ADV",
            &[(0, 4)],
        ),
        (
            "no element, `[]` included, matches a skipped token",
            "pattern p := [].",
            "a/X ,/PUNCT b/X",
            &[(0, 1), (2, 3)],
        ),
        (
            "an optional first element leaves no gap at the start of the match",
            "pattern p := [text=a]? [text=b].",
            "x/X ,/PUNCT b/X a/X ,/PUNCT b/X",
            &[(2, 3), (3, 6), (5, 6)],
        ),
        (
            "an optional last element leaves no gap at the end of the match",
            "pattern p := [text=a] [text=b]?.",
            "a/X ,/PUNCT x/X",
            &[(0, 1)],
        ),
        (
            "a quantifier steps over skipped tokens between iterations",
            "pattern p := [upos=NOUN]{2}.",
            "dog/NOUN ,/PUNCT cat/NOUN",
            &[(0, 3)],
        ),
        (
            "a quantified capture neither starts nor ends on a skipped token",
            "pattern p := (?<n> [upos=NOUN])+.",
            ",/PUNCT dog/NOUN ,/PUNCT cat/NOUN ,/PUNCT",
            &[(1, 4), (3, 4)],
        ),
        (
            "a negative lookbehind steps over skipped tokens",
            "pattern p := not_preceded_by([upos=NOUN]) [text=well].",
            "dog/NOUN ,/PUNCT well/INTJ ,/PUNCT well/INTJ",
            &[(4, 5)],
        ),
        (
            "a lookahead steps over skipped tokens",
            "pattern p := [text=well] followed_by([text=then]).",
            "well/INTJ ,/PUNCT then/ADV",
            &[(0, 1)],
        ),
        (
            "`^` steps over leading skipped tokens",
            "pattern p := ^ [text=well].",
            "“/PUNCT well/INTJ well/INTJ",
            &[(1, 2)],
        ),
        (
            "`$` steps over trailing skipped tokens",
            "pattern p := [text=well] $.",
            "well/INTJ well/INTJ ./PUNCT",
            &[(1, 2)],
        ),
        (
            "`^` inside a lookbehind body steps over skipped tokens",
            "pattern p := preceded_by(^ [upos=NOUN]) [text=b].",
            "“/PUNCT dog/NOUN ,/PUNCT b/X b/X",
            &[(3, 4)],
        ),
    ];
    for (claim, pattern, words, expected) in rows {
        let skipped = matches(&format!("{PUNCT}skip: @punct\n{pattern}"), words);
        assert_eq!(
            spans(&skipped),
            *expected,
            "{claim}: {pattern} over {words}"
        );
    }
}

#[test]
fn captures_and_where_clauses_report_original_positions() {
    assert_eq!(
        matches(
            &format!("{PUNCT}skip: @punct\npattern p := (?<x> [text=a] [text=b]?) [text=c]."),
            "a/X ,/PUNCT c/X",
        ),
        vec![("p".to_owned(), 0, 3, vec![("x".to_owned(), 0, 1)])],
    );
    assert_eq!(
        matches(
            &format!(
                "{PUNCT}skip: @punct\n\
                 pattern p := (?<v> [upos=VERB]) (?<o> [upos=NOUN]) where $v -[obj]-> $o."
            ),
            "eat/VERB ,/PUNCT food/NOUN/1/obj",
        ),
        vec![(
            "p".to_owned(),
            0,
            3,
            vec![("v".to_owned(), 0, 1), ("o".to_owned(), 2, 3)]
        )],
    );
}

#[test]
fn a_capture_can_match_the_empty_span() {
    let rows: [(&str, &str, Observed); 4] = [
        (
            "pattern p := (?<c> [text=a]? [text=b]?) [text=e].",
            "e/X",
            vec![("p".to_owned(), 0, 1, vec![("c".to_owned(), 0, 0)])],
        ),
        (
            "pattern p := [text=e] (?<c> [text=a]? [text=b]?).",
            "e/X",
            vec![("p".to_owned(), 0, 1, vec![("c".to_owned(), 1, 1)])],
        ),
        (
            "pattern p := [text=e] (?<c> (?<x> [text=a])*).",
            "e/X",
            vec![("p".to_owned(), 0, 1, vec![("c".to_owned(), 1, 1)])],
        ),
        (
            "pattern p := [text=a] (?<c> [text=b]? [text=d]?) [text=e].",
            "a/X ,/PUNCT e/X",
            vec![("p".to_owned(), 0, 3, vec![("c".to_owned(), 2, 2)])],
        ),
    ];
    for (pattern, words, expected) in rows {
        assert_eq!(
            matches(&format!("{PUNCT}skip: @punct\n{pattern}"), words),
            expected,
            "{pattern} over {words}"
        );
    }
}

#[test]
fn skip_applies_per_definition_and_through_defaults() {
    let words = "well/INTJ ,/PUNCT then/ADV";
    assert!(
        matches(
            &format!("{PUNCT}pattern p := [text=well] [text=then]."),
            words
        )
        .is_empty()
    );
    let catalog = format!(
        "defaults\n  skip: @punct\nend\n{PUNCT}\
         pattern inherits := [text=well] [text=then].\n\
         skip: none\npattern unset := [text=well] [text=then]."
    );
    assert_eq!(
        matches(&catalog, words)
            .into_iter()
            .map(|(pattern, start, end, _)| (pattern, start, end))
            .collect::<Vec<_>>(),
        vec![("inherits".to_owned(), 0, 3)],
    );
}

#[test]
fn skip_rejects_what_the_rewrite_cannot_apply_to() {
    let rows = [
        (
            "skip: @punct\npattern p := @f [text=b].\nfragment f := [text=a].",
            DiagnosticCode::SkipUnsupported,
        ),
        (
            "skip: @punct\npattern p := (?<x> [text=a]) same_as($x).",
            DiagnosticCode::SkipUnsupported,
        ),
        (
            "pattern p := @f.\nskip: @punct\nfragment f := [text=a].",
            DiagnosticCode::SkipUnsupported,
        ),
        (
            "skip: @missing\npattern p := [text=a] [text=b]? (?<c> [text=d]).",
            DiagnosticCode::UndefinedReference,
        ),
        (
            "pattern q := [text=x].\nskip: @q\npattern p := [text=a] [text=b]?.",
            DiagnosticCode::UndefinedReference,
        ),
    ];
    for (catalog, expected) in rows {
        let catalog = format!("{PUNCT}{catalog}");
        let error = match compile_catalog(
            PatternSource::named("skip.lx", &catalog),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("compiled: {catalog}"),
            Err(error) => error,
        };
        assert_eq!(
            error
                .diagnostics
                .iter()
                .map(lingrep_core::fsm::Diagnostic::code)
                .collect::<Vec<_>>(),
            [expected],
            "{catalog}: {:?}",
            error.diagnostics
        );
    }
}
