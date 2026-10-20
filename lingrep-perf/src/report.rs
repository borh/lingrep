//! The results document a campaign produces.
//!
//! Generated from an adjudicated campaign, so incomplete coverage is refused
//! before rendering.
//!
//! Two sections are prompted for and left blank on purpose. Where the bound
//! went, and what was ruled out, are judgement, and a tool that filled them in
//! would present inferred judgement as measured evidence.

use std::fmt::Write as _;

use crate::campaign::{Adjudication, Campaign, Role, StreamOutcome, Verdict};

fn verdict_line(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Win => {
            "**Kept.** The primary interval clears the minimum useful effect.".to_owned()
        }
        Verdict::WinPendingConfirmation { unblocked_by } => format!(
            "**Win pending confirmation.** A guardrail on `{unblocked_by}` was unresolved at an \
             earlier look and cleared at a later one, and that clearing is what unblocked this \
             win. Discharge it with a fresh, fixed-size confirmation campaign — not with more \
             blocks here."
        ),
        Verdict::RevertPrimaryRegressed => "**Reverted.** The primary interval lies entirely on \
             the worse side of parity, inside the guardrail ceiling but not an improvement."
            .to_owned(),
        Verdict::RevertBelowMinimum => "**Reverted.** A real improvement, entirely short of the \
             minimum useful effect, so it does not pay for its permanent cost."
            .to_owned(),
        Verdict::Inconclusive => "**Inconclusive.** The primary interval spans both meaningful \
             gain and meaningful regression. Escalate to the next ladder look, or stop."
            .to_owned(),
        Verdict::InconclusiveExhausted => "**Inconclusive, ladder exhausted.** Stop. The effect \
             is too small or too noisy for this design; the answer is a redesign or a larger \
             effect, not more blocks."
            .to_owned(),
        Verdict::RejectGuardrail { catalog, metric } => format!(
            "**Rejected.** `{catalog}` regressed past its {} ceiling. A guardrail failure \
             outranks the primary metric regardless of what the primary did.",
            metric.name()
        ),
        Verdict::UnresolvedGuardrail { catalog, metric } => format!(
            "**Unresolved.** `{catalog}`'s {} interval admits a regression beyond its ceiling. \
             Its mean passing is not enough. Escalate that catalog — escalation is per catalog, \
             not per campaign — or reject the candidate.",
            metric.name()
        ),
        Verdict::EscalateGuardrail { catalog, metric } => format!(
            "**Escalate.** `{catalog}`'s {} screen pair is over its ceiling, and a screen has \
             no interval, so it can be neither cleared nor rejected as it stands. Promote that \
             catalog to paired blocks and re-adjudicate. This outranks the primary: an \
             unadjudicated guardrail is not a passed one.",
            metric.name()
        ),
        Verdict::NotDecisive => "**Not decisive.** Tier 1 requires at least 20% on every \
             supported mode. Re-declare at Tier 2 or 3; this campaign cannot be escalated in \
             place."
            .to_owned(),
    }
}

fn outcome_note(outcome: StreamOutcome) -> &'static str {
    match outcome {
        StreamOutcome::RejectGuardrail => "over ceiling",
        StreamOutcome::UnresolvedGuardrail => "interval admits a regression past ceiling",
        StreamOutcome::EscalateGuardrail => "screen over ceiling — escalate, not reject",
        StreamOutcome::Screened => "screened (point estimate, no interval)",
        StreamOutcome::Within => "within ceiling",
    }
}

/// Render a campaign and its adjudication as a results document.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "one document rendered top to bottom; splitting it would scatter \
              the order a reader reads it in"
)]
pub fn render(campaign: &Campaign, adjudication: &Adjudication) -> String {
    let manifest = &campaign.manifest;
    let mut out = String::new();

    let _ = writeln!(out, "# {}\n", manifest.campaign);
    let _ = writeln!(out, "## Verdict\n");
    let _ = writeln!(out, "{}\n", verdict_line(&adjudication.verdict));

    if let Some(met) = adjudication.target_met {
        let _ = writeln!(
            out,
            "{}\n",
            if met {
                "**Target met — stop.** The declared absolute target is reached and every \
                 guardrail passed. Further optimization here is complexity nobody asked for."
            } else {
                "The declared absolute target is not yet reached."
            }
        );
    }

    let _ = writeln!(out, "## The bound, named before the change\n");
    let _ = writeln!(out, "- **Bound:** `{:?}`", manifest.bound);
    if let Some(runner_up) = manifest.runner_up_bound {
        let _ = writeln!(
            out,
            "- **Runner-up:** `{runner_up:?}` — usually what the next campaign hits"
        );
    }
    if let Some(evidence) = &manifest.profile_evidence {
        let _ = writeln!(out, "- **Profile that named it:** {evidence}");
    }
    let _ = writeln!(out, "- **Leverage rung:** `{:?}`", manifest.ladder_rung);
    let _ = writeln!(
        out,
        "- **Primary:** `{}` on {}\n",
        manifest.primary_catalog,
        adjudication.primary_metric.name()
    );

    let _ = writeln!(out, "## Setup\n");
    let _ = writeln!(
        out,
        "Tier {:?}. Ladder {:?}, at look {} of {}. Minimum useful effect {:.2}%.",
        manifest.tier,
        manifest.escalation_ladder,
        adjudication.look,
        adjudication.looks,
        manifest.minimum_useful_effect_pct
    );
    let _ = writeln!(
        out,
        "Build: profile `{}`, lto `{}`, codegen-units {}.",
        manifest.build.profile, manifest.build.lto, manifest.build.codegen_units
    );
    let _ = writeln!(out, "Manifest digest `{}`.", campaign.manifest_digest);
    let machine = &manifest.machine;
    let _ = writeln!(
        out,
        "Machine: {}, kernel {}, {}.",
        machine.cpu_model, machine.kernel, machine.rustc
    );
    let _ = writeln!(
        out,
        "\n| arm | label | commit | binary |\n|---|---|---|---|"
    );
    for (name, arm) in [("A", &manifest.arms.a), ("B", &manifest.arms.b)] {
        let _ = writeln!(
            out,
            "| {name} | {} | `{}` | `{}` |",
            arm.label, arm.commit, arm.binary_digest
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "Alpha is split by the union bound across {} looks x {} verdict-bearing metrics = {} \
         simultaneous decisions, so each uses a {:.2}% interval rather than 95%. The metric \
         family is priced because every one of them is computed and printed below, whatever \
         the campaign declared.\n",
        adjudication.looks,
        crate::campaign::Metric::VERDICT_BEARING.len(),
        adjudication.decisions,
        100.0 - 5.0 / f64::from(u32::try_from(adjudication.decisions).unwrap_or(1))
    );
    if !campaign.selections.is_empty() {
        let trail = campaign
            .selections
            .iter()
            .map(|selection| format!("`{}` (\"{}\")", selection.metric.name(), selection.reason))
            .collect::<Vec<_>>()
            .join(" -> ");
        let _ = writeln!(
            out,
            "Primary metric reselected {} time(s) after sealing: declared `{}`, then {trail}. \
             The budget above already covered this, so no interval moved.\n",
            campaign.selections.len(),
            manifest.primary_metric.name(),
        );
    }

    let _ = writeln!(out, "## Results\n");
    let _ = writeln!(out, "`r = B/A`; `r < 1` is an improvement.\n");
    let _ = writeln!(
        out,
        "| catalog | metric | r | interval | blocks | outcome |"
    );
    let _ = writeln!(out, "|---|---|---:|---|---:|---|");
    for stream in &adjudication.streams {
        let interval = if stream.effect.is_screen() {
            "— (screen pair)".to_owned()
        } else {
            format!("{:.4} – {:.4}", stream.effect.low, stream.effect.high)
        };
        let _ = writeln!(
            out,
            "| `{}` | {} | {:.4} | {} | {} | {} |",
            stream.catalog,
            stream.metric.name(),
            stream.effect.ratio,
            interval,
            stream.effect.blocks,
            outcome_note(stream.outcome)
        );
    }

    let _ = writeln!(out);
    match adjudication.aggregate {
        Some(aggregate) => {
            let _ = writeln!(
                out,
                "**Equal-weight geometric mean over substantive catalogs on {}: {:.4} ({:+.2}%).**",
                adjudication.primary_metric.name(),
                aggregate,
                (aggregate - 1.0) * 100.0
            );
            if adjudication.aggregate_includes_screens {
                let _ = writeln!(
                    out,
                    "\nThis aggregate mixes block estimates with screen pairs, so it is as \
                     strong as its weakest component. It describes broad effectiveness and \
                     never overrides a per-catalog guardrail."
                );
            }
        }
        None => {
            let _ = writeln!(out, "No substantive catalog produced an estimate.");
        }
    }

    if !adjudication.externally_paced.is_empty() {
        let _ = writeln!(out, "\n## Wall is externally paced\n");
        let primary_affected = adjudication
            .externally_paced
            .iter()
            .any(|paced| paced.catalog == manifest.primary_catalog);
        let _ = writeln!(
            out,
            "On these catalogs `user` moved and `wall` did not: the user interval excludes \
             parity while the wall interval still admits it. A rate limiter outside the \
             measured process — a digest consumer that cannot keep up, a disk that cannot \
             absorb the output — applies equally to both arms and produces exactly this \
             shape. **Their `wall` is not evidence that this change is neutral on them.**\n"
        );
        let _ = writeln!(out, "| catalog | wall | user |");
        let _ = writeln!(out, "|---|---|---|");
        for paced in &adjudication.externally_paced {
            let _ = writeln!(
                out,
                "| `{}` | {:.4} ({:.4} – {:.4}) | {:.4} ({:.4} – {:.4}) |",
                paced.catalog,
                paced.wall.ratio,
                paced.wall.low,
                paced.wall.high,
                paced.user.ratio,
                paced.user.low,
                paced.user.high,
            );
        }
        if manifest.output_capture == crate::campaign::OutputCapture::Stream {
            let _ = writeln!(
                out,
                "\nThis campaign declared `--stream-output`, which is the most common cause: \
                 the hasher becomes the pipeline's rate limiter on any catalog that publishes \
                 faster than it can consume. Re-run without it, or adjudicate on `user` with \
                 `lingrep-perf reselect`."
            );
        }
        if primary_affected && adjudication.primary_metric == crate::campaign::Metric::Wall {
            let _ = writeln!(
                out,
                "\n**The primary catalog is among them and the verdict turned on `wall`.** \
                 The verdict above rests on a paced measurement and should not be read as a \
                 statement about this change until the campaign is re-run on a rig that can \
                 keep up, or reselected onto `user`."
            );
        }
    }

    let occupancy = crate::campaign::worker_normalized_cpu_occupancy(campaign);
    let _ = writeln!(out, "\n## Saturation\n");
    if let Some(workers) = manifest.workers
        && !occupancy.is_empty()
    {
        let _ = writeln!(
            out,
            "`occupancy = mean(user) / (mean(wall) x {workers})` on arm A. It is average CPU \
             occupancy relative to the declared worker count, and **not** a bound on what a CPU \
             change can buy: `user` covers threads outside the pool, CPU and I/O overlap, \
             removed work need not be spread evenly, and relieving contention can move wall by \
             more than any linear projection.\n"
        );
        let _ = writeln!(out, "| catalog | occupancy |\n|---|---:|");
        let mut rows: Vec<(&String, &f64)> = occupancy.iter().collect();
        rows.sort_by(|left, right| right.1.total_cmp(left.1));
        for (catalog, value) in rows {
            let _ = writeln!(out, "| `{catalog}` | {:.0}% |", value * 100.0);
        }
        if let Some(estimate) = crate::campaign::wall_planning_estimate(campaign) {
            let _ = writeln!(out, "\n{estimate}");
        }
    } else {
        let _ = writeln!(
            out,
            "This campaign seals no worker count, so occupancy has no denominator and is not \
             reported."
        );
    }

    let floors: Vec<&str> = manifest
        .catalogs
        .iter()
        .filter(|catalog| catalog.role == Role::Floor)
        .map(|catalog| catalog.path.as_str())
        .collect();
    if !floors.is_empty() {
        let _ = writeln!(
            out,
            "\nReported and excluded from the aggregate by their `Floor` role: {}.",
            floors
                .iter()
                .map(|path| format!("`{path}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let escalate: Vec<&crate::campaign::Stream> = adjudication
        .streams
        .iter()
        .filter(|stream| {
            matches!(
                stream.outcome,
                StreamOutcome::UnresolvedGuardrail | StreamOutcome::EscalateGuardrail
            )
        })
        .collect();
    if !escalate.is_empty() {
        // The count, not only the name. "Escalate this catalog" without a
        // number leaves the operator to rederive the ladder by hand, which is
        // the knowledge the planner exists to hold.
        let plan = crate::plan::plan(campaign);
        let row_for = |catalog: &str| plan.iter().find(|row| row.catalog == catalog);
        // A catalog at the ladder's end owes nothing further, and the planner
        // already says so in `Row::reason`; without it this section would
        // print "20 blocks owed, 20 complete" beside an instruction to
        // escalate — advice no run can follow.
        let any_outstanding = escalate
            .iter()
            .any(|stream| row_for(&stream.catalog).is_some_and(crate::plan::Row::outstanding));

        let _ = writeln!(out, "\n## Escalation owed\n");
        if any_outstanding {
            let _ = writeln!(
                out,
                "Escalation is per catalog, not per campaign. `just perf-campaign suite --dir \
                 <campaign>` does this; the block counts are what it will run.\n"
            );
        } else {
            let _ = writeln!(
                out,
                "Nothing here can be escalated: every blocking catalog has reached the ladder's \
                 last rung. More blocks cannot change this campaign's answer — it is a redesign, \
                 a larger effect, or a fresh campaign whose ceilings were declared knowing \
                 this.\n"
            );
        }
        for stream in escalate {
            let owed = row_for(&stream.catalog).map_or_else(String::new, |row| {
                if row.outstanding() {
                    format!(
                        " — {} {} owed, {} complete",
                        row.required, row.kind, row.complete
                    )
                } else {
                    format!(" — {} {} complete; {}", row.complete, row.kind, row.reason)
                }
            });
            let _ = writeln!(
                out,
                "- `{}` on {} — {}{owed}",
                stream.catalog,
                stream.metric.name(),
                outcome_note(stream.outcome)
            );
        }
    }

    let _ = writeln!(out, "\n## Correctness\n");
    let _ = writeln!(
        out,
        "Output parity holds on every catalog: each carries one output digest across both arms, \
         with the `lingrep.query/v2` run-variant fields neutralized and nothing else.\n"
    );

    let _ = writeln!(out, "## Where the bound went\n");
    let _ = writeln!(
        out,
        "_Re-profile and record which bound now dominates. The bound usually moves after a \
         change, and the runner-up above is the first place to look._\n"
    );

    let _ = writeln!(out, "## What was ruled out, and why\n");
    let _ = writeln!(
        out,
        "_Candidates identified and not taken, with the reason. This is the most reused part of \
         a results document: it stops the next campaign re-deriving a dead end._\n"
    );

    let _ = writeln!(out, "## Retention guard\n");
    let _ = writeln!(
        out,
        "_A kept win owes a regression guard. Name it: `counted-work-pin` when one of the \
         counted-work counters captures the eliminated work, `benchmark`, `scheduled-timing`, or \
         `none-available` — which is legitimate here, and then requires the committed \
         reproduction harness and a stated trigger for re-measurement._"
    );

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::campaign::adjudicate;

    fn rendered(campaign: &Campaign) -> String {
        let adjudication = adjudicate(campaign).expect("a valid campaign adjudicates");
        render(campaign, &adjudication)
    }

    #[test]
    fn the_report_names_the_bound_the_rung_and_the_profile() {
        // A results document that does not say what bound was named cannot be
        // audited against its own outcome, which is how a misread profile
        // survives.
        let campaign = crate::campaign::tests::campaign_with(0.90);
        let report = rendered(&campaign);

        assert!(report.contains("MemoryLatency"), "{report}");
        assert!(report.contains("Representation"), "{report}");
        assert!(report.contains("AssertionSession 47.7%"), "{report}");
    }

    #[test]
    fn a_catalog_at_the_ladders_end_is_not_told_to_escalate() {
        // The renderer had the answer and threw it away: `plan` sets
        // `Row::reason` to "ladder exhausted", and this section printed
        // `required` and `complete` — "20 blocks owed, 20 complete" — beside a
        // standing instruction to run `suite`. No run can follow that advice.
        let campaign =
            crate::campaign::tests::campaign_with_a_stuck_catalog_beside_a_blocking_one();
        let report = rendered(&campaign);

        assert!(
            report.contains("20 blocks complete; ladder exhausted"),
            "the exhausted catalog states why nothing is owed: {report}"
        );
        assert!(
            !report.contains("20 blocks owed"),
            "and is not also billed for blocks it cannot run: {report}"
        );
        // Something here *can* still be escalated, so the instruction stands.
        assert!(report.contains("just perf-campaign suite"), "{report}");
    }

    #[test]
    fn a_campaign_with_nothing_left_to_escalate_says_so_instead_of_instructing() {
        let mut campaign =
            crate::campaign::tests::campaign_with_a_stuck_catalog_beside_a_blocking_one();
        // Drop the one catalog that could still be promoted, leaving only the
        // catalog whose ladder is spent.
        campaign
            .observations
            .retain(|observation| observation.catalog != "patterns/unaffected.lx");
        campaign
            .manifest
            .catalogs
            .retain(|catalog| catalog.path != "patterns/unaffected.lx");
        let campaign = crate::campaign::tests::seal(campaign);
        let report = rendered(&campaign);

        assert!(report.contains("Nothing here can be escalated"), "{report}");
        assert!(
            !report.contains("just perf-campaign suite"),
            "an instruction no run can follow is worse than none: {report}"
        );
    }

    #[test]
    fn every_measured_catalog_appears_in_the_table() {
        // Failure mode 6: a report published with four catalogs where the tier
        // required seven. Adjudication refuses a short campaign, and the
        // renderer prints whatever survived that.
        let campaign = crate::campaign::tests::campaign_with(0.90);
        let report = rendered(&campaign);

        for catalog in &campaign.manifest.catalogs {
            assert!(report.contains(&catalog.path), "{} missing", catalog.path);
        }
    }

    #[test]
    fn a_screen_pair_is_marked_rather_than_given_a_fabricated_interval() {
        let campaign = crate::campaign::tests::campaign_with(0.90);
        let report = rendered(&campaign);
        assert!(report.contains("— (screen pair)"), "{report}");
        assert!(
            report.contains("as strong as its weakest component"),
            "{report}"
        );
    }

    #[test]
    fn an_unresolved_guardrail_produces_an_escalation_section_naming_the_catalog() {
        let campaign = crate::campaign::tests::campaign_with_escalating_screen();
        let adjudication = adjudicate(&campaign).expect("a valid campaign adjudicates");
        let report = render(&campaign, &adjudication);

        assert!(report.contains("## Escalation owed"), "{report}");
        assert!(report.contains("patterns/unaffected.lx"), "{report}");
        assert!(report.contains("per catalog, not per campaign"), "{report}");
    }

    #[test]
    fn the_judgement_sections_are_prompted_and_left_empty() {
        let report = rendered(&crate::campaign::tests::campaign_with(0.90));
        assert!(report.contains("## Where the bound went"));
        assert!(report.contains("## What was ruled out, and why"));
        assert!(report.contains("## Retention guard"));
        assert!(report.contains("none-available"));
    }

    #[test]
    fn no_target_means_no_target_line() {
        let report = rendered(&crate::campaign::tests::campaign_with(0.90));
        assert!(!report.contains("Target met"));
    }

    #[test]
    fn the_escalation_section_names_the_block_count_not_only_the_catalog() {
        // "Escalate this catalog" without a number leaves the operator to
        // rederive the ladder by hand, which is the knowledge the planner
        // exists to hold — and it is what made `suite` a one-shot.
        let campaign = crate::campaign::tests::campaign_with_escalating_screen();
        let report = rendered(&campaign);

        assert!(report.contains("## Escalation owed"), "{report}");
        assert!(
            report.contains("patterns/unaffected.lx"),
            "the catalog by name: {report}"
        );
        assert!(
            report.contains("3 blocks owed"),
            "a screen escalates to blocks at the ladder's first rung: {report}"
        );
    }

    #[test]
    fn a_win_unblocked_by_an_escalation_says_so_rather_than_reading_as_a_clean_win() {
        let campaign = crate::campaign::tests::campaign_with(0.90);
        let adjudication = crate::campaign::Adjudication {
            verdict: crate::campaign::Verdict::WinPendingConfirmation {
                unblocked_by: "patterns/affected.lx on wall".to_owned(),
            },
            ..adjudicate(&campaign).expect("valid")
        };
        let report = render(&campaign, &adjudication);
        assert!(report.contains("Win pending confirmation"), "{report}");
        assert!(report.contains("patterns/affected.lx on wall"), "{report}");
    }

    #[test]
    fn the_setup_states_the_budget_it_charged_and_any_reselection() {
        let mut campaign = crate::campaign::tests::campaign_with(0.9);
        let report = rendered(&campaign);
        assert!(
            report.contains("3 looks x 3 verdict-bearing metrics = 9"),
            "{report}"
        );
        assert!(!report.contains("reselected"), "nothing was reselected yet");

        crate::campaign::reselect(
            &mut campaign,
            crate::campaign::Metric::User,
            "not CPU-bound",
        )
        .expect("a selection");
        let report = rendered(&campaign);
        assert!(report.contains("reselected 1 time(s)"), "{report}");
        assert!(
            report.contains("declared `wall`") && report.contains("`user` (\"not CPU-bound\")"),
            "both the sealed metric and the effective one, with the reason: {report}",
        );
        assert!(
            report.contains("no interval moved"),
            "the budget already covered it: {report}",
        );
    }

    #[test]
    fn saturation_reports_occupancy_without_calling_it_a_bound() {
        let report = rendered(&crate::campaign::tests::campaign_with(0.9));
        assert!(report.contains("## Saturation"), "{report}");
        assert!(
            report.contains("**not** a bound"),
            "the quantity was called a bound once and cost a campaign: {report}",
        );
        // The row, not the value: the fixture's occupancy lands on a float
        // that rounds either side of a half-percent, and pinning that would
        // be pinning the noise rather than the table.
        assert!(
            report.contains("| `patterns/primary.lx` | 3"),
            "every measured catalog gets a row: {report}",
        );
        assert!(
            report.contains("implies removing about"),
            "the planning estimate belongs beside the table: {report}",
        );
    }

    #[test]
    fn a_campaign_without_a_sealed_pool_says_so_rather_than_guessing() {
        let mut campaign = crate::campaign::tests::campaign_with(0.9);
        campaign.manifest.workers = None;
        // Re-sealed: the digest covers the manifest, so an edited one is
        // refused before it is rendered — which is the point of the digest.
        let campaign = crate::campaign::tests::seal(campaign);
        let report = rendered(&campaign);
        assert!(report.contains("## Saturation"), "{report}");
        assert!(report.contains("no worker count"), "{report}");
    }
}
