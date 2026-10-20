//! Verification of a catalog's documented claims against the catalog itself.
//!
//! Two entry points, one per claim shape, and they are siblings rather than one
//! generalization:
//!
//! * [`verify_examples`] runs the `examples:` and `counterexamples:` keys —
//!   string lists on emitting patterns, reached through `patterns()`.
//! * [`verify_anchor_claims`] runs the `max_nesting:` key — one compound value
//!   on a `fragment`, reached through the anchor set. Fragments never emit and
//!   so never appear in `patterns()`, and a mapping cannot join a
//!   `&[(&str, bool)]` table, so neither half of the first entry point fits it.
//!
//! A claim is the only executable statement a catalog makes about itself. Left
//! unchecked it is prose that decays: the definition moves, the example stays,
//! and the file documents behaviour that does not exist. The check is
//! catalog-independent so every catalog is verified, not only one with its own
//! test file.
//!
//! Both take a tokenizer because claim verification needs a dictionary and the
//! compiler deliberately has none. Consequently, the dictionary-free
//! `ling check` command calls neither function and does not verify claims.

use crate::fsm::CompiledCatalog;
use crate::fsm::policy::NestingEntry;
use crate::token_match::{TokenAttributeIndex, TokenInfoTrait};
use compact_str::CompactString;

/// Why one documented claim failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExampleFailureKind {
    /// An `examples:` entry its own definition does not match.
    ExpectedMatch,
    /// A `counterexamples:` entry its own definition does match.
    UnexpectedMatch,
    /// The key is present but is not a list of strings, so nothing could be
    /// executed.
    Malformed,
}

/// One documented claim that the compiled catalog contradicts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExampleFailure {
    pub definition: String,
    /// The example text, or — for [`ExampleFailureKind::Malformed`] — a
    /// rendering of the offending value, so a report can say what it found.
    pub example: String,
    pub kind: ExampleFailureKind,
}

/// What one verification pass executed, and what it found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExampleReport {
    /// Entries actually tokenized and matched.
    ///
    /// Carried separately from `failures` because an empty failure list is the
    /// same value whether every claim held or none were found, and those are
    /// opposite outcomes. A caller that reports only failures cannot tell a
    /// working check from a vacuous one.
    pub checked: usize,
    pub failures: Vec<ExampleFailure>,
    /// Entries re-checked against the records the catalog actually publishes.
    ///
    /// Zero when the catalog declares no result policy — there is nothing for
    /// a second pass to disagree with, and running one would only claim
    /// coverage that means nothing.
    pub post_policy_checked: usize,
    /// Claims that hold against the canonical stream but not against what is
    /// published.
    ///
    /// This is the gap the raw pass cannot see: a definition can match its own
    /// example perfectly and still publish nothing for it, because `terminal`
    /// dropped the record or a neighbour suppressed it. Before this, such a
    /// catalog documented behaviour its own output contradicted, and every
    /// check passed.
    pub post_policy_failures: Vec<ExampleFailure>,
}

/// The metadata keys this verifies, and what a match means for each.
///
/// A caveat that applies only to the `must_match == false` half: a
/// counterexample passes whenever the definition does not match it, including
/// when the text could never have matched *anything* in the catalog. Such a
/// claim is vacuous — it would pass against any definition — and nothing here
/// detects that. Measured, not hypothetical: `をいずれ近いうちに実現を目指す`
/// was first proposed for `double_wo_exclude_directional_frame` and matches no
/// pattern in that catalog at all, because its final token is a 動詞 and every
/// anchor excludes 動詞.
///
/// Writing a counterexample as the *minimal pair* of an example is what avoids
/// this, and is what that bucket now carries: one token apart from a string
/// that does match, so it necessarily reaches the same language and can only
/// fail on the constraint under test.
const VERIFIED_KEYS: &[(&str, bool)] = &[
    // (key, must_match)
    ("examples", true),
    ("counterexamples", false),
];

/// Why one `max_nesting` entry failed.
///
/// Keyed on rather than matched by message text, following this crate's
/// diagnostic convention: the prose is presentation and the kind is the fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimFailureKind {
    /// A `witness` whose derived anchor is not the sentinel it marks. The
    /// bounded recognizer does not reach where an unbounded scanner would, so
    /// the claim of agreement is false.
    WitnessMissedAnchor,
    /// A `divergence` whose derived anchor *is* the sentinel. It records a
    /// disagreement that does not happen, and so records nothing.
    DivergenceAgreed,
    /// The entry does not nest to the depth its role requires — `depth` for a
    /// witness, `depth + 1` for a divergence. Without this the number is
    /// decorative: `depth: 0` could be written `depth: 42` and nothing would
    /// contradict it.
    WrongNestingDepth,
    /// The sentinel falls **inside** a token, so it names no boundary. An
    /// authoring mistake rather than a rounding case: silently moving it to the
    /// nearest boundary would accept a claim about a position the tokenizer
    /// cannot express.
    SentinelSplitsAToken,
}

/// One `max_nesting` entry the compiled catalog contradicts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimFailure {
    /// The anchor whose claim this is, not the `exclude` fragment carrying it:
    /// two anchors may share one `exclude`, and the claim is verified per
    /// anchor.
    pub anchor: String,
    /// The entry as the author wrote it, sentinel included, so a report quotes
    /// a line the catalog contains.
    pub entry: String,
    pub kind: ClaimFailureKind,
    /// The measurement that failed, in words — the positions or depths
    /// involved. Presentation; assert on [`Self::kind`].
    pub detail: String,
}

/// What one `max_nesting` verification pass executed, and what it found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaimReport {
    /// Entries actually tokenized and executed.
    ///
    /// Carried for the same reason [`ExampleReport::checked`] is: an empty
    /// failure list is the same value whether every entry held or none was
    /// found, and those are opposite outcomes.
    pub checked: usize,
    pub failures: Vec<ClaimFailure>,
}

/// Verifies every declared anchor's `max_nesting` claim.
///
/// The claim states where an *unbounded* scanner would land — upstream's
/// answer. A `witness` asserts the bounded recognizer reaches it; a
/// `divergence` asserts, and thereby records, that it does not. A must-not-match
/// test cannot express the second: at depth N+1 the `exclude` fragment still
/// matches, and what fails is *where the anchor lands*.
///
/// Uses the matcher's own interned anchor program, so no synthetic probe is
/// needed. Tokenization is injected for the reason this module's doc states.
///
/// Failures come out in anchor order, then witnesses before divergences, then
/// declaration order — all three deterministic, so unlike [`verify_examples`]
/// this needs no final sort to keep a CLI diff stable.
pub fn verify_anchor_claims<T: TokenInfoTrait>(
    matcher: &CompiledCatalog,
    tokenize: &dyn Fn(&str) -> Vec<T>,
) -> ClaimReport {
    let mut report = ClaimReport::default();
    for (anchor, claim) in matcher.nesting_claims() {
        for (entries, must_equal) in [(&claim.witness, true), (&claim.divergence, false)] {
            for entry in entries {
                report.checked += 1;
                if let Some((kind, detail)) =
                    check_entry(matcher, tokenize, anchor, claim.depth, entry, must_equal)
                {
                    report.failures.push(ClaimFailure {
                        anchor: anchor.to_string(),
                        entry: entry.to_string(),
                        kind,
                        detail,
                    });
                }
            }
        }
    }
    report
}

/// One entry, checked: its nesting depth first, then where the anchor lands.
///
/// The depth is checked first and returns on failure, because an entry that
/// does not nest as claimed is not evidence about the anchor either way — and
/// reporting both would tell the author two things about one mistake.
fn check_entry<T: TokenInfoTrait>(
    matcher: &CompiledCatalog,
    tokenize: &dyn Fn(&str) -> Vec<T>,
    anchor: &str,
    depth: u32,
    entry: &NestingEntry,
    must_equal: bool,
) -> Option<(ClaimFailureKind, String)> {
    let tokens = tokenize(entry.sentence());

    let required = if must_equal {
        depth
    } else {
        depth.saturating_add(1)
    };
    let actual = bracket_depth(&tokens);
    if actual != required {
        return Some((
            ClaimFailureKind::WrongNestingDepth,
            format!("nests to {actual}, not to the required {required}"),
        ));
    }

    let Some(expected) = token_index_at_char(&tokens, entry.sentinel()) else {
        return Some((
            ClaimFailureKind::SentinelSplitsAToken,
            format!(
                "the sentinel at char {} falls inside a token, so it names no boundary",
                entry.sentinel()
            ),
        ));
    };
    // The positions come from the same `CompiledCatalog` whose claims are being
    // verified, so the digest matches by construction; and `nesting_claims` is
    // keyed by exactly `CatalogPolicy::anchors`, which is what `anchor_programs`
    // — and so `AnchorPositions` — is keyed by. Both halves are structural, so
    // there is no state in which this can be `Err`.
    let derived = matcher
        .anchor_positions(&tokens)
        .get(anchor, matcher.anchor_digest())
        .expect("a matcher resolves the anchors it declares claims for");
    if (derived == expected) == must_equal {
        return None;
    }
    let kind = if must_equal {
        ClaimFailureKind::WitnessMissedAnchor
    } else {
        ClaimFailureKind::DivergenceAgreed
    };
    Some((
        kind,
        format!("anchor derived at token {derived}, sentinel at token {expected}"),
    ))
}

/// Maximum bracket nesting in a tokenized sentence.
///
/// **A deliberate UniDic tagset coupling in this crate's claim verifier**, and
/// the only one. UniDic tags brackets structurally — the raw `Pos2` field is
/// `括弧開` or `括弧閉` — which is what lets a depth recognizer be a plain
/// alternation rather than a literal bracket list. Reading it here is what makes `depth:`
/// load-bearing rather than decorative; the price is that this one function
/// knows a tagset. It runs only with an injected dictionary.
///
/// `Pos2` is **raw**, not the derived UPOS-like [`TokenAttributeIndex::Pos`]
/// that `[upos=PUNCT]` matches on: UniDic writes `補助記号-括弧開`, so the
/// bracket class is in `pos2` and the punctuation class in `pos1`.
///
/// Unbalanced closers clamp at zero rather than going negative. A bare `）` is
/// a *different* approximation from the depth bound — recorded in that document
/// as one this schema does not express — so it must not be counted as nesting.
fn bracket_depth<T: TokenInfoTrait>(tokens: &[T]) -> u32 {
    let mut depth = 0u32;
    let mut deepest = 0u32;
    for token in tokens {
        match token
            .get(TokenAttributeIndex::Pos2)
            .map(CompactString::as_str)
        {
            Some("括弧開") => {
                depth = depth.saturating_add(1);
                deepest = deepest.max(depth);
            }
            Some("括弧閉") => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// The token boundary at char offset `offset`, or `None` if it splits a token.
///
/// Boundaries are counted the way an anchor position is: `0` is before the
/// first token and `tokens.len()` is the sentence end, so an offset equal to
/// the whole sentence's length is the final boundary rather than out of range.
///
/// Reads [`TokenAttributeIndex::Text`], the surface text, because that is what
/// the offsets in the entry are offsets into. `Orth` is the normalized
/// orthography and agrees with the surface only by accident.
///
/// One rejection, not two: an offset that splits a token and one past the end
/// both fall out of the same final check, because `seen` only ever grows and so
/// never returns to `offset` once it has passed it. An early `seen > offset`
/// exit would read like a distinct case while changing no answer.
fn token_index_at_char<T: TokenInfoTrait>(tokens: &[T], offset: usize) -> Option<usize> {
    let mut seen = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        if seen == offset {
            return Some(index);
        }
        seen += token
            .get(TokenAttributeIndex::Text)
            .map_or(0, |text| text.chars().count());
    }
    (seen == offset).then_some(tokens.len())
}

/// Check every documented claim against the definition that carries it.
///
/// Tokenization is injected because it needs a dictionary and this crate's
/// compiler deliberately does not: `check` without a dictionary must stay
/// dictionary-free and fast.
///
/// Fragments are not reachable here — they never emit and so never appear in
/// `patterns()`. A claim attached to one is either reported by the compiler or,
/// for `max_nesting:`, executed by [`verify_anchor_claims`].
pub fn verify_examples<T: TokenInfoTrait>(
    matcher: &CompiledCatalog,
    tokenize: &dyn Fn(&str) -> Vec<T>,
) -> ExampleReport {
    let mut report = ExampleReport::default();
    for (name, pattern) in matcher.patterns() {
        for (key, must_match) in VERIFIED_KEYS {
            let Some(value) = pattern.metadata.get(*key) else {
                continue;
            };
            // A key that is present but not a list of strings is reported, not
            // skipped. Skipping it would make an unexecutable claim
            // indistinguishable from a claim that held.
            let Some(entries) = value.as_array() else {
                report.failures.push(ExampleFailure {
                    definition: name.to_string(),
                    example: format!("{key}: {value}"),
                    kind: ExampleFailureKind::Malformed,
                });
                continue;
            };
            for entry in entries {
                let Some(entry) = entry.as_str() else {
                    report.failures.push(ExampleFailure {
                        definition: name.to_string(),
                        example: format!("{key}: {entry}"),
                        kind: ExampleFailureKind::Malformed,
                    });
                    continue;
                };
                let tokens = tokenize(entry);
                let matched = matcher
                    .find_matches(&tokens)
                    .iter()
                    .any(|matched| matched.pattern == *name);
                report.checked += 1;
                if matched != *must_match {
                    report.failures.push(ExampleFailure {
                        definition: name.to_string(),
                        example: entry.to_owned(),
                        kind: if *must_match {
                            ExampleFailureKind::ExpectedMatch
                        } else {
                            ExampleFailureKind::UnexpectedMatch
                        },
                    });
                }
            }
        }
    }
    verify_post_policy(matcher, tokenize, &mut report);
    // `patterns()` is an IndexMap in source order, which is stable, but the
    // sort makes the report independent of it so a reordering in the catalog
    // cannot reorder a CLI diff.
    let order = |left: &ExampleFailure, right: &ExampleFailure| {
        left.definition
            .cmp(&right.definition)
            .then_with(|| left.example.cmp(&right.example))
            .then_with(|| (left.kind as u8).cmp(&(right.kind as u8)))
    };
    report.failures.sort_by(order);
    report.post_policy_failures.sort_by(order);
    report
}

/// Re-check every claim against the published records.
///
/// The raw pass asks "does this definition match its example?". This asks the
/// question a reader of the output actually has: "does this example produce a
/// published record?" They differ exactly where policy acts — an `admit` that
/// drops the record, an `exclude` that removes it, an `output_name` that
/// renames it.
///
/// A claim is judged against the name the definition *publishes* under, so a
/// rename is not itself a failure. Synthesized emitters carry no metadata and
/// are absent from `patterns()`, so they are not claim-carriers here either.
fn verify_post_policy<T: TokenInfoTrait>(
    matcher: &CompiledCatalog,
    tokenize: &dyn Fn(&str) -> Vec<T>,
    report: &mut ExampleReport,
) {
    let policy = matcher.catalog_policy();
    if !policy.declared() {
        return;
    }
    for (name, pattern) in matcher.patterns() {
        let published_name = policy.published_name(name);
        for (key, must_match) in VERIFIED_KEYS {
            let Some(entries) = pattern
                .metadata
                .get(*key)
                .and_then(|value| value.as_array())
            else {
                // Malformed values are already reported by the raw pass;
                // reporting them twice would double-count one broken claim.
                continue;
            };
            for entry in entries.iter().filter_map(|entry| entry.as_str()) {
                let tokens = tokenize(entry);
                // The matcher's own seam rather than `apply_policy` directly,
                // which is the same computation with one more argument list to
                // keep right.
                //
                // The `expect` is the invariant this call site *is*: the
                // positions come from the same `CompiledCatalog` whose policy is
                // being applied, so the digest matches by construction, and
                // `resolve_anchor_references` refuses an `admit` naming an
                // undeclared anchor at compile time, so every reference resolves
                // — `anchor_programs` is keyed by exactly `CatalogPolicy::anchors`.
                // Skipping the entry instead would leave `post_policy_checked`
                // under-counting, which is the vacuous check this module refuses
                // everywhere else.
                //
                // The cost, stated because it is not free: if that invariant
                // ever breaks, `ling check` panics rather than reporting a
                // diagnostic. That is the right trade only while the invariant
                // is structural — the day this function is handed a matcher
                // whose policy came from somewhere else, it needs a failure
                // channel instead.
                let published = matcher
                    .find_published_matches(&tokens)
                    .expect("a compiled catalog resolves its own declared anchors");
                let matched = published
                    .iter()
                    .any(|matched| matched.pattern == published_name);
                report.post_policy_checked += 1;
                if matched != *must_match {
                    report.post_policy_failures.push(ExampleFailure {
                        definition: name.to_string(),
                        example: entry.to_owned(),
                        kind: if *must_match {
                            ExampleFailureKind::ExpectedMatch
                        } else {
                            ExampleFailureKind::UnexpectedMatch
                        },
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::token_index_at_char;
    use crate::token_match::{TokenAttributeIndex, TokenInfo};

    /// The sentinel is an offset into the sentence, so the boundary walk must
    /// measure the **surface**.
    ///
    /// Not reachable through a claim fixture: `Orth` and `Text` agree in char
    /// count on every one of the 1,996,911 tokens of `data-archive/corpus.jsonl`
    /// — UniDic's `orth` is the entry's own written form, which is what matched
    /// the surface — so a verifier reading `Orth` gives identical answers on
    /// real material and only diverges on data this dictionary does not
    /// contain. Hand-built tokens are the only thing that can separate them,
    /// which is why this is a unit test rather than another entry in
    /// `anchor_claim_test`.
    #[test]
    fn token_index_at_char_measures_the_surface_not_the_orthography() {
        let token = |text: &str, orth: &str| {
            let mut token = TokenInfo::default();
            token.set(TokenAttributeIndex::Text, text.into());
            token.set(TokenAttributeIndex::Orth, orth.into());
            token
        };
        // Surfaces of 2 and 1 chars; orthographies of 1 and 3. Every boundary
        // has a different answer under the two readings.
        let tokens = [token("ある", "あ"), token("。", "まる。")];

        assert_eq!(token_index_at_char(&tokens, 0), Some(0));
        assert_eq!(token_index_at_char(&tokens, 2), Some(1), "after `ある`");
        assert_eq!(token_index_at_char(&tokens, 3), Some(2), "the sentence end");
        assert_eq!(token_index_at_char(&tokens, 1), None, "inside `ある`");
        assert_eq!(token_index_at_char(&tokens, 4), None, "past the end");
    }
}
