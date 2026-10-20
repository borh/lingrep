//! The repository's lingrep patterns catalog inventory, discovered rather than listed.
//!
//! Tests that need per-catalog expectations still pin them by name — the point
//! is that the *set* comes from the tree, so a new catalog forces the
//! expectation to be written rather than being quietly skipped.
//!
//! This lives in `lingrep_core::testing` rather than in one crate's `tests/support`
//! because lingrep-web's catalog-projection baseline needs the same set. A second
//! copy over there would be a second thing to forget to update, which is the
//! failure this module exists to prevent.

use std::path::{Path, PathBuf};

/// The workspace root.
///
/// `CARGO_MANIFEST_DIR` is lingrep-core's directory whichever crate's test calls
/// this, because the constant is baked in where this file is compiled — so the
/// answer does not depend on the caller.
pub fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("lingrep-core must have a parent directory")
        .to_path_buf()
}

/// Repository-relative directories that hold generated output, not sources.
///
/// The skip list below turns away whole *classes* of directory by name. This
/// one names paths, because `benchmarks/harbor/jobs/` is ordinary-looking and
/// its contents are not: a harbor run writes each agent's tool invocations
/// there, and an agent asked to author lingrep patterns writes `.lx` files among them. The
/// repository already declares it generated — `benchmarks/harbor/.gitignore`
/// carries `jobs/` — so this restates that boundary for a walk that cannot
/// read it.
///
/// Left unskipped, one benchmark run turned seventeen tests red at once, all
/// of them pins over "every tracked catalog": a duplicate definition and a
/// syntax error in someone's generated `patterns/0.lx` were reported as
/// defects in this repository's catalogs.
const GENERATED_DIRECTORIES: &[&str] = &["benchmarks/harbor/jobs"];

/// Every `.lx` file the repository ships, found rather than listed.
///
/// Deliberately not `git ls-files`: the inventory must work in a source
/// archive with no git directory. The trade is that an untracked scratch
/// catalog under `patterns/` would be included too, which is the safe
/// direction to err — there. It is not the safe direction under a tree of
/// generated output, which is what [`GENERATED_DIRECTORIES`] is for.
pub fn tracked_catalogs() -> Vec<PathBuf> {
    let root = repository_root();
    let mut catalogs = Vec::new();

    let mut directories = vec![root.clone()];
    while let Some(directory) = directories.pop() {
        let entries = std::fs::read_dir(&directory).unwrap_or_else(|error| {
            #[expect(
                clippy::panic,
                reason = "catalog scanning requires a readable source tree"
            )]
            {
                panic!("{} must be readable: {error}", directory.display())
            }
        });
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                // Dot-directories hold other checkouts of this repository, not
                // this one's sources: `.direnv` caches a pinned flake input
                // that is a *snapshot of this repo at an older revision*, whose
                // catalogs may target an older compiler and must not be judged
                // by this checkout. `.git` and `.worktrees` are the
                // same hazard. Build output and vendored third-party trees
                // carry no first-party catalogs. `GENERATED_DIRECTORIES` adds
                // the trees that look like sources and are not.
                let generated = path.strip_prefix(&root).is_ok_and(|relative| {
                    GENERATED_DIRECTORIES
                        .iter()
                        .any(|generated| Path::new(generated) == relative)
                });
                let skipped = generated
                    || name.starts_with('.')
                    || matches!(name.as_ref(), "target" | "node_modules");
                if !skipped {
                    directories.push(path);
                }
            } else if path.extension().is_some_and(|extension| extension == "lx") {
                catalogs.push(path);
            }
        }
    }

    catalogs.sort();
    catalogs
}

/// Every tracked catalog as a `(repository-relative path, source)` pair, in
/// sorted path order.
pub fn tracked_catalog_sources() -> Vec<(String, String)> {
    let root = repository_root();
    tracked_catalogs()
        .into_iter()
        .map(|catalog| {
            let name = catalog
                .strip_prefix(&root)
                .unwrap_or(&catalog)
                .to_string_lossy()
                .into_owned();
            let source = std::fs::read_to_string(&catalog).unwrap_or_else(|error| {
                #[expect(
                    clippy::panic,
                    reason = "catalog names came from read_dir, so they must load"
                )]
                {
                    panic!("{name} must be readable: {error}")
                }
            });
            (name, source)
        })
        .collect()
}

/// What a tracked catalog contributes to a performance campaign.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuardrailRole {
    /// Carries paired blocks under the tier rules and enters the equal-weight
    /// aggregate.
    Substantive,
    /// A decode and publication floor and a correctness smoke case. Measured,
    /// but never evidence that an optimization is broadly effective.
    Floor,
}

/// A runtime mechanism a campaign can be scoped by.
///
/// These are **machine tags**, deliberately separate from the prose in
/// [`CatalogDescriptor::description`]. A campaign declares the regimes its
/// change can affect and the adjudicator selects catalogs by set membership;
/// prose descriptions are unique per catalog, so selecting on them would mean
/// substring matching against sentences nobody guaranteed to be stable.
///
/// The set is closed and grows only when a campaign needs a distinction it
/// cannot express. Tags are authored, but the ones with a reliable source
/// oracle are cross-checked by
/// `every_pattern_catalog_has_a_pinned_guardrail_role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RuntimeRegime {
    /// Compiles `preceded_by`/`followed_by` or their negations, so it drives
    /// the assertion evaluator. Exactly five catalogs do.
    Lookaround,
    /// Declares named groups, so capture extraction runs.
    Capture,
    /// Projects `key:` through a capture, the costlier publication path.
    CaptureKeyedPublication,
    /// Projects `key:` from bare attributes on a statically width-one match.
    AttributeKeyedPublication,
    /// Uses `same_as`, so it is recognized by the tree evaluator.
    SameAs,
    /// Declares a `where` clause, so the tree evaluator edge-filters every
    /// candidate match.
    Relations,
    /// Enumerates enhanced dependency graph-node and named-edge assignments.
    EnhancedRelations,
    /// Uses expression-level `&`, so it leaves the dense machine.
    Intersection,
    /// Uses bounded repetition `{n,m}`, which expands the NFA.
    BoundedRepeat,
    /// Uses `~` or `!~` with a regex operand.
    RegexPredicate,
    /// Emits a record on nearly every token: the record-stream amplifier.
    PublicationVolume,
    /// Declares a `policy … end` block, so publication runs a program.
    PolicyProgram,
    /// Declares a `transform:` list, so publication rewrites record spans.
    ///
    /// A distinct seam from [`RuntimeRegime::PolicyProgram`]: a policy block
    /// adjudicates *between* records and a transform rewrites *inside* one,
    /// and the two run in different passes. The tag exists because the
    /// mechanism had no tracked user at all until `npv-collocation.lx`, so a
    /// campaign touching `apply_transforms` selected nothing and looked
    /// cheaper rather than unmeasured.
    RecordTransform,
    /// Exercises decode and publication with almost no matching, so it is the
    /// floor a campaign measures against rather than evidence of effect.
    DecodeFloor,
}

/// The token schema family a campaign binds a catalog against.
///
/// Every catalog was implicitly bound to the UniDic morphology schema until
/// `ud-dependency.lx`,
/// whose `where` clauses require `dep.head`/`dep.rel` and therefore refuse a
/// morphology-only schema at binding — correctly, so the campaign harness has
/// to bind it against a schema that carries them rather than treat the
/// refusal as a broken catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CampaignBinding {
    /// `TokenSchema::unidic_morphology()`, the default.
    UnidicMorphology,
    /// The CoNLL-U reader's schema, for a catalog that predicates on
    /// dependency structure.
    ConlluSyntax,
}

/// A tracked catalog's standing in the stratified performance suite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogDescriptor {
    /// Repository-relative path — the key every per-catalog pin carries.
    pub path: &'static str,
    /// Whether the catalog is evidence of broad effectiveness or only a floor.
    pub role: GuardrailRole,
    /// The schema family a measurement binds this catalog against.
    pub binding: CampaignBinding,
    /// The runtime mechanisms this catalog exercises. The tier rules give a
    /// catalog paired blocks when a change can affect one of its regimes and
    /// a single screen pair otherwise, so this is what a campaign selects on.
    pub regimes: &'static [RuntimeRegime],
    /// Why this catalog is in the suite, for a human scoping a campaign.
    /// Never a selector: see [`RuntimeRegime`].
    pub description: &'static str,
}

impl CatalogDescriptor {
    /// Whether a change affecting `regimes` reaches this catalog.
    ///
    /// Set intersection, so a catalog exercising several mechanisms is
    /// selected by a campaign naming any one of them.
    #[must_use]
    pub fn affected_by(&self, regimes: &[RuntimeRegime]) -> bool {
        self.regimes.iter().any(|regime| regimes.contains(regime))
    }
}

/// The stratified performance suite of
/// [`docs/PERFORMANCE_REQUIREMENTS.md`](../../../../docs/PERFORMANCE_REQUIREMENTS.md),
/// in sorted path order.
///
/// This is the single source for catalog roles and regimes. It lives beside
/// catalog discovery, not inside a test, because it has consumers in two
/// crates: the completeness guard in `lingrep-core`'s integration suite, and the
/// `lingrep-perf` campaign adjudicator, which reads a role to decide what enters
/// the aggregate and a regime to decide what needs paired blocks. A second
/// copy could drift from campaign adjudication.
///
/// The set is pinned by name while the *discovered* set comes from the tree,
/// so a new catalog fails the guard until someone decides what it guards. That
/// list must account for every discovered catalog.
///
/// The regimes are machine tags and the description is the prose beside them.
/// `lingrep-perf declare` refuses a tag outside this set, so a mistyped regime
/// cannot silently select nothing — which would drop every guardrail to a
/// screen pair and make the campaign look cheaper rather than wrong. Deriving
/// the tags from each catalog's compiled form would be stronger still; the
/// authored ones with a reliable source oracle are cross-checked in the
/// integration suite in the meantime.
pub fn catalog_campaign_descriptors() -> Vec<CatalogDescriptor> {
    use GuardrailRole::{Floor, Substantive};
    use RuntimeRegime::{
        AttributeKeyedPublication, BoundedRepeat, Capture, CaptureKeyedPublication, DecodeFloor,
        EnhancedRelations, Intersection, Lookaround, PolicyProgram, PublicationVolume,
        RecordTransform, RegexPredicate, Relations, SameAs,
    };

    [
        (
            "patterns/alternation.lx",
            Substantive,
            &[
                PolicyProgram,
                Capture,
                CaptureKeyedPublication,
                RegexPredicate,
            ][..],
            "policy resolution under a metadata partition, and derived groups",
        ),
        (
            "patterns/collexeme.lx",
            Substantive,
            &[
                Lookaround,
                Capture,
                CaptureKeyedPublication,
                RegexPredicate,
                BoundedRepeat,
            ][..],
            "lookaround with capture-keyed publication",
        ),
        (
            "patterns/comment-register.lx",
            Substantive,
            &[
                Relations,
                Lookaround,
                Capture,
                CaptureKeyedPublication,
                RegexPredicate,
                BoundedRepeat,
            ][..],
            "English register word sketches over CoNLL-U dependency edges",
        ),
        (
            "patterns/concord.lx",
            Substantive,
            &[Lookaround][..],
            "rare-anchor unbounded gaps and sparse lookaround",
        ),
        (
            "patterns/counting.lx",
            Substantive,
            &[BoundedRepeat][..],
            "bounded-repeat expansion and larger regular NFAs",
        ),
        (
            "patterns/distributions.lx",
            Substantive,
            &[PublicationVolume, AttributeKeyedPublication][..],
            "any-token publication volume: the record-stream amplifier",
        ),
        // `-` sorts before `.`, so the successor precedes its ancestor in the
        // discovered order this list mirrors.
        (
            "patterns/double-wo-dep.lx",
            Substantive,
            &[Relations, Capture, BoundedRepeat][..],
            "head-sharing dependency successor to double-wo's candidate set",
        ),
        (
            "patterns/double-wo.lx",
            Substantive,
            &[Lookaround, Intersection, BoundedRepeat][..],
            "dense lookaround/assertion sweeps and intersections",
        ),
        (
            "patterns/ngrams.lx",
            Substantive,
            &[
                PublicationVolume,
                AttributeKeyedPublication,
                Capture,
                CaptureKeyedPublication,
            ][..],
            "overlapping one-to-five-token windows with keyed publication",
        ),
        (
            "patterns/npv-collocation.lx",
            Substantive,
            &[
                RecordTransform,
                Relations,
                Lookaround,
                Capture,
                CaptureKeyedPublication,
            ][..],
            "noun-particle-verb collocations with a capture-scoped verb normalization",
        ),
        (
            "patterns/passive-agent-dep.lx",
            Substantive,
            &[Relations, Capture, CaptureKeyedPublication][..],
            "passive subjects and co-governed agent candidates over CoNLL-U input",
        ),
        (
            "patterns/passive-oblique-realization.lx",
            Substantive,
            &[Relations, Capture, CaptureKeyedPublication][..],
            "passive-marker evidence with plural oblique assignments and an absent population",
        ),
        (
            "patterns/portable-content-words.lx",
            Substantive,
            &[AttributeKeyedPublication][..],
            "portable UPOS predicates and attribute-keyed publication",
        ),
        (
            "patterns/reduplication.lx",
            Substantive,
            &[SameAs, Capture][..],
            "capture history and bounded `same_as` execution",
        ),
        (
            "patterns/sentence-final.lx",
            Substantive,
            &[Lookaround, PolicyProgram][..],
            "compact lingrep-web sentence-final catalog with category grouping",
        ),
        (
            "patterns/sf-experimental.lx",
            Substantive,
            &[Lookaround, PolicyProgram][..],
            "large definition/reference catalog and sentence-boundary policy",
        ),
        (
            "patterns/sfe.lx",
            Substantive,
            &[Capture, RegexPredicate][..],
            "sentence-final expression probes, captures, and regex predicates",
        ),
        (
            "patterns/sketch-grammar.lx",
            Substantive,
            &[Capture, CaptureKeyedPublication, BoundedRepeat][..],
            "capture-key publication and mixed repetition",
        ),
        (
            "patterns/test-simple.lx",
            Floor,
            &[DecodeFloor][..],
            "decode and publication floor; correctness smoke case",
        ),
        (
            "patterns/ud-dependency.lx",
            Substantive,
            &[Relations, Capture][..],
            "dependency edge constraints between captures over CoNLL-U input",
        ),
        (
            "patterns/ud-enhanced-argument.lx",
            Substantive,
            &[
                EnhancedRelations,
                Relations,
                Capture,
                CaptureKeyedPublication,
            ][..],
            "assignment-preserving enhanced graph queries, including empty nodes and named edges",
        ),
        (
            "patterns/ud-enhanced-consistency.lx",
            Substantive,
            &[
                EnhancedRelations,
                Relations,
                Capture,
                CaptureKeyedPublication,
            ][..],
            "basic-to-enhanced endpoint preservation and representation-class candidates",
        ),
        (
            "patterns/ud-enhanced-propagation.lx",
            Substantive,
            &[
                EnhancedRelations,
                Relations,
                Capture,
                CaptureKeyedPublication,
            ][..],
            "fixed enhanced-graph motifs for coordination, control, relatives, and gapping",
        ),
        (
            "patterns/ud-nonprojectivity.lx",
            Substantive,
            &[Relations, Capture, CaptureKeyedPublication][..],
            "four-endpoint crossing-edge queries over CoNLL-U input",
        ),
        (
            "patterns/ud-object-realization.lx",
            Substantive,
            &[Relations, Capture, CaptureKeyedPublication][..],
            "anonymous direct-object existence and dependent-class predicates over CoNLL-U input",
        ),
        (
            "patterns/ud-subtree-profile.lx",
            Substantive,
            &[Relations, Capture, CaptureKeyedPublication][..],
            "three-node fork and chain profiles over CoNLL-U input",
        ),
        (
            "patterns/ud-valency.lx",
            Substantive,
            &[Relations, Capture, CaptureKeyedPublication][..],
            "positive argument-frame inventories over CoNLL-U input",
        ),
        (
            "patterns/wo-predicate-dep.lx",
            Substantive,
            &[
                Relations,
                Capture,
                CaptureKeyedPublication,
                BoundedRepeat,
                RegexPredicate,
            ][..],
            "the ヲ格 relation as a linear XPOS rule and as a dependency edge over one CoNLL-U token set",
        ),
    ]
    .into_iter()
    .map(|(path, role, regimes, description)| CatalogDescriptor {
        path,
        role,
        // Derived, not restated: a catalog exercising the relation filter
        // predicates on dependency structure, and the CoNLL-U schema is the
        // schema that carries it. A second dependency-bearing schema family
        // would make this a per-entry decision again.
        binding: if regimes.contains(&RuntimeRegime::Relations) {
            CampaignBinding::ConlluSyntax
        } else {
            CampaignBinding::UnidicMorphology
        },
        regimes,
        description,
    })
    .collect()
}
