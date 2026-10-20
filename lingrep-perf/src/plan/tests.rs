//! Planning, on synthetic campaigns.

use super::*;
use crate::campaign::tests::{
    campaign_with, campaign_with_a_stuck_catalog_beside_a_blocking_one,
    campaign_with_escalating_screen, campaign_with_unresolved_guardrail,
};

fn row<'a>(rows: &'a [Row], catalog: &str) -> &'a Row {
    rows.iter()
        .find(|row| row.catalog == catalog)
        .expect("every catalog in the suite is planned")
}

#[test]
fn a_catalog_at_its_tier_minimum_owes_nothing_when_the_campaign_resolves() {
    let campaign = campaign_with(0.80);
    let rows = plan(&campaign);
    assert!(
        rows.iter().all(|row| !row.outstanding()),
        "a resolved campaign at minimum coverage owes nothing: {rows:?}"
    );
}

#[test]
fn an_interrupted_block_is_reported_as_partial_rather_than_recorded() {
    // The bug this pins: counting distinct block numbers treats a block with
    // one run in it as recorded, so the next invocation starts at the
    // following block and the partial one stays permanently unbalanced — a
    // campaign refused for a reason no further measurement can repair.
    let mut campaign = campaign_with(0.80);
    campaign.observations.retain(|observation| {
        observation.catalog != "patterns/primary.lx"
            || observation.block < 2
            || observation.order == 0
    });

    let primary = row(&plan(&campaign), "patterns/primary.lx").clone();
    assert_eq!(primary.complete, 2, "two whole blocks, not three");
    assert_eq!(primary.partial_runs, 1, "block 2 has one run in it");
    assert!(primary.outstanding());
}

#[test]
fn an_unresolved_guardrail_makes_that_catalog_owe_the_next_rung() {
    // Without this, `suite` is a one-shot: once a Tier 2 catalog has its
    // three blocks nothing is ever outstanding again, even while the verdict
    // says in so many words that the catalog must escalate.
    let campaign = campaign_with_unresolved_guardrail();
    let rows = plan(&campaign);
    let affected = row(&rows, "patterns/affected.lx");

    assert_eq!(affected.required, 8, "the rung above three in [3, 8, 20]");
    assert!(affected.outstanding());
    assert!(
        !row(&rows, "patterns/unaffected.lx").outstanding(),
        "escalation is per catalog, not per campaign"
    );
}

#[test]
fn a_screen_that_must_escalate_owes_blocks_at_the_ladders_first_rung() {
    let campaign = campaign_with_escalating_screen();
    let rows = plan(&campaign);
    let escalating = row(&rows, "patterns/unaffected.lx");

    assert_eq!(escalating.kind, "blocks", "it stops being a screen");
    assert_eq!(escalating.required, 3);
    assert!(escalating.outstanding());
}

#[test]
fn a_catalog_at_the_last_rung_owes_nothing_further() {
    // The ladder is spent, so asking for more blocks would ask for evidence
    // that cannot change the answer.
    let ladder = [3, 8, 20];
    assert_eq!(next_rung(&ladder, 3), Some(8));
    assert_eq!(next_rung(&ladder, 8), Some(20));
    assert_eq!(next_rung(&ladder, 20), None);
}

#[test]
fn a_promoted_screen_owes_three_whole_blocks_beside_the_screen_it_already_has() {
    // The screen is what triggered promotion, not one of the blocks the rung
    // asks for. Counting it as one left the catalog measuring a screen plus
    // two blocks where the tier requires three blocks.
    let campaign = campaign_with_escalating_screen();
    let escalating = row(&plan(&campaign), "patterns/unaffected.lx").clone();

    assert_eq!(escalating.kind, "blocks");
    assert_eq!(escalating.required, 3, "three whole blocks");
    assert_eq!(escalating.complete, 0, "the screen is not one of them");
    assert_eq!(
        (
            escalating.next_block,
            escalating.next_order,
            escalating.next_width
        ),
        (1, 0, 4),
        "the next unit is a four-run block after the two-run screen"
    );
}

#[test]
fn a_promoted_catalog_can_be_driven_to_completion() {
    // End to end over the state machine, because every earlier failure here
    // was a transition rather than a value: the widths disagreed, so the
    // third run of the first promoted block was refused as out of sequence
    // and promotion could never finish.
    let mut campaign = campaign_with_escalating_screen();
    let catalog = "patterns/unaffected.lx";

    for _ in 0..64 {
        let row = row(&plan(&campaign), catalog).clone();
        if !row.outstanding() {
            break;
        }
        let arms: &[&str] = if row.next_width == 2 {
            &["a", "b"]
        } else if row.next_block.is_multiple_of(2) {
            &["a", "b", "b", "a"]
        } else {
            &["b", "a", "a", "b"]
        };
        for (order, arm) in arms.iter().enumerate().skip(row.next_order) {
            let expected = crate::campaign::progress(&campaign, catalog).next;
            assert_eq!(
                expected,
                (row.next_block, order),
                "the recorder and the planner must agree on the next position"
            );
            campaign.observations.push(crate::campaign::tests::run_at(
                catalog,
                arm,
                row.next_block,
                order,
                10.0,
            ));
        }
    }

    let final_row = row(&plan(&campaign), catalog).clone();
    assert!(
        !final_row.outstanding(),
        "promotion completes: {final_row:?}"
    );
    assert_eq!(final_row.complete, 3);
    assert_eq!(
        crate::campaign::progress(&campaign, catalog).blocks,
        3,
        "three four-run blocks, beside the screen at block 0"
    );
}

#[test]
fn a_catalog_at_the_ladders_end_does_not_starve_the_others() {
    // The defect this pins, found by running campaign `blake3:909e124d`:
    // `escalating` was one catalog taken from the verdict, and a verdict names
    // one thing. `patterns/collexeme.lx` was named, escalated 3 -> 8 -> 20,
    // and never resolved; `next_rung` then returned `None`, so it owed
    // nothing, nothing else was ever named, and `suite` printed "nothing
    // outstanding" with nine blocking streams still at their tier minimum.
    // The real answer had to be driven by hand.
    let campaign = campaign_with_a_stuck_catalog_beside_a_blocking_one();
    let rows = plan(&campaign);

    let stuck = row(&rows, "patterns/affected.lx");
    assert_eq!(stuck.complete, 20, "it reached the ladder's last rung");
    assert!(!stuck.outstanding(), "more blocks cannot change its answer");
    assert_eq!(stuck.reason, "ladder exhausted; no further blocks are owed");

    let blocked = row(&rows, "patterns/unaffected.lx");
    assert_eq!(blocked.kind, "blocks", "its screen was over its ceiling");
    assert_eq!(blocked.required, 3, "promoted to the ladder's first rung");
    assert!(
        blocked.outstanding(),
        "a blocking catalog owes its next rung whether or not the verdict \
         happens to name it: {blocked:?}"
    );
}

#[test]
fn escalation_is_owed_by_every_blocking_catalog_not_by_the_named_one() {
    // The same rule stated positively, so the fix is not satisfied by special
    // -casing an exhausted ladder: two catalogs blocking at once both owe.
    let campaign = campaign_with_a_stuck_catalog_beside_a_blocking_one();
    let rows = plan(&campaign);
    let escalating: Vec<&str> = rows
        .iter()
        .filter(|row| row.reason.starts_with("escalation:"))
        .map(|row| row.catalog.as_str())
        .collect();
    assert_eq!(escalating, ["patterns/unaffected.lx"]);
}
