//! Feature-gated measurements for spawn predicate candidate-set experiments.

use crate::token_match::AttributeKey;

/// The classifier's operator, which the dispatcher needs in default builds and
/// this module reports in retained evidence.
pub use super::spawn_dispatch::SpawnAtomOperator;

/// Static spawn-table classification for one compiled lingrep patterns catalog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnCandidateCatalogMetrics {
    /// Number of deduplicated spawn predicate groups.
    pub spawn_groups: usize,
    /// Groups with no finite-positive atom.
    pub zero_safe_groups: usize,
    /// Groups with exactly one finite-positive atom.
    pub one_safe_groups: usize,
    /// Groups with multiple finite-positive atoms.
    pub multiple_safe_groups: usize,
    /// Scheme A groups that cannot be indexed by a finite-positive atom.
    pub scheme_a_undiscriminated_groups: usize,
    /// Scheme B groups that cannot be indexed by a finite-positive atom.
    pub scheme_b_undiscriminated_groups: usize,
    /// Distinct attributes selected by Scheme A.
    pub scheme_a_indexed_attributes: usize,
    /// Distinct attributes consulted by Scheme B.
    pub scheme_b_indexed_attributes: usize,
    /// Safe-atom counts by attribute and operator.
    pub safe_atoms: Vec<SpawnSafeAtomMetric>,
    /// Predicate-shape counts with deterministic dense identifiers.
    pub shapes: Vec<SpawnPredicateShapeMetric>,
}

/// Static safe-atom count for one attribute/operator pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnSafeAtomMetric {
    /// Attribute read by the safe atom.
    pub attribute: AttributeKey,
    /// Finite-positive operator.
    pub operator: SpawnAtomOperator,
    /// Number of compiled safe atoms with this pair.
    pub atoms: usize,
}

/// Static count for one deterministic predicate-shape signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnPredicateShapeMetric {
    /// Dense identifier used by dynamic counters.
    pub id: usize,
    /// Sorted attribute/operator/polarity signature.
    pub signature: String,
    /// Number of spawn groups with this signature.
    pub groups: usize,
}

/// Reusable exact counters for spawn candidate-set shadow evaluation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnCandidateAccumulator {
    /// Token positions at which a nonempty spawn table was consulted.
    pub spawn_positions: u64,
    /// Predicate groups admitted by the existing readiness check.
    pub actual_spawn_calls: u64,
    /// Readiness-admitted groups selected by Scheme A.
    pub scheme_a_candidate_calls: u64,
    /// Readiness-admitted groups selected by Scheme B.
    pub scheme_b_candidate_calls: u64,
    /// Actual calls excluded by Scheme A.
    pub scheme_a_avoided_calls: u64,
    /// Actual calls excluded by Scheme B.
    pub scheme_b_avoided_calls: u64,
    /// Sum of Scheme A candidate widths over spawn positions.
    pub scheme_a_width_sum: u64,
    /// Sum of Scheme B candidate widths over spawn positions.
    pub scheme_b_width_sum: u64,
    /// Largest Scheme A candidate width.
    pub scheme_a_max_width: usize,
    /// Largest Scheme B candidate width.
    pub scheme_b_max_width: usize,
    /// Exact Scheme A width histogram indexed by width.
    pub scheme_a_width_histogram: Vec<u64>,
    /// Exact Scheme B width histogram indexed by width.
    pub scheme_b_width_histogram: Vec<u64>,
    /// Scheme A actual calls belonging to undiscriminated groups.
    pub scheme_a_undiscriminated_calls: u64,
    /// Scheme B actual calls belonging to undiscriminated groups.
    pub scheme_b_undiscriminated_calls: u64,
    /// Finite-positive atom checks performed by Scheme A.
    pub scheme_a_finite_atom_checks: u64,
    /// Finite-positive atom checks performed by Scheme B.
    pub scheme_b_finite_atom_checks: u64,
    /// Actual and avoided calls by deterministic predicate shape.
    pub shape_effects: Vec<SpawnCandidateShapeEffect>,
}

impl SpawnCandidateAccumulator {
    /// Zeros all counters while retaining vector allocations for reuse.
    pub fn reset(&mut self) {
        self.spawn_positions = 0;
        self.actual_spawn_calls = 0;
        self.scheme_a_candidate_calls = 0;
        self.scheme_b_candidate_calls = 0;
        self.scheme_a_avoided_calls = 0;
        self.scheme_b_avoided_calls = 0;
        self.scheme_a_width_sum = 0;
        self.scheme_b_width_sum = 0;
        self.scheme_a_max_width = 0;
        self.scheme_b_max_width = 0;
        self.scheme_a_width_histogram.fill(0);
        self.scheme_b_width_histogram.fill(0);
        self.scheme_a_undiscriminated_calls = 0;
        self.scheme_b_undiscriminated_calls = 0;
        self.scheme_a_finite_atom_checks = 0;
        self.scheme_b_finite_atom_checks = 0;
        for (shape_id, effect) in self.shape_effects.iter_mut().enumerate() {
            *effect = SpawnCandidateShapeEffect {
                shape_id,
                ..SpawnCandidateShapeEffect::default()
            };
        }
    }

    pub(crate) fn prepare_dimensions(&mut self, width_count: usize, shape_count: usize) {
        if self.spawn_positions != 0 || self.actual_spawn_calls != 0 {
            return;
        }
        self.scheme_a_width_histogram.resize(width_count, 0);
        self.scheme_a_width_histogram.truncate(width_count);
        self.scheme_b_width_histogram.resize(width_count, 0);
        self.scheme_b_width_histogram.truncate(width_count);
        self.shape_effects
            .resize_with(shape_count, SpawnCandidateShapeEffect::default);
        self.shape_effects.truncate(shape_count);
        for (shape_id, effect) in self.shape_effects.iter_mut().enumerate() {
            effect.shape_id = shape_id;
        }
    }
}

/// Dynamic actual and avoided calls for one predicate shape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnCandidateShapeEffect {
    /// Dense shape identifier from [`SpawnPredicateShapeMetric::id`].
    pub shape_id: usize,
    /// Readiness-admitted calls with this shape.
    pub actual_calls: u64,
    /// Calls with this shape excluded by Scheme A.
    pub scheme_a_avoided_calls: u64,
    /// Calls with this shape excluded by Scheme B.
    pub scheme_b_avoided_calls: u64,
}
