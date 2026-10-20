// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::panic_in_result_fn, clippy::unwrap_used)]

use lingrep_core::fsm::{
    Capture, CatalogPolicy, CompileLimits, Match, PatternSource, PolicyStats, ProgramStats,
    PublishedRecord, apply_policy_records, compile_catalog,
};

#[derive(Clone, Copy)]
struct DependencyNode<'a> {
    name: &'a str,
    lemma: &'a str,
    upos: &'a str,
    head: Option<&'a str>,
    relation: &'a str,
}

fn dependency_sentence(nodes: &[DependencyNode<'_>]) -> String {
    let mut sentence = String::new();
    for (offset, node) in nodes.iter().enumerate() {
        let head = node.head.map_or(0, |head| {
            nodes
                .iter()
                .position(|candidate| candidate.name == head)
                .unwrap_or_else(|| panic!("fixture head {head} must exist"))
                + 1
        });
        sentence.push_str(&format!(
            "{}\t{}\t{}\t{}\t_\t_\t{}\t{}\t_\t_\n",
            offset + 1,
            node.name,
            node.lemma,
            node.upos,
            head,
            node.relation,
        ));
    }
    sentence
}

fn captures<'a>(nodes: &[DependencyNode<'a>], names: &[&'a str]) -> Vec<(&'a str, usize, usize)> {
    names
        .iter()
        .map(|name| {
            let start = nodes
                .iter()
                .position(|node| node.name == *name)
                .unwrap_or_else(|| panic!("fixture capture {name} must exist"));
            (*name, start, start + 1)
        })
        .collect()
}

fn match_catalog(source_name: &str, source: &str, sentence: &str) -> Vec<Match> {
    let compiled = compile_catalog(
        PatternSource::named(source_name, source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("{source_name} must compile: {error}"))
    .matcher;
    let bound = compiled
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap_or_else(|error| panic!("{source_name} must bind to CoNLL-U: {error}"));
    let mut reader = lingrep_core::corpus::conllu::Reader::new(
        std::io::BufReader::new(sentence.as_bytes()),
        "dependency-repertoire-test",
    );
    let sentence = reader
        .next()
        .expect("fixture holds one sentence")
        .expect("fixture parses");
    assert!(reader.next().is_none(), "fixture holds one sentence");
    let sentence = sentence
        .project(bound.projection())
        .expect("fixture projects to the catalog schema");
    bound.find_matches(&sentence).expect("bound schema matches")
}

/// The publication sibling of [`match_catalog`]: the records a catalog
/// publishes over one CoNLL-U sentence, and the stats that publication kept.
///
/// `match_catalog` stops at the matcher, which is the right boundary for a
/// `where` clause. A catalog whose point is a `transform:` needs the seam
/// after it, because a scoped rewrite is invisible in a `Match`.
fn publish_over_conllu(
    source_name: &str,
    source: &str,
    sentence: &str,
) -> (Vec<PublishedRecord>, PolicyStats) {
    let output = compile_catalog(
        PatternSource::named(source_name, source),
        CompileLimits::default(),
    )
    .unwrap_or_else(|error| panic!("{source_name} must compile: {error}"));
    // `bind` resolves the attributes the catalog *matches* on. A `transform:`
    // reads more than that — `join` renders the written form of every token in
    // its span — so binding without the run's projection leaves those tokens
    // rendering empty and the unit silently short.
    let bound = output
        .matcher
        .bind_with_projection(
            &lingrep_core::corpus::conllu::token_schema(),
            lingrep_core::execution::cached_run_attributes(&output.matcher),
        )
        .unwrap_or_else(|error| panic!("{source_name} must bind to CoNLL-U: {error}"));
    let mut reader = lingrep_core::corpus::conllu::Reader::new(
        std::io::BufReader::new(sentence.as_bytes()),
        "dependency-repertoire-test",
    );
    let sentence = reader
        .next()
        .expect("fixture holds one sentence")
        .expect("fixture parses");
    assert!(reader.next().is_none(), "fixture holds one sentence");
    let sentence = sentence
        .project(bound.projection())
        .expect("fixture projects to the catalog schema");

    let matches = bound.find_matches(&sentence).expect("bound schema matches");
    let policy: &CatalogPolicy = output.matcher.catalog_policy();
    let anchors = output.matcher.anchor_positions(&sentence);
    let mut stats = PolicyStats::default();
    let mut program = ProgramStats::for_program(policy.program());
    let records = apply_policy_records(
        policy,
        &matches,
        &sentence,
        &anchors,
        &mut stats,
        &mut program,
        None,
    )
    .expect("the positions come from this catalog's own compilation");
    (records, stats)
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
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        graph_bindings: Box::default(),
        lexical: None,
    }
}

#[test]
fn passive_agent_catalog_covers_both_orders_and_requires_a_shared_predicate() {
    let catalog = include_str!("../../../patterns/passive-agent-dep.lx");
    let patient = DependencyNode {
        name: "patient",
        lemma: "作品",
        upos: "NOUN",
        head: Some("predicate"),
        relation: "nsubj:pass",
    };
    let agent = DependencyNode {
        name: "agent",
        lemma: "委員会",
        upos: "NOUN",
        head: Some("predicate"),
        relation: "obl:agent",
    };
    let predicate = DependencyNode {
        name: "predicate",
        lemma: "選ぶ",
        upos: "VERB",
        head: None,
        relation: "root",
    };
    for (pattern, nodes) in [
        ("patient_agent_predicate", [patient, agent, predicate]),
        ("agent_patient_predicate", [agent, patient, predicate]),
    ] {
        let sentence = dependency_sentence(&nodes);
        let expected_captures = captures(&nodes, &[nodes[0].name, nodes[1].name, "predicate"]);
        assert_eq!(
            match_catalog("passive-agent-dep.lx", catalog, &sentence),
            vec![matched(pattern, 0, 3, &expected_captures)],
            "{pattern} must cover its authored surface order",
        );
    }

    let broken = "1\t作品\t作品\tNOUN\t_\t_\t3\tnsubj:pass\t_\t_\n\
                  2\t委員会\t委員会\tNOUN\t_\t_\t1\tobl:agent\t_\t_\n\
                  3\t選ぶ\t選ぶ\tVERB\t_\t_\t0\troot\t_\t_\n";
    assert!(
        match_catalog("passive-agent-dep.lx", catalog, broken).is_empty(),
        "linear co-occurrence without the shared predicate must not publish"
    );
}

#[test]
fn passive_oblique_realization_retains_every_candidate_and_absent_head() {
    let catalog = include_str!("../../../patterns/passive-oblique-realization.lx");
    let first = DependencyNode {
        name: "first",
        lemma: "場所",
        upos: "NOUN",
        head: Some("marked"),
        relation: "obl",
    };
    let marked = DependencyNode {
        name: "marked",
        lemma: "認識",
        upos: "VERB",
        head: None,
        relation: "root",
    };
    let passive = DependencyNode {
        name: "passive",
        lemma: "れる",
        upos: "AUX",
        head: Some("marked"),
        relation: "aux",
    };
    let absent = DependencyNode {
        name: "absent",
        lemma: "示す",
        upos: "VERB",
        head: None,
        relation: "root",
    };
    let absent_passive = DependencyNode {
        name: "absent_passive",
        lemma: "られる",
        upos: "AUX",
        head: Some("absent"),
        relation: "aux",
    };
    let second = DependencyNode {
        name: "second",
        lemma: "方法",
        upos: "NOUN",
        head: Some("marked"),
        relation: "obl",
    };
    let nodes = [first, marked, passive, absent, absent_passive, second];

    assert_eq!(
        match_catalog(
            "passive-oblique-realization.lx",
            catalog,
            &dependency_sentence(&nodes),
        ),
        vec![
            matched(
                "japanese_aux_candidate_present",
                0,
                2,
                &[("candidate", 0, 1), ("predicate", 1, 2)],
            ),
            matched(
                "japanese_aux_candidate_present",
                1,
                6,
                &[("predicate", 1, 2), ("candidate", 5, 6)],
            ),
            matched(
                "japanese_aux_candidate_absent",
                3,
                4,
                &[("predicate", 3, 4)],
            ),
        ],
        "candidate assignments stay plural while absence publishes the head once",
    );
}

#[test]
fn passive_oblique_realization_uses_one_evidence_arm_per_predicate() {
    let catalog = include_str!("../../../patterns/passive-oblique-realization.lx");
    let sentence = "1\tvoice-candidate\t場所\tNOUN\t_\t_\t2\tobl\t_\t_\n\
                    2\tvoice-predicate\t認識\tVERB\t_\tVoice=Pass\t0\troot\t_\t_\n\
                    3\tvoice-aux-pass\tbe\tAUX\t_\t_\t2\taux:pass\t_\t_\n\
                    4\tvoice-japanese-aux\tれる\tAUX\t_\t_\t2\taux\t_\t_\n\
                    5\taux-candidate\t委員会\tPROPN\t_\t_\t6\tobl:agent\t_\t_\n\
                    6\taux-predicate\t選ぶ\tVERB\t_\t_\t0\troot\t_\t_\n\
                    7\taux-pass\tbe\tAUX\t_\t_\t6\taux:pass\t_\t_\n\
                    8\taux-japanese\tられる\tAUX\t_\t_\t6\taux\t_\t_\n";

    assert_eq!(
        match_catalog("passive-oblique-realization.lx", catalog, sentence),
        vec![
            matched(
                "voice_candidate_present",
                0,
                2,
                &[("candidate", 0, 1), ("predicate", 1, 2)],
            ),
            matched(
                "aux_pass_candidate_present",
                4,
                6,
                &[("candidate", 4, 5), ("predicate", 5, 6)],
            ),
        ],
        "Voice=Pass must outrank aux:pass, which must outrank the Japanese fallback",
    );
}

#[test]
fn passive_oblique_realization_does_not_turn_missing_lemmas_into_absence() {
    let catalog = include_str!("../../../patterns/passive-oblique-realization.lx");
    let sentence = "1\tcandidate\t_\tNOUN\t_\t_\t2\tobl\t_\t_\n\
                    2\tpredicate\t認識\tVERB\t_\tVoice=Pass\t0\troot\t_\t_\n";

    assert!(
        match_catalog("passive-oblique-realization.lx", catalog, sentence).is_empty(),
        "an edge-compatible candidate with no lemma makes its state unknown, not absent",
    );
}

#[test]
fn valency_catalog_covers_both_transitive_orders_without_exclusivity() {
    let catalog = include_str!("../../../patterns/ud-valency.lx");
    let subject = DependencyNode {
        name: "subject",
        lemma: "猫",
        upos: "NOUN",
        head: Some("predicate"),
        relation: "nsubj",
    };
    let object = DependencyNode {
        name: "object",
        lemma: "魚",
        upos: "NOUN",
        head: Some("predicate"),
        relation: "obj",
    };
    let predicate = DependencyNode {
        name: "predicate",
        lemma: "食べる",
        upos: "VERB",
        head: None,
        relation: "root",
    };

    for (pattern, nodes, transitive_captures) in [
        (
            "subject_object_predicate",
            [subject, object, predicate],
            ["subject", "object", "predicate"],
        ),
        (
            "object_subject_predicate",
            [object, subject, predicate],
            ["object", "subject", "predicate"],
        ),
    ] {
        let sentence = dependency_sentence(&nodes);
        let transitive_captures = captures(&nodes, &transitive_captures);
        let subject_captures = captures(&nodes, &["subject", "predicate"]);
        let subject_start = subject_captures[0].1;
        assert_eq!(
            match_catalog("ud-valency.lx", catalog, &sentence),
            vec![
                matched(pattern, 0, 3, &transitive_captures),
                matched("subject_predicate", subject_start, 3, &subject_captures,),
            ],
            "{pattern} must publish alongside the positive subject frame",
        );
    }
}

#[test]
fn valency_catalog_promotes_the_observed_relative_clause_argument_chain() {
    let catalog = include_str!("../../../patterns/ud-valency.lx");
    let nodes = [
        DependencyNode {
            name: "argument",
            lemma: "働き方",
            upos: "NOUN",
            head: Some("predicate"),
            relation: "obj",
        },
        DependencyNode {
            name: "predicate",
            lemma: "変える",
            upos: "VERB",
            head: Some("head"),
            relation: "acl",
        },
        DependencyNode {
            name: "head",
            lemma: "こと",
            upos: "NOUN",
            head: None,
            relation: "root",
        },
    ];
    let sentence = dependency_sentence(&nodes);
    assert_eq!(
        match_catalog("ud-valency.lx", catalog, &sentence),
        vec![matched(
            "relative_clause_argument",
            0,
            3,
            &captures(&nodes, &["argument", "predicate", "head"]),
        )],
    );

    let broken = "1\t働き方\t働き方\tNOUN\t_\t_\t2\tobj\t_\t_\n\
                  2\t変える\t変える\tVERB\t_\t_\t0\troot\t_\t_\n\
                  3\tこと\tこと\tNOUN\t_\t_\t0\troot\t_\t_\n";
    assert!(
        match_catalog("ud-valency.lx", catalog, broken).is_empty(),
        "a clause argument without the observed acl edge to the head must not publish"
    );
}

#[test]
fn valency_catalog_promotes_the_observed_adverbial_clause_argument_chain() {
    let catalog = include_str!("../../../patterns/ud-valency.lx");
    let nodes = [
        DependencyNode {
            name: "argument",
            lemma: "移民",
            upos: "NOUN",
            head: Some("clause"),
            relation: "obj",
        },
        DependencyNode {
            name: "clause",
            lemma: "受け入れる",
            upos: "VERB",
            head: Some("head"),
            relation: "advcl",
        },
        DependencyNode {
            name: "head",
            lemma: "認識",
            upos: "VERB",
            head: None,
            relation: "root",
        },
    ];
    let sentence = dependency_sentence(&nodes);
    assert_eq!(
        match_catalog("ud-valency.lx", catalog, &sentence),
        vec![matched(
            "adverbial_clause_argument",
            0,
            3,
            &captures(&nodes, &["argument", "clause", "head"]),
        )],
    );

    let broken = "1\t移民\t移民\tNOUN\t_\t_\t2\tobj\t_\t_\n\
                  2\t受け入れる\t受け入れる\tVERB\t_\t_\t0\troot\t_\t_\n\
                  3\t認識\t認識\tVERB\t_\t_\t0\troot\t_\t_\n";
    assert!(
        match_catalog("ud-valency.lx", catalog, broken).is_empty(),
        "a clause argument without the observed advcl edge to the matrix verb must not publish"
    );
}

#[test]
fn subtree_profile_catalog_covers_every_fork_and_chain_order() {
    let catalog = include_str!("../../../patterns/ud-subtree-profile.lx");
    let head = DependencyNode {
        name: "head",
        lemma: "走る",
        upos: "VERB",
        head: None,
        relation: "root",
    };
    let left = DependencyNode {
        name: "left",
        lemma: "猫",
        upos: "NOUN",
        head: Some("head"),
        relation: "nsubj",
    };
    let right = DependencyNode {
        name: "right",
        lemma: "速く",
        upos: "ADV",
        head: Some("head"),
        relation: "advmod",
    };
    for (pattern, nodes) in [
        ("head_left_right", [head, left, right]),
        ("left_head_right", [left, head, right]),
        ("left_right_head", [left, right, head]),
    ] {
        let sentence = dependency_sentence(&nodes);
        let expected_captures = captures(&nodes, &[nodes[0].name, nodes[1].name, nodes[2].name]);
        assert_eq!(
            match_catalog("ud-subtree-profile.lx", catalog, &sentence),
            vec![matched(pattern, 0, 3, &expected_captures)],
            "{pattern} must cover its authored fork order",
        );
    }

    let middle = DependencyNode {
        name: "middle",
        lemma: "猫",
        upos: "NOUN",
        head: Some("head"),
        relation: "nsubj",
    };
    let leaf = DependencyNode {
        name: "leaf",
        lemma: "黒い",
        upos: "ADJ",
        head: Some("middle"),
        relation: "amod",
    };
    for nodes in [
        [head, middle, leaf],
        [head, leaf, middle],
        [middle, head, leaf],
        [middle, leaf, head],
        [leaf, head, middle],
        [leaf, middle, head],
    ] {
        let sentence = dependency_sentence(&nodes);
        let expected_captures = captures(&nodes, &[nodes[0].name, nodes[1].name, nodes[2].name]);
        assert_eq!(
            match_catalog("ud-subtree-profile.lx", catalog, &sentence),
            vec![matched("dependency_chain", 0, 3, &expected_captures)],
            "dependency_chain must cover every surface order",
        );
    }
}

#[test]
fn nonprojectivity_catalog_covers_every_direction_and_rejects_nested_edges() {
    let catalog = include_str!("../../../patterns/ud-nonprojectivity.lx");
    let h1 = DependencyNode {
        name: "h1",
        lemma: "h1",
        upos: "VERB",
        head: None,
        relation: "root",
    };
    let h2 = DependencyNode {
        name: "h2",
        lemma: "h2",
        upos: "VERB",
        head: Some("h1"),
        relation: "conj",
    };
    let d1 = DependencyNode {
        name: "d1",
        lemma: "d1",
        upos: "NOUN",
        head: Some("h1"),
        relation: "obj",
    };
    let d2 = DependencyNode {
        name: "d2",
        lemma: "d2",
        upos: "NOUN",
        head: Some("h2"),
        relation: "obl",
    };
    for (pattern, nodes) in [
        ("heads_before_dependents", [h1, h2, d1, d2]),
        ("first_edge_reversed", [d1, h2, h1, d2]),
        ("second_edge_reversed", [h1, d2, d1, h2]),
        ("dependents_before_heads", [d1, d2, h1, h2]),
    ] {
        let sentence = dependency_sentence(&nodes);
        let expected_captures = captures(
            &nodes,
            &[nodes[0].name, nodes[1].name, nodes[2].name, nodes[3].name],
        );
        assert_eq!(
            match_catalog("ud-nonprojectivity.lx", catalog, &sentence),
            vec![matched(pattern, 0, 4, &expected_captures)],
            "{pattern} must cover its authored edge direction",
        );
    }

    let nested = "1\tA\tA\tVERB\t_\t_\t0\troot\t_\t_\n\
                  2\tB\tB\tVERB\t_\t_\t1\tconj\t_\t_\n\
                  3\tC\tC\tNOUN\t_\t_\t1\tobj\t_\t_\n\
                  4\tD\tD\tNOUN\t_\t_\t1\tobl\t_\t_\n";
    assert!(
        match_catalog("ud-nonprojectivity.lx", catalog, nested).is_empty(),
        "two nested edges must not be classified as a crossing"
    );
}

/// `npv-collocation.lx` publishes the normalized verb without narrowing the
/// record that carries it.
///
/// This is the catalog's whole reason for existing, and it is invisible to
/// [`match_catalog`], which stops at matching. A scoped `transform:` that was
/// silently treated as unscoped would still produce this match — and would
/// then narrow the record to the noun and drop the capture the `key:` needs.
/// So the assertion is on the published record: its span is still the sentence
/// it was found in, while its unit and key carry `理解する`.
///
/// The row is natsume-simple's own: `("談話", "を", "理解する")` is in the
/// occurrence table this catalog was ported against.
#[test]
fn npv_collocation_publishes_a_normalized_verb_over_an_unnarrowed_record() {
    let catalog = include_str!("../../../patterns/npv-collocation.lx");
    let sentence = dependency_sentence(&[
        DependencyNode {
            name: "談話",
            lemma: "談話",
            upos: "NOUN",
            head: Some("理解"),
            relation: "obj",
        },
        DependencyNode {
            name: "を",
            lemma: "を",
            upos: "ADP",
            head: Some("談話"),
            relation: "case",
        },
        DependencyNode {
            name: "理解",
            lemma: "理解",
            upos: "VERB",
            head: None,
            relation: "root",
        },
        // The substitution witness: the LEMMA column carries GiNZA's `norm_`,
        // so a する is spelled 為る there and the table rewrites it back.
        DependencyNode {
            name: "する",
            lemma: "為る",
            upos: "AUX",
            head: Some("理解"),
            relation: "aux",
        },
        // The truncation witness: natsume's normalizer stops before ため.
        DependencyNode {
            name: "ため",
            lemma: "ため",
            upos: "SCONJ",
            head: Some("理解"),
            relation: "mark",
        },
        DependencyNode {
            name: "に",
            lemma: "に",
            upos: "ADP",
            head: Some("理解"),
            relation: "case",
        },
    ]);

    let (records, stats) = publish_over_conllu("npv-collocation.lx", catalog, &sentence);
    let [record] = records.as_slice() else {
        panic!("expected one published record, got {}", records.len());
    };

    assert_eq!(
        (record.start, record.end),
        (0, 6),
        "the record keeps the span it was found in; only the capture is rewritten"
    );
    assert_eq!(record.unit.as_deref(), Some("理解する"));
    assert_eq!(
        record
            .key
            .as_ref()
            .expect("the key projects")
            .iter()
            .map(|field| field.value.clone())
            .collect::<Vec<_>>(),
        ["談話", "を", "理解する"],
        "the key reads the substitution, so it cannot disagree with the unit"
    );
    assert_eq!(
        (stats.records_unkeyed, stats.captures_dropped),
        (0, 0),
        "a scoped transform drops no capture, which is what vibrato-pipe-clx was"
    );
}

fn crossing_topology_oracle(heads: &[usize]) -> Vec<Match> {
    let orientations = [
        ("heads_before_dependents", ["h1", "h2", "d1", "d2"]),
        ("first_edge_reversed", ["d1", "h2", "h1", "d2"]),
        ("second_edge_reversed", ["h1", "d2", "d1", "h2"]),
        ("dependents_before_heads", ["d1", "d2", "h1", "h2"]),
    ];
    let mut expected = Vec::new();
    for (pattern, roles) in orientations {
        for a in 0..heads.len() {
            let mut passing = Vec::new();
            for b in a + 1..heads.len() {
                for c in b + 1..heads.len() {
                    for d in c + 1..heads.len() {
                        let positions = [a, b, c, d];
                        let position =
                            |role| positions[roles.iter().position(|r| *r == role).unwrap()];
                        if heads[position("d1")] == position("h1") + 1
                            && heads[position("d2")] == position("h2") + 1
                        {
                            passing.push((d, b, c));
                        }
                    }
                }
            }
            if let Some((d, b, c)) = passing.into_iter().max() {
                let positions = [a, b, c, d];
                let captures = roles
                    .iter()
                    .zip(positions)
                    .map(|(role, p)| (*role, p, p + 1))
                    .collect::<Vec<_>>();
                expected.push(matched(pattern, a, d + 1, &captures));
            }
        }
    }
    expected.sort_by(|a, b| (a.start, a.end, &a.pattern).cmp(&(b.start, b.end, &b.pattern)));
    expected
}

#[test]
fn nonprojectivity_matches_independent_generated_edge_topologies() {
    let source = include_str!("../../../patterns/ud-nonprojectivity.lx");
    let output = compile_catalog(
        PatternSource::named("ud-nonprojectivity.lx", source),
        CompileLimits::default(),
    )
    .unwrap();
    let bound = output
        .matcher
        .bind(&lingrep_core::corpus::conllu::token_schema())
        .unwrap();
    for (length, count) in [(4, 625usize), (6, 512)] {
        for seed in 0..count {
            let mut state = seed;
            let heads = (0..length)
                .map(|_| {
                    let head = state % (length + 1);
                    state = if length == 4 {
                        state / (length + 1)
                    } else {
                        state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223)
                    };
                    head
                })
                .collect::<Vec<_>>();
            let sentence = heads
                .iter()
                .enumerate()
                .map(|(i, head)| format!("{}\tw{i}\tw{i}\tNOUN\t_\t_\t{head}\tobj\t_\t_\n", i + 1))
                .collect::<String>();
            let mut reader = lingrep_core::corpus::conllu::Reader::new(
                std::io::BufReader::new(sentence.as_bytes()),
                "topology-oracle",
            );
            let sentence = reader
                .next()
                .unwrap()
                .unwrap()
                .project(bound.projection())
                .unwrap();
            assert_eq!(
                bound.find_matches(&sentence).unwrap(),
                crossing_topology_oracle(&heads),
                "heads={heads:?}"
            );
        }
    }
}

#[test]
fn captured_relations_wait_for_reentered_captures_and_preserve_branch_semantics() {
    let sentence = "1\ta\ta\tVERB\t_\t_\t0\troot\t_\t_\n\
                    2\tb\tb\tVERB\t_\t_\t1\tconj\t_\t_\n\
                    3\td\td\tNOUN\t_\t_\t2\tobj\t_\t_\n\
                    4\tx\tx\tNOUN\t_\t_\t1\tobl\t_\t_\n";
    for body in [
        "(?<d> [text=d]) (?<h> [])+",
        "(?<d> [text=d]) ((?<h> []))+",
        "(?<d> [text=d]) (?<h> []){2}",
    ] {
        let reordered = "1\td\td\tNOUN\t_\t_\t3\tobj\t_\t_\n\
                         2\ta\ta\tVERB\t_\t_\t0\troot\t_\t_\n\
                         3\tb\tb\tVERB\t_\t_\t2\tconj\t_\t_\n";
        assert_eq!(
            match_catalog(
                "reentered",
                &format!("pattern p := {body} where $h -[obj]-> $d."),
                reordered
            ),
            vec![matched("p", 0, 3, &[("d", 0, 1), ("h", 2, 3)])],
            "{body}"
        );
    }
    let repeated_dependent = "1\th\th\tVERB\t_\t_\t0\troot\t_\t_\n\
                              2\tx\tx\tNOUN\t_\t_\t0\tobj\t_\t_\n\
                              3\td\td\tNOUN\t_\t_\t1\tobj\t_\t_\n";
    assert_eq!(
        match_catalog(
            "reentered-dependent",
            "pattern p := (?<h> []) (?<d> [])+ where $h -[obj]-> $d.",
            repeated_dependent
        ),
        vec![matched("p", 0, 3, &[("h", 0, 1), ("d", 2, 3)])],
        "a failing dependent may be rebound by the next repetition",
    );
    for (body, expected) in [
        (
            "(?<h> [text=b]) ((?<d> [text=d]))",
            vec![("h", 1, 2), ("d", 2, 3)],
        ),
        (
            "(?<h> [text=b]) (?<d> [text=d])?",
            vec![("h", 1, 2), ("d", 2, 3)],
        ),
        (
            "(?<h> [text=b]) ((?<d> [text=d]) | [text=x])",
            vec![("h", 1, 2), ("d", 2, 3)],
        ),
        (
            "(?<h> [text=b]) []* (?<d> [])",
            vec![("h", 1, 2), ("d", 2, 3)],
        ),
    ] {
        assert_eq!(
            match_catalog(
                "branch",
                &format!("pattern p := {body} where $h -[obj]-> $d."),
                sentence
            ),
            vec![matched("p", 1, 3, &expected)],
            "{body}: shorter passing assignments must survive"
        );
    }
}

#[test]
fn captured_relations_reject_missing_and_malformed_endpoint_observations() {
    use lingrep_core::token_match::{BoundTokenSentence, TokenSchema};
    let output = compile_catalog(
        PatternSource::named(
            "observation",
            "pattern p := (?<h> []) (?<d> []) where $h -[obj]-> $d.",
        ),
        CompileLimits::default(),
    )
    .unwrap();
    let bound = output
        .matcher
        .bind(&TokenSchema::from_keys(["dep.head", "dep.rel"]).unwrap())
        .unwrap();
    for (head, relation) in [
        (None, Some("obj")),
        (Some("oops"), Some("obj")),
        (Some("-1"), Some("obj")),
        (Some("999999999999999999999999999"), Some("obj")),
        (Some("3"), Some("obj")),
        (Some("0"), Some("obj")),
        (Some("1"), None),
        (Some("1"), Some("obl")),
        (Some("1"), Some("obj")),
    ] {
        let values = [(Some("0"), Some("root")), (head, relation)]
            .into_iter()
            .flat_map(|(head, relation)| {
                bound
                    .projection()
                    .keys()
                    .iter()
                    .map(move |key| match key.as_str() {
                        "dep.head" => head.map(Into::into),
                        "dep.rel" => relation.map(Into::into),
                        unexpected => panic!(
                            "observation fixture has no value for projected key {unexpected}"
                        ),
                    })
            })
            .collect();
        let tokens =
            BoundTokenSentence::from_projected_values(bound.projection(), 2, values).unwrap();
        let expected = if head == Some("1") && relation == Some("obj") {
            vec![matched("p", 0, 2, &[("h", 0, 1), ("d", 1, 2)])]
        } else {
            Vec::new()
        };
        assert_eq!(
            bound.find_matches(&tokens).unwrap(),
            expected,
            "head={head:?}, relation={relation:?}"
        );
    }
}
