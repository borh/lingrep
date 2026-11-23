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
        interpretations: Box::default(),
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
        "a scoped transform drops no capture"
    );
}

#[test]
fn npv_collocation_excludes_a_non_particle_neighbor_with_the_same_head() {
    let sentence = dependency_sentence(&[
        DependencyNode {
            name: "対象",
            lemma: "対象",
            upos: "NOUN",
            head: Some("調べる"),
            relation: "obj",
        },
        DependencyNode {
            name: "を",
            lemma: "を",
            upos: "ADP",
            head: Some("対象"),
            relation: "case",
        },
        DependencyNode {
            name: "詳しく",
            lemma: "詳しい",
            upos: "ADV",
            head: Some("対象"),
            relation: "advmod",
        },
        DependencyNode {
            name: "調べる",
            lemma: "調べる",
            upos: "VERB",
            head: None,
            relation: "root",
        },
    ]);
    assert_eq!(
        match_catalog(
            "npv-collocation.lx",
            include_str!("../../../patterns/npv-collocation.lx"),
            &sentence
        ),
        Vec::<Match>::new(),
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

#[test]
fn word_graph_bindings_publish_norm_and_omit_absent_scalars() {
    use lingrep_core::corpus::conllu::EnhancedNodeId;
    use lingrep_core::fsm::{GraphBinding, GraphNodeAttribute};
    use lingrep_core::token_match::{AttributeKey, BoundTokenSentence, TokenSchema};

    let output = compile_catalog(
        PatternSource::named(
            "norm-binding.lx",
            "key: [$v.norm]\nnp := (?<n> [upos=NOUN]) where (?<v> [upos=VERB]) -[obj]-> $n.",
        ),
        CompileLimits::default(),
    )
    .unwrap();
    let schema = TokenSchema::from_keys([
        "text", "lemma", "norm", "upos", "xpos", "dep.head", "dep.rel",
    ])
    .unwrap();
    let bound = output
        .matcher
        .bind_with_projection(&schema, schema.keys().iter().cloned())
        .unwrap();
    let sentence = |verb_norm: Option<&str>, verb_xpos: Option<&str>| {
        let words: [[Option<&str>; 7]; 2] = [
            [
                Some("坂"),
                Some("坂"),
                Some("坂"),
                Some("NOUN"),
                Some("名詞"),
                Some("2"),
                Some("obj"),
            ],
            [
                Some("上った"),
                Some("上る"),
                verb_norm,
                Some("VERB"),
                verb_xpos,
                Some("0"),
                Some("root"),
            ],
        ];
        let order = [
            "text", "lemma", "norm", "upos", "xpos", "dep.head", "dep.rel",
        ];
        let values = words
            .iter()
            .flat_map(|word| {
                bound.projection().keys().iter().map(move |key| {
                    let slot = order.iter().position(|name| *name == key.as_str()).unwrap();
                    word[slot].map(Into::into)
                })
            })
            .collect();
        BoundTokenSentence::from_projected_values(bound.projection(), 2, values).unwrap()
    };
    let attribute = |name: &str, value: &str| GraphNodeAttribute {
        name: AttributeKey::new(name).unwrap(),
        value: value.into(),
    };
    let binding = |attributes: Vec<GraphNodeAttribute>| {
        vec![GraphBinding::Node {
            name: "v".into(),
            node: EnhancedNodeId::Word { token_index: 1 },
            attributes: attributes.into_boxed_slice(),
        }]
    };

    let present = bound
        .find_matches(&sentence(Some("上る"), Some("動詞")))
        .unwrap();
    assert_eq!(present.len(), 1);
    assert_eq!(
        present[0].graph_bindings.to_vec(),
        binding(vec![
            attribute("node.kind", "word"),
            attribute("node.id", "2"),
            attribute("text", "上った"),
            attribute("lemma", "上る"),
            attribute("norm", "上る"),
            attribute("upos", "VERB"),
            attribute("xpos", "動詞"),
        ])
    );

    let absent = bound.find_matches(&sentence(None, None)).unwrap();
    assert_eq!(absent.len(), 1);
    assert_eq!(
        absent[0].graph_bindings.to_vec(),
        binding(vec![
            attribute("node.kind", "word"),
            attribute("node.id", "2"),
            attribute("text", "上った"),
            attribute("lemma", "上る"),
            attribute("upos", "VERB"),
        ])
    );
}

#[test]
fn counted_edges_select_heads_and_preserve_unknown_counts() {
    let source = "p := (?<v> [upos=VERB]) where $v -[*]-> [upos=NOUN]{2}.";
    let root = "1\tv\tv\tVERB\t_\t_\t0\troot\t_\t_\n";
    let mut sentence = root.to_owned();
    for count in 0..=3 {
        let expected = if count == 2 {
            vec![matched("p", 0, 1, &[("v", 0, 1)])]
        } else {
            Vec::new()
        };
        assert_eq!(match_catalog("count.lx", source, &sentence), expected);
        let measure = "key: [$n]\np := (?<v> [upos=VERB]) where $v -[*]-> [upos=NOUN] as $n.";
        let mut measured = matched("p", 0, 1, &[("v", 0, 1)]);
        measured.graph_bindings = Box::new([lingrep_core::fsm::GraphBinding::Count {
            name: "n".into(),
            value: Some(count),
        }]);
        assert_eq!(match_catalog("counts.lx", measure, &sentence), [measured]);
        let (rows, _) = publish_over_conllu("counts.lx", measure, &sentence);
        assert_eq!(rows[0].key.as_ref().unwrap()[0].value, count.to_string());
        sentence.push_str(&format!("{}\tn\tn\tNOUN\t_\t_\t1\tobj\t_\t_\n", count + 2));
    }
    let zero = source.replace("{2}", "{0}");
    assert_eq!(
        match_catalog("zero.lx", &zero, root),
        [matched("p", 0, 1, &[("v", 0, 1)])]
    );
    let missing = format!("{root}2\tn\tn\t_\t_\t_\t1\tobj\t_\t_\n");
    assert!(match_catalog("unknown.lx", &zero, &missing).is_empty());
    let measure = "key: [$n]\np := (?<v> [upos=VERB]) where $v -[*]-> [upos=NOUN] as $n.";
    let (rows, stats) = publish_over_conllu("unknown.lx", measure, &missing);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        &*rows[0].raw.graph_bindings,
        &[lingrep_core::fsm::GraphBinding::Count {
            name: "n".into(),
            value: None
        }]
    );
    assert_eq!(rows[0].key, None);
    assert_eq!(stats.records_unkeyed, 1);
}

#[test]
fn comparisons_filter_bound_word_assignments_before_publication() {
    let source = "p := (?<v> [upos=VERB]) where $v -[nsubj]-> (?<s> []), $v -[obj]-> (?<o> []), $s.lemma = $o.lemma, $s.surface != $o.surface.";
    let inventory = source
        .replace(", $s.lemma = $o.lemma, $s.surface != $o.surface", "")
        .replace(
            "(?<v> [upos=VERB])",
            "(?<v> [upos=VERB & surface~/.+/ & lemma~/.+/])",
        );
    let sentence = "1\t私\t私\tPRON\t_\t_\t3\tnsubj\t_\t_\n2\tわたし\t私\tPRON\t_\t_\t3\tobj\t_\t_\n3\t見る\t見る\tVERB\t_\t_\t0\troot\t_\t_\n";
    let expected = match_catalog("inventory.lx", &inventory, sentence);
    assert_eq!(expected.len(), 1);
    assert_eq!(match_catalog("compare.lx", source, sentence), expected);
    for variant in [
        sentence.replace("わたし\t私", "私\t私"),
        sentence.replace("わたし\t私", "彼\t彼"),
        sentence.replace("わたし\t私", "わたし\t_"),
    ] {
        assert!(match_catalog("compare.lx", source, &variant).is_empty());
    }
}

#[test]
fn interpreted_readings_must_satisfy_the_published_graph_assignment() {
    let sentence = "1\ta\ta\tNOUN\t_\t_\t2\tobj\t_\t_\n2\tb\tb\tVERB\t_\t_\t0\troot\t_\t_\n";
    let annotations = "interpret $left as slot {side: 1}\ninterpret $right as slot {side: 2}\n";
    let source = format!(
        "{annotations}p := ((?<left> (?<d> [])) [] | [] (?<right> (?<e> []))) where (?<h> []) -[obj]-> $d."
    );
    let hits = match_catalog("readings.lx", &source, sentence);
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.graph_bindings.len(), 1);
    assert_eq!(hit.interpretations.len(), 1);
    assert_eq!(hit.interpretations[0].roles.len(), 1);
    assert_eq!(hit.interpretations[0].roles[0].start, 0);
}

#[test]
fn one_edge_per_dependent_makes_pooled_label_counts_additive() {
    let inventory = "edge := (?<d> []) where (?<v> [upos=VERB]) -[nsubj|obj]-> $d.";
    let sentence = "1\tv\tv\tVERB\t_\t_\t0\troot\t_\t_\n2\ta\ta\tNOUN\t_\t_\t1\tobj\t_\t_\n3\tb\tb\tNOUN\t_\t_\t1\tnsubj\t_\t_\n4\tc\tc\tNOUN\t_\t_\t1\tobj\t_\t_\n";
    let pooled = match_catalog("pooled.lx", inventory, sentence);
    let mut separate = Vec::new();
    for (label, expected) in [("nsubj", 1), ("obj", 2)] {
        let hits = match_catalog("label.lx", &inventory.replace("nsubj|obj", label), sentence);
        assert_eq!(hits.len(), expected);
        separate.extend(hits);
    }
    separate.sort_by_key(|hit| hit.start);
    assert_eq!(pooled, separate);
}

#[test]
fn measures_keep_the_verb_and_its_original_sentence_coordinates() {
    let source = "test nominal := upos=NOUN.
        key: [$distance]
        p := (?<v> [upos=VERB]) measure mean(($v -[*]-> [@nominal]).length) as $distance.";
    let sentence = "1\tn\tn\tNOUN\t_\t_\t3\tobj\t_\t_\n2\tn\tn\tNOUN\t_\t_\t3\tnsubj\t_\t_\n3\tv\tv\tVERB\t_\t_\t0\troot\t_\t_\n";
    let (rows, _) = publish_over_conllu("measure.lx", source, sentence);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].key.as_ref().unwrap()[0].value, "1.5");
}

#[test]
fn invalid_measurements_are_rejected_at_compile_time() {
    for source in [
        "fragment f := [] measure count(match.words[]) as $n.",
        "p := [] measure count(match.words[]) as $n, max(tree(word).nodes.depth) as $n.",
        "p := (?<v> []) measure count(match.words[]) as $v.",
        "p := [] measure tree(word).depth($missing) as $n.",
        "p := (?<v> []+) measure tree(word).depth($v) as $n.",
        "p := [] measure mean(tree(word).links.depth) as $n.",
        "p := [] measure max(match.words[]) as $n.",
        "p := [] measure count(match.words[@undefined]) as $n.",
    ] {
        assert!(
            compile_catalog(
                PatternSource::named("invalid.lx", source),
                CompileLimits::default()
            )
            .is_err(),
            "{source}"
        );
    }
}

fn measured(hit: &Match) -> Vec<(&str, Option<f64>, &'static str)> {
    hit.graph_bindings
        .iter()
        .filter_map(|binding| match binding {
            lingrep_core::fsm::GraphBinding::Measure { name, value } => {
                Some((name.as_str(), value.number(), value.status()))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_word_without_a_set_feature_does_not_hold_it() {
    let sentence = "1\ta\ta\tNOUN\t_\tCase=Nom\t3\tnsubj\t_\t_\n2\tb\tb\tNOUN\t_\t_\t3\tobj\t_\t_\n3\tv\tv\tVERB\t_\t_\t0\troot\t_\t_\n";
    let whole = "p := ^ []+ $ measure count(match.words[feat.Case has Nom]) as $words.";
    assert_eq!(
        measured(&match_catalog("words.lx", whole, sentence)[0]),
        [("words", Some(1.0), "ok")]
    );
    let verb = "p := (?<v> [upos=VERB]) where $v -[*]-> [feat.Case has Nom] as $n
        measure count($v -[*]-> [feat.Case has Nom]) as $links.";
    let hit = &match_catalog("links.lx", verb, sentence)[0];
    assert_eq!(measured(hit), [("links", Some(1.0), "ok")]);
    assert!(
        hit.graph_bindings
            .contains(&lingrep_core::fsm::GraphBinding::Count {
                name: "n".into(),
                value: Some(1)
            })
    );
}

#[test]
fn a_missing_scalar_leaves_a_count_unknown_only_when_no_alternative_holds() {
    let source = "p := ^ []+ $ measure count(match.words[upos=NOUN | xpos=NN]) as $n.";
    let decided = "1\ta\ta\tNOUN\t_\t_\t3\tdep\t_\t_\n2\tb\tb\tX\tNN\t_\t3\tdep\t_\t_\n3\tv\tv\tVERB\tVB\t_\t0\troot\t_\t_\n";
    assert_eq!(
        measured(&match_catalog("decided.lx", source, decided)[0]),
        [("n", Some(2.0), "ok")]
    );
    let undecided = format!("{decided}4\tc\tc\tX\t_\t_\t3\tdep\t_\t_\n");
    assert_eq!(
        measured(&match_catalog("undecided.lx", source, &undecided)[0]),
        [("n", None, "unavailable")]
    );
}

#[test]
fn tree_measures_on_every_word_of_a_long_sentence_stay_within_the_budget() {
    let source =
        "p := [] measure mean(tree(word).links.length) as $a, mean(tree(word).nodes.depth) as $b,
        max(tree(word).nodes.depth) as $c, max(tree(word).boundaries.open_links) as $d.";
    let words = 2_000;
    let sentence: String = (1..=words)
        .map(|id| {
            let relation = if id == 1 { "root" } else { "dep" };
            format!("{id}\tw\tw\tX\t_\t_\t{}\t{relation}\t_\t_\n", id - 1)
        })
        .collect();
    let hits = match_catalog("long.lx", source, &sentence);
    assert_eq!(hits.len(), words);
    assert_eq!(
        measured(&hits[words - 1]),
        [
            ("a", Some(1.0), "ok"),
            ("b", Some((words - 1) as f64 / 2.0), "ok"),
            ("c", Some((words - 1) as f64), "ok"),
            ("d", Some(1.0), "ok")
        ]
    );
}

#[test]
fn a_root_relation_may_carry_a_subtype() {
    let sentence = "1\ta\ta\tX\t_\t_\t0\troot:x\t_\t_\n2\tb\tb\tX\t_\t_\t1\tdep\t_\t_\n";
    let source = "p := ^ []+ $ measure max(tree(word).nodes.depth) as $height.";
    assert_eq!(
        measured(&match_catalog("subtype.lx", source, sentence)[0]),
        [("height", Some(1.0), "ok")]
    );
}

#[test]
fn readings_with_different_measures_are_separate_matches() {
    let sentence = "1\tn\tn\tNOUN\t_\t_\t3\tobj\t_\t_\n2\tp\tp\tADP\t_\t_\t1\tcase\t_\t_\n3\tv\tv\tVERB\t_\t_\t0\troot\t_\t_\n";
    let source = "interpret $a as reading {side: 1}\ninterpret $b as reading {side: 2}\n
        p := ((?<a> [upos=NOUN]) [upos=ADP] | [upos=NOUN] (?<b> [upos=ADP]))
        measure tree(word).depth($a) as $da, tree(word).depth($b) as $db.";
    let mut readings: Vec<_> = match_catalog("readings.lx", source, sentence)
        .iter()
        .map(|hit| {
            assert_eq!(hit.interpretations.len(), 1);
            (hit.interpretations[0].roles[0].start, measured(hit))
        })
        .map(|(start, values)| (start, format!("{values:?}")))
        .collect();
    readings.sort();
    assert_eq!(
        readings,
        [
            (
                0,
                r#"[("da", Some(1.0), "ok"), ("db", None, "unavailable")]"#.to_owned()
            ),
            (
                1,
                r#"[("da", None, "unavailable"), ("db", Some(2.0), "ok")]"#.to_owned()
            )
        ]
    );
}

#[test]
fn readings_with_different_measures_share_the_longest_span_from_their_start() {
    // From the noun the body matches one token with one set of measures and
    // two tokens with two others.
    let sentence = "1\tn\tn\tNOUN\t_\t_\t3\tobj\t_\t_\n2\tp\tp\tADP\t_\t_\t1\tcase\t_\t_\n3\tv\tv\tVERB\t_\t_\t0\troot\t_\t_\n";
    let source = "interpret $a as reading {side: 1}\ninterpret $b as reading {side: 2}\n
        interpret $c as reading {side: 3}\n
        p := ((?<c> [upos=NOUN]) | (?<a> [upos=NOUN]) [upos=ADP] | [upos=NOUN] (?<b> [upos=ADP]))
        measure tree(word).depth($a) as $da, tree(word).depth($b) as $db,
                tree(word).depth($c) as $dc.
        longest := [upos=NOUN] [upos=ADP]?.
        short := [upos=NOUN].";
    let hits = match_catalog("readings.lx", source, sentence);
    let published: Vec<_> = hits
        .iter()
        .filter(|hit| hit.pattern == "p")
        .map(|hit| (hit.start, hit.end, format!("{:?}", measured(hit))))
        .collect();
    assert_eq!(
        published,
        [
            (
                0,
                2,
                r#"[("da", Some(1.0), "ok"), ("db", None, "unavailable"), ("dc", None, "unavailable")]"#.to_owned()
            ),
            (
                0,
                2,
                r#"[("da", None, "unavailable"), ("db", Some(2.0), "ok"), ("dc", None, "unavailable")]"#.to_owned()
            ),
        ]
    );
    let checks = compile_catalog(
        PatternSource::named("readings.lx", source),
        CompileLimits::default(),
    )
    .expect("the fixture compiles")
    .no_match;
    use lingrep_core::fsm::Containment;
    assert_eq!(checks.containment("p", "longest"), Containment::Contained);
    assert_eq!(checks.containment("longest", "p"), Containment::Contained);
    assert!(matches!(
        checks.containment("short", "p"),
        Containment::Distinguished { span, .. } if span == (0..1)
    ));
}

#[test]
fn an_undefined_measure_capture_is_reported_against_the_measure_clause() {
    let error = compile_catalog(
        PatternSource::named("m.lx", "p := [] measure tree(word).depth($missing) as $n."),
        CompileLimits::default(),
    )
    .err()
    .expect("an undefined capture does not compile")
    .to_string();
    assert!(error.contains("`measure` operand `$missing`"), "{error}");
}

#[test]
fn basic_path_publishes_the_same_endpoints_as_a_direct_edge() {
    let chain = "1\tverb\tv\tVERB\t_\t_\t0\troot\t_\t_\n2\tparticle\tp\tADP\t_\t_\t1\tcomp:obj\t_\t_\n3\tnoun\tn\tNOUN\t_\t_\t2\tcomp\t_\t_\n";
    let direct = chain.replace("2\tcomp\t_", "1\tcomp\t_");
    let expected = match_catalog(
        "p",
        "p := (?<n> [upos=NOUN]) where (?<v> [upos=VERB]) -[comp]-> $n.",
        &direct,
    );
    assert_eq!(expected.len(), 1);
    let matches = |path| {
        let pattern = format!("p := (?<n> [upos=NOUN]) where (?<v> [upos=VERB]) -[{path}]-> $n.");
        match_catalog("p", &pattern, chain)
    };
    for path in [
        "+ comp|comp:obj",
        "+ comp first comp:obj",
        "+ comp:obj last comp",
    ] {
        assert_eq!(matches(path), expected);
    }
    for misplaced_boundary in ["+ comp last comp:obj", "+ comp:obj first comp"] {
        assert!(matches(misplaced_boundary).is_empty());
    }
    let decorated = chain
        .replace("comp:obj\t", "comp:obj@expl\t")
        .replace("comp\t", "comp@pass\t");
    for path in [
        "+ comp@pass first comp:obj@expl",
        "+ comp:obj@!pass last *@pass",
    ] {
        let pattern = format!("p := (?<n> [upos=NOUN]) where (?<v> [upos=VERB]) -[{path}]-> $n.");
        assert_eq!(match_catalog("p", &pattern, &decorated), expected);
        assert!(match_catalog("p", &pattern, chain).is_empty());
    }
}

#[test]
fn basic_path_negation_requires_an_observed_chain_to_root() {
    let sentence = "1\th\th\tVERB\t_\t_\t0\troot\t_\t_\n2\td\td\tNOUN\t_\t_\t1\tobj\t_\t_\n";
    let pattern = |edge: &str| format!("p := (?<h> [text=h]) (?<d> [text=d]) where {edge}.");
    let expected = match_catalog("p", &pattern("$h -[obj]-> $d"), sentence);
    assert_eq!(expected.len(), 1);
    for edge in [
        "$h -[+ conj first obj]-> $d",
        "$h -[+ conj last obj]-> $d",
        "not $h -[+ conj]-> $d",
    ] {
        assert_eq!(match_catalog("p", &pattern(edge), sentence), expected);
    }
    assert!(match_catalog("p", &pattern("not $h -[+ *]-> $d"), sentence).is_empty());
    for malformed in [
        sentence.replace("0\troot", "2\tdep"),
        sentence.replace("0\troot", "_\t_"),
    ] {
        for edge in ["$h -[+ *]-> $d", "not $h -[+ conj]-> $d"] {
            assert!(match_catalog("p", &pattern(edge), &malformed).is_empty());
        }
    }
}
