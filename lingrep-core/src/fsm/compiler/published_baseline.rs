//! Published-record baseline for the policy sublanguage migration.
//!
//! Pins published records across the policy-sublanguage migration. Metadata
//! cannot remain byte-identical because that migration removes policy keys.
//!
//! Driven by two input sets. Each catalog's own `examples:` are committed
//! beside the definitions they exercise, need no external input, and cover the
//! sites the migration touches by construction — each of the 13 copula
//! definitions has an example carrying the copula. But an example is authored
//! *for* its definition, so a migration error that only shows on a sentence
//! nobody wrote as an example would pass unseen. The committed token fixtures
//! ([`campaign_sentences`](crate::testing::campaign_sentences)) are the
//! sentences this repository has that nobody wrote for that purpose.
//!
//! **The claim this supports is scoped to those two sets.** It is not "no
//! sentence can differ": an edge-removal parity case can disagree when a `DA`
//! record covers a subject whole, and refuting that
//! statically needs a proof that no token satisfies both a subject's
//! first-token predicate and `DA`'s — a dictionary fact the compiler
//! deliberately does not depend on
//! (`docs/design/2026-08-06-policy-sublanguage-syntax.md` §3).
//!
//! Both the published and the authored name are recorded. `output_name` makes
//! them differ on exactly the definitions the migration touches, so recording
//! one alone would hide a rename.

#![cfg(test)]

use std::collections::BTreeMap;

use super::compile_catalog;
use super::limits::CompileLimits;
use crate::fsm::PatternSource;
use crate::fsm::policy::{PolicyStats, apply_policy_records};
use crate::fsm::policy_engine::ProgramStats;
use crate::testing::campaign_sentences::baseline_sentences;
use crate::testing::catalogs::tracked_catalog_sources;

#[test]
fn repository_catalog_published_records() {
    let baseline = tracked_catalog_sources()
        .into_iter()
        .map(|(name, text)| {
            let output =
                compile_catalog(PatternSource::named(&name, &text), CompileLimits::default())
                    .unwrap_or_else(|error| panic!("{name} must compile: {error}"));

            let mut per_sentence = BTreeMap::new();

            // Two input sets, one rendering. The catalogs' own `examples:`
            // cover the definitions the migration touches by construction; the
            // committed fixtures cover sentences nobody wrote for that purpose,
            // which is what makes this a parity gate rather than a check that
            // the examples still work. Both are assembled by
            // `campaign_sentences`, which the `lingrep-perf` counted-work screen
            // reads too — one assembly, so the two cannot drift apart.
            let sentences = baseline_sentences(&output.matcher);

            {
                for (label, tokens) in sentences {
                    let matches = output.matcher.find_matches(&tokens);
                    let anchors = output.matcher.anchor_positions(&tokens);
                    let mut stats = PolicyStats::default();
                    let mut program =
                        ProgramStats::for_program(output.matcher.catalog_policy().program());
                    let records = apply_policy_records(
                        output.matcher.catalog_policy(),
                        &matches,
                        &tokens,
                        &anchors,
                        &mut stats,
                        &mut program,
                        None,
                    )
                    .unwrap_or_else(|error| panic!("{name}/{label}: {error}"));

                    per_sentence.insert(
                        label,
                        records
                            .iter()
                            .map(|record| {
                                // Every component in full, not a count. The
                                // criterion is identical spans, names *and*
                                // captures, and `trim_*` drops a capture whose
                                // span leaves the published span — so a change
                                // to a capture's name, hygienic path or
                                // coordinates while the count holds is exactly
                                // what this baseline exists to catch.
                                let captures = record
                                    .components
                                    .iter()
                                    .map(|capture| {
                                        format!(
                                            "{}<{}>{}..{}",
                                            capture.name, capture.path, capture.start, capture.end
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                format!(
                                    "{} ({}) {}..{} raw {}..{} captures=[{captures}]",
                                    record.name,
                                    record.raw.pattern,
                                    record.start,
                                    record.end,
                                    record.raw.start,
                                    record.raw.end,
                                )
                            })
                            .collect::<Vec<_>>(),
                    );
                }
            }
            (name, per_sentence)
        })
        .collect::<BTreeMap<_, _>>();

    insta::assert_debug_snapshot!(baseline);
}
