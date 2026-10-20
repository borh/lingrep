//! What each catalog still owes.
//!
//! Two questions, and the second is the one an earlier version answered wrong
//! by not asking it:
//!
//! 1. Has this catalog reached the coverage its tier requires?
//! 2. Given what the campaign currently adjudicates to, does it need to
//!    advance to the next ladder rung?
//!
//! Answering only the first makes `suite` a one-shot: once a Tier 2 catalog
//! has its three blocks nothing is ever reported outstanding again, even when
//! the verdict is `unresolved-guardrail` and says in so many words that the
//! catalog must escalate. The advertised 3 → 8 → 20 escalation then has to be
//! driven by hand, which is exactly the knowledge this module exists to hold.
//!
//! Pure, like the adjudicator: `plan` is a function of a campaign file.

use std::collections::BTreeSet;

use crate::campaign::{
    Campaign, Coverage, StreamOutcome, Verdict, adjudicate, progress, required_coverage,
};

/// One catalog's standing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub catalog: String,
    /// `blocks` or `screen`.
    pub kind: &'static str,
    /// Complete units owed in total, including any escalation.
    pub required: usize,
    /// Units with every run recorded.
    pub complete: usize,
    /// Runs recorded in the unit currently in progress. Non-zero means an
    /// interrupted block that must be *resumed*, not skipped.
    pub partial_runs: usize,
    /// The `(block, order)` the next run must carry, and how many runs that
    /// block holds. Emitted so the orchestrator holds no model of its own:
    /// the screen at block 0 is two runs and a promoted block is four, and
    /// duplicating that arithmetic in a shell is how it went wrong.
    pub next_block: usize,
    pub next_order: usize,
    pub next_width: usize,
    /// Why this catalog owes what it owes.
    pub reason: &'static str,
}

impl Row {
    /// Whether anything is still owed here.
    #[must_use]
    pub fn outstanding(&self) -> bool {
        self.complete < self.required || self.partial_runs > 0
    }
}

/// The next ladder rung strictly above `blocks`, if the ladder has one.
fn next_rung(ladder: &[usize], blocks: usize) -> Option<usize> {
    ladder.iter().copied().find(|rung| *rung > blocks)
}

/// What every catalog in the campaign's suite still owes.
#[must_use]
pub fn plan(campaign: &Campaign) -> Vec<Row> {
    let manifest = &campaign.manifest;

    // Only consulted once minimum coverage is met everywhere; before that it
    // refuses, and a refusal is not an escalation instruction.
    //
    // Every blocking stream, not the one the verdict names. A verdict names a
    // single catalog because it must report one thing; escalation is owed by
    // each catalog that blocks. Deriving the set from the verdict starved
    // them: one catalog that reaches the ladder's end still unresolved is
    // named forever, `next_rung` returns `None` so it owes nothing further,
    // and every other blocking catalog is left at its tier minimum with
    // `suite` reporting nothing outstanding. Campaign `blake3:909e124d` ran
    // to that state with nine blocking streams un-escalated and had to be
    // driven by hand.
    let escalating: BTreeSet<String> =
        adjudicate(campaign)
            .ok()
            .map_or_else(BTreeSet::new, |adjudication| {
                let mut catalogs: BTreeSet<String> = adjudication
                    .streams
                    .iter()
                    .filter(|stream| {
                        matches!(
                            stream.outcome,
                            StreamOutcome::UnresolvedGuardrail | StreamOutcome::EscalateGuardrail
                        )
                    })
                    .map(|stream| stream.catalog.clone())
                    .collect();
                // The primary straddles a landmark: more blocks on the primary is
                // precisely what the ladder is for.
                if adjudication.verdict == Verdict::Inconclusive {
                    catalogs.insert(manifest.primary_catalog.clone());
                }
                catalogs
            });

    let mut rows = Vec::new();
    for catalog in &manifest.catalogs {
        let (kind, minimum) = match required_coverage(manifest, catalog) {
            Coverage::Blocks(count) => ("blocks", count),
            Coverage::ScreenPair => ("screen", 1),
        };
        let standing = progress(campaign, &catalog.path);
        // A screen-based catalog's unit is its screen until it is promoted;
        // after that only four-run blocks count toward the rung.
        let complete = if kind == "screen" {
            usize::from(standing.screened || standing.next.0 > 0)
        } else {
            standing.blocks
        };
        let partial_runs = standing.partial_runs;

        // Promotion is a property of the *data*, not of the current verdict.
        // While a promoted catalog is part-way through its blocks the
        // campaign is short of coverage and adjudicates to a refusal, so the
        // verdict names nothing — and a catalog that decided its promotion
        // from the verdict alone would fall back to "screen, 1 of 1" and
        // stop mid-promotion.
        let escalating_here = escalating.contains(&catalog.path);
        let promoted = kind == "screen" && (standing.blocks > 0 || escalating_here);

        let (required, reason) = if escalating_here {
            // A screen escalates by becoming blocks at the ladder's first
            // rung; a block escalates to the next rung above where it is.
            let reached = if kind == "screen" {
                standing.blocks
            } else {
                complete
            };
            match next_rung(&manifest.escalation_ladder, reached) {
                Some(rung) => (rung, "escalation: this catalog blocks the verdict"),
                // The ladder is spent. Reporting more would ask for blocks
                // that cannot change the answer.
                None => (
                    reached.max(minimum),
                    "ladder exhausted; no further blocks are owed",
                ),
            }
        } else if promoted {
            // Mid-promotion: finish the rung this catalog was sent to.
            let rung = manifest
                .escalation_ladder
                .iter()
                .copied()
                .find(|rung| *rung >= standing.blocks)
                .unwrap_or(standing.blocks);
            (rung, "promoted from a screen; finishing its first rung")
        } else {
            (minimum, "tier minimum")
        };

        rows.push(Row {
            catalog: catalog.path.clone(),
            // A catalog escalating out of a screen owes blocks from then on.
            kind: if promoted { "blocks" } else { kind },
            required,
            complete: if promoted { standing.blocks } else { complete },
            partial_runs,
            next_block: standing.next.0,
            next_order: standing.next.1,
            next_width: standing.next_width,
            reason,
        });
    }
    rows
}

#[cfg(test)]
mod tests;
