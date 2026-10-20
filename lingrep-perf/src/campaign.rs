//! The campaign manifest, its refusals, and the verdict algebra.
//!
//! Pure: a campaign is a value, adjudication is a function of it plus the
//! catalog set, and nothing here reads a clock, a file, or `lingrep-core`. That is
//! what lets every refusal and every verdict be tested on a synthetic campaign
//! rather than on a measurement that takes twenty minutes to produce.
//!
//! The shape of this module is set by one rule: **an invalid campaign must
//! fail loudly rather than produce a confident wrong answer.** Refusals are
//! therefore checked before any arithmetic runs, and they are a distinct
//! outcome from every verdict.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::stats::{Block, Effect, EstimateError, geometric_mean, paired_effect};

/// The evidence tier, which fixes the coverage a campaign owes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Decisive simplifying substitution: one fresh-process A/B per supported
    /// mode, stopping only when every mode clears 20%.
    One,
    /// Moderate effect, or ordinary run variance could change the decision.
    Two,
    /// Marginal, noisy, or costly.
    Three,
}

impl Tier {
    /// The ladder a campaign gets when it does not declare one.
    #[must_use]
    pub fn default_ladder(self) -> Vec<usize> {
        match self {
            Self::One => vec![1],
            Self::Two => vec![3, 8, 20],
            Self::Three => vec![5, 12, 30],
        }
    }

    /// The smallest first look this tier admits.
    #[must_use]
    pub fn first_look_floor(self) -> usize {
        match self {
            Self::One => 1,
            Self::Two => 3,
            Self::Three => 5,
        }
    }

    /// Tier 1's early-stop bar: every mode must improve by at least this much.
    #[must_use]
    pub fn decisive_improvement(self) -> Option<f64> {
        matches!(self, Self::One).then_some(0.20)
    }
}

/// What a catalog contributes to a campaign.
///
/// Mirrors `lingrep_core::testing::catalogs::GuardrailRole` rather than importing it,
/// because this module must stay free of `lingrep-core`. Two variants, so the mirror
/// is cheap; the regimes are carried as opaque tag names for the same reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Substantive,
    Floor,
}

/// A catalog the campaign measures.
///
/// Carried **inside** the manifest, and therefore covered by the manifest
/// digest. Reading the suite from the tree at adjudication time would mean a
/// catalog added later silently changed how an old campaign adjudicates; a
/// campaign measured the suite that existed when it was declared, and that is
/// the set it must be judged against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CampaignCatalog {
    pub path: String,
    pub role: Role,
    /// Regime tag names, as rendered from `lingrep-core`'s `RuntimeRegime`.
    pub regimes: BTreeSet<String>,
}

/// A metric a campaign measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Wall,
    User,
    PeakRss,
}

impl Metric {
    /// Every verdict-bearing metric.
    ///
    /// Peak RSS participates in verdicts so a performance win cannot ignore a
    /// memory regression.
    /// System time is deliberately absent — it is reported, never adjudicated.
    pub const VERDICT_BEARING: [Self; 3] = [Self::Wall, Self::User, Self::PeakRss];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Wall => "wall",
            Self::User => "user",
            Self::PeakRss => "peak_rss",
        }
    }
}

/// The dominant constraint a campaign named before proposing a change.
///
/// One row of `optimizing-performance`'s bound taxonomy. Required, because a
/// campaign that cannot name its bound has skipped the step the whole method
/// turns on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bound {
    Work,
    Algorithmic,
    Io,
    AsyncRuntime,
    Allocation,
    MemoryBandwidth,
    MemoryLatency,
    Branch,
    Compute,
    Synchronisation,
    InstructionCache,
}

/// The leverage-ladder rung a change occupies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LadderRung {
    EliminateWork,
    BetterAlgorithm,
    CheaperBoundaries,
    Representation,
    Parallelism,
    Vectorisation,
    MicroOptimisation,
}

/// An absolute goal whose achievement is a reason to stop.
///
/// Typed rather than prose because `target_met` is computed: an adjudicator
/// cannot evaluate "under 16 ms per step", and leaving it as a string would
/// make the field another human assertion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub catalog: String,
    pub metric: Metric,
    /// Absolute ceiling the candidate arm's mean must fall below.
    pub below: f64,
}

/// The build the arms were compiled with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildProfile {
    pub profile: String,
    pub lto: String,
    pub codegen_units: u32,
}

/// The machine a campaign was measured on.
///
/// Inside the manifest, and so covered by the manifest digest: changing it
/// invalidates every observation rather than rewriting history. Campaigns are
/// machine-local and self-contained, so this is provenance for a reader and a
/// pin for the orchestrator — never a key for comparing results across
/// machines, which this design does not do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Machine {
    pub cpu_model: String,
    pub kernel: String,
    pub rustc: String,
}

/// One built arm, content-addressed.
///
/// `commit` is provenance for a reader; `binary_digest` is the identity the
/// orchestrator rehashes before every run. Nothing is referenced by a mutable
/// path, because thirteen indistinguishable frozen binaries is one of the
/// failure modes this tooling exists to prevent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arm {
    pub label: String,
    pub commit: String,
    pub binary_digest: String,
}

/// The two arms a campaign compares.
///
/// Exactly two, by construction: the skill combines candidates only after each
/// is screened individually, and a combination is measured as an ordinary
/// campaign whose candidate arm is the combined change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Arms {
    pub a: Arm,
    pub b: Arm,
}

/// How a workload input's identity is established.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadKind {
    /// A file small enough to rehash before every run.
    File,
    /// A prepared V1 artifact. Rehashing gigabytes before every run would
    /// cost more than the measurement, so the artifact's own **recorded**
    /// identity stands in: the token and location component digests, the
    /// dictionary fingerprint and token-schema identity the tokens were
    /// produced under, the source and analysis identities, and the document
    /// inventory. That is a manifest parse and no hashing, so it runs before
    /// every execution; the full `corpus inspect --verify` is paid once at
    /// declare time.
    ///
    /// The document inventory alone would not do. The same documents
    /// retokenized under a different dictionary leave it identical while
    /// changing token count, attributes, predicates, matches and therefore
    /// work — which for a matcher campaign is the whole quantity being
    /// measured, not an edge case.
    PreparedArtifact,
}

/// One input the campaign measures over.
///
/// In the manifest, and so under its seal. Without this the workload was
/// identified only by a path inside argv, and a path is not an identity: a
/// different valid artifact could be moved into place between arms and every
/// check would still agree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadInput {
    pub path: String,
    pub kind: WorkloadKind,
    pub identity: String,
}

/// How a run's output reaches the hasher.
///
/// Sealed, because it is a property of how the campaign was measured: the
/// digest is the same number either way, but a run that streams pays a
/// hashing consumer instead of ~42 GB of writes, and a campaign that changed
/// its mind halfway would be comparing blocks measured under two rigs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputCapture {
    /// Written to a file, then hashed. The historical path.
    #[default]
    File,
    /// Piped through `lingrep-perf digest-stream`, never materialized.
    Stream,
}

impl OutputCapture {
    /// Whether this is the value a campaign declared before the field
    /// existed. Used to keep the field out of the encoding, and so out of
    /// every archived manifest digest.
    #[must_use]
    pub fn is_default(&self) -> bool {
        matches!(self, Self::File)
    }
}

/// One argument of the command every run executes.
///
/// The manifest seals the whole command rather than the extra flags alone,
/// because a check that the sealed flags *appear* in argv admits an argv that
/// also contradicts them: `--limit all` sealed against `--limit all … --limit
/// 5` observed satisfies any containment test, and clap takes the last one.
/// Reconstructing the command and comparing it exactly leaves no such gap.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemplateArg {
    /// Reproduced exactly, in this position.
    Literal(String),
    /// The catalog this run measures.
    Catalog,
    /// The prepared corpus assigned to this run's catalog.
    Corpus,
}

impl TemplateArg {
    /// The spelling `declare` accepts for each hole.
    pub const CATALOG_HOLE: &'static str = "{catalog}";
    /// The spelling `declare` accepts for a per-catalog prepared corpus.
    pub const CORPUS_HOLE: &'static str = "{corpus}";

    /// Parse one token of a declared template.
    ///
    /// A token that looks like a hole and names none is an error rather than a
    /// literal, because `{catlog}` would otherwise seal a command no run can
    /// ever match and the campaign would refuse every block.
    pub fn parse(token: &str) -> Result<Self, String> {
        if token == Self::CATALOG_HOLE {
            return Ok(Self::Catalog);
        }
        if token == Self::CORPUS_HOLE {
            return Ok(Self::Corpus);
        }
        if token.starts_with('{') && token.ends_with('}') && token.len() > 2 {
            return Err(format!(
                "{token} is not a template hole. The holes are {} and {}",
                Self::CATALOG_HOLE,
                Self::CORPUS_HOLE,
            ));
        }
        if token.contains('\n') || token.contains('\0') {
            return Err(format!(
                "a template argument may not contain a newline or a NUL: {token:?}"
            ));
        }
        Ok(Self::Literal(token.to_owned()))
    }
}

/// The immutable declaration a campaign is bound to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub campaign: String,
    pub tier: Tier,
    pub bound: Bound,
    pub runner_up_bound: Option<Bound>,
    pub profile_evidence: Option<String>,
    pub ladder_rung: LadderRung,
    pub target: Option<Target>,
    pub primary_catalog: String,
    pub primary_metric: Metric,
    pub affected_regimes: BTreeSet<String>,
    /// The guardrail suite as it stood when this campaign was declared.
    pub catalogs: Vec<CampaignCatalog>,
    /// Materially different supported modes. Only Tier 1 requires one pair per
    /// mode; other tiers carry it for the record.
    #[serde(default)]
    pub modes: Vec<String>,
    pub minimum_useful_effect_pct: f64,
    pub guardrail_ceilings_pct: BTreeMap<Metric, f64>,
    pub escalation_ladder: Vec<usize>,
    pub build: BuildProfile,
    pub machine: Machine,
    pub arms: Arms,
    /// The inputs this campaign measures over. Empty is legal only for a
    /// campaign that measures nothing, which no verdict-bearing one does.
    #[serde(default)]
    pub workload: Vec<WorkloadInput>,
    /// Which prepared artifact each catalog executes against.
    ///
    /// Empty for direct-input campaigns and campaigns written before the
    /// corpus template hole existed. Omitted when empty so adding the field
    /// does not move archived manifest digests.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub catalog_corpora: BTreeMap<String, String>,
    /// The worker pool every run is executed with.
    ///
    /// `None` for a campaign declared before this field existed. Sealed
    /// rather than recorded per observation because it is a property of the
    /// campaign, and because occupancy is meaningless if it moved mid-run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers: Option<u32>,
    /// How each run's output reaches the hasher.
    #[serde(default, skip_serializing_if = "OutputCapture::is_default")]
    pub output_capture: OutputCapture,
    /// The command every run executes, with the per-run holes named.
    ///
    /// Empty only for a campaign declared before this field existed. `declare`
    /// requires one; `record` skips the comparison when it is absent, so the
    /// archived campaigns stay adjudicable. Skipped when empty so that adding
    /// this field moved no archived manifest digest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_template: Vec<TemplateArg>,
}

impl Manifest {
    /// The manifest's own BLAKE3 digest.
    ///
    /// Unkeyed BLAKE3 over the canonical JSON encoding, which is the same
    /// primitive the V1 artifact format already authenticates components with.
    /// Determinism comes from the encoding: struct fields serialize in
    /// declaration order, and every map and set here is ordered.
    ///
    /// `None` when the manifest cannot be encoded. Returning `Option` makes a
    /// newly unencodable field surface as [`Refusal::ManifestNotDigestible`]
    /// instead of a panic. This does *not* catch a non-finite number: the
    /// encoder writes those as null, which is why
    /// [`Refusal::NonFiniteThreshold`] is a separate check.
    #[must_use]
    pub fn digest(&self) -> Option<String> {
        let canonical = serde_json::to_vec(self).ok()?;
        Some(format!("blake3:{}", blake3::hash(&canonical).to_hex()))
    }

    /// The exact argv this campaign's template says `catalog` must be run with.
    ///
    /// `None` when no template was sealed, which is a campaign declared before
    /// the field existed and the one case where the command cannot be checked.
    pub fn expected_argv(
        &self,
        catalog: &str,
    ) -> Result<Option<Vec<String>>, TemplateResolutionError> {
        if self.command_template.is_empty() {
            return Ok(None);
        }
        let corpus = if self
            .command_template
            .iter()
            .any(|argument| matches!(argument, TemplateArg::Corpus))
        {
            Some(self.catalog_corpora.get(catalog).ok_or_else(|| {
                TemplateResolutionError::CorpusNotMapped {
                    catalog: catalog.to_owned(),
                }
            })?)
        } else {
            None
        };
        Ok(Some(
            self.command_template
                .iter()
                .map(|argument| match argument {
                    TemplateArg::Literal(text) => text.clone(),
                    TemplateArg::Catalog => catalog.to_owned(),
                    TemplateArg::Corpus => corpus
                        .expect("the corpus hole was resolved before reconstruction")
                        .clone(),
                })
                .collect(),
        ))
    }
}

/// Why a sealed command cannot be reconstructed for one catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateResolutionError {
    /// The command contains `{corpus}`, but the manifest assigns none to this
    /// catalog.
    CorpusNotMapped { catalog: String },
}

impl std::fmt::Display for TemplateResolutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CorpusNotMapped { catalog } => write!(
                formatter,
                "the sealed command needs a prepared corpus for {catalog}, but the manifest \
                 maps none"
            ),
        }
    }
}

impl std::error::Error for TemplateResolutionError {}

/// One measured run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    /// Copy of the manifest digest this run was bound to.
    pub manifest_digest: String,
    /// What the block actually executed: resolved argv, allowlisted
    /// environment, affinity, workers, the rehashed binary, the freshly
    /// observed input digests, and the machine fingerprint.
    ///
    /// Arm-specific by construction, because it covers the binary. It can
    /// therefore only ever be compared *within* one arm.
    pub exec_fingerprint: String,
    /// The same, minus the binary: everything both arms must have shared.
    ///
    /// This is the half `exec_fingerprint` cannot check. A one-pair screen
    /// runs A once and B once, so two differing `exec_fingerprint` values are
    /// expected and prove nothing — an input edited between the two arms
    /// would pass unnoticed. Output parity is not a substitute: different
    /// inputs can yield identical matches at very different cost.
    #[serde(default)]
    pub input_fingerprint: String,
    /// Digest of the run's output with `RUN_VARIANT_FIELDS` neutralized.
    pub output_digest: String,
    pub catalog: String,
    /// `"a"` or `"b"`.
    pub arm: String,
    pub block: usize,
    /// Position within the block, `0..4` for a block and `0..2` for a screen.
    pub order: usize,
    pub mode: Option<String>,
    pub ok: bool,
    pub wall_s: f64,
    pub user_s: f64,
    pub sys_s: f64,
    pub peak_rss_kb: f64,
}

impl Observation {
    fn metric(&self, metric: Metric) -> f64 {
        match metric {
            Metric::Wall => self.wall_s,
            Metric::User => self.user_s,
            Metric::PeakRss => self.peak_rss_kb,
        }
    }
}

/// One post-hoc choice of primary metric.
///
/// Unsealed on purpose: a selection is made *after* the manifest is sealed,
/// and pretending otherwise would be the lie the seal exists to prevent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricSelection {
    pub metric: Metric,
    /// Why. Required and non-blank, because a selection without a stated
    /// reason is exactly the artifact this log exists to make visible.
    pub reason: String,
}

/// A campaign file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Campaign {
    pub manifest: Manifest,
    pub manifest_digest: String,
    pub observations: Vec<Observation>,
    /// Append-only record of every post-hoc primary-metric change.
    ///
    /// Audit only. It does **not** widen the alpha budget, because the budget
    /// already covers the whole metric family: `build_streams` computes an
    /// interval for every [`Metric::VERDICT_BEARING`] on every catalog and
    /// `verdict` prints all of them, so the family is inspected whether or not
    /// anything is recorded here. Pricing recorded selections would charge for
    /// the wrong event and would count transitions rather than hypotheses —
    /// `wall -> user -> wall` is two hypotheses, not three.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selections: Vec<MetricSelection>,
}

impl Campaign {
    /// The metric the verdict turns on: the last selection, or the sealed one.
    #[must_use]
    pub fn effective_primary_metric(&self) -> Metric {
        self.selections
            .last()
            .map_or(self.manifest.primary_metric, |selection| selection.metric)
    }

    /// The number of simultaneous decisions the alpha budget is split across.
    ///
    /// `ladder x |VERDICT_BEARING|`. The ladder prices *escalation*: each rung
    /// is another look at the same data. The metric family prices *selection*:
    /// every verdict-bearing metric is computed and printed on every run, so
    /// an operator choosing between them after seeing the report is choosing
    /// from a family of that size, whatever they declared beforehand.
    ///
    /// Guardrail streams share the same widened value. They are not priced
    /// separately because guardrail multiplicity errs toward *blocking* — a
    /// wider interval makes `RejectGuardrail` rarer and `UnresolvedGuardrail`
    /// commoner, so it costs escalation rather than validity. One critical
    /// value per campaign is a conservative simplification, and stated as one.
    #[must_use]
    pub fn decisions(&self) -> usize {
        self.manifest
            .escalation_ladder
            .len()
            .max(1)
            .saturating_mul(Metric::VERDICT_BEARING.len())
    }
}

/// Record a post-hoc primary-metric selection.
///
/// Separate from [`adjudicate`], which stays pure. Nothing about the campaign
/// is resealed: the manifest still says what was declared, and the report
/// names both.
///
/// # Errors
///
/// [`Refusal::SelectionRepeatsMetric`] when the metric is already in force —
/// nothing was learned. [`Refusal::SelectionReasonEmpty`] when `--reason` is
/// blank.
pub fn reselect(campaign: &mut Campaign, metric: Metric, reason: &str) -> Result<(), Refusal> {
    if reason.trim().is_empty() {
        return Err(Refusal::SelectionReasonEmpty);
    }
    if campaign.effective_primary_metric() == metric {
        return Err(Refusal::SelectionRepeatsMetric { metric });
    }
    campaign.selections.push(MetricSelection {
        metric,
        reason: reason.trim().to_owned(),
    });
    Ok(())
}

/// Write a campaign atomically.
///
/// Temporary file plus rename, because this file is the campaign's *only*
/// record. Overwriting it in place means an interruption mid-write destroys
/// every block measured so far, and those cost minutes each.
///
/// # Errors
///
/// When the campaign cannot be encoded, or the write or rename fails.
pub fn write(path: &std::path::Path, campaign: &Campaign) -> Result<(), std::io::Error> {
    let rendered = serde_json::to_vec_pretty(campaign)?;
    let temporary = path.with_extension("json.writing");
    std::fs::write(&temporary, rendered)?;
    std::fs::rename(&temporary, path)
}

/// Average CPU occupancy relative to the declared worker count.
///
/// `mean(user) / (mean(wall) * workers)` over one catalog's arm-A runs.
///
/// It is **not** an Amdahl bound and must not be reported as one. `user`
/// includes the main thread and any non-pool threads, so the numerator is not
/// the pool's work; CPU and I/O overlap, so a value below 1 does not mean the
/// remainder is serial; removed CPU work need not be spread evenly across
/// workers; and a change that reduces contention can move wall by more than
/// any linear projection allows.
///
/// What it is good for is planning. `minimum_useful_effect_pct / occupancy`
/// estimates the user-time reduction a declared wall effect implies, which is
/// the number an operator wants before choosing `--primary-metric`. The first
/// campaign run through this tooling declared `wall` on a workload at 0.47 and
/// spent a whole suite discovering that.
///
/// Empty when the campaign seals no worker count, which is the one case where
/// the quantity has no denominator.
#[must_use]
pub fn worker_normalized_cpu_occupancy(campaign: &Campaign) -> BTreeMap<String, f64> {
    let Some(workers) = campaign.manifest.workers else {
        return BTreeMap::new();
    };
    if workers == 0 {
        return BTreeMap::new();
    }
    let mut totals: BTreeMap<String, (f64, f64, usize)> = BTreeMap::new();
    for observation in &campaign.observations {
        if observation.arm != "a" || !observation.ok {
            continue;
        }
        let entry = totals.entry(observation.catalog.clone()).or_default();
        entry.0 += observation.wall_s;
        entry.1 += observation.user_s;
        entry.2 += 1;
    }
    totals
        .into_iter()
        .filter_map(|(catalog, (wall, user, count))| {
            if count == 0 || wall <= 0.0 {
                return None;
            }
            let mean_wall = wall / count as f64;
            let mean_user = user / count as f64;
            Some((catalog, mean_user / (mean_wall * f64::from(workers))))
        })
        .collect()
}

/// What a declared wall effect implies about user time, as a sentence.
///
/// Two tiers, and only the first is a claim. When the implied reduction
/// exceeds 100% of user time it follows from the definition that no CPU-only
/// change reaches the bar, and that is stated firmly. Below that the estimate
/// is printed with the occupancy beside it and no judgement is attached —
/// the campaign this tooling was built from removed 13.3% of user time, which
/// an earlier and more confident version of this rule would have called out
/// of reach.
///
/// `None` when the primary metric is not wall, or the primary catalog's
/// occupancy is unknown.
#[must_use]
pub fn wall_planning_estimate(campaign: &Campaign) -> Option<String> {
    let manifest = &campaign.manifest;
    if manifest.primary_metric != Metric::Wall {
        return None;
    }
    let occupancy = *worker_normalized_cpu_occupancy(campaign).get(&manifest.primary_catalog)?;
    if occupancy <= 0.0 {
        return None;
    }
    let implied = manifest.minimum_useful_effect_pct / occupancy;
    Some(if implied >= 100.0 {
        format!(
            "Reaching the declared {:.2}% wall effect on {} implies removing about {implied:.0}% \
             of its user time, which is more user time than there is (occupancy {occupancy:.2}). \
             No CPU-only change reaches this bar; a wall bound over an I/O or decode change is \
             another matter.",
            manifest.minimum_useful_effect_pct, manifest.primary_catalog,
        )
    } else {
        format!(
            "Reaching the declared {:.2}% wall effect on {} implies removing about {implied:.0}% \
             of its user time (occupancy {occupancy:.2}).",
            manifest.minimum_useful_effect_pct, manifest.primary_catalog,
        )
    })
}

/// Why a campaign cannot be adjudicated.
///
/// Distinct from every verdict, and returned before any arithmetic runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A run the orchestrator itself marked failed, or a nonsense measurement.
    FailedRun { catalog: String, block: usize },
    /// An observation is bound to a different manifest than the one it sits
    /// beside — the manifest was edited after the fact.
    ManifestDigestMismatch { catalog: String, found: String },
    /// The recorded digest is not the digest of the manifest beside it. The
    /// manifest was edited after it was declared, so nothing it says about
    /// what was measured can be trusted.
    ManifestAltered {
        recorded: String,
        recomputed: String,
    },
    /// The manifest cannot be encoded, so it cannot be digested.
    ManifestNotDigestible,
    /// A bar the verdict is measured against is not a finite number. Every
    /// comparison against it would be false, so the campaign would adjudicate
    /// confidently and wrongly.
    NonFiniteThreshold { field: String },
    /// Two runs of the same catalog and arm executed different things.
    ExecFingerprintDiffers { catalog: String, arm: String },
    /// Two runs of the same catalog saw different inputs, command lines,
    /// environments or machines — including across the two arms, which is the
    /// comparison `ExecFingerprintDiffers` structurally cannot make.
    InputFingerprintDiffers { catalog: String },
    /// A campaign that measures a workload must say which one.
    WorkloadNotDeclared,
    /// A catalog's arms disagree on output, so parity does not hold.
    OutputParityBroken { catalog: String, digests: usize },
    /// Unequal run counts, or an ordering that is not balanced.
    UnbalancedArms { catalog: String, block: usize },
    /// A catalog has fewer blocks or screen pairs than its tier requires.
    CoverageShort {
        catalog: String,
        required: usize,
        found: usize,
    },
    /// A catalog in the descriptor set was never measured.
    CatalogNotMeasured { catalog: String },
    /// The ladder is absent, non-increasing, or starts below the tier floor.
    InvalidLadder { ladder: Vec<usize> },
    /// The arms were not built in the shipping release configuration.
    NotAReleaseBuild { profile: String },
    /// The bound was named but the profile that named it was not recorded.
    BoundWithoutProfileEvidence,
    /// Reselecting the metric already in force. Nothing was learned.
    SelectionRepeatsMetric { metric: Metric },
    /// A selection was recorded without saying why.
    SelectionReasonEmpty,
    /// Tier 1 declared a mode it never measured.
    ModeNotMeasured { mode: String },
    /// The primary catalog is not in the descriptor set.
    UnknownPrimaryCatalog { catalog: String },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FailedRun { catalog, block } => write!(
                formatter,
                "{catalog} block {block} contains a failed or nonsense run; \
                 a campaign with a broken run is not adjudicated"
            ),
            Self::ManifestAltered {
                recorded,
                recomputed,
            } => write!(
                formatter,
                "the manifest digests to {recomputed} but records {recorded}; it was edited \
                 after it was declared, and a campaign is judged against the manifest it \
                 was measured under"
            ),
            Self::ManifestNotDigestible => write!(
                formatter,
                "the manifest holds a value JSON cannot encode, so it has no digest and \
                 was never valid"
            ),
            Self::NonFiniteThreshold { field } => write!(
                formatter,
                "{field} is not a finite number; every comparison against it is false, so \
                 this campaign would produce a confident wrong verdict"
            ),
            Self::ManifestDigestMismatch { catalog, found } => write!(
                formatter,
                "{catalog} carries manifest digest {found}, which is not this \
                 manifest's; the manifest was edited after these runs"
            ),
            Self::InputFingerprintDiffers { catalog } => write!(
                formatter,
                "{catalog} was measured over more than one distinct input, command line, \
                 environment or machine; the two arms did not face the same workload and \
                 their difference is not attributable to the change"
            ),
            Self::WorkloadNotDeclared => write!(
                formatter,
                "the manifest declares no workload, so nothing identifies what was measured \
                 beyond a path inside the command line, and a path is not an identity"
            ),
            Self::ExecFingerprintDiffers { catalog, arm } => write!(
                formatter,
                "{catalog} arm {arm} executed more than one distinct command, \
                 binary, input or machine across its blocks"
            ),
            Self::OutputParityBroken { catalog, digests } => write!(
                formatter,
                "{catalog} produced {digests} distinct outputs; output parity \
                 is mandatory and this campaign does not have it"
            ),
            Self::UnbalancedArms { catalog, block } => write!(
                formatter,
                "{catalog} block {block} is not a balanced ABBA/BAAB block or \
                 a single A/B screen pair"
            ),
            Self::CoverageShort {
                catalog,
                required,
                found,
            } => write!(
                formatter,
                "{catalog} has {found} of the {required} blocks its tier requires"
            ),
            Self::CatalogNotMeasured { catalog } => write!(
                formatter,
                "{catalog} is in the guardrail suite but was never measured"
            ),
            Self::InvalidLadder { ladder } => write!(
                formatter,
                "escalation ladder {ladder:?} must be non-empty, strictly \
                 increasing, and start at or above the tier floor"
            ),
            Self::NotAReleaseBuild { profile } => write!(
                formatter,
                "arms were built with profile {profile}; an unoptimized build \
                 invalidates everything downstream"
            ),
            Self::BoundWithoutProfileEvidence => write!(
                formatter,
                "the named bound has no recorded profile evidence, so nothing \
                 says the bound was measured rather than guessed"
            ),
            Self::SelectionRepeatsMetric { metric } => write!(
                formatter,
                "{} is already the primary metric, so reselecting it learns nothing",
                metric.name()
            ),
            Self::SelectionReasonEmpty => write!(
                formatter,
                "a metric reselection must say why. The reason is what makes the choice \
                 auditable rather than merely recorded."
            ),
            Self::ModeNotMeasured { mode } => write!(
                formatter,
                "tier 1 declared mode {mode} but never measured it"
            ),
            Self::UnknownPrimaryCatalog { catalog } => write!(
                formatter,
                "the primary catalog {catalog} is not in the guardrail suite"
            ),
        }
    }
}

/// What a campaign owes a catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// Full paired blocks, which yield an interval.
    Blocks(usize),
    /// A single fresh-process A/B pair, which yields a point estimate only.
    ScreenPair,
}

impl Coverage {
    #[must_use]
    pub fn units(self) -> usize {
        match self {
            Self::Blocks(count) => count,
            Self::ScreenPair => 1,
        }
    }
}

/// What `catalog` owes under `manifest`.
///
/// The contract in `docs/PERFORMANCE_REQUIREMENTS.md` gives Tier 2 three
/// blocks on the primary and on every catalog whose regime the change can
/// affect, and **one fresh-process paired guardrail** on the rest. Requiring
/// blocks everywhere would contradict it and make Tier 2 far more expensive,
/// which is the opposite of what this tooling is for.
#[must_use]
pub fn required_coverage(manifest: &Manifest, catalog: &CampaignCatalog) -> Coverage {
    let is_primary = catalog.path == manifest.primary_catalog;
    let is_affected = catalog
        .regimes
        .iter()
        .any(|regime| manifest.affected_regimes.contains(regime));

    match manifest.tier {
        // Every Tier 1 measurement is a screen pair, one per mode, so a Tier 1
        // campaign never reports an interval.
        Tier::One => Coverage::ScreenPair,
        Tier::Two => {
            if is_primary || is_affected {
                Coverage::Blocks(3)
            } else {
                Coverage::ScreenPair
            }
        }
        Tier::Three => {
            if is_primary || is_affected {
                Coverage::Blocks(5)
            } else if catalog.role == Role::Substantive {
                Coverage::Blocks(3)
            } else {
                Coverage::ScreenPair
            }
        }
    }
}

/// How many runs the block at `index` holds for this catalog.
///
/// A catalog whose tier coverage is a screen pair has a **two-run screen at
/// block 0 and four-run blocks from 1 on**, because escalation *promotes* it
/// rather than reinterpreting what it already measured. Treating the width as
/// a property of the catalog instead of the block made promotion impossible to
/// complete: the shell wrote four runs while this counted two, and the third
/// run of a promoted block was refused as out of sequence.
#[must_use]
pub fn block_width(manifest: &Manifest, catalog: &str, index: usize) -> usize {
    let screened = manifest
        .catalogs
        .iter()
        .find(|entry| entry.path == catalog)
        .is_some_and(|entry| required_coverage(manifest, entry) == Coverage::ScreenPair);
    if screened && index == 0 { 2 } else { 4 }
}

/// What a catalog has finished and what it is part-way through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Whether the two-run screen at block 0 is complete. Always `false` for a
    /// catalog whose coverage is blocks from the start.
    pub screened: bool,
    /// Complete four-run blocks. **Only these count toward a ladder rung**: a
    /// screen is what triggered promotion, not one of the blocks the rung
    /// asks for.
    pub blocks: usize,
    /// Runs recorded in the unit currently in progress; `0` when none is.
    pub partial_runs: usize,
    /// The `(block, order)` this catalog's next run must carry.
    pub next: (usize, usize),
    /// How many runs the block at `next.0` holds.
    pub next_width: usize,
}

/// Where a catalog left off.
///
/// Walks block indices in order, each against its own width, and stops at the
/// first incomplete one. Counting distinct block numbers instead treats a
/// block with one run in it as recorded, which leaves it permanently
/// unbalanced.
#[must_use]
pub fn progress(campaign: &Campaign, catalog: &str) -> Progress {
    let manifest = &campaign.manifest;
    let mut per_block: BTreeMap<usize, usize> = BTreeMap::new();
    for observation in &campaign.observations {
        if observation.catalog == catalog {
            *per_block.entry(observation.block).or_default() += 1;
        }
    }

    let screen_first = block_width(manifest, catalog, 0) == 2;
    let mut index = 0;
    let mut blocks = 0;
    loop {
        let width = block_width(manifest, catalog, index);
        let runs = per_block.get(&index).copied().unwrap_or(0);
        if runs < width {
            return Progress {
                screened: screen_first && index > 0,
                blocks,
                partial_runs: runs,
                next: (index, runs),
                next_width: width,
            };
        }
        if width == 4 {
            blocks += 1;
        }
        index += 1;
    }
}

/// The outcome for one catalog and metric.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamOutcome {
    /// Interval entirely at or beyond the ceiling.
    RejectGuardrail,
    /// Interval admits a regression past the ceiling.
    UnresolvedGuardrail,
    /// A screen pair's point estimate is over the ceiling. Never a rejection:
    /// a screen has no interval, so the interval-based rule cannot be applied
    /// to it, and the catalog joins the ladder instead.
    EscalateGuardrail,
    /// A screen pair under its ceiling. **Not** a verdict backed by an
    /// interval, and marked as such wherever it is reported.
    Screened,
    /// Within its ceiling, with an interval behind it.
    Within,
}

/// A per-catalog, per-metric result.
#[derive(Clone, Debug, PartialEq)]
pub struct Stream {
    pub catalog: String,
    pub metric: Metric,
    pub effect: Effect,
    pub outcome: StreamOutcome,
}

/// The campaign verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Primary interval entirely at or beyond the minimum useful effect.
    Win,
    /// A `Win` that a guardrail's escalation unblocked. Discharged by a fresh,
    /// fixed-size confirmation campaign, never by more blocks here.
    WinPendingConfirmation { unblocked_by: String },
    /// Primary interval entirely on the worse side of parity.
    RevertPrimaryRegressed,
    /// Primary interval entirely better than parity but short of the minimum.
    RevertBelowMinimum,
    /// Primary interval straddles a landmark.
    Inconclusive,
    /// Unresolved at the ladder's end. Stop: the answer is a redesign or a
    /// larger effect, not more blocks.
    InconclusiveExhausted,
    /// A guardrail is definitely past its ceiling.
    RejectGuardrail { catalog: String, metric: Metric },
    /// A guardrail's interval admits a regression past its ceiling.
    UnresolvedGuardrail { catalog: String, metric: Metric },
    /// A guardrail's screen pair is over its ceiling, with no interval behind
    /// it. Promote that catalog to blocks; it can be neither cleared nor
    /// rejected as it stands.
    EscalateGuardrail { catalog: String, metric: Metric },
    /// Tier 1 did not clear its decisive bar on every mode.
    NotDecisive,
}

/// A catalog whose wall clock did not move while its user time did.
///
/// This is the signature of work paced by something outside the measured
/// process: the change removed CPU work — `user`'s interval excludes parity —
/// and the wall clock did not notice, its interval still admitting parity. A
/// digest consumer that cannot keep up with the output, a disk that cannot
/// absorb it, or any other external rate limiter produces exactly this shape,
/// because a limiter applies equally to both arms.
///
/// It is a **diagnostic, never a verdict**. The observations are valid and the
/// arms really did take that long; what the flag says is that this catalog's
/// `wall` is reporting the limiter rather than the change, so reading it as
/// "neutral" is a mistake. The recorded case is
/// `docs/results/2026-08-22-decode-owner-cursor`, where a `--stream-output`
/// campaign put four catalogs at `wall` 0.99-1.01 while their `user` sat
/// between 0.67 and 0.89; re-run without streaming, the same catalogs moved to
/// 0.65-0.97.
///
/// Deliberately threshold-free. It asks only whether the two intervals
/// disagree about parity, so it needs no calibrated bytes-per-second and it
/// catches pacing this tool does not know about — an un-streamed campaign
/// whose catalog is bound by writing its own output flags identically.
#[derive(Clone, Debug, PartialEq)]
pub struct ExternallyPaced {
    pub catalog: String,
    /// The wall estimate, whose interval admits parity.
    pub wall: Effect,
    /// The user estimate, whose interval excludes parity on the better side.
    pub user: Effect,
}

/// Find every catalog whose wall is paced by something outside the process.
///
/// Screens are skipped: a point estimate has no interval to disagree with.
fn externally_paced(streams: &[Stream]) -> Vec<ExternallyPaced> {
    let mut flagged = Vec::new();
    let mut catalogs: Vec<&str> = streams.iter().map(|s| s.catalog.as_str()).collect();
    catalogs.dedup();
    for catalog in catalogs {
        let find = |metric: Metric| {
            streams
                .iter()
                .find(|s| s.catalog == catalog && s.metric == metric)
                .map(|s| s.effect)
        };
        let (Some(wall), Some(user)) = (find(Metric::Wall), find(Metric::User)) else {
            continue;
        };
        if wall.is_screen() || user.is_screen() {
            continue;
        }
        // Wall saw nothing: its interval still admits parity. User saw a real
        // improvement: its whole interval is on the better side. Only the
        // improving direction is flagged — a user *regression* alongside flat
        // wall is a different phenomenon and not this one.
        if wall.low <= 1.0 && 1.0 <= wall.high && user.high < 1.0 {
            flagged.push(ExternallyPaced {
                catalog: catalog.to_owned(),
                wall,
                user,
            });
        }
    }
    flagged
}

/// A complete adjudication.
#[derive(Clone, Debug, PartialEq)]
pub struct Adjudication {
    pub verdict: Verdict,
    pub streams: Vec<Stream>,
    /// Equal-weight geometric mean over substantive catalogs on the primary
    /// metric. `None` when no substantive catalog produced an estimate.
    pub aggregate: Option<f64>,
    /// Whether any component of the aggregate is a screen rather than an
    /// interval. An aggregate mixing three-block estimates with one-pair
    /// screens is as strong as its weakest component.
    pub aggregate_includes_screens: bool,
    /// Which ladder look this adjudication ran at, one-based.
    ///
    /// Derived from the primary catalog's block count against the ladder, and
    /// **not** the ladder's length. The two are different quantities that an
    /// earlier version conflated: the ladder length is Bonferroni's `K` and is
    /// fixed for the whole campaign, while the look is where this campaign has
    /// reached. Conflating them made `InconclusiveExhausted` unreachable.
    pub look: usize,
    /// The declared ladder length.
    pub looks: usize,
    /// Bonferroni's `K`: `looks x |VERDICT_BEARING|`.
    pub decisions: usize,
    /// The metric the verdict turned on — the sealed one unless a reselection
    /// was recorded.
    pub primary_metric: Metric,
    /// `Some` only when the manifest declared a target.
    pub target_met: Option<bool>,
    /// Catalogs whose wall is paced by something outside the process. See
    /// [`ExternallyPaced`]. Empty on a campaign whose rig measured what it
    /// meant to.
    pub externally_paced: Vec<ExternallyPaced>,
}

#[expect(
    clippy::too_many_lines,
    reason = "the refusal sequence is one contract read in order; splitting it \
              across helpers would hide the order that makes it checkable"
)]
fn check_refusals(
    campaign: &Campaign,
    grouped: &BTreeMap<String, BTreeMap<usize, Vec<&Observation>>>,
) -> Result<(), Refusal> {
    let manifest = &campaign.manifest;
    let catalogs = &manifest.catalogs;

    // Every later check reads the manifest, so authenticate it first.
    let recomputed = manifest.digest().ok_or(Refusal::ManifestNotDigestible)?;
    if recomputed != campaign.manifest_digest {
        return Err(Refusal::ManifestAltered {
            recorded: campaign.manifest_digest.clone(),
            recomputed,
        });
    }

    // Every bar a verdict is measured against must be a number. Checked here
    // rather than at each comparison, because a NaN bar fails silently: the
    // encoding writes it as null and every `<=` against it is false.
    if !manifest.minimum_useful_effect_pct.is_finite() {
        return Err(Refusal::NonFiniteThreshold {
            field: "minimum_useful_effect_pct".to_owned(),
        });
    }
    for (metric, ceiling) in &manifest.guardrail_ceilings_pct {
        if !ceiling.is_finite() {
            return Err(Refusal::NonFiniteThreshold {
                field: format!("guardrail_ceilings_pct.{}", metric.name()),
            });
        }
    }
    if let Some(target) = &manifest.target
        && !target.below.is_finite()
    {
        return Err(Refusal::NonFiniteThreshold {
            field: "target.below".to_owned(),
        });
    }

    // The loop's own discipline first: a campaign that cannot name its bound,
    // or was built unoptimized, is invalid regardless of what it measured.
    if manifest.build.profile != "release" {
        return Err(Refusal::NotAReleaseBuild {
            profile: manifest.build.profile.clone(),
        });
    }
    if manifest
        .profile_evidence
        .as_ref()
        .is_none_or(|evidence| evidence.trim().is_empty())
    {
        return Err(Refusal::BoundWithoutProfileEvidence);
    }

    if manifest.workload.is_empty() {
        return Err(Refusal::WorkloadNotDeclared);
    }

    let ladder = &manifest.escalation_ladder;
    let increasing = ladder.windows(2).all(|pair| pair[0] < pair[1]);
    if ladder.is_empty() || !increasing || ladder[0] < manifest.tier.first_look_floor() {
        return Err(Refusal::InvalidLadder {
            ladder: ladder.clone(),
        });
    }

    if !catalogs
        .iter()
        .any(|catalog| catalog.path == manifest.primary_catalog)
    {
        return Err(Refusal::UnknownPrimaryCatalog {
            catalog: manifest.primary_catalog.clone(),
        });
    }

    for observation in &campaign.observations {
        if !observation.ok
            || observation.wall_s <= 0.0
            || observation.user_s < 0.0
            || observation.peak_rss_kb <= 0.0
            || !observation.wall_s.is_finite()
        {
            return Err(Refusal::FailedRun {
                catalog: observation.catalog.clone(),
                block: observation.block,
            });
        }
        if observation.manifest_digest != campaign.manifest_digest {
            return Err(Refusal::ManifestDigestMismatch {
                catalog: observation.catalog.clone(),
                found: observation.manifest_digest.clone(),
            });
        }
    }

    for (catalog, blocks) in grouped {
        // One execution identity per arm: the same command, binary, inputs and
        // machine across every block. Argv alone would not catch an edited
        // catalog, which is why the fingerprint covers observed digests.
        let mut per_arm: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        let mut outputs: BTreeSet<&str> = BTreeSet::new();
        let mut inputs: BTreeSet<&str> = BTreeSet::new();
        for observation in blocks.values().flatten() {
            per_arm
                .entry(observation.arm.as_str())
                .or_default()
                .insert(observation.exec_fingerprint.as_str());
            outputs.insert(observation.output_digest.as_str());
            inputs.insert(observation.input_fingerprint.as_str());
        }
        // Across **both** arms. This is the comparison the exec fingerprint
        // cannot make, because it covers the binary and so differs by arm by
        // construction — leaving a one-pair screen with no cross-arm identity
        // check at all.
        if inputs.len() > 1 {
            return Err(Refusal::InputFingerprintDiffers {
                catalog: catalog.clone(),
            });
        }
        for (arm, fingerprints) in &per_arm {
            if fingerprints.len() > 1 {
                return Err(Refusal::ExecFingerprintDiffers {
                    catalog: catalog.clone(),
                    arm: (*arm).to_owned(),
                });
            }
        }
        // Output parity is derived from the digests rather than self-asserted.
        if outputs.len() > 1 {
            return Err(Refusal::OutputParityBroken {
                catalog: catalog.clone(),
                digests: outputs.len(),
            });
        }

        for (index, observations) in blocks {
            let arms: Vec<&str> = {
                let mut ordered: Vec<&Observation> = observations.clone();
                ordered.sort_by_key(|observation| observation.order);
                ordered.iter().map(|o| o.arm.as_str()).collect()
            };
            let balanced = matches!(
                arms.as_slice(),
                ["a", "b", "b", "a"] | ["b", "a", "a", "b"] | ["a", "b"] | ["b", "a"]
            );
            if !balanced {
                return Err(Refusal::UnbalancedArms {
                    catalog: catalog.clone(),
                    block: *index,
                });
            }
        }
    }

    for catalog in catalogs {
        let Some(blocks) = grouped.get(&catalog.path) else {
            return Err(Refusal::CatalogNotMeasured {
                catalog: catalog.path.clone(),
            });
        };
        let required = required_coverage(manifest, catalog);
        let full_blocks = blocks
            .values()
            .filter(|observations| observations.len() == 4)
            .count();
        let units = match required {
            Coverage::Blocks(_) => full_blocks,
            Coverage::ScreenPair => blocks.len(),
        };
        if units < required.units() {
            return Err(Refusal::CoverageShort {
                catalog: catalog.path.clone(),
                required: required.units(),
                found: units,
            });
        }
    }

    if manifest.tier == Tier::One {
        let measured: BTreeSet<&str> = campaign
            .observations
            .iter()
            .filter_map(|observation| observation.mode.as_deref())
            .collect();
        for mode in &manifest.modes {
            if !measured.contains(mode.as_str()) {
                return Err(Refusal::ModeNotMeasured { mode: mode.clone() });
            }
        }
    }

    Ok(())
}

fn blocks_for(observations: &BTreeMap<usize, Vec<&Observation>>, metric: Metric) -> Vec<Block> {
    let promoted = observations.values().any(|runs| runs.len() >= 4)
        && observations.values().any(|runs| runs.len() == 2);
    let mut blocks = Vec::new();
    for group in observations.values() {
        let mut ordered: Vec<&Observation> = group.clone();
        ordered.sort_by_key(|observation| observation.order);
        let a: Vec<f64> = ordered
            .iter()
            .filter(|observation| observation.arm == "a")
            .map(|observation| observation.metric(metric))
            .collect();
        let b: Vec<f64> = ordered
            .iter()
            .filter(|observation| observation.arm == "b")
            .map(|observation| observation.metric(metric))
            .collect();
        // Once a catalog has been promoted, its original screen no longer
        // enters the estimate. It is what triggered promotion, not one of the
        // blocks the rung asked for, and mixing an un-drift-corrected pair
        // into drift-corrected blocks would make the reported block count
        // mean two different things.
        if promoted && ordered.len() < 4 {
            continue;
        }
        match (a.as_slice(), b.as_slice()) {
            ([a0, a1], [b0, b1]) => blocks.push(Block {
                a: [*a0, *a1],
                b: [*b0, *b1],
            }),
            // A screen pair is one A and one B; duplicating each into the
            // block's two slots leaves the mean, and therefore the ratio,
            // exactly what the pair measured.
            ([a0], [b0]) => blocks.push(Block {
                a: [*a0, *a0],
                b: [*b0, *b0],
            }),
            _ => {}
        }
    }
    blocks
}

/// Adjudicate `campaign`.
///
/// # Errors
///
/// [`Refusal`] when the campaign's own validity conditions fail. No verdict is
/// produced in that case, which is the whole point: an invalid campaign must
/// not yield a confident answer.
pub fn adjudicate(campaign: &Campaign) -> Result<Adjudication, Refusal> {
    let manifest = &campaign.manifest;

    let mut grouped: BTreeMap<String, BTreeMap<usize, Vec<&Observation>>> = BTreeMap::new();
    for observation in &campaign.observations {
        grouped
            .entry(observation.catalog.clone())
            .or_default()
            .entry(observation.block)
            .or_default()
            .push(observation);
    }

    check_refusals(campaign, &grouped)?;

    let looks = manifest.escalation_ladder.len();
    // The ladder positions the campaign; the decision count prices the
    // interval. They were one quantity when only escalation was priced.
    let decisions = campaign.decisions();
    let primary_metric = campaign.effective_primary_metric();
    let built = build_streams(manifest, &grouped, decisions, primary_metric, None)?;
    let streams = built.streams;

    // Where this campaign has reached: the highest rung the primary's block
    // count has met. A campaign resting between rungs counts as the lower one.
    let primary_blocks = streams
        .iter()
        .find(|stream| {
            stream.catalog == manifest.primary_catalog && stream.metric == primary_metric
        })
        .map_or(0, |stream| stream.effect.blocks);
    let look = manifest
        .escalation_ladder
        .iter()
        .filter(|rung| **rung <= primary_blocks)
        .count()
        .max(1);

    let mut verdict = decide(manifest, primary_metric, &streams, look, looks);
    if verdict == Verdict::Win
        && let Some(unblocked_by) = cleared_by_escalation(
            manifest,
            &grouped,
            decisions,
            primary_metric,
            look,
            &streams,
        )?
    {
        verdict = Verdict::WinPendingConfirmation { unblocked_by };
    }

    let target_met = manifest.target.as_ref().map(|target| {
        let levels: Vec<f64> = campaign
            .observations
            .iter()
            .filter(|observation| observation.catalog == target.catalog && observation.arm == "b")
            .map(|observation| observation.metric(target.metric))
            .collect();
        // The **mean** of the candidate arm, which is the statistic a target
        // declares. This once took the maximum, which silently adjudicated a
        // stricter contract than the one written down — and a target is a
        // reason to stop, so the statistic has to be the one the author meant.
        let count = u32::try_from(levels.len()).unwrap_or(u32::MAX);
        count > 0 && levels.iter().sum::<f64>() / f64::from(count) < target.below
    });

    let externally_paced = externally_paced(&streams);

    Ok(Adjudication {
        verdict,
        streams,
        aggregate: geometric_mean(&built.aggregate_ratios),
        aggregate_includes_screens: built.aggregate_includes_screens,
        look,
        looks,
        decisions,
        primary_metric,
        target_met,
        externally_paced,
    })
}

/// The streams a set of observations produces, plus what feeds the aggregate.
struct BuiltStreams {
    streams: Vec<Stream>,
    aggregate_ratios: Vec<f64>,
    aggregate_includes_screens: bool,
}

/// Build every stream, optionally as of an earlier look.
///
/// `block_limit` is what makes a campaign's own history replayable: passing
/// `Some(n)` reconstructs the adjudication that the first `n` blocks would
/// have produced. Nothing else needs to be stored for that, because a campaign
/// file is append-only and every observation carries its block index.
fn build_streams(
    manifest: &Manifest,
    grouped: &BTreeMap<String, BTreeMap<usize, Vec<&Observation>>>,
    decisions: usize,
    primary_metric: Metric,
    block_limit: Option<usize>,
) -> Result<BuiltStreams, Refusal> {
    let mut streams = Vec::new();
    let mut aggregate_ratios = Vec::new();
    let mut aggregate_includes_screens = false;

    for catalog in &manifest.catalogs {
        let Some(observations) = grouped.get(&catalog.path) else {
            continue;
        };
        let observations: BTreeMap<usize, Vec<&Observation>> = observations
            .iter()
            .filter(|(block, _)| block_limit.is_none_or(|limit| **block < limit))
            .map(|(block, runs)| (*block, runs.clone()))
            .collect();
        if observations.is_empty() {
            continue;
        }
        for metric in Metric::VERDICT_BEARING {
            let blocks = blocks_for(&observations, metric);
            let effect = match paired_effect(&blocks, decisions) {
                Ok(effect) => effect,
                Err(EstimateError::NoBlocks) => continue,
                Err(EstimateError::NonPositiveMeasurement) => {
                    return Err(Refusal::FailedRun {
                        catalog: catalog.path.clone(),
                        block: 0,
                    });
                }
            };

            let ceiling = 1.0
                + manifest
                    .guardrail_ceilings_pct
                    .get(&metric)
                    .copied()
                    .unwrap_or(f64::INFINITY)
                    / 100.0;

            let outcome = if effect.is_screen() {
                if effect.ratio > ceiling {
                    StreamOutcome::EscalateGuardrail
                } else {
                    StreamOutcome::Screened
                }
            } else if effect.low >= ceiling {
                StreamOutcome::RejectGuardrail
            } else if effect.high > ceiling {
                StreamOutcome::UnresolvedGuardrail
            } else {
                StreamOutcome::Within
            };

            if metric == primary_metric && catalog.role == Role::Substantive {
                aggregate_ratios.push(effect.ratio);
                aggregate_includes_screens |= effect.is_screen();
            }

            streams.push(Stream {
                catalog: catalog.path.clone(),
                metric,
                effect,
                outcome,
            });
        }
    }

    Ok(BuiltStreams {
        streams,
        aggregate_ratios,
        aggregate_includes_screens,
    })
}

/// The guardrail whose clearing unblocked a win, if one did.
///
/// This is an optional-stopping hazard. Continuing to measure a guardrail is
/// not always conservative:
/// a guardrail that blocked the verdict at an earlier look and clears at a
/// later one *unblocks* a win that was previously unavailable. Reporting that
/// as an ordinary `Win` would launder a stopping rule chosen after seeing the
/// data.
///
/// Detected by replay rather than by stored history: each earlier rung of the
/// ladder is re-adjudicated from the blocks that existed then, and a guardrail
/// that was `Reject`, `Unresolved` or `Escalate` at any of them and is clear
/// now is named.
fn cleared_by_escalation(
    manifest: &Manifest,
    grouped: &BTreeMap<String, BTreeMap<usize, Vec<&Observation>>>,
    decisions: usize,
    primary_metric: Metric,
    look: usize,
    current: &[Stream],
) -> Result<Option<String>, Refusal> {
    let blocked = |outcome: StreamOutcome| {
        matches!(
            outcome,
            StreamOutcome::RejectGuardrail
                | StreamOutcome::UnresolvedGuardrail
                | StreamOutcome::EscalateGuardrail
        )
    };
    for rung in manifest
        .escalation_ladder
        .iter()
        .take(look.saturating_sub(1))
    {
        let earlier = build_streams(manifest, grouped, decisions, primary_metric, Some(*rung))?;
        for stream in &earlier.streams {
            // Skip only the primary *decision* stream. Every other metric on
            // that same catalog is a guardrail like any other, and skipping
            // the whole catalog meant an unresolved peak-RSS on the primary
            // could clear after escalation and hand back an ordinary `Win`.
            let is_primary_decision = stream.catalog == manifest.primary_catalog
                && stream.metric == manifest.primary_metric;
            if is_primary_decision || !blocked(stream.outcome) {
                continue;
            }
            let cleared = current
                .iter()
                .find(|now| now.catalog == stream.catalog && now.metric == stream.metric)
                .is_some_and(|now| !blocked(now.outcome));
            if cleared {
                return Ok(Some(format!(
                    "{} on {}",
                    stream.catalog,
                    stream.metric.name()
                )));
            }
        }
    }
    Ok(None)
}

/// The precedence order: guardrails outrank the primary, always.
fn decide(
    manifest: &Manifest,
    primary_metric: Metric,
    streams: &[Stream],
    look: usize,
    looks: usize,
) -> Verdict {
    let exhausted = look >= looks;
    for stream in streams {
        if stream.outcome == StreamOutcome::RejectGuardrail {
            return Verdict::RejectGuardrail {
                catalog: stream.catalog.clone(),
                metric: stream.metric,
            };
        }
    }
    for stream in streams {
        if stream.outcome == StreamOutcome::UnresolvedGuardrail {
            return if exhausted {
                Verdict::InconclusiveExhausted
            } else {
                Verdict::UnresolvedGuardrail {
                    catalog: stream.catalog.clone(),
                    metric: stream.metric,
                }
            };
        }
    }

    // A screen pair over its ceiling is not evidence of a regression and not
    // evidence against one — it has no interval. It is an unadjudicated
    // guardrail, so it outranks the primary for the same reason the other
    // two do. Omitting it let a campaign publish `Win` while its own report
    // listed the catalog under "Escalation owed".
    //
    // Never `InconclusiveExhausted`: escalating a screen means promoting that
    // catalog from one pair to blocks, which stays available however far the
    // primary's ladder has advanced.
    for stream in streams {
        if stream.outcome == StreamOutcome::EscalateGuardrail {
            return Verdict::EscalateGuardrail {
                catalog: stream.catalog.clone(),
                metric: stream.metric,
            };
        }
    }

    let Some(primary) = streams.iter().find(|stream| {
        stream.catalog == manifest.primary_catalog && stream.metric == primary_metric
    }) else {
        return Verdict::Inconclusive;
    };

    if manifest.tier == Tier::One {
        let bar = 1.0 - manifest.tier.decisive_improvement().unwrap_or(0.0);
        return if primary.effect.ratio <= bar {
            Verdict::Win
        } else {
            Verdict::NotDecisive
        };
    }

    let minimum = 1.0 - manifest.minimum_useful_effect_pct / 100.0;
    if primary.effect.high <= minimum {
        Verdict::Win
    } else if primary.effect.low >= 1.0 {
        Verdict::RevertPrimaryRegressed
    } else if primary.effect.low > minimum && primary.effect.high < 1.0 {
        Verdict::RevertBelowMinimum
    } else if exhausted {
        Verdict::InconclusiveExhausted
    } else {
        Verdict::Inconclusive
    }
}

#[cfg(test)]
pub(crate) mod tests;
