//! The parts of orchestration that are decidable without running a campaign.

use std::io::Write as _;

use super::*;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lingrep-perf-orchestrate-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn write(path: &Path, contents: &str) {
    let mut file = std::fs::File::create(path).expect("scratch file");
    file.write_all(contents.as_bytes()).expect("scratch write");
}

#[test]
fn the_output_digest_ignores_exactly_the_run_variant_fields() {
    // The failure this replaces: a hand-written `sed` restating the field
    // list, wrong twice in one afternoon. The list belongs to the crate that
    // owns the schema, and this pins that it is the list being applied.
    let dir = scratch("output-digest");
    let (first, second, third) = (
        dir.join("a.jsonl"),
        dir.join("b.jsonl"),
        dir.join("c.jsonl"),
    );
    write(
        &first,
        "{\"type\":\"run\",\"data\":{\"run_id\":\"one\",\"published\":7}}\n\
         {\"type\":\"complete\",\"elapsed_millis\":1234,\"records\":7}\n",
    );
    write(
        &second,
        "{\"type\":\"run\",\"data\":{\"run_id\":\"two\",\"published\":7}}\n\
         {\"type\":\"complete\",\"elapsed_millis\":9999,\"records\":7}\n",
    );
    write(
        &third,
        "{\"type\":\"run\",\"data\":{\"run_id\":\"one\",\"published\":7}}\n\
         {\"type\":\"complete\",\"elapsed_millis\":1234,\"records\":8}\n",
    );

    assert_eq!(
        output_digest(&first).expect("digest"),
        output_digest(&second).expect("digest"),
        "run id and elapsed time vary per run and are not a parity difference"
    );
    assert_ne!(
        output_digest(&first).expect("digest"),
        output_digest(&third).expect("digest"),
        "a different record count is exactly the difference parity must catch"
    );
}

#[test]
fn key_order_does_not_change_the_output_digest() {
    // Re-encoding is what makes this true, and it is why the digest is not
    // simply a hash of the bytes.
    let dir = scratch("key-order");
    let (first, second) = (dir.join("a.jsonl"), dir.join("b.jsonl"));
    write(&first, "{\"b\":[{\"d\":4,\"c\":3}],\"a\":1}\n");
    write(&second, "{\"a\":1,\"b\":[{\"c\":3,\"d\":4}]}\n");
    assert_eq!(
        output_digest(&first).expect("digest"),
        output_digest(&second).expect("digest")
    );
}

#[test]
fn non_json_output_is_refused_rather_than_hashed_verbatim() {
    // Hashing raw bytes would work — and would silently compare unnormalized
    // output, which is the failure mode, not a fallback.
    let dir = scratch("not-jsonl");
    let path = dir.join("out.tsv");
    write(&path, "a\tb\tc\n");
    assert!(matches!(
        output_digest(&path),
        Err(OrchestrateError::NotJsonl { line: 1, .. })
    ));
}

#[test]
fn the_exec_fingerprint_moves_when_an_input_is_edited_under_an_unchanged_argv() {
    // The whole point. Argv carries paths, so an edited catalog leaves every
    // command line identical; without the observed digests the manifest would
    // prove nothing.
    let dir = scratch("fingerprint");
    let binary = dir.join("ling");
    let catalog = dir.join("patterns.lx");
    write(&binary, "pretend-binary");
    write(&catalog, "pattern a := [].");

    let argv = vec!["matches".to_owned(), catalog.to_string_lossy().into_owned()];
    let env: Vec<String> = vec!["LINGREP_DICTIONARY_PATH=/dict".to_owned()];
    let inputs = vec![catalog.clone()];
    let machine = Machine {
        cpu_model: "test cpu".to_owned(),
        kernel: "0.0.0".to_owned(),
        rustc: "rustc test".to_owned(),
    };
    let execution = Execution {
        argv: &argv,
        env: &env,
        affinity: "0-7",
        workers: Some(8),
        binary: &binary,
        inputs: &inputs,
    };

    let before = exec_fingerprint(&execution, &machine, &[]).expect("fingerprint");
    write(&catalog, "pattern a := [] [].");
    let after = exec_fingerprint(&execution, &machine, &[]).expect("fingerprint");
    assert_ne!(before, after, "an edited input must change the fingerprint");
}

#[test]
fn the_exec_fingerprint_moves_with_the_machine_and_with_affinity() {
    let dir = scratch("fingerprint-machine");
    let binary = dir.join("ling");
    write(&binary, "pretend-binary");
    let argv = vec!["matches".to_owned()];
    let env: Vec<String> = Vec::new();
    let inputs: Vec<PathBuf> = Vec::new();
    let machine = Machine {
        cpu_model: "test cpu".to_owned(),
        kernel: "0.0.0".to_owned(),
        rustc: "rustc test".to_owned(),
    };
    let execution = |affinity| Execution {
        argv: &argv,
        env: &env,
        affinity,
        workers: Some(8),
        binary: &binary,
        inputs: &inputs,
    };

    let pinned = exec_fingerprint(&execution("0-7"), &machine, &[]).expect("fingerprint");
    let other_ccd = exec_fingerprint(&execution("8-15"), &machine, &[]).expect("fingerprint");
    assert_ne!(
        pinned, other_ccd,
        "the two CCDs have different last-level caches, so runs on them are not paired"
    );

    let elsewhere = Machine {
        kernel: "0.0.1".to_owned(),
        ..machine
    };
    assert_ne!(
        pinned,
        exec_fingerprint(&execution("0-7"), &elsewhere, &[]).expect("fingerprint")
    );
}

#[test]
fn the_build_profile_is_read_from_the_workspace_rather_than_claimed() {
    let root = lingrep_core::testing::catalogs::repository_root();
    let release = read_build_profile(&root, "release").expect("the workspace declares release");
    assert_eq!(release.profile, "release");
    assert_eq!(
        (release.lto.as_str(), release.codegen_units),
        ("fat", 1),
        "these are the settings every performance claim in this repository rests on"
    );
    assert!(matches!(
        read_build_profile(&root, "no-such-profile"),
        Err(OrchestrateError::ProfileUnreadable { .. })
    ));
}

#[test]
fn a_declared_campaign_carries_the_suite_that_exists_now() {
    let mirrored = campaign_catalogs();
    let source = lingrep_core::testing::catalogs::catalog_campaign_descriptors();
    assert_eq!(
        mirrored.len(),
        source.len(),
        "the mirror is the whole suite or it is a second thing to forget"
    );
    for (mirror, descriptor) in mirrored.iter().zip(&source) {
        assert_eq!(mirror.path, descriptor.path);
        assert_eq!(mirror.regimes.len(), descriptor.regimes.len());
    }
    assert!(
        known_regimes().contains("Lookaround"),
        "the tag a campaign scopes an assertion change by"
    );
}

fn machine() -> Machine {
    Machine {
        cpu_model: "test cpu".to_owned(),
        kernel: "0.0.0".to_owned(),
        rustc: "rustc test".to_owned(),
    }
}

/// A manifest that is valid enough to carry a workload. Only the workload is
/// read by the functions under test here.
fn manifest_with(workload: Vec<crate::campaign::WorkloadInput>) -> Manifest {
    use crate::campaign::{Bound, BuildProfile, LadderRung, Metric, Tier};
    Manifest {
        campaign: "workload".to_owned(),
        tier: Tier::Two,
        bound: Bound::Work,
        runner_up_bound: None,
        profile_evidence: Some("test".to_owned()),
        ladder_rung: LadderRung::EliminateWork,
        target: None,
        primary_catalog: "patterns/test-simple.lx".to_owned(),
        primary_metric: Metric::Wall,
        affected_regimes: BTreeSet::new(),
        catalogs: Vec::new(),
        modes: Vec::new(),
        minimum_useful_effect_pct: 1.0,
        guardrail_ceilings_pct: BTreeMap::new(),
        escalation_ladder: vec![3],
        build: BuildProfile {
            profile: "release".to_owned(),
            lto: "fat".to_owned(),
            codegen_units: 1,
        },
        machine: machine(),
        arms: Arms {
            a: Arm {
                label: "baseline".to_owned(),
                commit: "a".to_owned(),
                binary_digest: "blake3:a".to_owned(),
            },
            b: Arm {
                label: "candidate".to_owned(),
                commit: "b".to_owned(),
                binary_digest: "blake3:b".to_owned(),
            },
        },
        workload,
        catalog_corpora: BTreeMap::new(),
        workers: None,
        output_capture: OutputCapture::File,
        // Deliberately unsealed: this is also the campaign shape declared
        // before the command was sealed, and `record` must still accept one.
        command_template: Vec::new(),
    }
}

#[test]
fn the_shared_fingerprint_is_identical_across_arms_and_the_exec_one_is_not() {
    // The hole this closes. `exec_fingerprint` covers the binary, so the two
    // arms differ by construction and it can only ever be compared *within*
    // one arm — leaving a one-pair screen with no cross-arm identity check at
    // all. An input edited between A and B would have passed unnoticed, and
    // output parity is not a substitute: different inputs can yield identical
    // matches at very different cost.
    let dir = scratch("shared-fingerprint");
    let (arm_a, arm_b, catalog) = (dir.join("vs-a"), dir.join("vs-b"), dir.join("patterns.lx"));
    write(&arm_a, "arm-a");
    write(&arm_b, "arm-b");
    write(&catalog, "pattern a := [].");

    let argv = vec!["matches".to_owned()];
    let env: Vec<String> = Vec::new();
    let inputs = vec![catalog.clone()];
    fn for_arm<'a>(
        binary: &'a Path,
        argv: &'a [String],
        env: &'a [String],
        inputs: &'a [PathBuf],
    ) -> Execution<'a> {
        Execution {
            argv,
            env,
            affinity: "0-7",
            workers: Some(8),
            binary,
            inputs,
        }
    }

    assert_ne!(
        exec_fingerprint(&for_arm(&arm_a, &argv, &env, &inputs), &machine(), &[])
            .expect("fingerprint"),
        exec_fingerprint(&for_arm(&arm_b, &argv, &env, &inputs), &machine(), &[])
            .expect("fingerprint"),
        "the arms differ in their binary, so this must differ"
    );
    let shared = shared_fingerprint(&for_arm(&arm_a, &argv, &env, &inputs), &machine(), &[])
        .expect("fingerprint");
    assert_eq!(
        shared,
        shared_fingerprint(&for_arm(&arm_b, &argv, &env, &inputs), &machine(), &[])
            .expect("fingerprint"),
        "everything but the binary is what both arms must have shared"
    );

    write(&catalog, "pattern a := [] [].");
    assert_ne!(
        shared,
        shared_fingerprint(&for_arm(&arm_a, &argv, &env, &inputs), &machine(), &[])
            .expect("fingerprint"),
        "an input edited between arms must move the one fingerprint that spans them"
    );
}

#[test]
fn a_declared_workload_that_moved_is_refused() {
    let dir = scratch("workload-moved");
    let corpus = dir.join("corpus.jsonl");
    write(&corpus, "{\"sentences\":[\"a\"]}\n");
    let path = corpus.to_string_lossy().into_owned();
    let declared = workload_input(&path, crate::campaign::WorkloadKind::File).expect("identity");
    let manifest = manifest_with(vec![declared]);

    assert!(observe_workload(&manifest).is_ok());
    write(&corpus, "{\"sentences\":[\"a\",\"b\"]}\n");
    assert!(matches!(
        observe_workload(&manifest),
        Err(OrchestrateError::WorkloadChanged { .. })
    ));
}

#[test]
fn a_prepared_artifact_that_cannot_be_read_is_refused_rather_than_assumed() {
    // The identity is re-observed from the artifact's own manifest, never
    // copied from the campaign. Accepting an unreadable artifact would make
    // it the one input nothing checks, which is how a path came to stand in
    // for an identity.
    let manifest = manifest_with(vec![crate::campaign::WorkloadInput {
        path: "no-such.lingcorpus".to_owned(),
        kind: crate::campaign::WorkloadKind::PreparedArtifact,
        identity: "blake3:declared".to_owned(),
    }]);
    assert!(matches!(
        observe_workload(&manifest),
        Err(OrchestrateError::Unreadable { .. })
    ));
}

/// A readable artifact of the current generation, if this checkout has one.
///
/// The repository keeps published corpora at its root, and older generations
/// sit beside current ones — V6 and earlier are unreadable by design and must
/// be rebuilt. A checkout with none skips rather than fails, because these are
/// large and are not fixtures.
fn tracked_artifact() -> Option<(PathBuf, lingrep_corpus::CorpusArtifact)> {
    let root = lingrep_core::testing::catalogs::repository_root();
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "lingcorpus")
        })
        .collect();
    candidates.sort();
    candidates.into_iter().find_map(|path| {
        lingrep_corpus::CorpusArtifact::open(&path)
            .ok()
            .map(|open| (path, open))
    })
}

#[test]
fn a_prepared_artifacts_identity_covers_more_than_its_document_inventory() {
    // `ordered_inventory_digest` alone is not an identity for performance
    // evidence: the same documents retokenized under a different dictionary
    // change token count, attributes, predicates, matches and therefore work,
    // while leaving that digest identical. The recorded identity has to carry
    // the token and location component digests, the dictionary, and the token
    // schema as well.
    let Some((artifact, opened)) = tracked_artifact() else {
        return;
    };
    let recorded = opened.recorded_identity();

    assert!(!recorded.dictionary_fingerprint.is_empty());
    assert!(!recorded.token_schema_identity.is_empty());
    assert!(
        recorded.components.contains_key("tokens.v1.zst"),
        "the token component must be covered: {:?}",
        recorded.components.keys().collect::<Vec<_>>()
    );
    assert!(
        recorded.components.contains_key("locations.v1"),
        "the location component must be covered: {:?}",
        recorded.components.keys().collect::<Vec<_>>()
    );

    // Each of these alone moves the aggregate, which is what makes it an
    // identity rather than a label.
    let baseline = recorded.digest();
    for edit in [
        |identity: &mut lingrep_corpus::PreparedIdentity| {
            identity.dictionary_fingerprint = "other".to_owned();
        },
        |identity: &mut lingrep_corpus::PreparedIdentity| {
            identity.token_schema_identity = "other".to_owned();
        },
        |identity: &mut lingrep_corpus::PreparedIdentity| {
            identity
                .components
                .insert("tokens.v1.zst".to_owned(), "other".to_owned());
        },
        |identity: &mut lingrep_corpus::PreparedIdentity| {
            identity.ordered_inventory_digest = "other".to_owned();
        },
        |identity: &mut lingrep_corpus::PreparedIdentity| identity.token_count += 1,
    ] {
        let mut edited = recorded.clone();
        edit(&mut edited);
        assert_ne!(baseline, edited.digest());
    }

    assert_eq!(
        prepared_identity(&artifact).expect("identity"),
        baseline,
        "what `record` observes is the same value"
    );
}

/// A declaration valid enough for `declare`, sealing `template`.
fn declaration(template: Vec<TemplateArg>, binary: &Path) -> Declaration {
    use crate::campaign::{Bound, BuildProfile, LadderRung, Metric, Tier};
    let primary = campaign_catalogs()
        .into_iter()
        .next()
        .expect("the suite is never empty")
        .path;
    Declaration {
        campaign: "sealed-command".to_owned(),
        tier: Tier::Two,
        bound: Bound::Work,
        runner_up_bound: None,
        profile_evidence: "test".to_owned(),
        ladder_rung: LadderRung::EliminateWork,
        target: None,
        primary_catalog: primary,
        primary_metric: Metric::Wall,
        affected_regimes: BTreeSet::new(),
        modes: Vec::new(),
        minimum_useful_effect_pct: 5.0,
        guardrail_ceilings_pct: BTreeMap::new(),
        escalation_ladder: vec![3],
        build: BuildProfile {
            profile: "release".to_owned(),
            lto: "fat".to_owned(),
            codegen_units: 1,
        },
        arms: Arms {
            a: arm("baseline", "a", binary).expect("the scratch binary is hashable"),
            b: arm("candidate", "b", binary).expect("the scratch binary is hashable"),
        },
        workload: Vec::new(),
        command_template: template,
        workers: Some(4),
        output_capture: OutputCapture::File,
    }
}

/// The seal exists so that two campaigns measuring different things cannot
/// carry the same digest. Before the command was sealed they could, and did:
/// one declaration with its target flags broken by a quoting bug and one with
/// them working produced the same manifest digest.
#[test]
fn two_campaigns_measuring_different_commands_do_not_share_a_seal() {
    let dir = scratch("sealed-command-digest");
    let binary = dir.join("arm");
    write(&binary, "binary");

    let digest = |limit: &str| {
        declare(declaration(
            vec![
                TemplateArg::Literal("matches".to_owned()),
                TemplateArg::Literal("--patterns".to_owned()),
                TemplateArg::Catalog,
                TemplateArg::Literal("--limit".to_owned()),
                TemplateArg::Literal(limit.to_owned()),
            ],
            &binary,
        ))
        .expect("a sealed declaration")
        .manifest_digest
    };

    assert_ne!(
        digest("all"),
        digest("5"),
        "the manifest digest must separate two campaigns that measure \
         different commands"
    );
}

#[test]
fn a_campaign_that_seals_no_command_is_refused_at_declare() {
    let dir = scratch("unsealed-command");
    let binary = dir.join("arm");
    write(&binary, "binary");
    assert!(
        matches!(
            declare(declaration(Vec::new(), &binary)),
            Err(OrchestrateError::CommandTemplateMissing)
        ),
        "requiring it here is what leaves only already-written campaigns \
         without a sealed command"
    );
}

fn corpus_mapping(path: &str) -> BTreeMap<String, String> {
    campaign_catalogs()
        .into_iter()
        .map(|catalog| (catalog.path, path.to_owned()))
        .collect()
}

fn prepared_workload(path: &str) -> crate::campaign::WorkloadInput {
    crate::campaign::WorkloadInput {
        path: path.to_owned(),
        kind: crate::campaign::WorkloadKind::PreparedArtifact,
        identity: format!("blake3:{path}"),
    }
}

#[test]
fn a_per_catalog_corpus_mapping_is_sealed_into_the_reconstructed_command() {
    let dir = scratch("per-catalog-corpus");
    let binary = dir.join("arm");
    write(&binary, "binary");
    let morphology = "morphology.lingcorpus";
    let dependencies = "dependencies.lingcorpus";
    let mut declaration = declaration(
        vec![
            TemplateArg::Literal("matches".to_owned()),
            TemplateArg::Literal("--patterns".to_owned()),
            TemplateArg::Catalog,
            TemplateArg::Literal("--corpus".to_owned()),
            TemplateArg::Corpus,
        ],
        &binary,
    );
    declaration.workload = vec![
        prepared_workload(morphology),
        prepared_workload(dependencies),
    ];
    let mut mapping = corpus_mapping(morphology);
    let dependency_catalog = "patterns/ud-dependency.lx";
    mapping.insert(dependency_catalog.to_owned(), dependencies.to_owned());

    let campaign = declare_with_catalog_corpora(declaration, mapping)
        .expect("every artifact and catalog is sealed");
    assert_eq!(
        campaign
            .manifest
            .expected_argv(dependency_catalog)
            .expect("the mapping resolves")
            .expect("the command is sealed")
            .last()
            .map(String::as_str),
        Some(dependencies)
    );
}

#[test]
fn a_corpus_hole_requires_a_declared_mapping_for_every_catalog() {
    let dir = scratch("missing-catalog-corpus");
    let binary = dir.join("arm");
    write(&binary, "binary");
    let mut declaration = declaration(vec![TemplateArg::Corpus], &binary);
    declaration.workload = vec![prepared_workload("morphology.lingcorpus")];
    let mut mapping = corpus_mapping("morphology.lingcorpus");
    let missing = campaign_catalogs()[0].path.clone();
    mapping.remove(&missing);

    assert!(matches!(
        declare_with_catalog_corpora(declaration, mapping),
        Err(OrchestrateError::CorpusNotMapped { catalog }) if catalog == missing
    ));
}

#[test]
fn a_catalog_corpus_must_be_an_authenticated_prepared_workload() {
    let dir = scratch("undeclared-catalog-corpus");
    let binary = dir.join("arm");
    write(&binary, "binary");
    let declaration = declaration(vec![TemplateArg::Corpus], &binary);

    assert!(matches!(
        declare_with_catalog_corpora(declaration, corpus_mapping("unsealed.lingcorpus")),
        Err(OrchestrateError::CorpusNotDeclared { .. })
    ));
}

#[test]
fn every_declared_prepared_workload_must_be_used_by_the_corpus_mapping() {
    let dir = scratch("unused-prepared-workload");
    let binary = dir.join("arm");
    write(&binary, "binary");
    let mut declaration = declaration(vec![TemplateArg::Corpus], &binary);
    declaration.workload = vec![
        prepared_workload("used.lingcorpus"),
        prepared_workload("unused.lingcorpus"),
    ];

    assert!(matches!(
        declare_with_catalog_corpora(declaration, corpus_mapping("used.lingcorpus")),
        Err(OrchestrateError::PreparedArtifactUnmapped { corpus })
            if corpus == "unused.lingcorpus"
    ));
}

#[test]
fn a_catalog_mapping_without_a_corpus_hole_is_refused() {
    let dir = scratch("mapping-without-hole");
    let binary = dir.join("arm");
    write(&binary, "binary");
    let declaration = declaration(vec![TemplateArg::Catalog], &binary);

    assert!(matches!(
        declare_with_catalog_corpora(declaration, corpus_mapping("corpus.lingcorpus")),
        Err(OrchestrateError::CorpusMappingWithoutHole)
    ));
}

/// The subsequence check this replaced would have accepted the second argv:
/// it contains every sealed argument, in order, and also contradicts one.
#[test]
fn a_run_whose_command_contradicts_the_seal_is_refused() {
    let dir = scratch("command-differs");
    let binary = dir.join("arm");
    write(&binary, "binary");
    let output = dir.join("run.jsonl");
    write(&output, "{\"type\":\"complete\",\"records\":0}\n");
    let path = dir.join("campaign.json");

    let template = vec![
        TemplateArg::Literal("matches".to_owned()),
        TemplateArg::Literal("--patterns".to_owned()),
        TemplateArg::Catalog,
        TemplateArg::Literal("--limit".to_owned()),
        TemplateArg::Literal("all".to_owned()),
    ];
    let campaign = declare(declaration(template, &binary)).expect("a sealed declaration");
    let catalog = campaign.manifest.primary_catalog.clone();
    write_campaign(&path, &campaign).expect("the campaign is writable");

    let measurement = |order: usize| Measurement {
        catalog: catalog.clone(),
        arm: "a".to_owned(),
        block: 0,
        order,
        mode: None,
        ok: true,
        wall_s: 1.0,
        user_s: 1.0,
        sys_s: 0.1,
        peak_rss_kb: 1024.0,
    };
    let run = |argv: &[String], order: usize| {
        let execution = Execution {
            argv,
            env: &[],
            affinity: "",
            workers: Some(4),
            binary: &binary,
            inputs: &[],
        };
        record(
            &path,
            measurement(order),
            &execution,
            &OutputIdentity::File(&output),
        )
    };

    let sealed: Vec<String> = campaign
        .manifest
        .expected_argv(&catalog)
        .expect("the command resolves")
        .expect("the template was sealed");
    let mut contradicting = sealed.clone();
    contradicting.extend(["--limit".to_owned(), "5".to_owned()]);

    assert!(
        matches!(
            run(&contradicting, 0),
            Err(OrchestrateError::CommandDiffers { .. })
        ),
        "a run that appends a second --limit measures something else"
    );
    assert!(
        run(&sealed, 0).is_ok(),
        "and the sealed command itself must still record, or the check is \
         refusing everything"
    );
}

/// Occupancy is `user / (wall x workers)`, so a run at a different pool width
/// would be reported against the wrong denominator — and the two arms would
/// not be comparable in the first place.
#[test]
fn a_run_at_a_pool_width_the_campaign_does_not_seal_is_refused() {
    let dir = scratch("worker-count-differs");
    let binary = dir.join("arm");
    write(&binary, "binary");
    let output = dir.join("run.jsonl");
    write(&output, "{\"type\":\"complete\",\"records\":0}\n");
    let path = dir.join("campaign.json");

    let campaign = declare(declaration(
        vec![TemplateArg::Literal("matches".to_owned())],
        &binary,
    ))
    .expect("a sealed declaration");
    let catalog = campaign.manifest.primary_catalog.clone();
    assert_eq!(campaign.manifest.workers, Some(4), "the sealed pool width");
    write_campaign(&path, &campaign).expect("the campaign is writable");

    let run = |workers: Option<u32>| {
        let argv = vec!["matches".to_owned()];
        let execution = Execution {
            argv: &argv,
            env: &[],
            affinity: "",
            workers,
            binary: &binary,
            inputs: &[],
        };
        record(
            &path,
            Measurement {
                catalog: catalog.clone(),
                arm: "a".to_owned(),
                block: 0,
                order: 0,
                mode: None,
                ok: true,
                wall_s: 1.0,
                user_s: 1.0,
                sys_s: 0.1,
                peak_rss_kb: 1024.0,
            },
            &execution,
            &OutputIdentity::File(&output),
        )
    };

    assert!(matches!(
        run(Some(8)),
        Err(OrchestrateError::WorkerCountDiffers {
            sealed: Some(4),
            observed: Some(8)
        })
    ));
    assert!(
        matches!(run(None), Err(OrchestrateError::WorkerCountDiffers { .. })),
        "an unset pool is a different pool, not an unchecked one"
    );
    assert!(run(Some(4)).is_ok(), "and the sealed width still records");
}

#[test]
fn occupancy_is_the_mean_ratio_against_the_sealed_pool() {
    use crate::campaign::{Campaign, Observation, worker_normalized_cpu_occupancy};

    let observation = |arm: &str, wall: f64, user: f64| Observation {
        manifest_digest: "blake3:m".to_owned(),
        exec_fingerprint: "blake3:e".to_owned(),
        input_fingerprint: "blake3:i".to_owned(),
        output_digest: "blake3:o".to_owned(),
        catalog: "patterns/double-wo.lx".to_owned(),
        arm: arm.to_owned(),
        block: 0,
        order: 0,
        mode: None,
        ok: true,
        wall_s: wall,
        user_s: user,
        sys_s: 0.0,
        peak_rss_kb: 0.0,
    };
    let mut manifest = manifest_with(Vec::new());
    manifest.workers = Some(8);
    let mut campaign = Campaign {
        manifest_digest: "blake3:m".to_owned(),
        manifest,
        observations: vec![
            observation("a", 3.0, 12.0),
            observation("a", 3.0, 12.0),
            // Arm B is the change under test and says nothing about the
            // workload the campaign was planned against.
            observation("b", 1.0, 1.0),
        ],
        selections: Vec::new(),
    };
    assert_eq!(
        worker_normalized_cpu_occupancy(&campaign)
            .get("patterns/double-wo.lx")
            .copied(),
        Some(0.5),
    );

    campaign.manifest.workers = None;
    assert!(
        worker_normalized_cpu_occupancy(&campaign).is_empty(),
        "without a sealed pool the quantity has no denominator, and guessing \
         one is how it came to be called a bound",
    );
}

/// The two digest paths must be one number, or a baseline recorded through a
/// file cannot be compared with a run measured through a pipe.
#[test]
fn a_streamed_digest_equals_the_digest_of_the_same_bytes_on_disk() {
    let dir = scratch("stream-digest");
    let path = dir.join("run.jsonl");
    let text = "{\"type\":\"run\",\"data\":{\"run_id\":\"one\",\"published\":7}}\n\
                \n\
                {\"type\":\"match\",\"data\":{\"start\":0}}\n\
                {\"type\":\"complete\",\"elapsed_millis\":1234,\"records\":7}\n";
    write(&path, text);

    let from_file = output_digest(&path).expect("the file digests");
    let from_stream =
        stream_digest(&mut std::io::BufReader::new(text.as_bytes())).expect("the stream digests");
    assert_eq!(from_file, from_stream);

    // And the run-variant fields are still what is being neutralized, on both.
    let varied = text.replace("\"one\"", "\"two\"").replace("1234", "999999");
    let other = dir.join("other.jsonl");
    write(&other, &varied);
    assert_eq!(output_digest(&other).expect("digests"), from_file);
    assert_eq!(
        stream_digest(&mut std::io::BufReader::new(varied.as_bytes())).expect("digests"),
        from_stream,
    );
}

/// An early exit would deliver `EPIPE` to the process being measured and turn
/// a correct run into a recorded failure, so the whole input is consumed even
/// once the answer cannot be used.
#[test]
fn a_streamed_digest_consumes_its_whole_input_after_a_parse_failure() {
    struct Counting<'a> {
        inner: std::io::Cursor<&'a [u8]>,
        reads: std::rc::Rc<std::cell::Cell<usize>>,
    }
    impl std::io::Read for Counting<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            if read > 0 {
                self.reads.set(self.reads.get() + read);
            }
            Ok(read)
        }
    }

    let text = "{\"type\":\"run\"}\nnot json\n{\"type\":\"complete\"}\n";
    let reads = std::rc::Rc::new(std::cell::Cell::new(0));
    let mut reader = std::io::BufReader::with_capacity(
        4,
        Counting {
            inner: std::io::Cursor::new(text.as_bytes()),
            reads: std::rc::Rc::clone(&reads),
        },
    );

    assert!(matches!(
        stream_digest(&mut reader),
        Err(OrchestrateError::NotJsonl { line: 2, .. })
    ));
    assert_eq!(
        reads.get(),
        text.len(),
        "every byte must be drained, or the measured process takes the signal",
    );
}

/// A truncated or empty digest means the hasher did not finish. Recording it
/// would compare the run against nothing and call the arms equal.
#[test]
fn an_output_digest_that_is_not_one_is_refused() {
    let good = format!("blake3:{}", "a".repeat(64));
    assert_eq!(
        OutputIdentity::Digest(&good).resolve().expect("accepted"),
        good,
    );
    for bad in [
        "",
        "blake3:",
        "blake3:abc",
        &format!("blake3:{}", "a".repeat(63)),
        &format!("blake3:{}", "a".repeat(65)),
        &format!("blake3:{}", "A".repeat(64)),
        &format!("sha256:{}", "a".repeat(64)),
        &"a".repeat(64),
    ] {
        assert!(
            matches!(
                OutputIdentity::Digest(bad).resolve(),
                Err(OrchestrateError::MalformedOutputDigest { .. })
            ),
            "{bad:?} must not be accepted as a digest",
        );
    }
}
