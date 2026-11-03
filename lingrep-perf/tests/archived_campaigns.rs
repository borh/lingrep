// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]
//! Sealed campaign fixtures preserve manifest digests and adjudication.

use lingrep_perf::campaign::{Campaign, adjudicate};

fn archived() -> Vec<(&'static str, std::path::PathBuf)> {
    let results =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/archived-campaigns");
    ["campaign-wall.json", "campaign-cpu.json"]
        .into_iter()
        .map(|name| (name, results.join(name)))
        .collect()
}

fn read(path: &std::path::Path) -> Campaign {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} must still parse: {error}", path.display()))
}

#[test]
fn an_archived_manifest_digest_survives_a_round_trip() {
    for (name, path) in archived() {
        let campaign = read(&path);
        let recomputed = campaign
            .manifest
            .digest()
            .unwrap_or_else(|| panic!("{name} must be digestible"));
        assert_eq!(
            recomputed, campaign.manifest_digest,
            "{name}: re-encoding the manifest moved its digest, so a field \
             added since it was archived is serializing when it is absent",
        );
    }
}

#[test]
fn an_archived_campaign_still_adjudicates() {
    for (name, path) in archived() {
        let campaign = read(&path);
        let adjudication = adjudicate(&campaign)
            .unwrap_or_else(|refusal| panic!("{name} must still adjudicate: {refusal:?}"));
        assert!(
            !adjudication.streams.is_empty(),
            "{name}: a campaign that adjudicates to nothing would pass the \
             check above without exercising it",
        );
    }
}

#[test]
fn fixed_block_estimate_rejects_short_samples_and_reports_known_ratio() {
    let path = std::env::temp_dir().join(format!("ling-estimate-{}.json", std::process::id()));
    for (input, valid) in [
        ("[[10,10,11,11],[20,20,22,22],[30,30,33,33]]", true),
        ("[[10,10,11,11]]", false),
        ("[[10,10,11,11],[0,0,11,11]]", false),
    ] {
        std::fs::write(&path, input).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_lingrep-perf"))
            .arg("estimate")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(output.status.success(), valid);
        if valid {
            let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!((result["ratio"].as_f64().unwrap() - 1.1).abs() < 1e-12);
            assert!((result["high"].as_f64().unwrap() - 1.1).abs() < 1e-12);
            assert_eq!(result["blocks"], 3);
        }
    }
    std::fs::remove_file(path).unwrap();
}

#[test]
fn comparison_cli_reports_missing_matching_and_changed_calibration() {
    use lingrep_perf::calibration::{CalibrationReference, MeasurementRig};
    let directory =
        std::env::temp_dir().join(format!("ling-calibration-cli-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let mut comparison = read(&archived()[0].1);
    comparison.manifest.measurement_rig = Some(MeasurementRig {
        hostname: "minimal".to_owned(),
        affinity: String::new(),
        backend: "gnu-time".to_owned(),
    });
    comparison.manifest.workers = Some(8);
    comparison.manifest.warmup_runs_per_arm = Some(1);
    comparison.manifest.declared_at_unix_seconds = Some(200);
    let mut calibration = comparison.clone();
    let primary = calibration.manifest.primary_catalog.clone();
    calibration
        .manifest
        .catalogs
        .retain(|catalog| catalog.path == primary);
    calibration
        .observations
        .retain(|observation| observation.catalog == primary);
    calibration.manifest.escalation_ladder = vec![3];
    calibration.manifest.calibration_wall_tolerance_pct = Some(2.0);
    calibration.manifest.arms.b.binary_digest = calibration.manifest.arms.a.binary_digest.clone();
    for observation in &mut calibration.observations {
        observation.wall_s = 1.0;
    }
    let seal = |campaign: &mut Campaign| {
        campaign.manifest_digest = campaign.manifest.digest().unwrap();
        for observation in &mut campaign.observations {
            observation.manifest_digest = campaign.manifest_digest.clone();
        }
    };
    seal(&mut calibration);
    let evidence = directory.join("calibration.json");
    std::fs::write(&evidence, serde_json::to_vec(&calibration).unwrap()).unwrap();
    let reference = CalibrationReference::from_file(&evidence).unwrap();
    let campaign_path = directory.join("comparison.json");
    for (linked, expected) in [
        (false, "calibration: missing"),
        (true, "calibration: matched"),
    ] {
        comparison.manifest.calibration = linked.then(|| reference.clone());
        seal(&mut comparison);
        std::fs::write(&campaign_path, serde_json::to_vec(&comparison).unwrap()).unwrap();
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_lingrep-perf"))
            .args(["adjudicate", "--campaign"])
            .arg(&campaign_path)
            .arg("--verdict-only")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(expected));
    }
    std::fs::write(&evidence, b"changed evidence").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lingrep-perf"))
        .args(["adjudicate", "--campaign"])
        .arg(&campaign_path)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("Calibration: unqualified: calibration evidence changed")
    );
    std::fs::remove_dir_all(directory).unwrap();
}
