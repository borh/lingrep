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
