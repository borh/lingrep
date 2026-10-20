//! The counted-work screen.
//!
//! One question, answered in seconds instead of by a timing campaign: **did
//! this change alter how much execution work the pipeline does, or only the cost per unit
//! of it?**
//!
//! The answer routes the iteration. Counters unchanged means a cost-per-unit
//! change — representation, layout, branch behavior — which only a timing
//! campaign can evaluate. Counters changed means the measured work itself
//! moved, and the delta is direct evidence of the mechanism.
//!
//! **A green screen is not a null result.** The assertion-frontier bitset change
//! moved none of these counters but improved measured performance. The screen
//! says "cost-per-unit change, go measure it", which is the routing it exists
//! to provide.
//!
//! What a delta establishes is **changed measured work on this fixture**. Not
//! a complexity class, and not by itself the mechanism: these are eighteen
//! specific quantities and a change may move them for a reason other than the
//! intended one.
//!
//! Durations are deliberately absent. A duration is not portable, and the one
//! property that makes this screen usable on any machine is that every number
//! it reports is a count.

use std::collections::BTreeMap;
use std::sync::Arc;

use lingrep_core::corpus::DocumentId;
use lingrep_core::execution::{
    ExecutionError, MatchSink, PolicySink, SentenceLocation, SinkControl,
};
use lingrep_core::fsm::{
    BoundCatalog, CompileLimits, Match, PatternSource, PublishedRecord, compile_catalog,
};
use lingrep_core::testing::campaign_sentences::{baseline_sentences, conllu_campaign_sentences};
use lingrep_core::testing::catalogs::{
    CampaignBinding, catalog_campaign_descriptors, tracked_catalog_sources,
};
use lingrep_core::token_match::{
    AttributeOrdinal, AttributeValueRef, BoundTokenSentence, TokenSchema,
};
use serde::{Deserialize, Serialize};

/// Counted execution work for one catalog over the committed sentence set.
///
/// Field order is the order the human-readable table prints them in, which is
/// demand before work-after-memoization, then assertion work and publication
/// work from coarsest to finest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogWork {
    /// Total predicate demand.
    pub predicate_calls: u64,
    /// Demand surviving memoization — what an optimization actually removes.
    pub predicate_evaluations: u64,
    /// Assertion row demand.
    pub row_queries: u64,
    /// Assertion rows actually built.
    pub row_materializations: u64,
    /// Active assertion states visited across closure and consume steps.
    pub assertion_state_visits: u64,
    /// Closure work performed after the frontier has been seeded.
    pub edge_inspections: u64,
    /// Largest assertion frontier observed in any measured sentence.
    pub assertion_max_frontier_states: u64,
    /// Candidates the tree evaluator constructed, before deduplication.
    ///
    /// The dense machine does not produce candidates, so this is zero for a
    /// catalog with no captures, `same_as`, `&` or `where`.
    pub candidates_produced: u64,
    /// Those surviving deduplication.
    ///
    /// Paired with the one above the way `predicate_evaluations` is paired
    /// with `predicate_calls`: the ratio is what the deduplication key
    /// controls, so a change to the key moves this and not the other.
    pub candidates_retained: u64,
    /// Owned canonical match records handed to publication.
    pub canonical_records: u64,
    /// Canonical records carrying a heap-backed capture slice.
    pub canonical_capture_boxes: u64,
    /// Capture elements retained across those canonical records.
    pub canonical_capture_elements: u64,
    /// Records surviving the production publication seam.
    pub published_records: u64,
    /// Records whose declared key could not be projected.
    pub records_unkeyed: u64,
    /// Candidate visits across authored policy statements.
    pub policy_candidate_visits: u64,
    /// Records removed by authored policy statements.
    pub policy_records_dropped: u64,
    /// Public spans narrowed by authored policy statements.
    pub policy_records_narrowed: u64,
    /// Captures removed when authored policy narrowed a public span.
    pub policy_captures_dropped: u64,
}

impl CatalogWork {
    /// The eighteen counters, paired with the field names the report prints.
    #[must_use]
    pub fn counters(&self) -> [(&'static str, u64); 18] {
        [
            ("predicate_calls", self.predicate_calls),
            ("predicate_evaluations", self.predicate_evaluations),
            ("row_queries", self.row_queries),
            ("row_materializations", self.row_materializations),
            ("assertion_state_visits", self.assertion_state_visits),
            ("edge_inspections", self.edge_inspections),
            (
                "assertion_max_frontier_states",
                self.assertion_max_frontier_states,
            ),
            ("candidates_produced", self.candidates_produced),
            ("candidates_retained", self.candidates_retained),
            ("canonical_records", self.canonical_records),
            ("canonical_capture_boxes", self.canonical_capture_boxes),
            (
                "canonical_capture_elements",
                self.canonical_capture_elements,
            ),
            ("published_records", self.published_records),
            ("records_unkeyed", self.records_unkeyed),
            ("policy_candidate_visits", self.policy_candidate_visits),
            ("policy_records_dropped", self.policy_records_dropped),
            ("policy_records_narrowed", self.policy_records_narrowed),
            ("policy_captures_dropped", self.policy_captures_dropped),
        ]
    }

    /// Whether every counter is zero.
    ///
    /// A catalog that does no measurable work is a screen that cannot detect
    /// anything, so the report marks it rather than printing a row of zeroes that
    /// look like a clean result.
    #[must_use]
    pub fn is_silent(&self) -> bool {
        self.counters().iter().all(|(_, value)| *value == 0)
    }
}

/// The report format. Bumped when the counter list or the digest changes.
///
/// Carried explicitly so an incompatible baseline is refused by name rather
/// than by whatever Serde happens to do with a missing or extra field.
pub const REPORT_SCHEMA: &str = "lingrep-perf.work/v1";

/// What a catalog was measured over, and what the measurement found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogMeasurement {
    /// BLAKE3 over the catalog source and the exact labelled token sequence.
    ///
    /// A sentence *count* is not identity: editing an example or a fixture
    /// without changing how many sentences there are would otherwise produce a
    /// counter delta reported as a matcher work change. Comparison refuses
    /// when this differs.
    pub input_digest: String,
    /// How many sentences the catalog was measured over.
    pub sentences: usize,
    /// The counters.
    pub work: CatalogWork,
}

/// One `lingrep-perf work` run, keyed by repository-relative catalog path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkReport {
    pub schema: String,
    pub catalogs: BTreeMap<String, CatalogMeasurement>,
}

impl Default for WorkReport {
    fn default() -> Self {
        Self {
            schema: REPORT_SCHEMA.to_owned(),
            catalogs: BTreeMap::new(),
        }
    }
}

/// Which catalogs a screen covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopeSelection {
    /// The stratified performance suite: every catalog under `patterns/`,
    /// taken from [`catalog_campaign_descriptors`]. This is the default,
    /// because it is the set the tier rules adjudicate over — the fixture
    /// catalogs scattered through `tests/` are not campaign evidence, and
    /// several of them do no measurable work at all.
    CampaignSuite,
    /// Every `.lx` file discovered anywhere in the tree.
    AllDiscovered,
    /// One catalog, by repository-relative path.
    One(String),
}

impl ScopeSelection {
    fn admits(&self, name: &str, suite: &[&str]) -> bool {
        match self {
            Self::CampaignSuite => suite.contains(&name),
            Self::AllDiscovered => true,
            Self::One(only) => name == only,
        }
    }
}

/// Why a screen could not run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MeasureError {
    /// `--catalog` named a path that discovery did not find.
    UnknownCatalog {
        requested: String,
        nearest: Vec<String>,
    },
    /// A selected catalog no longer compiles.
    ///
    /// An error rather than a panic, and never a skipped row: a catalog
    /// missing from a comparison would read as "unchanged".
    CatalogDoesNotCompile { path: String, error: String },
    /// The campaign suite did not measure exactly the descriptor set.
    ///
    /// The descriptors are pinned against the tree by
    /// `every_pattern_catalog_has_a_pinned_guardrail_role`, so reaching this
    /// means discovery and the descriptor list disagree at runtime — which
    /// would silently narrow every campaign scoped through this screen.
    SuiteIncomplete { missing: Vec<String> },
}

impl std::fmt::Display for MeasureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownCatalog { requested, nearest } => {
                write!(formatter, "no tracked catalog at {requested}")?;
                if !nearest.is_empty() {
                    write!(formatter, "; did you mean one of: {}", nearest.join(", "))?;
                }
                Ok(())
            }
            Self::CatalogDoesNotCompile { path, error } => {
                write!(formatter, "{path} must compile to be screened: {error}")
            }
            Self::SuiteIncomplete { missing } => write!(
                formatter,
                "the campaign suite is incomplete; discovery did not find: {}",
                missing.join(", ")
            ),
        }
    }
}

/// Digest of exactly what a catalog was measured over.
///
/// Covers the catalog source and the full labelled token sequence, attribute
/// by attribute in index order. A sentence count is not identity — an edited
/// example that keeps the count would otherwise show up as a matcher work
/// change — and the label is included so a reordering is caught too.
fn input_digest(source: &str, sentences: &[(String, BoundTokenSentence)]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(source.as_bytes());
    for (label, tokens) in sentences {
        hasher.update(b"\x00label\x00");
        hasher.update(label.as_bytes());
        for token_index in 0..tokens.len() {
            hasher.update(b"\x00token\x00");
            let token = tokens
                .token(token_index)
                .expect("bounded campaign token index");
            for ordinal in 0..tokens.width() {
                // The present/absent tag is distinct from an empty value, so a
                // cleared attribute cannot collide with a blank one.
                match token.value(AttributeOrdinal::new(ordinal as u8)) {
                    Some(AttributeValueRef::Scalar(value)) => {
                        hasher.update(b"\x01");
                        hasher.update(value.as_bytes());
                    }
                    Some(AttributeValueRef::StringSet(values)) => {
                        hasher.update(b"\x03");
                        for value in values {
                            hasher.update(&(value.len() as u64).to_le_bytes());
                            hasher.update(value.as_bytes());
                        }
                    }
                    None => {
                        hasher.update(b"\x02");
                    }
                }
            }
        }
    }
    hasher.finalize().to_hex().to_string()
}

/// Counts what crosses the real publication seam without retaining records.
#[derive(Default)]
struct CountingSink {
    published_records: u64,
}

impl CountingSink {
    fn add(&mut self, records: usize) {
        self.published_records +=
            u64::try_from(records).expect("the committed fixture record count fits in u64");
    }
}

impl MatchSink for CountingSink {
    fn push(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn lingrep_core::token_match::TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        self.add(matches.len());
        Ok(SinkControl::Continue {
            records_emitted: matches.len(),
        })
    }

    fn push_records(
        &mut self,
        _location: &SentenceLocation,
        _sentence: &dyn lingrep_core::token_match::TokenSequence,
        records: Vec<PublishedRecord>,
    ) -> Result<SinkControl, ExecutionError> {
        let records_emitted = records.len();
        self.add(records_emitted);
        Ok(SinkControl::Continue { records_emitted })
    }
}

fn record_publication_work(
    bound: &BoundCatalog,
    tokens: &BoundTokenSentence,
    location: &SentenceLocation,
    matches: &[Match],
    work: &mut CatalogWork,
) -> Result<(), ExecutionError> {
    work.canonical_records +=
        u64::try_from(matches.len()).expect("the committed fixture record count fits in u64");
    work.canonical_capture_boxes += u64::try_from(
        matches
            .iter()
            .filter(|matched| !matched.captures.is_empty())
            .count(),
    )
    .expect("the committed fixture capture-box count fits in u64");
    work.canonical_capture_elements += u64::try_from(
        matches
            .iter()
            .map(|matched| matched.captures.len())
            .sum::<usize>(),
    )
    .expect("the committed fixture capture count fits in u64");

    let mut counting = CountingSink::default();
    let (
        stats,
        policy_candidate_visits,
        policy_records_dropped,
        policy_records_narrowed,
        program_captures_dropped,
    ) = {
        let mut publication = PolicySink::new(
            &mut counting,
            bound.catalog_policy().clone(),
            bound.anchor_evaluator(),
        );
        publication.push(location, tokens, matches)?;
        let policy_candidate_visits: u64 = publication
            .program_stats()
            .per_operation()
            .iter()
            .map(|counters| counters.before)
            .sum();
        let policy_records_dropped: u64 = publication
            .program_stats()
            .per_operation()
            .iter()
            .map(|counters| counters.dropped)
            .sum();
        let policy_records_narrowed: u64 = publication
            .program_stats()
            .per_operation()
            .iter()
            .map(|counters| counters.narrowed)
            .sum();
        let program_captures_dropped: u64 = publication
            .program_stats()
            .per_operation()
            .iter()
            .map(|counters| counters.captures_dropped)
            .sum();
        (
            publication.stats(),
            policy_candidate_visits,
            policy_records_dropped,
            policy_records_narrowed,
            program_captures_dropped,
        )
    };
    work.published_records += counting.published_records;
    work.records_unkeyed += stats.records_unkeyed;
    work.policy_candidate_visits += policy_candidate_visits;
    work.policy_records_dropped += policy_records_dropped;
    work.policy_records_narrowed += policy_records_narrowed;
    work.policy_captures_dropped += stats.captures_dropped + program_captures_dropped;
    Ok(())
}

/// Measure counted work over `scope`.
///
/// # Errors
///
/// Returns [`MeasureError`] when the scope selects nothing it was supposed to,
/// or when a selected catalog does not compile. A mistyped `--catalog` must not
/// succeed with an empty report: no work would then be indistinguishable from a
/// successful measurement, which is the same failure the missing-feature path
/// already refuses.
pub fn measure(scope: &ScopeSelection) -> Result<WorkReport, MeasureError> {
    let mut report = WorkReport::default();
    let suite: Vec<&str> = catalog_campaign_descriptors()
        .into_iter()
        .map(|descriptor| descriptor.path)
        .collect();

    let discovered = tracked_catalog_sources();

    if let ScopeSelection::One(requested) = scope
        && !discovered.iter().any(|(name, _)| name == requested)
    {
        let stem = std::path::Path::new(requested).file_name().map_or_else(
            || requested.clone(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        // Exact file name first — the common mistake is the right catalog
        // under the wrong directory. A typo matches nothing, so fall back to
        // the campaign suite, which is short and is what the operator almost
        // always wanted.
        let mut nearest: Vec<String> = discovered
            .iter()
            .map(|(name, _)| name)
            .filter(|name| !stem.is_empty() && name.ends_with(&stem))
            .take(5)
            .cloned()
            .collect();
        if nearest.is_empty() {
            nearest = suite.iter().map(|path| (*path).to_owned()).collect();
        }
        return Err(MeasureError::UnknownCatalog {
            requested: requested.clone(),
            nearest,
        });
    }

    for (name, source) in discovered {
        if !scope.admits(&name, &suite) {
            continue;
        }

        let output = compile_catalog(
            PatternSource::named(&name, &source),
            CompileLimits::default(),
        )
        .map_err(|error| MeasureError::CatalogDoesNotCompile {
            path: name.clone(),
            error: error.to_string(),
        })?;
        let matcher = &output.matcher;
        // The suite descriptor names which schema family this catalog binds
        // against; a discovered catalog outside the suite keeps the Vibrato
        // default. Binding a `where`-bearing catalog against the morphology
        // schema is refused by design, so measuring it means binding it
        // against the schema its predicates require.
        let binding = catalog_campaign_descriptors()
            .into_iter()
            .find(|descriptor| descriptor.path == name)
            .map_or(CampaignBinding::UnidicMorphology, |descriptor| {
                descriptor.binding
            });
        let (bound, sentences) = match binding {
            CampaignBinding::UnidicMorphology => {
                let sentences = baseline_sentences(matcher);
                let schema = TokenSchema::unidic_morphology();
                let bound =
                    matcher
                        .bind(&schema)
                        .map_err(|error| MeasureError::CatalogDoesNotCompile {
                            path: name.clone(),
                            error: error.to_string(),
                        })?;
                let sentences = sentences
                    .into_iter()
                    .map(|(label, tokens)| {
                        BoundTokenSentence::project_for_schema(&schema, bound.projection(), &tokens)
                            .map(|tokens| (label, tokens))
                            .map_err(|error| MeasureError::CatalogDoesNotCompile {
                                path: name.clone(),
                                error: error.to_string(),
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                (bound, sentences)
            }
            CampaignBinding::ConlluSyntax => {
                let schema = lingrep_core::corpus::conllu::token_schema();
                let bound =
                    matcher
                        .bind(&schema)
                        .map_err(|error| MeasureError::CatalogDoesNotCompile {
                            path: name.clone(),
                            error: error.to_string(),
                        })?;
                let sentences = conllu_campaign_sentences()
                    .into_iter()
                    .map(|(label, sentence)| {
                        sentence
                            .project(bound.projection())
                            .map(|tokens| (label, tokens))
                            .map_err(|error| MeasureError::CatalogDoesNotCompile {
                                path: name.clone(),
                                error: error.to_string(),
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                (bound, sentences)
            }
        };
        let mut work = CatalogWork::default();
        for (ordinal, (label, tokens)) in sentences.iter().enumerate() {
            // Two entry points, so two matching passes over each sentence. The
            // screen is measured in seconds and the counts are exact either
            // way, so the duplicated matching costs nothing that matters here.
            let (matches, execution) = bound
                .find_matches_with_execution_metrics(tokens)
                .expect("projected campaign sentence matches the bound schema");
            let (_, assertion) = bound
                .find_matches_with_assertion_metrics(tokens)
                .expect("projected campaign sentence matches the bound schema");

            work.predicate_calls += execution.predicate_calls;
            work.predicate_evaluations += execution.predicate_evaluations;
            work.row_queries += assertion.row_queries;
            work.row_materializations += assertion.row_materializations;
            work.assertion_state_visits += assertion.state_visits;
            work.edge_inspections += assertion.edge_inspections;
            work.assertion_max_frontier_states = work
                .assertion_max_frontier_states
                .max(assertion.max_frontier_states);
            work.candidates_produced += execution.candidates_produced;
            work.candidates_retained += execution.candidates_retained;
            let ordinal = u64::try_from(ordinal).expect("the committed fixture ordinal fits u64");
            record_publication_work(
                &bound,
                tokens,
                &SentenceLocation {
                    source_metadata: None,
                    corpus_sentence_ordinal: ordinal,
                    document_id: DocumentId::new(ordinal),
                    document_ordinal: ordinal,
                    document_sentence_ordinal: 0,
                    source_name: Arc::from(label.as_str()),
                },
                &matches,
                &mut work,
            )
            .expect("the compiled catalog's publication seam accepts its own matches");
        }

        report.catalogs.insert(
            name,
            CatalogMeasurement {
                input_digest: input_digest(&source, &sentences),
                sentences: sentences.len(),
                work,
            },
        );
    }

    if *scope == ScopeSelection::CampaignSuite {
        let missing: Vec<String> = suite
            .iter()
            .filter(|path| !report.catalogs.contains_key(**path))
            .map(|path| (*path).to_owned())
            .collect();
        if !missing.is_empty() {
            return Err(MeasureError::SuiteIncomplete { missing });
        }
    }

    Ok(report)
}

/// How one counter moved between two runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CounterDelta {
    pub counter: &'static str,
    pub before: u64,
    pub after: u64,
}

impl CounterDelta {
    /// Signed change, which is `i128` because two `u64` counters can differ by
    /// more than `i64::MAX` in principle and a screen must not wrap.
    #[must_use]
    pub fn change(&self) -> i128 {
        i128::from(self.after) - i128::from(self.before)
    }

    #[must_use]
    pub fn moved(&self) -> bool {
        self.before != self.after
    }
}

/// What changed between a saved run and the current one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogComparison {
    /// Present in both runs over identical input. Empty `deltas` means every
    /// counter held.
    Compared { deltas: Vec<CounterDelta> },
    /// Present in both runs, but over different input. **Not comparable**: a
    /// counter delta here says the input changed, not that matcher work did.
    InputChanged {
        before_sentences: usize,
        after_sentences: usize,
    },
    /// Present only in the current run.
    Added,
    /// Present only in the saved run.
    Removed,
}

/// Why two reports cannot be compared at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaMismatch {
    pub expected: String,
    pub found: String,
}

impl std::fmt::Display for SchemaMismatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "baseline is {} but this build writes {}; re-record the baseline",
            self.found, self.expected
        )
    }
}

/// Compare a saved report against the current one, catalog by catalog.
///
/// Added and removed catalogs are reported rather than skipped: a screen that
/// silently ignored them would let a catalog leave the suite between two runs
/// and still print a clean comparison.
///
/// # Errors
///
/// Returns [`SchemaMismatch`] when the baseline was written by a build with a
/// different report format. Refusing by name beats relying on whatever Serde
/// does with a changed field set, because a counter-list change is exactly
/// when a stale baseline would otherwise compare as "unchanged".
pub fn compare(
    before: &WorkReport,
    after: &WorkReport,
) -> Result<BTreeMap<String, CatalogComparison>, SchemaMismatch> {
    if before.schema != after.schema {
        return Err(SchemaMismatch {
            expected: after.schema.clone(),
            found: before.schema.clone(),
        });
    }

    let mut comparisons = BTreeMap::new();

    for (name, after_measurement) in &after.catalogs {
        let Some(before_measurement) = before.catalogs.get(name) else {
            comparisons.insert(name.clone(), CatalogComparison::Added);
            continue;
        };

        // Identity, not sentence count. An edited example or fixture that
        // keeps the count would otherwise produce a counter delta reported as
        // a matcher work change.
        if before_measurement.input_digest != after_measurement.input_digest {
            comparisons.insert(
                name.clone(),
                CatalogComparison::InputChanged {
                    before_sentences: before_measurement.sentences,
                    after_sentences: after_measurement.sentences,
                },
            );
            continue;
        }

        let deltas = before_measurement
            .work
            .counters()
            .into_iter()
            .zip(after_measurement.work.counters())
            .map(|((counter, before), (_, after))| CounterDelta {
                counter,
                before,
                after,
            })
            .filter(CounterDelta::moved)
            .collect();

        comparisons.insert(name.clone(), CatalogComparison::Compared { deltas });
    }

    for name in before.catalogs.keys() {
        if !after.catalogs.contains_key(name) {
            comparisons.insert(name.clone(), CatalogComparison::Removed);
        }
    }

    Ok(comparisons)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

    fn report(name: &str, digest: &str, sentences: usize, work: CatalogWork) -> WorkReport {
        let mut report = WorkReport::default();
        report.catalogs.insert(
            name.to_owned(),
            CatalogMeasurement {
                input_digest: digest.to_owned(),
                sentences,
                work,
            },
        );
        report
    }

    fn some_work() -> CatalogWork {
        CatalogWork {
            predicate_calls: 10,
            predicate_evaluations: 4,
            row_queries: 2,
            row_materializations: 2,
            assertion_state_visits: 9,
            edge_inspections: 7,
            assertion_max_frontier_states: 3,
            candidates_produced: 12,
            candidates_retained: 11,
            canonical_records: 5,
            canonical_capture_boxes: 3,
            canonical_capture_elements: 6,
            published_records: 4,
            records_unkeyed: 1,
            policy_candidate_visits: 8,
            policy_records_dropped: 1,
            policy_records_narrowed: 2,
            policy_captures_dropped: 1,
        }
    }

    fn publication_work(source: &str, tokens: Vec<TokenInfo>) -> CatalogWork {
        let matcher = compile_catalog(
            PatternSource::named("publication-work.lx", source),
            CompileLimits::default(),
        )
        .expect("publication-work fixture compiles")
        .matcher;
        let schema = TokenSchema::unidic_morphology();
        let bound = matcher.bind(&schema).expect("fixture binds");
        let projected =
            BoundTokenSentence::project_for_schema(&schema, bound.projection(), &tokens)
                .expect("fixture projects");
        let (matches, _) = bound
            .find_matches_with_execution_metrics(&projected)
            .expect("fixture matches");
        let mut work = CatalogWork::default();
        record_publication_work(
            &bound,
            &projected,
            &SentenceLocation {
                source_metadata: None,
                corpus_sentence_ordinal: 0,
                document_id: DocumentId::new(0),
                document_ordinal: 0,
                document_sentence_ordinal: 0,
                source_name: Arc::from("publication-work"),
            },
            &matches,
            &mut work,
        )
        .expect("fixture publishes");
        work
    }

    #[test]
    fn publication_work_counts_owned_captures_and_policy_filtering() {
        let work = publication_work(
            concat!(
                "pattern first := (?<x> []).\n",
                "pattern second := (?<y> []).\n",
                "policy\n",
                "  resolve all rank: [longest, start, end, pattern].\n",
                "end\n",
            ),
            vec![TokenInfo::default()],
        );

        assert_eq!(work.canonical_records, 2);
        assert_eq!(work.canonical_capture_boxes, 2);
        assert_eq!(work.canonical_capture_elements, 2);
        assert_eq!(work.published_records, 1);
        assert_eq!(work.records_unkeyed, 0);
        assert_eq!(work.policy_candidate_visits, 2);
        assert_eq!(work.policy_records_dropped, 1);
        assert_eq!(work.policy_records_narrowed, 0);
        assert_eq!(work.policy_captures_dropped, 0);
    }

    #[test]
    fn publication_work_counts_an_unsatisfied_declared_key() {
        let mut token = TokenInfo::default();
        token.set(TokenAttributeIndex::Orth, "b".into());
        let work = publication_work(
            concat!(
                "key: [$x.text]\n",
                "pattern optional := ((?<x> [text=a]) | [text=b]).\n",
            ),
            vec![token],
        );

        assert_eq!(work.canonical_records, 1);
        assert_eq!(work.published_records, 1);
        assert_eq!(work.records_unkeyed, 1);
    }

    #[test]
    fn publication_work_counts_a_capture_removed_by_detach() {
        let mut head = TokenInfo::default();
        head.set(TokenAttributeIndex::Orth, "a".into());
        let mut tail = TokenInfo::default();
        tail.set(TokenAttributeIndex::Orth, "b".into());
        let work = publication_work(
            concat!(
                "pattern body := [text=a] (?<tail> [text=b]).\n",
                "fragment TAIL := [text=b].\n",
                "policy\n",
                "  detach @body with: @TAIL, relation: at_end, evidence: raw.\n",
                "end\n",
            ),
            vec![head, tail],
        );

        assert_eq!(work.policy_candidate_visits, 1);
        assert_eq!(work.policy_records_dropped, 0);
        assert_eq!(work.policy_records_narrowed, 1);
        assert_eq!(work.policy_captures_dropped, 1);
    }

    /// The counted-work regression guard.
    ///
    /// This pins semantic work, not instruction counts or elapsed time. It
    /// detects changes only when one of the recorded counters represents the
    /// work that changed.
    ///
    /// So a retained campaign records its own `retention_guard` rather than
    /// crediting this one by default. `none-available` is a legitimate answer
    /// on this repository, and is better than crediting a guard that does not
    /// guard.
    ///
    /// Every discovered catalog needs an explicit pin. Sentence counts are in
    /// the tuple so fixture changes cannot appear as work regressions.
    ///
    /// That provision is what the 2026-09-07 repin rests on: every
    /// Vibrato-bound catalog's sentence count rose by exactly six, because
    /// `r7_natsume_tokens.jsonl` grew from probe P1's six doctest rows to all
    /// twelve of natsume-simple's `NORMALIZATION_CASES` and
    /// `COMMITTED_FIXTURE_SENTENCES` went 11 -> 17. The CoNLL-U-bound
    /// catalogs, which read their own witnesses, did not move. No matcher
    /// change is in that repin.
    #[test]
    fn counted_matcher_work_per_catalog_is_pinned() {
        let mut expected = std::collections::BTreeMap::from([
            ("patterns/alternation.lx", (33, 1105, 570, 0, 0, 0)),
            ("patterns/collexeme.lx", (42, 1717, 1089, 490, 84, 442)),
            ("patterns/concord.lx", (41, 472, 422, 24, 24, 368)),
            ("patterns/counting.lx", (26, 8316, 234, 0, 0, 0)),
            ("patterns/distributions.lx", (17, 51, 51, 0, 0, 0)),
            // ConlluSyntax-bound like ud-dependency: the committed dependency
            // witnesses, every pattern tree-evaluated. 1584 -> 198 calls with
            // the 2026-08-29 label-set collapse: the eight head-sharing
            // patterns each walked one shared skeleton over every sentence,
            // and one pattern walks it once.
            //
            // 198 -> 267 with the fifth witness, the 二重ヲ格 sentence that
            // gave this catalog its first published record here. Before it,
            // the four sentences matched nothing and the whole publication
            // row was zero, so the screen watched this catalog match nothing
            // and called it a pin.
            ("patterns/double-wo-dep.lx", (5, 239, 226, 0, 0, 0)),
            ("patterns/double-wo.lx", (24, 1664, 1388, 485, 240, 2030)),
            ("patterns/ngrams.lx", (17, 226, 226, 0, 0, 0)),
            ("patterns/npv-collocation.lx", (5, 155, 153, 14, 8, 0)),
            // Direct-edge constraints plus matching dependent token
            // predicates prune impossible assignments before relation checks.
            ("patterns/passive-agent-dep.lx", (5, 68, 68, 0, 0, 0)),
            // The committed dependency witnesses carry neither `Voice=Pass`,
            // `aux:pass`, nor the Japanese passive auxiliary lemma. The six
            // relation-bearing arms still walk their one-token heads here;
            // corpus behavior is characterized separately against the
            // Markdown/GiNZA and UD artifacts.
            (
                "patterns/passive-oblique-realization.lx",
                (5, 324, 324, 0, 0, 0),
            ),
            ("patterns/portable-content-words.lx", (17, 34, 17, 0, 0, 0)),
            ("patterns/reduplication.lx", (21, 146, 146, 0, 0, 0)),
            (
                "patterns/sf-experimental.lx",
                (133, 9217, 4173, 850, 133, 0),
            ),
            ("patterns/sentence-final.lx", (17, 261, 202, 51, 17, 0)),
            ("patterns/sfe.lx", (19, 2941, 550, 0, 0, 0)),
            ("patterns/sketch-grammar.lx", (35, 855, 395, 0, 0, 0)),
            ("patterns/test-simple.lx", (17, 53, 51, 0, 0, 0)),
            // The first ConlluSyntax-bound catalog: the committed dependency
            // witnesses rather than the eleven morphology fixtures, and every
            // pattern tree-evaluated, so calls equal evaluations and the
            // dense-machine columns stay zero. 184 -> 283 with the fifth
            // pattern, `verbal_argument`: a label set is one bracket, but the
            // `[upos=NOUN] []* [upos=VERB]` skeleton it hangs on is the widest
            // in the catalog, so the added walk and not the added terms is
            // what the counters see. 283 -> 367 with the fifth witness, which
            // this catalog reads incidentally — it was committed for
            // double-wo-dep.
            // English CoNLL-U catalog measured against the same Japanese
            // screen corpus as every other row: it matches almost nothing
            // there, and that is the point — the row exists so a work
            // regression in the mixed dense/tree path still shows up.
            // 449 -> 478 when `dash_aside` gained its two neighbour
            // assertions: the extra walk is the boundary sweep, paid on the
            // catalog's cheapest definition.
            ("patterns/comment-register.lx", (5, 382, 352, 0, 0, 0)),
            ("patterns/ud-dependency.lx", (5, 367, 367, 0, 0, 0)),
            // Enhanced-graph execution uses the same five committed CoNLL-U
            // witnesses. They exercise assignment enumeration without
            // publishing a catalog record on this Japanese fixture.
            ("patterns/ud-enhanced-argument.lx", (5, 68, 68, 0, 0, 0)),
            (
                "patterns/ud-enhanced-consistency.lx",
                (5, 136, 136, 0, 0, 0),
            ),
            (
                "patterns/ud-enhanced-propagation.lx",
                (5, 238, 238, 0, 0, 0),
            ),
            // Four surface-order variants expose the current cost of spelling
            // crossing graph edges through linear capture assignment.
            ("patterns/ud-nonprojectivity.lx", (5, 2388, 2027, 0, 0, 0)),
            // Two one-token head classifiers over anonymous positive/negative
            // edge existence. The five fixture verbs partition one-for-one.
            ("patterns/ud-object-realization.lx", (5, 181, 179, 0, 0, 0)),
            // Three fork and six chain linearizations are the deliberate
            // workload for a future dependency-join execution plan.
            ("patterns/ud-subtree-profile.lx", (5, 2312, 2081, 0, 0, 0)),
            ("patterns/ud-valency.lx", (5, 544, 541, 0, 0, 0)),
            ("patterns/wo-predicate-dep.lx", (5, 101, 101, 0, 0, 0)),
        ]);
        let mut expected_publication = std::collections::BTreeMap::from([
            ("patterns/alternation.lx", (54, 25, 25, 23, 23)),
            ("patterns/collexeme.lx", (28, 28, 28, 28, 28)),
            ("patterns/concord.lx", (48, 0, 0, 48, 0)),
            ("patterns/counting.lx", (90, 0, 0, 90, 0)),
            ("patterns/distributions.lx", (102, 0, 0, 102, 102)),
            ("patterns/double-wo-dep.lx", (1, 1, 5, 1, 0)),
            ("patterns/double-wo.lx", (7, 0, 0, 7, 0)),
            ("patterns/ngrams.lx", (116, 65, 175, 116, 116)),
            ("patterns/npv-collocation.lx", (7, 7, 28, 7, 0)),
            ("patterns/passive-agent-dep.lx", (0, 0, 0, 0, 0)),
            ("patterns/passive-oblique-realization.lx", (0, 0, 0, 0, 0)),
            ("patterns/portable-content-words.lx", (17, 0, 0, 17, 0)),
            ("patterns/reduplication.lx", (4, 4, 4, 4, 0)),
            ("patterns/sf-experimental.lx", (391, 0, 0, 167, 0)),
            ("patterns/sentence-final.lx", (10, 0, 0, 10, 0)),
            ("patterns/sfe.lx", (110, 0, 0, 110, 0)),
            ("patterns/sketch-grammar.lx", (21, 21, 42, 21, 21)),
            ("patterns/test-simple.lx", (2, 0, 0, 2, 0)),
            // 11 -> 17: `verbal_argument` adds two records per argument-order
            // witness and one for each verb of the shadow sentence, where 犬
            // is an argument of 走る and not of 鳴く. 17 -> 20 with the fifth
            // witness: 政策 and 社会的 are both `obj` of 掲げる, and 理想 is a
            // compound dependent rather than an argument, so the sentence
            // contributes three of the four noun/verb pairs it spans.
            ("patterns/comment-register.lx", (34, 0, 0, 34, 0)),
            ("patterns/ud-dependency.lx", (20, 20, 42, 20, 0)),
            ("patterns/ud-enhanced-argument.lx", (0, 0, 0, 0, 0)),
            ("patterns/ud-enhanced-consistency.lx", (0, 0, 0, 0, 0)),
            ("patterns/ud-enhanced-propagation.lx", (0, 0, 0, 0, 0)),
            ("patterns/ud-nonprojectivity.lx", (0, 0, 0, 0, 0)),
            ("patterns/ud-object-realization.lx", (11, 11, 11, 11, 11)),
            ("patterns/ud-subtree-profile.lx", (31, 31, 93, 31, 31)),
            ("patterns/ud-valency.lx", (8, 8, 19, 8, 8)),
            ("patterns/wo-predicate-dep.lx", (3, 3, 9, 3, 0)),
        ]);
        // Candidate volume inside the tree evaluator, as (produced, retained).
        // Zero for a catalog the dense machine executes whole; the evaluator
        // runs for captures, `same_as`, `&` and `where`, which is nine of the
        // sixteen. This is the row a candidate-growth regression moves.
        //
        // The two numbers are **equal on every catalog today**, which is the
        // measurement and not a redundancy: under the wide key deduplication
        // removes nothing these witnesses reach. Divergence is the signal.
        let mut expected_candidates = std::collections::BTreeMap::from([
            ("patterns/alternation.lx", (159, 159)),
            ("patterns/collexeme.lx", (140, 140)),
            ("patterns/concord.lx", (0, 0)),
            ("patterns/counting.lx", (0, 0)),
            ("patterns/distributions.lx", (0, 0)),
            ("patterns/double-wo-dep.lx", (339, 339)),
            ("patterns/double-wo.lx", (85, 85)),
            ("patterns/ngrams.lx", (415, 415)),
            ("patterns/npv-collocation.lx", (151, 151)),
            ("patterns/passive-agent-dep.lx", (0, 0)),
            ("patterns/passive-oblique-realization.lx", (40, 40)),
            ("patterns/portable-content-words.lx", (0, 0)),
            ("patterns/reduplication.lx", (34, 34)),
            ("patterns/sf-experimental.lx", (0, 0)),
            ("patterns/sentence-final.lx", (0, 0)),
            ("patterns/sfe.lx", (0, 0)),
            ("patterns/sketch-grammar.lx", (217, 217)),
            ("patterns/test-simple.lx", (0, 0)),
            ("patterns/comment-register.lx", (233, 233)),
            ("patterns/ud-dependency.lx", (337, 337)),
            ("patterns/ud-enhanced-argument.lx", (44, 44)),
            ("patterns/ud-enhanced-consistency.lx", (232, 232)),
            ("patterns/ud-enhanced-propagation.lx", (476, 476)),
            ("patterns/ud-nonprojectivity.lx", (4678, 4678)),
            ("patterns/ud-object-realization.lx", (50, 50)),
            ("patterns/ud-subtree-profile.lx", (3725, 3725)),
            ("patterns/ud-valency.lx", (437, 437)),
            ("patterns/wo-predicate-dep.lx", (63, 63)),
        ]);
        let mut expected_pressure_and_policy = std::collections::BTreeMap::from([
            ("patterns/alternation.lx", (0, 0, 72, 2, 0, 0)),
            ("patterns/comment-register.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/collexeme.lx", (1718, 9, 0, 0, 0, 0)),
            ("patterns/concord.lx", (764, 8, 0, 0, 0, 0)),
            ("patterns/counting.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/distributions.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/double-wo-dep.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/double-wo.lx", (4892, 22, 0, 0, 0, 0)),
            ("patterns/ngrams.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/npv-collocation.lx", (132, 2, 0, 0, 0, 0)),
            ("patterns/passive-agent-dep.lx", (0, 0, 0, 0, 0, 0)),
            (
                "patterns/passive-oblique-realization.lx",
                (0, 0, 0, 0, 0, 0),
            ),
            ("patterns/portable-content-words.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/reduplication.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/sf-experimental.lx", (1994, 2, 1777, 167, 17, 0)),
            ("patterns/sentence-final.lx", (136, 2, 10, 0, 0, 0)),
            ("patterns/sfe.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/sketch-grammar.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/test-simple.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-dependency.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-enhanced-argument.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-enhanced-consistency.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-enhanced-propagation.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-nonprojectivity.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-object-realization.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-subtree-profile.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/ud-valency.lx", (0, 0, 0, 0, 0, 0)),
            ("patterns/wo-predicate-dep.lx", (0, 0, 0, 0, 0, 0)),
        ]);

        let report = measure(&ScopeSelection::CampaignSuite).expect("the campaign suite measures");
        // Report every moved catalog so one run exposes the complete repin.
        let mut moved = Vec::new();
        for (name, measurement) in &report.catalogs {
            let work = &measurement.work;
            let actual = (
                measurement.sentences,
                work.predicate_calls,
                work.predicate_evaluations,
                work.row_queries,
                work.row_materializations,
                work.edge_inspections,
            );
            let pinned = expected.remove(name.as_str());
            if pinned != Some(actual) {
                moved.push(format!("  {name}: pinned {pinned:?}, measured {actual:?}"));
            }
            let actual_publication = (
                work.canonical_records,
                work.canonical_capture_boxes,
                work.canonical_capture_elements,
                work.published_records,
                work.records_unkeyed,
            );
            let pinned_publication = expected_publication.remove(name.as_str());
            if pinned_publication != Some(actual_publication) {
                moved.push(format!(
                    "  {name} publication: pinned {pinned_publication:?}, measured \
                     {actual_publication:?}"
                ));
            }
            let actual_candidates = (work.candidates_produced, work.candidates_retained);
            let pinned_candidates = expected_candidates.remove(name.as_str());
            if pinned_candidates != Some(actual_candidates) {
                moved.push(format!(
                    "  {name} candidates: pinned {pinned_candidates:?}, measured \
                     {actual_candidates:?}"
                ));
            }
            let actual_pressure_and_policy = (
                work.assertion_state_visits,
                work.assertion_max_frontier_states,
                work.policy_candidate_visits,
                work.policy_records_dropped,
                work.policy_records_narrowed,
                work.policy_captures_dropped,
            );
            let pinned_pressure_and_policy = expected_pressure_and_policy.remove(name.as_str());
            if pinned_pressure_and_policy != Some(actual_pressure_and_policy) {
                moved.push(format!(
                    "  {name} pressure/policy: pinned {pinned_pressure_and_policy:?}, measured \
                     {actual_pressure_and_policy:?}"
                ));
            }
        }
        assert!(
            moved.is_empty(),
            "counted work moved. If this is a deliberate optimization, each new tuple is \
             the evidence for it; if it is not, it is a work regression.\n{}",
            moved.join("\n")
        );
        assert!(
            expected.is_empty(),
            "tracked catalogs the screen did not measure: {expected:?}"
        );
        assert!(
            expected_publication.is_empty(),
            "tracked catalogs the publication screen did not measure: {expected_publication:?}"
        );
        assert!(
            expected_candidates.is_empty(),
            "tracked catalogs the candidate screen did not measure: {expected_candidates:?}"
        );
        assert!(
            expected_pressure_and_policy.is_empty(),
            "tracked catalogs the pressure/policy screen did not measure: \
             {expected_pressure_and_policy:?}"
        );
    }

    #[test]
    fn the_default_scope_is_the_campaign_suite_not_every_discovered_catalog() {
        let suite = ["patterns/a.lx", "patterns/b.lx"];

        assert!(ScopeSelection::CampaignSuite.admits("patterns/a.lx", &suite));
        // A fixture catalog is discovered by `tracked_catalog_sources` but is
        // not campaign evidence, and screening it dilutes the signal the
        // screen exists to give.
        assert!(!ScopeSelection::CampaignSuite.admits("tests/fixtures/one.lx", &suite));
        assert!(ScopeSelection::AllDiscovered.admits("tests/fixtures/one.lx", &suite));
        assert!(ScopeSelection::One("patterns/b.lx".to_owned()).admits("patterns/b.lx", &suite));
        assert!(!ScopeSelection::One("patterns/b.lx".to_owned()).admits("patterns/a.lx", &suite));
    }

    #[test]
    fn an_unchanged_counter_set_over_identical_input_produces_no_deltas() {
        let comparisons = compare(
            &report("a.lx", "digest", 3, some_work()),
            &report("a.lx", "digest", 3, some_work()),
        )
        .expect("same schema compares");

        let CatalogComparison::Compared { deltas } = &comparisons["a.lx"] else {
            panic!("identical input is comparable");
        };
        assert!(deltas.is_empty());
    }

    #[test]
    fn only_the_counters_that_moved_are_reported() {
        let before = CatalogWork {
            predicate_calls: 10,
            predicate_evaluations: 4,
            ..CatalogWork::default()
        };
        let after = CatalogWork {
            predicate_calls: 10,
            predicate_evaluations: 3,
            ..CatalogWork::default()
        };
        let comparisons = compare(
            &report("a.lx", "digest", 3, before),
            &report("a.lx", "digest", 3, after),
        )
        .expect("same schema compares");

        let CatalogComparison::Compared { deltas } = &comparisons["a.lx"] else {
            panic!("identical input is comparable");
        };
        assert_eq!(deltas.len(), 1);
        assert_eq!(deltas[0].counter, "predicate_evaluations");
        assert_eq!(deltas[0].change(), -1);
    }

    #[test]
    fn changed_input_at_an_unchanged_sentence_count_refuses_to_compare() {
        // The case a sentence-count check misses entirely: an example edited
        // in place. The counters would differ because the input differs, and
        // reporting that as a matcher work change is the wrong answer.
        let before = CatalogWork {
            predicate_calls: 10,
            ..CatalogWork::default()
        };
        let after = CatalogWork {
            predicate_calls: 12,
            ..CatalogWork::default()
        };
        let comparisons = compare(
            &report("a.lx", "before-digest", 3, before),
            &report("a.lx", "after-digest", 3, after),
        )
        .expect("same schema compares");

        assert_eq!(
            comparisons["a.lx"],
            CatalogComparison::InputChanged {
                before_sentences: 3,
                after_sentences: 3,
            }
        );
    }

    #[test]
    fn a_catalog_leaving_or_joining_the_suite_is_not_silently_skipped() {
        let comparisons = compare(
            &report("gone.lx", "digest", 1, CatalogWork::default()),
            &report("new.lx", "digest", 1, CatalogWork::default()),
        )
        .expect("same schema compares");

        assert_eq!(comparisons["gone.lx"], CatalogComparison::Removed);
        assert_eq!(comparisons["new.lx"], CatalogComparison::Added);
    }

    #[test]
    fn a_baseline_from_another_report_format_is_refused_by_name() {
        let mut stale = report("a.lx", "digest", 3, some_work());
        stale.schema = "lingrep-perf.work/v0".to_owned();

        let error = compare(&stale, &report("a.lx", "digest", 3, some_work()))
            .expect_err("a different schema cannot be compared");
        assert_eq!(error.found, "lingrep-perf.work/v0");
        assert_eq!(error.expected, REPORT_SCHEMA);
    }

    #[test]
    fn an_all_zero_catalog_is_marked_silent() {
        assert!(CatalogWork::default().is_silent());
        assert!(
            !CatalogWork {
                edge_inspections: 1,
                ..CatalogWork::default()
            }
            .is_silent()
        );
    }

    #[test]
    fn the_counter_list_is_the_eighteen_the_design_names() {
        let names: Vec<&str> = CatalogWork::default()
            .counters()
            .iter()
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            names,
            [
                "predicate_calls",
                "predicate_evaluations",
                "row_queries",
                "row_materializations",
                "assertion_state_visits",
                "edge_inspections",
                "assertion_max_frontier_states",
                "candidates_produced",
                "candidates_retained",
                "canonical_records",
                "canonical_capture_boxes",
                "canonical_capture_elements",
                "published_records",
                "records_unkeyed",
                "policy_candidate_visits",
                "policy_records_dropped",
                "policy_records_narrowed",
                "policy_captures_dropped",
            ]
        );
    }

    #[test]
    fn an_unknown_catalog_is_refused_rather_than_measured_as_nothing() {
        let error = measure(&ScopeSelection::One("patterns/no-such.lx".to_owned()))
            .expect_err("a mistyped path must not succeed with an empty report");

        let MeasureError::UnknownCatalog { requested, .. } = error else {
            panic!("a path discovery did not find is an UnknownCatalog");
        };
        assert_eq!(requested, "patterns/no-such.lx");
    }

    #[test]
    fn a_near_miss_names_the_catalogs_it_could_have_meant() {
        let error = measure(&ScopeSelection::One("wrong/dir/double-wo.lx".to_owned()))
            .expect_err("a wrong directory is still an unknown catalog");

        let MeasureError::UnknownCatalog { nearest, .. } = error else {
            panic!("a path discovery did not find is an UnknownCatalog");
        };
        assert!(
            nearest.contains(&"patterns/double-wo.lx".to_owned()),
            "the suggestion list should name the catalog with that file name: {nearest:?}"
        );
    }

    #[test]
    fn the_campaign_suite_measures_exactly_the_pinned_descriptor_set() {
        let report = measure(&ScopeSelection::CampaignSuite).expect("the suite is complete");
        let measured: Vec<&str> = report.catalogs.keys().map(String::as_str).collect();
        let pinned: Vec<&str> = catalog_campaign_descriptors()
            .into_iter()
            .map(|descriptor| descriptor.path)
            .collect();

        assert_eq!(measured, pinned);
    }
}
