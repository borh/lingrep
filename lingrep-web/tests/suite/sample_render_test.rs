use crate::support::tok;
use lingrep_core::token_match::TokenInfo;
use lingrep_web::sample::{SampleAnalysis, analyze_tokens};
use lingrep_web::templates::SampleResults;

#[test]
fn matched_renders_inline_tokens_and_captures_without_fetch_urls() {
    let lines = vec![vec![tok("猫", "名詞"), tok("走る", "動詞")]];
    let analysis = analyze_tokens("pattern v := (?<verb> [upos=動詞]).", &lines);
    assert!(matches!(analysis, SampleAnalysis::Matched { .. }));
    let html = SampleResults {
        analysis,
        run: None,
    }
    .render()
    .expect("renders");
    assert!(html.contains("走る"), "match surface shown");
    assert!(html.contains("verb"), "named capture shown");
    // A capture bar names its match and its ordinal; the match's template
    // must hold the row those two select, or selecting the bar shows nothing.
    assert!(
        html.contains("data-sample-bar=\"0-0\" data-sample-match=\"0\" data-sample-capture=\"0\""),
        "the capture bar is rendered: {html}"
    );
    assert!(
        html.contains("<template id=\"sample-match-0\">")
            && html.contains("data-sample-capture-row=\"0\""),
        "the capture's row is rendered in its match's detail: {html}"
    );
    // Inline contract: NO per-token inspect fetch URLs in the sample fragment.
    assert!(
        !html.contains("/api/inspect/"),
        "sample detail is inline, never fetched"
    );
    assert!(
        html.contains("data-sample-token=\"0\""),
        "sample tokens are interactive buttons: {html}"
    );
    assert!(
        html.contains("<template id=\"sample-token-0\">"),
        "each token's inspection is rendered once, inert: {html}"
    );
    assert!(
        html.contains("title=\"動詞"),
        "sample token buttons carry a morphological tooltip: {html}"
    );
}

#[test]
fn fragments_only_shows_its_distinct_message() {
    let lines = vec![vec![tok("走る", "動詞")]];
    let html = SampleResults {
        analysis: analyze_tokens("fragment F := [upos=動詞].", &lines),
        run: None,
    }
    .render()
    .unwrap();
    assert!(
        html.to_lowercase().contains("fragment"),
        "explains fragment-only documents"
    );
    assert!(
        !html.contains("No matches"),
        "not the bare no-match message"
    );
}

#[test]
fn failed_shows_the_compile_error_message() {
    let lines = vec![vec![tok("猫", "名詞")]];
    let html = SampleResults {
        analysis: analyze_tokens("pattern broken := [upos=", &lines),
        run: None,
    }
    .render()
    .unwrap();
    assert!(
        html.contains("error") || html.contains("Error") || html.contains(":"),
        "shows a diagnostic"
    );
}

#[test]
fn a_sentence_whose_policy_hid_a_candidate_lists_reported_and_hidden_candidates() {
    let source = include_str!("../../../patterns/sf-experimental.lx");
    let analysis = analyze_tokens(
        source,
        &[
            vec![
                TokenInfo::default().with_orth("もの").with_lemma("物"),
                TokenInfo::default().with_orth("で").with_lemma("だ"),
                TokenInfo::default().with_orth("あろう").with_lemma("有る"),
            ],
            // `exclude @ている with: @NEG` suppresses the ている record.
            vec![
                TokenInfo::default().with_orth("て").with_lemma("て"),
                TokenInfo::default().with_orth("い").with_lemma("居る"),
                TokenInfo::default().with_orth("ない").with_lemma("ない"),
            ],
        ],
    );
    let html = SampleResults {
        analysis,
        run: None,
    }
    .render()
    .expect("renders");

    assert_eq!(
        html.matches("data-sample-policy").count(),
        1,
        "only the second sentence has a hidden candidate: {html}"
    );
    assert!(html.contains("S2C"), "a reported match shows its class");
    assert!(html.contains("data-publication=\"suppressed\""));
    assert!(html.contains("data-i18n=\"sample.hidden\""));
    // The view must not resurrect the pattern policy excluded: it is neither
    // a bar nor a listed candidate.
    assert!(!html.contains("<span class=\"sample-bar-label\">ものである</span>"));
    let policy = html
        .split_once("data-sample-policy")
        .and_then(|(_, rest)| rest.split_once("</details>"))
        .expect("the policy list is rendered")
        .0;
    assert!(!policy.contains("ものである"), "{policy}");
}

#[test]
fn a_sentence_whose_policy_hid_nothing_paints_its_matches_and_has_no_policy_list() {
    let lines = vec![vec![
        tok("猫", "名詞"),
        tok("は", "助詞"),
        tok("走る", "動詞"),
    ]];
    let analysis = analyze_tokens(
        "pattern noun := [upos=名詞]. pattern verb := [upos=動詞].",
        &lines,
    );

    let html = SampleResults {
        analysis,
        run: None,
    }
    .render()
    .expect("renders");

    assert!(!html.contains("data-sample-policy"), "{html}");
    assert_eq!(html.matches("kwic-token kwic-token-match").count(), 2);
    for (name, token) in [("noun", 1), ("verb", 3)] {
        assert!(
            html.contains(&format!(
                "<span class=\"sample-bar-label\">{name}</span><span class=\"visually-hidden\">\
                 <span data-i18n=\"sample.token_at\" data-i18n-args='{{\"n\": {token}}}'>"
            )),
            "the {name} bar starts under token {token}: {html}"
        );
    }
    assert!(
        html.contains(
            r##"<a href="#sample-sentence-1"><code>noun</code></a></th><td class="numeric">1</td>"##
        ),
        "each definition is counted and links to its first sentence: {html}"
    );
}

#[test]
fn sample_lines_are_one_document_for_document_lookarounds() {
    let lines = vec![
        vec![tok("猫", "名詞")],
        vec![tok("走る", "動詞")],
        Vec::new(),
        vec![tok("犬", "名詞")],
    ];
    let analysis = analyze_tokens(
        "pattern before_verb := [upos=名詞] followed_by([upos=動詞], within=document).\n\
         pattern last := [upos=名詞] not_followed_by([], within=document).",
        &lines,
    );
    let SampleAnalysis::Matched { sentences } = analysis else {
        panic!("the sample matches");
    };
    let mut found: Vec<(String, String)> = sentences
        .into_iter()
        .flat_map(|sentence| sentence.matches)
        .map(|matched| (matched.pattern_name, matched.matched_text))
        .collect();
    found.sort();
    assert_eq!(
        found,
        [
            ("before_verb".to_owned(), "猫".to_owned()),
            ("last".to_owned(), "犬".to_owned()),
        ]
    );
}

#[test]
fn a_run_names_its_tokenizer_its_missing_attributes_and_each_failed_claim() {
    use lingrep_web::sample::{
        ClaimFailure, ClaimFailureKind, ClaimsReport, SampleBinding, SampleClaims, SampleRun,
        UnavailableAttribute,
    };
    use lingrep_web::sample::{WhyNot, WhyNotElement, WhyNotOutcome, WhyNotStop, WhyNotStopKind};
    let why = |outcome| WhyNot {
        definition: "v".into(),
        outcome,
        starts_tried: 2,
        truncated: true,
        add_counterexample: Some(lingrep_web::sample::ClaimInsert {
            offset: 7,
            text: "counterexamples: [\"<鳥>\"]\n".into(),
            expects: "pattern v".into(),
        }),
    };
    let lines = vec![vec![tok("走る", "動詞")], vec![tok("<鳥>", "名詞")]];
    let render = |binding, claims| {
        let mut analysis = analyze_tokens("pattern v := [upos=動詞].", &lines);
        let SampleAnalysis::Matched { sentences } = &mut analysis else {
            panic!("走る matches");
        };
        sentences[1].why_not = vec![why(WhyNotOutcome::WhereRejected {
            spans: "0..2".into(),
        })];
        SampleResults {
            analysis,
            run: Some(SampleRun {
                analyzer: "vibrato morphology · unidic.dic".into(),
                stored: false,
                corpus_analyzer: Some("conllu pretokenized".into()),
                binding,
                claims,
            }),
        }
        .render()
        .unwrap()
    };
    let html = render(
        SampleBinding::Bound {
            unavailable: vec![UnavailableAttribute {
                attribute: "dep.rel".into(),
                definitions: vec!["v".into()],
            }],
        },
        SampleClaims::Unverifiable,
    );
    assert!(html.contains("<code>vibrato morphology · unidic.dic</code>"));
    assert!(html.contains("data-sample-corpus-differs"));
    assert!(html.contains("<code>conllu pretokenized</code>"));
    assert!(html.contains("data-sample-unavailable"));
    assert!(html.contains("<code>dep.rel</code>"));
    // `i18n.js` replaces a translated element's text, so the element holds
    // text only.
    assert!(html.contains(
        "<span data-i18n=\"sample.attribute_unavailable_note\">. Comparisons on this \
         attribute fail on every token. The complement !(…) of a failed comparison \
         succeeds.</span>"
    ));
    assert!(html.contains(r#"data-sample-claims="unverifiable""#));

    let html = render(
        SampleBinding::Bound {
            unavailable: Vec::new(),
        },
        SampleClaims::Checked(ClaimsReport {
            checked: 3,
            failures: vec![ClaimFailure {
                definition: "v".into(),
                example: "<猫>".into(),
                kind: ClaimFailureKind::ExpectedMatch,
                why_not: Some(why(WhyNotOutcome::Stopped(WhyNotStop {
                    start: 1,
                    consumed: 2,
                    position: 3,
                    token: Some("<い>".into()),
                    kind: WhyNotStopKind::Token,
                    element: Some(WhyNotElement {
                        line: 4,
                        column: 9,
                        text: "[lemma=居る & unidic.cform~終止形]".into(),
                    }),
                    tests: vec![lingrep_core::fsm::AttributeTestOutcome {
                        attribute: "unidic.cform".into(),
                        test: "~終止形".into(),
                        actual: None,
                        held: false,
                    }],
                }))),
            }],
            vacuous: Vec::new(),
            unchecked: 0,
            unrelaxed: 0,
        }),
    );
    assert!(!html.contains("data-sample-unavailable"));
    assert!(html.contains(r#"data-sample-claims="violated""#));
    assert!(html.contains(r#"data-i18n-args='{"failed": 1, "checked": 3}'"#));
    assert!(html.contains(r#"data-i18n="sample.claim_expected_match""#));
    assert!(
        html.contains("data-i18n-title=\"sample.claim_use\">&#60;猫&#62;</button>"),
        "the claim text is escaped: {html}"
    );
    for expected in [
        r#"data-why-not="stopped""#,
        r#"data-i18n-args='{"start": 2, "consumed": 2}'"#,
        "<span lang=\"ja\" data-why-not-token>「&#60;い&#62;」</span>",
        "<code data-why-not-element>[lemma=居る &#38; unidic.cform~終止形]</code>",
        r#"data-i18n-args='{"line": 4, "column": 9}'"#,
        "<li data-held=\"false\">",
        "<code>unidic.cform~終止形</code>",
        r#"data-i18n="sample.why_no_value""#,
        r#"data-i18n-args='{"starts": 2}'"#,
        r#"data-i18n="sample.why_truncated""#,
        "title=\"名詞\">&#60;鳥&#62;</button>",
        r#"data-why-not="where-rejected""#,
        "<code>0..2</code>",
        "data-claim-insert=\"counterexamples\" data-claim-offset=\"7\" \
         data-claim-text=\"counterexamples: [&#34;&#60;鳥&#62;&#34;]\n\" \
         data-claim-expects=\"pattern v\"",
    ] {
        assert!(html.contains(expected), "missing {expected}: {html}");
    }

    let html = render(
        SampleBinding::Bound {
            unavailable: Vec::new(),
        },
        SampleClaims::Checked(ClaimsReport {
            checked: 0,
            failures: vec![ClaimFailure {
                definition: "v".into(),
                example: "not a list".into(),
                kind: ClaimFailureKind::Malformed,
                why_not: None,
            }],
            vacuous: Vec::new(),
            unchecked: 0,
            unrelaxed: 0,
        }),
    );
    assert!(html.contains("Examples and counterexamples checked (0)."));
    assert!(html.contains("Failed checks or invalid entries (1)."));

    let html = render(
        SampleBinding::Refused("no dep.head".into()),
        SampleClaims::Checked(ClaimsReport {
            checked: 2,
            failures: Vec::new(),
            vacuous: vec![lingrep_web::sample::VacuousClaim {
                definition: "v".into(),
                example: "<丁>".into(),
                tried: 4,
            }],
            unchecked: 0,
            unrelaxed: 0,
        }),
    );
    assert!(!html.contains("data-sample-claims-unchecked"));
    assert!(!html.contains("data-sample-claims-unrelaxed"));
    assert!(html.contains("data-sample-refused"));
    assert!(html.contains(r#"data-sample-claims="hold""#));
    assert!(html.contains("data-sample-claims-vacuous"));
    assert!(html.contains(r#"<code>v</code> <span lang="ja">&#60;丁&#62;</span>"#));
    assert!(html.contains("(4 relaxations tried)"));

    let html = render(
        SampleBinding::Bound {
            unavailable: Vec::new(),
        },
        SampleClaims::Checked(ClaimsReport {
            checked: 256,
            failures: Vec::new(),
            vacuous: Vec::new(),
            unchecked: 44,
            unrelaxed: 7,
        }),
    );
    assert!(html.contains(
        "data-sample-claims-unchecked data-i18n=\"sample.claims_unchecked\" \
         data-i18n-args='{\"unchecked\": 44, \"limit\": 256}'>\
         Examples and counterexamples not checked (44). \
         The sample debugger checks the first 256 entries of a catalog.</p>"
    ));
    assert!(html.contains(
        "data-sample-claims-unrelaxed data-i18n=\"sample.claims_unrelaxed\" \
         data-i18n-args='{\"count\": 7}'>"
    ));
}
