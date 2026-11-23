// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::expect_used)]

use lingrep_core::fsm::{
    AttributeTestOutcome, CompileLimits, CompileOutput, DefinitionOutcome, ExplainedMatch,
    PatternSource, StopReason, StoppedAttempt, compile_catalog,
};
use lingrep_core::testing::token;
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

fn compile(source: &str) -> CompileOutput {
    compile_catalog(
        PatternSource::named("t.lx", source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("must compile: {error}"))
}

fn word(text: &str, lemma: &str) -> TokenInfo {
    let mut word = token(text);
    word.set(TokenAttributeIndex::Lemma, lemma.into());
    word
}

fn stopped(source: &str, definition: &str, tokens: &[TokenInfo]) -> (StoppedAttempt, String) {
    let explanation = compile(source)
        .explain_definition(definition, tokens)
        .expect("matching succeeds");
    assert!(!explanation.truncated);
    assert_eq!(explanation.starts_tried, tokens.len());
    let DefinitionOutcome::Stopped(attempt) = explanation.outcome else {
        panic!("expected a stopped attempt, got {:?}", explanation.outcome);
    };
    let element = attempt
        .element
        .as_ref()
        .and_then(|element| source.get(element.byte_range.clone()))
        .map(str::to_owned)
        .unwrap_or_default();
    (attempt, element)
}

#[test]
fn a_failed_token_test_names_the_element_the_token_and_each_attribute_test() {
    let source = "p := [text=猫] [text=が & lemma=が] [text=鳴く].";
    let tokens = [
        word("犬", "犬"),
        word("猫", "猫"),
        word("が", "賀"),
        word("鳴く", "鳴く"),
    ];
    let (attempt, element) = stopped(source, "p", &tokens);

    assert_eq!((attempt.start, attempt.position), (1, 2));
    assert_eq!(element, "[text=が & lemma=が]");
    let location = attempt.element.expect("a token element has a location");
    assert_eq!(
        (&*location.source, location.line, location.column),
        ("t.lx", 1, 17)
    );
    let test = |attribute: &str, actual: &str, held| AttributeTestOutcome {
        attribute: attribute.into(),
        test: "=が".into(),
        actual: Some(actual.into()),
        held,
    };
    assert_eq!(
        attempt.reason,
        StopReason::Token {
            tests: vec![test("text", "が", true), test("lemma", "賀", false)],
        }
    );
}

#[test]
fn a_fragment_reference_reports_the_element_inside_the_fragment() {
    let source = "fragment particle := [text in {が, は}].\np := [text=猫] @particle.";
    let (attempt, element) = stopped(source, "p", &[token("猫"), token("を")]);

    assert_eq!((attempt.start, attempt.position), (0, 1));
    assert_eq!(element, "[text in {が, は}]");
    assert_eq!(
        attempt.reason,
        StopReason::Token {
            tests: vec![AttributeTestOutcome {
                attribute: "text".into(),
                test: "in {が, は}".into(),
                actual: Some("を".into()),
                held: false,
            }],
        }
    );
}

#[test]
fn a_sentence_that_ends_early_and_an_absent_attribute_are_reported() {
    let source = "p := [text=猫] [lemma=が].";
    let (attempt, element) = stopped(source, "p", &[token("猫")]);
    assert_eq!(
        (attempt.position, &attempt.reason),
        (1, &StopReason::SentenceEnded)
    );
    assert_eq!(element, "[lemma=が]");

    let (attempt, _) = stopped(source, "p", &[token("猫"), token("が")]);
    assert_eq!(
        attempt.reason,
        StopReason::Token {
            tests: vec![AttributeTestOutcome {
                attribute: "lemma".into(),
                test: "=が".into(),
                actual: None,
                held: false,
            }],
        }
    );
}

#[test]
fn a_failed_lookaround_is_the_stop_and_its_body_is_not_progress() {
    let source = "p := [text=猫] not_followed_by([text=が] [text=いる]) [text=が].";
    let tokens = [token("猫"), token("が"), token("いる")];
    let (attempt, element) = stopped(source, "p", &tokens);

    assert_eq!((attempt.start, attempt.position), (0, 1));
    assert_eq!(element, "not_followed_by([text=が] [text=いる])");
    assert_eq!(
        attempt.reason,
        StopReason::Lookaround {
            preceded_by: false,
            negative: true,
        }
    );
}

#[test]
fn a_match_that_policy_does_not_publish_is_not_a_stopped_attempt() {
    let output = compile(
        "short := [text=猫].\n\
         fragment tail := [text=犬].\n\
         policy\n  \
         exclude @short with: @tail, relation: after, evidence: raw.\n\
         end\n",
    );
    let outcome = |tokens: &[TokenInfo]| {
        output
            .explain_definition("short", tokens)
            .expect("matching succeeds")
            .outcome
    };

    assert_eq!(
        outcome(&[token("猫"), token("犬")]),
        DefinitionOutcome::NotPublished {
            spans: vec![(0, 1)]
        }
    );
    assert_eq!(
        outcome(&[token("猫"), token("鳥")]),
        DefinitionOutcome::Published {
            spans: vec![(0, 1)]
        }
    );
}

#[test]
fn a_body_match_the_where_clause_rejects_is_reported_with_its_spans() {
    let output = compile("p := (?<n> [upos=NOUN]) (?<v> [upos=VERB]) where $v -[nsubj]-> $n.");
    let bound = output
        .matcher
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .expect("binds to CoNLL-U");
    let explain = |relation: &str| {
        let conllu = format!(
            "1\t猫\t猫\tNOUN\t_\t_\t2\t{relation}\t_\t_\n2\t鳴く\t鳴く\tVERB\t_\t_\t0\troot\t_\t_\n"
        );
        let sentence = lingrep_core::corpus::conllu::Reader::new(
            std::io::BufReader::new(conllu.as_bytes()),
            "no-match-explanation-test",
        )
        .next()
        .expect("one sentence")
        .expect("parses")
        .project(bound.projection())
        .expect("projects");
        let candidates = bound
            .find_matches(&sentence)
            .expect("matches")
            .into_iter()
            .map(|raw| ExplainedMatch {
                published: Some(raw.clone()),
                raw,
            })
            .collect::<Vec<_>>();
        output
            .no_match
            .explain_definition("p", &sentence, &candidates)
            .outcome
    };

    assert_eq!(
        explain("obj"),
        DefinitionOutcome::WhereRejected {
            spans: vec![(0, 2)]
        }
    );
    assert_eq!(
        explain("nsubj"),
        DefinitionOutcome::Published {
            spans: vec![(0, 2)]
        }
    );
}

#[test]
fn a_construct_the_pass_does_not_evaluate_is_named_instead_of_guessed() {
    let outcome = compile("p := (?<a> [text=猫]) [text=と] same_as($a).")
        .explain_definition("p", &[token("猫"), token("と"), token("犬")])
        .expect("matching succeeds")
        .outcome;

    assert_eq!(
        outcome,
        DefinitionOutcome::Unexplained {
            reason: "the explanation pass does not evaluate a same-as reference to a capture"
                .into(),
        }
    );
}

#[test]
fn a_claim_site_is_the_definitions_own_key_or_its_start() {
    use lingrep_core::fsm::ClaimSite;
    let source = "defaults\n  counterexamples: [\"鳥\"]\nend\n\n\
                  examples: [\"猫\"]\npattern p := [text=猫].\n\npattern q := [text=犬].\n";
    let output = compile(source);
    let at = |needle: &str| source.find(needle).expect("the fixture contains it");

    let own = at("\nexamples") + 1;
    assert_eq!(
        output.claim_site("p", "examples"),
        Some(ClaimSite::Declared {
            key: own..own + "examples".len()
        })
    );
    assert_eq!(
        output.claim_site("p", "counterexamples"),
        Some(ClaimSite::Inherited)
    );
    assert_eq!(
        output.claim_site("q", "examples"),
        Some(ClaimSite::Absent {
            definition_start: at("pattern q")
        })
    );
    assert_eq!(output.claim_site("missing", "examples"), None);
}

#[test]
fn consumed_tokens_name_the_element_and_the_reference_that_reached_it() {
    let source = "fragment N := [lemma=猫].\npattern p := [text=の]? @N{1,2} [text=だ].\n";
    let output = compile(source);
    let pairs = |surfaces: &[&str]| {
        let tokens = surfaces
            .iter()
            .map(|surface| word(surface, surface))
            .collect::<Vec<_>>();
        output
            .consumed_tokens("p", &tokens[..], 0, tokens.len())
            .expect("the body derives the span")
            .into_iter()
            .map(|consumed| {
                let elements = consumed
                    .elements
                    .iter()
                    .map(|element| {
                        source
                            .get(element.byte_range.clone())
                            .expect("a source range")
                    })
                    .collect::<Vec<_>>();
                (consumed.token, elements)
            })
            .collect::<Vec<_>>()
    };

    // The optional element is skipped and the repetition runs twice.
    assert_eq!(
        pairs(&["猫", "猫", "だ"]),
        vec![
            (0, vec!["[lemma=猫]", "@N"]),
            (1, vec!["[lemma=猫]", "@N"]),
            (2, vec!["[text=だ]"]),
        ]
    );
    // The optional element is taken and the repetition runs once.
    assert_eq!(
        pairs(&["の", "猫", "だ"]),
        vec![
            (0, vec!["[text=の]"]),
            (1, vec!["[lemma=猫]", "@N"]),
            (2, vec!["[text=だ]"]),
        ]
    );
    let tokens = [word("猫", "猫"), word("だ", "だ")];
    assert_eq!(output.consumed_tokens("p", &tokens[..], 0, 1), None);
}

#[test]
fn consumed_tokens_report_each_attribute_test_with_the_token_value() {
    let output = compile("pattern p := [lemma=猫 & text!=犬] [text=だ].\n");
    let tokens = [word("ねこ", "猫"), word("だ", "だ")];
    let tests = output
        .consumed_tokens("p", &tokens[..], 0, 2)
        .expect("the body derives the span")
        .into_iter()
        .map(|consumed| {
            consumed
                .tests
                .into_iter()
                .map(|test| (test.attribute, test.test, test.actual, test.held))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        tests,
        vec![
            vec![
                ("lemma".to_owned(), "=猫".to_owned(), Some("猫".to_owned()), true),
                ("text".to_owned(), "not in {犬}".to_owned(), Some("ねこ".to_owned()), true),
            ],
            vec![("text".to_owned(), "=だ".to_owned(), Some("だ".to_owned()), true)],
        ]
    );
}
