//! Every refusal and every verdict, on synthetic campaigns.
//!
//! Synthetic on purpose: the point of a pure adjudicator is that its decisions
//! can be checked without producing twenty minutes of measurement first.

use super::*;

/// Stamp a campaign with the digest of its own manifest.
///
/// Every observation carries it too, so a test that edits a manifest must
/// reseal deliberately — which is exactly what `ManifestAltered` is for.
pub(crate) fn seal(mut campaign: Campaign) -> Campaign {
    let digest = campaign
        .manifest
        .digest()
        .expect("a synthetic manifest encodes");
    for observation in &mut campaign.observations {
        observation.manifest_digest.clone_from(&digest);
    }
    campaign.manifest_digest = digest;
    campaign
}

fn catalogs() -> Vec<CampaignCatalog> {
    vec![
        CampaignCatalog {
            path: "patterns/primary.lx".to_owned(),
            role: Role::Substantive,
            regimes: ["Lookaround".to_owned()].into_iter().collect(),
        },
        CampaignCatalog {
            path: "patterns/affected.lx".to_owned(),
            role: Role::Substantive,
            regimes: ["Lookaround".to_owned()].into_iter().collect(),
        },
        CampaignCatalog {
            path: "patterns/unaffected.lx".to_owned(),
            role: Role::Substantive,
            regimes: ["SameAs".to_owned()].into_iter().collect(),
        },
        CampaignCatalog {
            path: "patterns/floor.lx".to_owned(),
            role: Role::Floor,
            regimes: ["DecodeFloor".to_owned()].into_iter().collect(),
        },
    ]
}

fn manifest() -> Manifest {
    Manifest {
        campaign: "synthetic".to_owned(),
        tier: Tier::Two,
        bound: Bound::MemoryLatency,
        runner_up_bound: Some(Bound::Work),
        profile_evidence: Some("perf -F 499, AssertionSession 47.7%".to_owned()),
        ladder_rung: LadderRung::Representation,
        target: None,
        primary_catalog: "patterns/primary.lx".to_owned(),
        primary_metric: Metric::Wall,
        affected_regimes: ["Lookaround".to_owned()].into_iter().collect(),
        catalogs: catalogs(),
        modes: Vec::new(),
        minimum_useful_effect_pct: 1.0,
        guardrail_ceilings_pct: [
            (Metric::Wall, 2.0),
            (Metric::User, 2.0),
            (Metric::PeakRss, 5.0),
        ]
        .into_iter()
        .collect(),
        escalation_ladder: vec![3, 8, 20],
        build: BuildProfile {
            profile: "release".to_owned(),
            lto: "fat".to_owned(),
            codegen_units: 1,
        },
        machine: Machine {
            cpu_model: "AMD Ryzen 9 7950X3D".to_owned(),
            kernel: "7.1.7".to_owned(),
            rustc: "rustc 1.95.0".to_owned(),
        },
        workload: vec![WorkloadInput {
            path: "corpus.lingcorpus".to_owned(),
            kind: WorkloadKind::PreparedArtifact,
            identity: "blake3:corpus".to_owned(),
        }],
        catalog_corpora: BTreeMap::new(),
        workers: Some(8),
        output_capture: OutputCapture::File,
        command_template: vec![
            TemplateArg::Literal("matches".to_owned()),
            TemplateArg::Literal("--patterns".to_owned()),
            TemplateArg::Catalog,
            TemplateArg::Literal("--corpus".to_owned()),
            TemplateArg::Literal("corpus.lingcorpus".to_owned()),
            TemplateArg::Literal("--limit".to_owned()),
            TemplateArg::Literal("all".to_owned()),
        ],
        arms: Arms {
            a: Arm {
                label: "baseline".to_owned(),
                commit: "9bb64e23".to_owned(),
                binary_digest: "blake3:aaaa".to_owned(),
            },
            b: Arm {
                label: "candidate".to_owned(),
                commit: "worktree".to_owned(),
                binary_digest: "blake3:bbbb".to_owned(),
            },
        },
    }
}

/// One run at an explicit position, for tests that drive the state machine.
pub(crate) fn run_at(
    catalog: &str,
    arm: &str,
    block: usize,
    order: usize,
    wall: f64,
) -> Observation {
    let mut observation = run(catalog, arm, block, order, wall);
    // Sealed campaigns stamp this; a test appending to one must carry the
    // digest already in force.
    observation.manifest_digest = manifest().digest().expect("encodes");
    observation
}

/// One run, healthy by default.
fn run(catalog: &str, arm: &str, block: usize, order: usize, wall: f64) -> Observation {
    Observation {
        // Stamped by `seal`, which is the only thing that knows the digest.
        manifest_digest: String::new(),
        exec_fingerprint: format!("{catalog}:{arm}"),
        input_fingerprint: format!("{catalog}:input"),
        output_digest: format!("{catalog}-output"),
        catalog: catalog.to_owned(),
        arm: arm.to_owned(),
        block,
        order,
        mode: None,
        ok: true,
        wall_s: wall,
        user_s: wall * 3.0,
        sys_s: 0.1,
        peak_rss_kb: 100_000.0,
    }
}

/// `count` balanced ABBA blocks where B is `ratio` times A.
fn blocks(catalog: &str, count: usize, ratio: f64) -> Vec<Observation> {
    let mut observations = Vec::new();
    for index in 0..count {
        // A tiny per-block level shift, so the estimator sees real variation
        // rather than a degenerate zero-variance sample.
        let base = 10.0 + f64::from(u32::try_from(index).expect("small")) * 0.1;
        observations.push(run(catalog, "a", index, 0, base));
        observations.push(run(catalog, "b", index, 1, base * ratio));
        observations.push(run(catalog, "b", index, 2, base * ratio));
        observations.push(run(catalog, "a", index, 3, base));
    }
    observations
}

/// `count` blocks whose per-block ratios cycle through `ratios`, so the
/// estimator sees real spread and the interval can straddle a landmark.
fn noisy_blocks(catalog: &str, count: usize, ratios: &[f64]) -> Vec<Observation> {
    let mut observations = Vec::new();
    for index in 0..count {
        let base = 10.0;
        let ratio = ratios[index % ratios.len()];
        observations.push(run(catalog, "a", index, 0, base));
        observations.push(run(catalog, "b", index, 1, base * ratio));
        observations.push(run(catalog, "b", index, 2, base * ratio));
        observations.push(run(catalog, "a", index, 3, base));
    }
    observations
}

/// One block per entry in `ratios`, so a test can shape a catalog's history
/// across looks rather than only its final estimate.
fn blocks_with_ratios(catalog: &str, ratios: &[f64]) -> Vec<Observation> {
    let mut observations = Vec::new();
    for (index, ratio) in ratios.iter().enumerate() {
        let base = 10.0;
        observations.push(run(catalog, "a", index, 0, base));
        observations.push(run(catalog, "b", index, 1, base * ratio));
        observations.push(run(catalog, "b", index, 2, base * ratio));
        observations.push(run(catalog, "a", index, 3, base));
    }
    observations
}

/// One A/B screen pair.
fn screen(catalog: &str, ratio: f64) -> Vec<Observation> {
    vec![
        run(catalog, "a", 0, 0, 10.0),
        run(catalog, "b", 0, 1, 10.0 * ratio),
    ]
}

/// A campaign whose primary wins cleanly and whose guardrails are flat.
pub(crate) fn campaign_with(primary_ratio: f64) -> Campaign {
    let mut observations = blocks("patterns/primary.lx", 3, primary_ratio);
    observations.extend(blocks("patterns/affected.lx", 3, 1.0));
    observations.extend(screen("patterns/unaffected.lx", 1.0));
    observations.extend(screen("patterns/floor.lx", 1.0));
    seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    })
}

fn adjudicated(campaign: &Campaign) -> Adjudication {
    adjudicate(campaign).expect("a valid campaign adjudicates")
}

// ---------------------------------------------------------------- coverage

#[test]
fn tier_two_gives_blocks_to_the_primary_and_affected_and_a_screen_pair_to_the_rest() {
    // The contract this design originally contradicted: requiring blocks
    // everywhere would make Tier 2 far more expensive than the repository
    // rule, which is the opposite of the point.
    let manifest = manifest();
    let catalogs = catalogs();

    assert_eq!(
        required_coverage(&manifest, &catalogs[0]),
        Coverage::Blocks(3)
    );
    assert_eq!(
        required_coverage(&manifest, &catalogs[1]),
        Coverage::Blocks(3)
    );
    assert_eq!(
        required_coverage(&manifest, &catalogs[2]),
        Coverage::ScreenPair
    );
    assert_eq!(
        required_coverage(&manifest, &catalogs[3]),
        Coverage::ScreenPair
    );
}

#[test]
fn tier_three_gives_blocks_to_every_substantive_catalog() {
    let mut manifest = manifest();
    manifest.tier = Tier::Three;
    manifest.escalation_ladder = vec![5, 12, 30];
    let catalogs = catalogs();

    assert_eq!(
        required_coverage(&manifest, &catalogs[0]),
        Coverage::Blocks(5)
    );
    assert_eq!(
        required_coverage(&manifest, &catalogs[1]),
        Coverage::Blocks(5)
    );
    assert_eq!(
        required_coverage(&manifest, &catalogs[2]),
        Coverage::Blocks(3)
    );
    assert_eq!(
        required_coverage(&manifest, &catalogs[3]),
        Coverage::ScreenPair
    );
}

#[test]
fn tier_one_is_screen_pairs_throughout_so_it_never_reports_an_interval() {
    let mut manifest = manifest();
    manifest.tier = Tier::One;
    for catalog in &catalogs() {
        assert_eq!(required_coverage(&manifest, catalog), Coverage::ScreenPair);
    }
}

#[test]
fn a_tier_two_campaign_with_one_screen_pair_on_an_unaffected_catalog_is_accepted() {
    // The case the first draft of this design would have wrongly refused, at
    // twice the measurement cost.
    let adjudication = adjudicated(&campaign_with(0.95));
    assert_eq!(adjudication.verdict, Verdict::Win);
}

// ---------------------------------------------------------------- refusals

#[test]
fn a_failed_run_is_refused_rather_than_adjudicated() {
    let mut campaign = campaign_with(0.95);
    campaign.observations[0].ok = false;
    assert!(matches!(
        adjudicate(&campaign),
        Err(Refusal::FailedRun { .. })
    ));
}

#[test]
fn a_zero_second_run_is_refused() {
    // Failure mode 3: a rig bug that printed a well-formed `0.00s` row.
    let mut campaign = campaign_with(0.95);
    campaign.observations[0].wall_s = 0.0;
    assert!(matches!(
        adjudicate(&campaign),
        Err(Refusal::FailedRun { .. })
    ));
}

#[test]
fn editing_the_manifest_after_the_runs_is_refused() {
    // The digest is recomputed, not merely compared to itself. Loosening the
    // minimum useful effect after seeing the numbers is the specific edit this
    // exists to catch, and it leaves no other trace.
    let mut campaign = campaign_with(0.95);
    campaign.manifest.minimum_useful_effect_pct = 0.1;
    assert!(matches!(
        adjudicate(&campaign),
        Err(Refusal::ManifestAltered { .. })
    ));
}

#[test]
fn an_observation_bound_to_another_manifest_is_refused() {
    // A block appended from a different campaign directory: the manifest is
    // intact, but this run was not measured under it.
    let mut campaign = campaign_with(0.95);
    campaign.observations[0].manifest_digest = "blake3:somewhere-else".to_owned();
    assert!(matches!(
        adjudicate(&campaign),
        Err(Refusal::ManifestDigestMismatch { .. })
    ));
}

#[test]
fn the_manifest_digest_covers_every_field_that_changes_a_verdict() {
    // Not a tautology: `digest` hashes the encoding, so a field the encoder
    // skipped would silently fall outside the seal. Each of these has moved a
    // verdict in the tests above.
    let base = manifest().digest().expect("encodes");
    for edit in [
        |m: &mut Manifest| m.minimum_useful_effect_pct = 0.1,
        |m: &mut Manifest| m.tier = Tier::Three,
        |m: &mut Manifest| m.primary_metric = Metric::PeakRss,
        |m: &mut Manifest| m.escalation_ladder = vec![3, 8, 21],
        |m: &mut Manifest| m.catalogs.truncate(3),
        |m: &mut Manifest| m.build.profile = "dev".to_owned(),
        |m: &mut Manifest| m.machine.kernel = "7.1.8".to_owned(),
        |m: &mut Manifest| m.arms.b.binary_digest = "blake3:cccc".to_owned(),
    ] {
        let mut edited = manifest();
        edit(&mut edited);
        assert_ne!(
            base,
            edited.digest().expect("encodes"),
            "an edit that changes what a campaign means must change its digest"
        );
    }
}

#[test]
fn a_non_finite_threshold_is_refused_rather_than_never_matching() {
    // A NaN bar makes every comparison against it false, so the campaign
    // would adjudicate confidently and wrongly. It is also invisible in the
    // encoding, which writes non-finite numbers as null.
    for edit in [
        |m: &mut Manifest| m.minimum_useful_effect_pct = f64::NAN,
        |m: &mut Manifest| {
            m.guardrail_ceilings_pct.insert(Metric::Wall, f64::INFINITY);
        },
    ] {
        let mut campaign = campaign_with(0.95);
        edit(&mut campaign.manifest);
        assert!(matches!(
            adjudicate(&seal(campaign)),
            Err(Refusal::NonFiniteThreshold { .. })
        ));
    }
}

#[test]
fn an_input_swapped_between_the_two_arms_is_refused() {
    // `ExecFingerprintDiffers` structurally cannot catch this: it covers the
    // binary, so the arms differ by construction and it is only ever compared
    // within one arm. A one-pair screen therefore had no cross-arm identity
    // check at all, and output parity is not one — different inputs can yield
    // identical matches at very different cost.
    let mut campaign = campaign_with(0.95);
    let swapped = campaign
        .observations
        .iter_mut()
        .find(|observation| observation.catalog == "patterns/unaffected.lx")
        .expect("the screened catalog is measured");
    swapped.input_fingerprint = "blake3:a-different-corpus".to_owned();
    assert!(matches!(
        adjudicate(&campaign),
        Err(Refusal::InputFingerprintDiffers { .. })
    ));
}

#[test]
fn a_campaign_declaring_no_workload_is_refused() {
    // Without this the workload was identified only by a path inside argv,
    // and a path is not an identity.
    let mut campaign = campaign_with(0.95);
    campaign.manifest.workload.clear();
    assert_eq!(
        adjudicate(&seal(campaign)),
        Err(Refusal::WorkloadNotDeclared)
    );
}

#[test]
fn swapping_a_binary_or_input_between_blocks_is_refused() {
    let mut campaign = campaign_with(0.95);
    campaign.observations[1].exec_fingerprint = "different-binary".to_owned();
    assert!(matches!(
        adjudicate(&campaign),
        Err(Refusal::ExecFingerprintDiffers { .. })
    ));
}

#[test]
fn arms_that_disagree_on_output_are_refused() {
    let mut campaign = campaign_with(0.95);
    campaign.observations[1].output_digest = "different-output".to_owned();
    assert!(matches!(
        adjudicate(&campaign),
        Err(Refusal::OutputParityBroken { .. })
    ));
}

#[test]
fn an_unbalanced_block_is_refused() {
    let mut campaign = campaign_with(0.95);
    // Three A runs and one B, rather than ABBA. The fingerprint moves with the
    // arm so that the balance check is what this exercises rather than the
    // execution-identity check, which would otherwise fire first.
    campaign.observations[1].arm = "a".to_owned();
    campaign.observations[1].exec_fingerprint = "patterns/primary.lx:a".to_owned();
    assert!(matches!(
        adjudicate(&campaign),
        Err(Refusal::UnbalancedArms { .. })
    ));
}

#[test]
fn short_coverage_is_refused() {
    let mut campaign = campaign_with(0.95);
    campaign.observations.retain(|observation| {
        !(observation.catalog == "patterns/primary.lx" && observation.block == 2)
    });
    assert!(matches!(
        adjudicate(&campaign),
        Err(Refusal::CoverageShort { .. })
    ));
}

#[test]
fn a_catalog_in_the_suite_that_was_never_measured_is_refused() {
    let mut campaign = campaign_with(0.95);
    campaign
        .observations
        .retain(|observation| observation.catalog != "patterns/floor.lx");
    assert!(matches!(
        adjudicate(&campaign),
        Err(Refusal::CatalogNotMeasured { .. })
    ));
}

#[test]
fn an_invalid_ladder_is_refused() {
    for ladder in [vec![], vec![2, 8], vec![8, 3]] {
        let mut campaign = campaign_with(0.95);
        campaign.manifest.escalation_ladder = ladder;
        let campaign = seal(campaign);
        assert!(matches!(
            adjudicate(&campaign),
            Err(Refusal::InvalidLadder { .. })
        ));
    }
}

#[test]
fn a_dev_build_invalidates_the_whole_campaign() {
    let mut campaign = campaign_with(0.95);
    campaign.manifest.build.profile = "dev".to_owned();
    let campaign = seal(campaign);
    assert!(matches!(
        adjudicate(&campaign),
        Err(Refusal::NotAReleaseBuild { .. })
    ));
}

#[test]
fn a_bound_with_no_profile_behind_it_is_refused() {
    for evidence in [None, Some(String::new()), Some("   ".to_owned())] {
        let mut campaign = campaign_with(0.95);
        campaign.manifest.profile_evidence = evidence;
        let campaign = seal(campaign);
        assert_eq!(
            adjudicate(&campaign),
            Err(Refusal::BoundWithoutProfileEvidence)
        );
    }
}

#[test]
fn a_tier_one_campaign_missing_a_declared_mode_is_refused() {
    let mut campaign = campaign_with(0.95);
    campaign.manifest.tier = Tier::One;
    campaign.manifest.escalation_ladder = vec![1];
    campaign.manifest.modes = vec!["prepared-v7".to_owned(), "direct".to_owned()];
    for observation in &mut campaign.observations {
        observation.mode = Some("prepared-v7".to_owned());
    }
    let campaign = seal(campaign);
    assert_eq!(
        adjudicate(&campaign),
        Err(Refusal::ModeNotMeasured {
            mode: "direct".to_owned()
        })
    );
}

#[test]
fn a_primary_catalog_outside_the_suite_is_refused() {
    let mut campaign = campaign_with(0.95);
    campaign.manifest.primary_catalog = "patterns/not-in-suite.lx".to_owned();
    let campaign = seal(campaign);
    assert!(matches!(
        adjudicate(&campaign),
        Err(Refusal::UnknownPrimaryCatalog { .. })
    ));
}

// ---------------------------------------------------------------- verdicts

#[test]
fn a_clear_improvement_beyond_the_minimum_is_a_win() {
    assert_eq!(adjudicated(&campaign_with(0.90)).verdict, Verdict::Win);
}

#[test]
fn an_improvement_short_of_the_minimum_is_reverted_not_kept() {
    // 0.5% better against a 1% minimum: real, and not worth its permanent cost.
    assert_eq!(
        adjudicated(&campaign_with(0.995)).verdict,
        Verdict::RevertBelowMinimum
    );
}

#[test]
fn a_primary_that_regressed_is_its_own_verdict_not_inconclusive() {
    // Entirely on the worse side of parity but inside the guardrail ceiling.
    // Calling this inconclusive would invite more blocks for a change already
    // known not to help.
    let campaign = campaign_with(1.015);
    let mut manifest = campaign.manifest.clone();
    manifest.guardrail_ceilings_pct.insert(Metric::Wall, 10.0);
    let campaign = seal(Campaign {
        manifest,
        ..campaign
    });

    assert_eq!(
        adjudicated(&campaign).verdict,
        Verdict::RevertPrimaryRegressed
    );
}

#[test]
fn a_guardrail_past_its_ceiling_outranks_a_primary_win() {
    // The precedence rule: guardrails are step 2 and the primary is step 3, so
    // a change that wins on the primary and wrecks a guardrail is rejected.
    let mut observations = blocks("patterns/primary.lx", 3, 0.80);
    observations.extend(blocks("patterns/affected.lx", 3, 1.20));
    observations.extend(screen("patterns/unaffected.lx", 1.0));
    observations.extend(screen("patterns/floor.lx", 1.0));
    let campaign = seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    });

    assert!(matches!(
        adjudicated(&campaign).verdict,
        Verdict::RejectGuardrail { .. }
    ));
}

#[test]
fn a_screen_pair_over_its_ceiling_escalates_and_never_rejects() {
    // Rule 5 is interval-based, and a screen pair has no interval, so it
    // cannot be adjudicated as a rejection. This is the `sketch-grammar` path:
    // a screen said "possibly over", and blocks were what settled it.
    let mut observations = blocks("patterns/primary.lx", 3, 0.95);
    observations.extend(blocks("patterns/affected.lx", 3, 1.0));
    observations.extend(screen("patterns/unaffected.lx", 1.30));
    observations.extend(screen("patterns/floor.lx", 1.0));
    let campaign = seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    });

    let adjudication = adjudicated(&campaign);
    let stream = adjudication
        .streams
        .iter()
        .find(|stream| stream.catalog == "patterns/unaffected.lx" && stream.metric == Metric::Wall)
        .expect("the screened catalog has a wall stream");

    assert_eq!(stream.outcome, StreamOutcome::EscalateGuardrail);
    assert!(stream.effect.is_screen());
    assert!(
        !adjudication
            .streams
            .iter()
            .any(|stream| stream.outcome == StreamOutcome::RejectGuardrail),
        "a screen must never produce a rejection"
    );
}

#[test]
fn a_screen_pair_within_its_ceiling_is_marked_screened_not_passed() {
    let adjudication = adjudicated(&campaign_with(0.90));
    let stream = adjudication
        .streams
        .iter()
        .find(|stream| stream.catalog == "patterns/unaffected.lx" && stream.metric == Metric::Wall)
        .expect("the screened catalog has a wall stream");

    assert_eq!(stream.outcome, StreamOutcome::Screened);
    assert!(adjudication.aggregate_includes_screens);
}

// ---------------------------------------------------------------- aggregate

#[test]
fn the_aggregate_covers_substantive_catalogs_and_excludes_the_floor() {
    let adjudication = adjudicated(&campaign_with(0.90));
    let aggregate = adjudication
        .aggregate
        .expect("substantive catalogs measured");

    // Three substantive catalogs at 0.90, 1.0 and 1.0.
    let expected = (0.90_f64.ln() / 3.0).exp();
    assert!(
        (aggregate - expected).abs() < 1.0e-6,
        "aggregate {aggregate} should be the geometric mean {expected}"
    );
}

#[test]
fn an_aggregate_containing_a_screen_says_so() {
    // An equal-weight mean over three-block estimates and one-pair screens is
    // as strong as its weakest component, and printing it unmarked would
    // overstate the campaign.
    assert!(adjudicated(&campaign_with(0.95)).aggregate_includes_screens);
}

// ---------------------------------------------------------------- target

#[test]
fn no_declared_target_emits_no_target_met() {
    assert_eq!(adjudicated(&campaign_with(0.90)).target_met, None);
}

#[test]
fn a_declared_target_is_computed_not_asserted() {
    let mut campaign = campaign_with(0.50);
    campaign.manifest.target = Some(Target {
        catalog: "patterns/primary.lx".to_owned(),
        metric: Metric::Wall,
        below: 6.0,
    });
    // Candidate arm runs at about 5.0-5.1s against a 6.0s target.
    assert_eq!(adjudicated(&seal(campaign.clone())).target_met, Some(true));

    campaign.manifest.target = Some(Target {
        catalog: "patterns/primary.lx".to_owned(),
        metric: Metric::Wall,
        below: 4.0,
    });
    assert_eq!(adjudicated(&seal(campaign)).target_met, Some(false));
}

/// A campaign whose affected catalog is unresolved at three blocks: its
/// interval admits a regression past the ceiling while its mean passes.
pub(crate) fn campaign_with_unresolved_guardrail() -> Campaign {
    let mut observations = blocks("patterns/primary.lx", 3, 0.80);
    observations.extend(noisy_blocks("patterns/affected.lx", 3, &[0.98, 1.02, 1.00]));
    observations.extend(screen("patterns/unaffected.lx", 1.0));
    observations.extend(screen("patterns/floor.lx", 1.0));
    seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    })
}

/// A campaign with two blocking catalogs, one of which can never resolve.
///
/// `affected` sits at the ladder's last rung with an interval still straddling
/// its ceiling — the shape campaign `blake3:909e124d` reached on
/// `patterns/collexeme.lx`, 1.0220 across [1.0160, 1.0280] after twenty
/// blocks. `unaffected` screens over its ceiling and has never been promoted.
/// A planner that takes its escalation set from the verdict names only the
/// first, which owes nothing further, and reports the campaign complete.
pub(crate) fn campaign_with_a_stuck_catalog_beside_a_blocking_one() -> Campaign {
    let mut observations = blocks("patterns/primary.lx", 3, 0.80);
    observations.extend(noisy_blocks(
        "patterns/affected.lx",
        20,
        &[1.015, 1.025, 1.020],
    ));
    observations.extend(screen("patterns/unaffected.lx", 1.30));
    observations.extend(screen("patterns/floor.lx", 1.0));
    seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    })
}

/// A campaign whose unaffected catalog screens over its ceiling, so the report
/// must ask for escalation on that catalog by name.
pub(crate) fn campaign_with_escalating_screen() -> Campaign {
    let mut observations = blocks("patterns/primary.lx", 3, 0.95);
    observations.extend(blocks("patterns/affected.lx", 3, 1.0));
    observations.extend(screen("patterns/unaffected.lx", 1.30));
    observations.extend(screen("patterns/floor.lx", 1.0));
    seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    })
}

// ------------------------------------------------------------------- ladder

#[test]
fn the_current_look_is_derived_from_blocks_not_from_the_ladder_length() {
    // Conflating these made `InconclusiveExhausted` unreachable and made every
    // report claim the ladder was already spent. Found by reading real output,
    // not by these tests, which is why it is pinned here now.
    let adjudication = adjudicated(&campaign_with(0.90));
    assert_eq!(
        adjudication.look, 1,
        "three blocks is the first rung of [3, 8, 20]"
    );
    assert_eq!(adjudication.looks, 3, "the ladder length is Bonferroni's K");
}

#[test]
fn a_campaign_at_the_last_rung_is_exhausted_rather_than_merely_inconclusive() {
    // 20 blocks of a [3, 8, 20] ladder with an effect that straddles parity:
    // there is no further rung, so the answer is stop, not escalate.
    let mut observations = noisy_blocks("patterns/primary.lx", 20, &[0.98, 1.02, 0.99, 1.01]);
    observations.extend(blocks("patterns/affected.lx", 3, 1.0));
    observations.extend(screen("patterns/unaffected.lx", 1.0));
    observations.extend(screen("patterns/floor.lx", 1.0));
    let campaign = seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    });

    let adjudication = adjudicated(&campaign);
    assert_eq!(adjudication.look, 3);
    assert_eq!(adjudication.verdict, Verdict::InconclusiveExhausted);
}

#[test]
fn a_campaign_at_an_early_rung_is_inconclusive_and_can_still_escalate() {
    // Spread tight enough to stay inside the 2% guardrail ceiling but wide
    // enough to straddle both parity and the 0.5% minimum. A looser spread
    // returns `UnresolvedGuardrail` instead — correctly, because at three
    // blocks the interval is wide and guardrails outrank the primary.
    let mut observations = noisy_blocks("patterns/primary.lx", 3, &[0.998, 1.002, 1.0]);
    observations.extend(blocks("patterns/affected.lx", 3, 1.0));
    observations.extend(screen("patterns/unaffected.lx", 1.0));
    observations.extend(screen("patterns/floor.lx", 1.0));
    let campaign = seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    });

    let adjudication = adjudicated(&campaign);
    assert_eq!(adjudication.look, 1);
    assert_eq!(adjudication.verdict, Verdict::Inconclusive);
}

// -------------------------------------------------------- guardrail primacy

#[test]
fn a_screen_over_its_ceiling_outranks_a_winning_primary() {
    // The failure this pins: the campaign published `Win` while its own
    // report listed the catalog under "Escalation owed". A screen pair has no
    // interval, so it is a guardrail nobody has adjudicated — not a passed
    // one. Missed by every test here for exactly one reason: the existing
    // screen test asserted the *stream outcome* and never the verdict.
    let mut observations = blocks("patterns/primary.lx", 3, 0.80);
    observations.extend(blocks("patterns/affected.lx", 3, 1.0));
    observations.extend(screen("patterns/unaffected.lx", 1.30));
    observations.extend(screen("patterns/floor.lx", 1.0));
    let campaign = seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    });

    let adjudication = adjudicated(&campaign);
    assert_eq!(
        adjudication.verdict,
        Verdict::EscalateGuardrail {
            catalog: "patterns/unaffected.lx".to_owned(),
            metric: Metric::Wall,
        },
        "an unadjudicated guardrail outranks a primary that would otherwise win"
    );
}

#[test]
fn an_escalate_guardrail_is_never_reported_as_exhausted() {
    // Escalating a screen means promoting that catalog from one pair to
    // blocks, which stays available however far the primary's ladder has run.
    let mut observations = blocks("patterns/primary.lx", 20, 0.80);
    observations.extend(blocks("patterns/affected.lx", 3, 1.0));
    observations.extend(screen("patterns/unaffected.lx", 1.30));
    observations.extend(screen("patterns/floor.lx", 1.0));
    let campaign = seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    });

    let adjudication = adjudicated(&campaign);
    assert_eq!(adjudication.look, 3, "the primary is at the last rung");
    assert!(matches!(
        adjudication.verdict,
        Verdict::EscalateGuardrail { .. }
    ));
}

// ------------------------------------------------------- optional stopping

#[test]
fn a_win_unblocked_by_a_guardrail_clearing_is_not_an_ordinary_win() {
    // Continuing to measure a guardrail is *not* always conservative. One
    // that blocked the verdict at an earlier look and clears at a later one
    // unblocks a win that was previously unavailable, and reporting that as
    // an ordinary `Win` launders a stopping rule chosen after seeing the
    // data. Detected by replaying the earlier rung, which a campaign file
    // supports because it is append-only and every observation carries its
    // block.
    let mut observations = blocks("patterns/primary.lx", 8, 0.80);
    // Wide and straddling the 2% ceiling over the first three blocks; tight
    // and flat over the remaining five, so the eight-block interval clears.
    observations.extend(blocks_with_ratios(
        "patterns/affected.lx",
        &[0.98, 1.02, 1.00, 1.0, 1.0, 1.0, 1.0, 1.0],
    ));
    observations.extend(screen("patterns/unaffected.lx", 1.0));
    observations.extend(screen("patterns/floor.lx", 1.0));
    let campaign = seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    });

    let adjudication = adjudicated(&campaign);
    assert_eq!(adjudication.look, 2, "eight blocks is the second rung");
    assert_eq!(
        adjudication.verdict,
        Verdict::WinPendingConfirmation {
            unblocked_by: "patterns/affected.lx on wall".to_owned()
        },
        "the guardrail was unresolved at look 1 and clear at look 2"
    );
}

#[test]
fn a_win_whose_guardrails_were_always_clear_is_an_ordinary_win() {
    // The other half: replay must not turn every multi-look win into a
    // pending one.
    let mut observations = blocks("patterns/primary.lx", 8, 0.80);
    observations.extend(blocks("patterns/affected.lx", 8, 1.0));
    observations.extend(screen("patterns/unaffected.lx", 1.0));
    observations.extend(screen("patterns/floor.lx", 1.0));
    let campaign = seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    });

    assert_eq!(adjudicated(&campaign).verdict, Verdict::Win);
}

#[test]
fn the_target_is_the_candidate_arms_mean_not_its_worst_run() {
    // These differ, and the difference is a silently stricter contract than
    // the one written down. Candidate runs here are 8.0 and one outlier at
    // 12.0, mean 8.8; a maximum-based rule would call a 10.0 target unmet.
    let mut campaign = campaign_with(0.80);
    campaign.manifest.target = Some(Target {
        catalog: "patterns/primary.lx".to_owned(),
        metric: Metric::Wall,
        below: 10.0,
    });
    for observation in &mut campaign.observations {
        if observation.catalog == "patterns/primary.lx" && observation.arm == "b" {
            observation.wall_s = 8.0;
        }
    }
    if let Some(outlier) = campaign
        .observations
        .iter_mut()
        .find(|observation| observation.catalog == "patterns/primary.lx" && observation.arm == "b")
    {
        outlier.wall_s = 12.0;
    }
    assert_eq!(adjudicated(&seal(campaign)).target_met, Some(true));
}

#[test]
fn a_guardrail_on_the_primary_catalog_also_gates_a_pending_win() {
    // Replay skipped the primary *catalog*, which also skipped its peak-RSS
    // and user-CPU guardrails. A primary whose wall clock wins while its own
    // RSS was unresolved at look 1 and clears at look 2 is unblocked by
    // exactly the optional stopping this detects — and it read as an ordinary
    // win.
    let mut observations = blocks("patterns/primary.lx", 8, 0.80);
    // Reshape the primary's own RSS: wide across the first three blocks,
    // flat across the remaining five.
    for observation in &mut observations {
        let spread = match observation.block {
            0 => 0.98,
            1 => 1.02,
            _ => 1.0,
        };
        if observation.arm == "b" {
            observation.peak_rss_kb = 100_000.0 * spread;
        }
    }
    observations.extend(blocks("patterns/affected.lx", 3, 1.0));
    observations.extend(screen("patterns/unaffected.lx", 1.0));
    observations.extend(screen("patterns/floor.lx", 1.0));
    let campaign = seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    });

    assert_eq!(
        adjudicated(&campaign).verdict,
        Verdict::WinPendingConfirmation {
            unblocked_by: "patterns/primary.lx on peak_rss".to_owned()
        },
        "a guardrail on the primary catalog is still a guardrail"
    );
}

#[test]
fn a_sealed_command_reconstructs_exactly_for_the_catalog_under_measurement() {
    let manifest = manifest();
    assert_eq!(
        manifest.expected_argv("patterns/double-wo.lx"),
        Ok(Some(vec![
            "matches".to_owned(),
            "--patterns".to_owned(),
            "patterns/double-wo.lx".to_owned(),
            "--corpus".to_owned(),
            "corpus.lingcorpus".to_owned(),
            "--limit".to_owned(),
            "all".to_owned(),
        ])),
        "the hole takes the catalog and every other argument is reproduced \
         in place",
    );
}

#[test]
fn a_sealed_command_selects_the_prepared_corpus_assigned_to_each_catalog() {
    let mut manifest = manifest();
    manifest.command_template = vec![
        TemplateArg::Literal("matches".to_owned()),
        TemplateArg::Literal("--patterns".to_owned()),
        TemplateArg::Catalog,
        TemplateArg::Literal("--corpus".to_owned()),
        TemplateArg::Corpus,
    ];
    manifest.catalog_corpora = [
        (
            "patterns/primary.lx".to_owned(),
            "morphology.lingcorpus".to_owned(),
        ),
        (
            "patterns/affected.lx".to_owned(),
            "dependencies.lingcorpus".to_owned(),
        ),
    ]
    .into_iter()
    .collect();

    assert_eq!(
        manifest
            .expected_argv("patterns/primary.lx")
            .expect("the primary has a corpus"),
        Some(vec![
            "matches".to_owned(),
            "--patterns".to_owned(),
            "patterns/primary.lx".to_owned(),
            "--corpus".to_owned(),
            "morphology.lingcorpus".to_owned(),
        ])
    );
    assert_eq!(
        manifest
            .expected_argv("patterns/affected.lx")
            .expect("the affected catalog has a corpus")
            .expect("the command is sealed")
            .last()
            .map(String::as_str),
        Some("dependencies.lingcorpus")
    );
    assert!(matches!(
        manifest.expected_argv("patterns/unaffected.lx"),
        Err(TemplateResolutionError::CorpusNotMapped { .. })
    ));
}

#[test]
fn an_empty_catalog_corpus_map_preserves_the_archived_manifest_encoding() {
    let manifest = manifest();
    let encoded = serde_json::to_vec(&manifest).expect("manifest encoding");
    let json: serde_json::Value = serde_json::from_slice(&encoded).expect("the encoding is JSON");
    assert!(
        json.get("catalog_corpora").is_none(),
        "the default must stay absent so archived manifest digests do not move"
    );
    let decoded: Manifest = serde_json::from_slice(&encoded).expect("manifest round trip");
    assert_eq!(decoded.digest(), manifest.digest());
}

#[test]
fn changing_a_catalogs_prepared_corpus_changes_the_manifest_seal() {
    let mut first = manifest();
    first.catalog_corpora.insert(
        "patterns/primary.lx".to_owned(),
        "morphology.lingcorpus".to_owned(),
    );
    let mut second = first.clone();
    second.catalog_corpora.insert(
        "patterns/primary.lx".to_owned(),
        "dependencies.lingcorpus".to_owned(),
    );
    assert_ne!(first.digest(), second.digest());
}

#[test]
fn a_campaign_declared_before_the_command_was_sealed_cannot_be_checked() {
    let mut manifest = manifest();
    manifest.command_template = Vec::new();
    assert_eq!(
        manifest.expected_argv("patterns/double-wo.lx"),
        Ok(None),
        "`None` is what makes `record` skip the comparison rather than refuse \
         every archived campaign",
    );
}

#[test]
fn a_misspelled_template_hole_is_refused_rather_than_taken_as_a_literal() {
    // Taking it as a literal would seal a command no run can match, so every
    // block would be refused for a reason pointing at the run rather than at
    // the declaration that caused it.
    assert!(TemplateArg::parse("{catlog}").is_err());
    assert!(TemplateArg::parse("{}").is_ok(), "not hole-shaped");
    assert_eq!(
        TemplateArg::parse("{catalog}"),
        Ok(TemplateArg::Catalog),
        "the one hole there is",
    );
    assert_eq!(
        TemplateArg::parse("{corpus}"),
        Ok(TemplateArg::Corpus),
        "the prepared corpus is the other sealed per-catalog hole",
    );
    assert_eq!(
        TemplateArg::parse("--limit"),
        Ok(TemplateArg::Literal("--limit".to_owned())),
    );
}

#[test]
fn a_template_argument_carrying_a_newline_is_refused() {
    // `lingrep-perf template` prints one argument per line for the runner to read
    // back, so an argument containing a newline would be read as two.
    assert!(TemplateArg::parse("--patterns\nx").is_err());
    assert!(TemplateArg::parse("a\0b").is_err());
}

// ------------------------------------------------------------- saturation

#[test]
fn occupancy_is_reported_per_catalog_against_the_sealed_pool() {
    let campaign = campaign_with(1.0);
    let occupancy = worker_normalized_cpu_occupancy(&campaign);
    let primary = occupancy
        .get("patterns/primary.lx")
        .copied()
        .expect("the primary was measured");
    assert!(
        (primary - 0.375).abs() < 1.0e-9,
        "the fixture runs at user = 3 x wall on a sealed pool of 8: {primary}",
    );
    assert_eq!(
        occupancy.len(),
        4,
        "every measured catalog, including the ones that only screened",
    );
}

/// The estimate is a planning number and says so. An earlier version of this
/// rule would have called 11% of user time out of reach on the campaign that
/// went on to remove 13.3% of it.
#[test]
fn a_wall_campaign_is_told_what_its_minimum_implies_about_user_time() {
    let mut campaign = campaign_with(1.0);
    campaign.manifest.minimum_useful_effect_pct = 5.0;
    let estimate = wall_planning_estimate(&campaign).expect("a wall campaign gets an estimate");
    assert!(estimate.contains("13%"), "5% / 0.375: {estimate}");
    assert!(estimate.contains("occupancy 0.37"), "{estimate}");
    assert!(
        !estimate.contains("more user time than there is"),
        "13% of user time is reachable, and the estimate must not claim \
         otherwise: {estimate}",
    );
}

/// The one tier that is a claim rather than an estimate, and it follows from
/// the definition rather than from a chosen threshold.
#[test]
fn a_minimum_implying_more_user_time_than_exists_is_stated_as_unreachable() {
    let mut campaign = campaign_with(1.0);
    campaign.manifest.minimum_useful_effect_pct = 40.0;
    let estimate = wall_planning_estimate(&campaign).expect("a wall campaign gets an estimate");
    assert!(
        estimate.contains("more user time than there is"),
        "40% / 0.375 is 107%: {estimate}",
    );
    assert!(
        estimate.contains("I/O or decode"),
        "and it must still not read as a refusal: {estimate}",
    );
}

#[test]
fn a_campaign_that_is_not_measured_on_wall_gets_no_estimate() {
    let mut campaign = campaign_with(1.0);
    campaign.manifest.primary_metric = Metric::User;
    assert!(
        wall_planning_estimate(&campaign).is_none(),
        "the estimate answers a question only a wall campaign is asking",
    );

    campaign.manifest.primary_metric = Metric::Wall;
    campaign.manifest.workers = None;
    assert!(
        wall_planning_estimate(&campaign).is_none(),
        "and it has no denominator without a sealed pool",
    );
}

// ----------------------------------------------------------- alpha budget

#[test]
fn the_budget_prices_the_metric_family_and_not_the_ladder_alone() {
    let campaign = campaign_with(0.9);
    assert_eq!(campaign.manifest.escalation_ladder.len(), 3);
    assert_eq!(
        campaign.decisions(),
        9,
        "three looks x three verdict-bearing metrics. Every one of them is \
         computed for every catalog and printed, so the family available for \
         post-hoc selection is that size whatever the campaign declared.",
    );
    assert_eq!(
        adjudicated(&campaign).decisions,
        9,
        "and the adjudication reports what it charged",
    );
}

/// The budget is a property of what the report exposes, not of what anyone
/// recorded. Pricing recorded selections would let an operator inspect three
/// and pay for one, and would count `wall -> user -> wall` as three
/// hypotheses when there are two.
#[test]
fn recording_a_selection_does_not_change_the_budget() {
    let mut campaign = campaign_with(0.9);
    let before = campaign.decisions();
    reselect(&mut campaign, Metric::User, "the workload is not CPU-bound").expect("a selection");
    reselect(&mut campaign, Metric::Wall, "and back again").expect("a second selection");
    assert_eq!(campaign.decisions(), before);
}

#[test]
fn a_reselection_moves_the_metric_the_verdict_turns_on() {
    let mut campaign = campaign_with(0.9);
    assert_eq!(campaign.effective_primary_metric(), Metric::Wall);

    reselect(&mut campaign, Metric::User, "the workload is not CPU-bound").expect("a selection");
    assert_eq!(campaign.effective_primary_metric(), Metric::User);
    assert_eq!(
        campaign.manifest.primary_metric,
        Metric::Wall,
        "the manifest still says what was declared; the selection sits \
         outside it because it was made after sealing",
    );
    assert_eq!(adjudicated(&campaign).primary_metric, Metric::User);
}

#[test]
fn a_selection_that_learns_nothing_or_says_nothing_is_refused() {
    let mut campaign = campaign_with(0.9);
    assert_eq!(
        reselect(&mut campaign, Metric::Wall, "no reason to"),
        Err(Refusal::SelectionRepeatsMetric {
            metric: Metric::Wall
        }),
    );
    assert_eq!(
        reselect(&mut campaign, Metric::User, "   "),
        Err(Refusal::SelectionReasonEmpty),
        "a selection without a stated reason is the artifact this log exists \
         to make visible",
    );
    assert!(campaign.selections.is_empty(), "neither was recorded");
}

// ------------------------------------------------- externally paced wall

/// `count` balanced blocks whose wall and user ratios differ.
///
/// The shape a rate limiter outside the measured process produces: the
/// candidate arm does materially less CPU work and finishes at the same wall
/// clock, because something neither arm controls is setting the pace.
fn paced_blocks(catalog: &str, count: usize, wall_ratio: f64, user_ratio: f64) -> Vec<Observation> {
    let mut observations = Vec::new();
    for index in 0..count {
        let base = 10.0 + f64::from(u32::try_from(index).expect("small")) * 0.1;
        for (arm, order) in [("a", 0), ("b", 1), ("b", 2), ("a", 3)] {
            let candidate = arm == "b";
            let wall = base * if candidate { wall_ratio } else { 1.0 };
            let mut observation = run(catalog, arm, index, order, wall);
            observation.user_s = base * 3.0 * if candidate { user_ratio } else { 1.0 };
            observations.push(observation);
        }
    }
    observations
}

fn paced_campaign(wall_ratio: f64, user_ratio: f64) -> Campaign {
    let mut observations = blocks("patterns/primary.lx", 3, 0.5);
    observations.extend(paced_blocks(
        "patterns/affected.lx",
        3,
        wall_ratio,
        user_ratio,
    ));
    observations.extend(screen("patterns/unaffected.lx", 1.0));
    observations.extend(screen("patterns/floor.lx", 1.0));
    seal(Campaign {
        manifest: manifest(),
        manifest_digest: String::new(),
        observations,
        selections: Vec::new(),
    })
}

#[test]
fn a_flat_wall_beside_a_moving_user_is_reported_as_externally_paced() {
    // The recorded case: a `--stream-output` campaign put four catalogs at
    // wall 0.99-1.01 while their user sat between 0.67 and 0.89, and the
    // verdict read them as neutral. They were not neutral; the hasher was
    // setting the pace for both arms.
    let campaign = paced_campaign(1.0, 0.7);
    let adjudication = adjudicated(&campaign);

    let paced: Vec<&str> = adjudication
        .externally_paced
        .iter()
        .map(|entry| entry.catalog.as_str())
        .collect();
    assert_eq!(paced, ["patterns/affected.lx"]);
}

#[test]
fn a_catalog_whose_wall_moved_with_its_user_is_not_paced() {
    // The ordinary case, and the one that must never be flagged: wall and
    // user agree that the change did something, so nothing outside the
    // process is setting the rate.
    let campaign = paced_campaign(0.7, 0.7);
    assert!(adjudicated(&campaign).externally_paced.is_empty());
}

#[test]
fn a_user_regression_beside_a_flat_wall_is_not_paced() {
    // Only the improving direction. User rising while wall stays put is a
    // different phenomenon — work moved off the critical path, or onto more
    // threads — and calling it "paced" would file it under the wrong cause.
    let campaign = paced_campaign(1.0, 1.4);
    assert!(adjudicated(&campaign).externally_paced.is_empty());
}

#[test]
fn a_screen_pair_is_never_paced() {
    // A point estimate has no interval, so it cannot disagree with another
    // interval about parity. `unaffected` and `floor` are screens in every
    // campaign here and must stay out of the list whatever their ratios.
    let campaign = paced_campaign(1.0, 0.7);
    let adjudication = adjudicated(&campaign);
    assert!(
        !adjudication
            .externally_paced
            .iter()
            .any(|entry| entry.catalog == "patterns/unaffected.lx"
                || entry.catalog == "patterns/floor.lx")
    );
}
