//! The bunsetu definitions in the CLI guide's spaCy adapter section, read from
//! the guide and run over GiNZA's `BunsetuBILabel` values for the sentence the
//! guide describes.

use lingrep_core::fsm::{Capture, CompileLimits, Match, PatternSource, compile_catalog};
use lingrep_core::token_match::{
    AttributeKey, AttributeKind, BoundTokenSentence, SchemaAttribute, TokenSchema,
};

const GUIDE: &str = include_str!("../../../docs/cli-usage-guide.md");

/// The fenced `pattern` block that defines `BUNSETU`.
fn guide_catalog() -> String {
    let (_, block) = GUIDE
        .split_once("```pattern\nfragment BUNSETU")
        .expect("the guide defines the BUNSETU fragment");
    let (body, _) = block
        .split_once("```")
        .expect("the BUNSETU block is closed");
    format!("fragment BUNSETU{body}")
}

fn matched(pattern: &str, start: usize, end: usize, captures: &[(&str, usize, usize)]) -> Match {
    Match {
        pattern: pattern.into(),
        start,
        end,
        captures: captures
            .iter()
            .map(|(name, start, end)| Capture {
                name: (*name).into(),
                path: format!("{pattern}/{name}").into(),
                start: *start,
                end: *end,
            })
            .collect(),
        graph_bindings: Box::default(),
        interpretations: Box::default(),
        lexical: None,
    }
}

#[test]
fn the_guide_bunsetu_definitions_match_ginza_bunsetu_labels() {
    let catalog = compile_catalog(
        PatternSource::named("cli-usage-guide.md", &guide_catalog()),
        CompileLimits::default(),
    )
    .expect("the guide's bunsetu catalog compiles")
    .matcher;
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("upos", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("spacy.ginza.bunsetu", AttributeKind::Scalar).unwrap(),
    ])
    .unwrap();
    let bound = catalog
        .bind(&schema)
        .expect("the schema supplies every key");

    let run = |sentence: &[(&str, &str, &str)]| {
        let keys: Vec<&str> = bound
            .projection()
            .keys()
            .iter()
            .map(AttributeKey::as_str)
            .collect();
        let values = sentence
            .iter()
            .flat_map(|(text, upos, bunsetu)| {
                keys.iter().map(move |key| {
                    Some(
                        match *key {
                            "text" => *text,
                            "upos" => *upos,
                            "spacy.ginza.bunsetu" => *bunsetu,
                            other => panic!("unexpected projected key {other}"),
                        }
                        .into(),
                    )
                })
            })
            .collect();
        let tokens =
            BoundTokenSentence::from_projected_values(bound.projection(), sentence.len(), values)
                .expect("the values follow the projection");
        bound.find_matches(&tokens).expect("schemas agree")
    };

    // 猫が|庭で|寝ている。 as `ginza -f conllu` labels it.
    let matches = run(&[
        ("猫", "NOUN", "B"),
        ("が", "ADP", "I"),
        ("庭", "NOUN", "B"),
        ("で", "ADP", "I"),
        ("寝", "VERB", "B"),
        ("て", "SCONJ", "I"),
        ("いる", "AUX", "I"),
        ("。", "PUNCT", "I"),
    ]);

    assert_eq!(
        matches,
        vec![
            matched("bunsetu", 0, 2, &[]),
            matched("ga_bunsetu", 0, 2, &[]),
            matched("bunsetu", 2, 4, &[]),
            matched("bunsetu", 4, 8, &[]),
            matched("verb_bunsetu", 4, 8, &[("head", 4, 5)]),
        ]
    );

    // GiNZA keeps the comma in the bunsetu, so 猫が、 does not end in が.
    let matches = run(&[
        ("猫", "NOUN", "B"),
        ("が", "ADP", "I"),
        ("、", "PUNCT", "I"),
        ("寝る", "VERB", "B"),
    ]);
    assert_eq!(
        matches,
        vec![
            matched("bunsetu", 0, 3, &[]),
            matched("bunsetu", 3, 4, &[]),
            matched("verb_bunsetu", 3, 4, &[("head", 3, 4)]),
        ]
    );
}

#[test]
fn span_annotations_filter_candidates_before_longest_selection() {
    use lingrep_core::token_match::SentenceSpan;
    let span = |start, end, governor, label: &str| SentenceSpan {
        kind: "bunsetsu".into(),
        start,
        end,
        head: Some(start),
        governor,
        label: Some(label.into()),
    };
    let spans = vec![span(0, 2, Some(1), "NP"), span(2, 3, None, "VP")];
    let run = |source, spans: Vec<SentenceSpan>| {
        let catalog = compile_catalog(
            PatternSource::named("spans", source),
            CompileLimits::default(),
        )
        .unwrap()
        .matcher;
        let bound = catalog
            .bind(
                &TokenSchema::new([SchemaAttribute::new("text", AttributeKind::Scalar).unwrap()])
                    .unwrap(),
            )
            .unwrap();
        let words = BoundTokenSentence::from_projected_values(
            bound.projection(),
            3,
            vec![Some("x".into()); 3 * bound.projection().len()],
        )
        .unwrap()
        .with_spans(spans)
        .unwrap();
        bound.find_matches(&words).unwrap()
    };
    let scoped = r#"within_span: {kind: "bunsetsu", label: "NP"}
                            phrase := ^ []+."#;
    assert_eq!(run(scoped, spans.clone()), [matched("phrase", 0, 2, &[])]);
    assert!(run(scoped, Vec::new()).is_empty());
    let heads = r#"within_span: {kind: "bunsetsu", capture: "h", position: "head"}
        head := (?<h> []) []."#;
    assert_eq!(
        run(heads, spans.clone()),
        [matched("head", 0, 2, &[("h", 0, 1)])]
    );
    let chunks = r#"within_span: {kind: "bunsetsu", position: "whole"}
        chunk := []+."#;
    assert_eq!(
        run(chunks, spans.clone()),
        [matched("chunk", 0, 2, &[]), matched("chunk", 2, 3, &[])]
    );
    let edges = r#"span_dependency: {kind: "bunsetsu", dependent: "d", governor: "g"}
                           edge := ^ (?<d> []+) (?<g> []) $."#;
    assert_eq!(
        run(edges, spans.clone()),
        [matched("edge", 0, 3, &[("d", 0, 2), ("g", 2, 3)])]
    );
    let head_edge = r#"span_dependency: "bunsetsu: $g.head -> $d.head"
        edge := ^ (?<d> []) [] (?<g> []) $."#;
    assert_eq!(
        run(head_edge, spans.clone()),
        [matched("edge", 0, 3, &[("d", 0, 1), ("g", 2, 3)])]
    );
    let chain = r#"span_dependency: "bunsetsu: $g.whole -> $m.head -> $d"
        chain := ^ (?<d> []) (?<m> []) (?<g> []) $."#;
    let mut layers = vec![
        span(0, 1, Some(1), "D"),
        span(1, 2, None, "M"),
        span(1, 2, Some(3), "M"),
        span(2, 3, None, "G"),
    ];
    assert!(run(chain, layers.clone()).is_empty());
    layers[0].governor = Some(2);
    assert_eq!(
        run(chain, layers),
        [matched(
            "chain",
            0,
            3,
            &[("d", 0, 1), ("m", 1, 2), ("g", 2, 3)]
        )]
    );
    let labelled = r#"span_dependency: ["bunsetsu: $g.head@VP -> $d.whole@NP", "bunsetsu: $g.whole"]
        edge := ^ (?<d> []+) (?<g> []) $."#;
    assert_eq!(run(labelled, spans.clone()), run(edges, spans.clone()));
    let mut wrong_label = spans.clone();
    wrong_label[1].label = None;
    assert!(run(labelled, wrong_label).is_empty());
    let native = r#"edge := ^ (?<d> []+) (?<g> []) $
        where span(bunsetsu, $g.head@"VP" -> $d.whole@NP), span("bunsetsu", $g.whole)."#;
    assert_eq!(run(native, spans.clone()), run(labelled, spans.clone()));
    let variable =
        r#"edge := ^ (?<d> []+) [] (?<g> []) $ where span(bunsetsu, $g.head -> $d.head)."#;
    assert_eq!(run(variable, spans.clone()), run(head_edge, spans.clone()));
    let reversed = vec![span(0, 2, None, "NP"), span(2, 3, Some(0), "VP")];
    assert!(run(edges, reversed).is_empty());
    for spaced in [
        r#"span_dependency: "bunsetsu: $g.head@ VP -> $d.whole@ NP"
            edge := ^ (?<d> []+) (?<g> []) $."#,
        r#"span_dependency: "bunsetsu: $g.head @VP -> $d.whole @NP"
            edge := ^ (?<d> []+) (?<g> []) $."#,
        r#"span_dependency: " bunsetsu :$ g . head @ VP->$d . whole @ NP "
            edge := ^ (?<d> []+) (?<g> []) $."#,
        r#"edge := ^ (?<d> []+) (?<g> []) $
            where span( bunsetsu ,$ g . head @ VP->$d . whole @ NP )."#,
    ] {
        assert_eq!(
            run(spaced, spans.clone()),
            run(edges, spans.clone()),
            "{spaced}"
        );
    }
    let nested: Vec<_> = [
        span(0, 1, Some(1), "NP"),
        span(0, 2, Some(2), "Inner"),
        span(0, 2, Some(3), "Outer"),
        span(0, 3, None, "S"),
        span(2, 3, Some(3), "Sibling"),
    ]
    .into_iter()
    .map(|mut annotation| {
        annotation.kind = "phrase".into();
        annotation
    })
    .collect();
    let coextensive = r#"
        inner := ^ (?<phrase> []{2}) where span(phrase, $phrase.whole@Inner).
        outer := ^ (?<phrase> []{2}) where span(phrase, $phrase.whole@Outer)."#;
    assert_eq!(
        run(coextensive, nested.clone()),
        [
            matched("inner", 0, 2, &[("phrase", 0, 2)]),
            matched("outer", 0, 2, &[("phrase", 0, 2)]),
        ]
    );

    let direct = r#"edge := ^ (?<child> []) [] (?<last> []) $
        where span(phrase, $last.inside@S -> $child.whole@NP)."#;
    assert!(run(direct, nested.clone()).is_empty());
    let mut direct_governor = nested;
    direct_governor[0].governor = Some(3);
    assert_eq!(
        run(direct, direct_governor),
        [matched("edge", 0, 3, &[("child", 0, 1), ("last", 2, 3)])]
    );

    let inherited = r#"defaults
            within_span: {kind: "bunsetsu", label: "NP"}
        end
        fragment any := [].
        lexicon words by [text] := {"x"}.
        phrase := ^ @any+."#;
    let unconstrained: Vec<_> = run(inherited, spans.clone())
        .into_iter()
        .map(|found| (found.pattern.to_string(), found.start, found.end))
        .collect();
    assert_eq!(
        unconstrained,
        [
            ("words".to_owned(), 0, 1),
            ("phrase".to_owned(), 0, 2),
            ("words".to_owned(), 1, 2),
            ("words".to_owned(), 2, 3),
        ]
    );
}

#[test]
fn a_span_constraint_written_on_a_fragment_is_reported_at_that_annotation() {
    let source = r#"defaults
            within_span: "bunsetsu"
        end
        span_dependency: "chunk: $a"
        fragment f := (?<a> []).
        p := @f."#;
    let error = compile_catalog(
        PatternSource::named("span", source),
        CompileLimits::default(),
    )
    .err()
    .expect("a fragment cannot carry a span constraint");
    let reported = source.get(error.diagnostics[0].location().byte_range());
    assert!(
        reported.is_some_and(|text| text.starts_with("span_dependency")),
        "{reported:?}"
    );
}

#[test]
fn malformed_span_constraints_fail_during_compilation() {
    for source in [
        r#"p := (?<x> []) where span(chunk., $x)."#,
        r#"p := (?<x> []) where span(chunk, $x.head@)."#,
        r#"p := (?<x> []) where span(chunk, $x -> )."#,
        r#"span_dependency: "bunsetsu: $x.head ->" p := (?<x> [])."#,
        r#"span_dependency: "bunsetsu: $x.lemma" p := (?<x> [])."#,
        r#"span_dependency: "bunsetsu: $missing" p := []."#,
        r#"within_span: {kind: "bunsetsu", capture: "missing"} p := []."#,
        r#"within_span: {kind: "bunsetsu", position: "elsewhere"} p := []."#,
        r#"within_span: "" p := []."#,
        r#"span_dependency: "bunsetsu: $x@ " p := (?<x> [])."#,
        r#"span_dependency: "bunsetsu: $x. " p := (?<x> [])."#,
        r#"span_dependency: "bunsetsu: $x@ -> $x" p := (?<x> [])."#,
        r#"within_span: "bunsetsu" fragment f := []."#,
        r#"span_dependency: {kind: "bunsetsu", dependent: "a", governor: "a"} p := (?<a> [])."#,
    ] {
        assert!(
            compile_catalog(
                PatternSource::named("invalid", source),
                CompileLimits::default()
            )
            .is_err(),
            "{source}"
        );
    }
}

#[test]
fn native_span_diagnostics_point_to_the_invalid_operand() {
    for (source, needle) in [
        ("p := (?<n> []) where span(chunk, $n.hed).", "hed"),
        (
            "p := (?<n> []) where span(chunk, $missing.head).",
            "$missing",
        ),
        ("p := (?<n> [] []) where span(chunk, $n.head).", "$n"),
    ] {
        let error = compile_catalog(
            PatternSource::named("span", source),
            CompileLimits::default(),
        )
        .err()
        .expect("invalid operand must fail compilation");
        assert_eq!(
            source.get(error.diagnostics[0].location().byte_range()),
            Some(needle)
        );
    }
}

#[test]
fn span_operand_diagnostics_keep_their_catalog_source() {
    let mut builder = lingrep_core::fsm::FsmBuilder::new();
    let invalid = "p := (?<a> []) where span(chunk, $missing).";
    for (file, source) in [("first.lx", "first := []."), ("second.lx", invalid)] {
        builder
            .add_document(PatternSource::named(file, source))
            .unwrap();
    }
    let error = builder.build().err().expect("undefined capture");
    let location = error.diagnostics[0].location();
    assert_eq!(location.source_name(), "second.lx");
    assert_eq!(invalid.get(location.byte_range()), Some("$missing"));
}

#[test]
fn wo_bunsetsu_attachment_accepts_a_particle_governed_by_an_earlier_compound_member() {
    use lingrep_core::corpus::conllu;
    let catalog = compile_catalog(
        PatternSource::named(
            "wo-predicate-dep.lx",
            include_str!("../../../patterns/wo-predicate-dep.lx"),
        ),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;
    let bound = catalog.bind(&conllu::token_schema()).unwrap();
    let source = concat!(
        "1\t器質\t器質\tNOUN\t名詞\t_\t4\tobj\t_\tBunsetuBILabel=B\n",
        "2\t要因\t要因\tNOUN\t名詞\t_\t1\tcompound\t_\tBunsetuBILabel=I\n",
        "3\tを\tを\tADP\t助詞-格助詞\t_\t1\tcase\t_\tBunsetuBILabel=I\n",
        "4\t伴う\t伴う\tVERB\t動詞\t_\t0\troot\t_\tBunsetuBILabel=B\n",
    );
    let run = |input: &str| {
        let sentence = conllu::Reader::new(input.as_bytes(), "test")
            .next()
            .unwrap()
            .unwrap();
        bound
            .find_matches(&sentence.project(bound.projection()).unwrap())
            .unwrap()
    };
    let linear = matched(
        "wo_predicate_linear",
        1,
        4,
        &[("collocate", 1, 2), ("head", 3, 4)],
    );
    let phrase = matched(
        "wo_predicate_bunsetsu",
        1,
        4,
        &[
            ("collocate", 1, 2),
            ("marked", 1, 3),
            ("w", 2, 3),
            ("head", 3, 4),
        ],
    );
    assert_eq!(run(source), [phrase, linear.clone()]);
    assert_eq!(
        run(&source
            .replace("BunsetuBILabel=B", "_")
            .replace("BunsetuBILabel=I", "_")),
        [linear]
    );
}
