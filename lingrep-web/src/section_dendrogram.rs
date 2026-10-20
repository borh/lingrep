//! Hierarchical clustering of the section comparison, in both orientations.
//!
//! [`SectionComparison`] puts every pattern's section profile on one scale,
//! which answers "does this pattern favour methods more than that one does".
//! It does not answer "which patterns behave alike", nor "which sections
//! attract the same patterns"; a reader comparing every row or column by eye
//! is performing a clustering badly. This performs it and draws the result.
//!
//! Both trees use the same profiles, selectable Jensen–Shannon or cosine
//! distance, and average linkage (UPGMA); they differ only in which margin is
//! clustered:
//! [`SectionDendrogram::new`] clusters the rows, and
//! [`SectionDendrogram::by_pattern_profile`] the columns.
//!
//! Built **from** the comparison rather than from the tally, so the table and
//! the trees cannot disagree about which patterns and which sections are in
//! scope.

use crate::section_comparison::SectionComparison;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DistanceMetric {
    #[default]
    JensenShannon,
    Cosine,
}

impl DistanceMetric {
    #[must_use]
    pub fn from_form_value(value: Option<&str>) -> Self {
        match value {
            Some("cosine") => Self::Cosine,
            _ => Self::JensenShannon,
        }
    }

    #[must_use]
    pub const fn form_value(self) -> &'static str {
        match self {
            Self::JensenShannon => "jensen-shannon",
            Self::Cosine => "cosine",
        }
    }
}

/// Pixels between adjacent leaves.
const ROW_HEIGHT: i64 = 22;
/// Narrowest the leaf-label gutter is ever drawn.
const MIN_LABEL_WIDTH: i64 = 120;
/// Widest, past which a label is truncated rather than given more room. A
/// gutter wider than the plot would make the tree the smaller half of its own
/// drawing.
const MAX_LABEL_WIDTH: i64 = 300;
/// Approximate advance of a halfwidth glyph at the label's font size, and of a
/// CJK one. Estimated rather than measured because the drawing is server-side
/// and the browser's font is not knowable here; both are deliberate
/// over-estimates, so a label errs toward fitting.
const HALFWIDTH_ADVANCE: i64 = 7;
const FULLWIDTH_ADVANCE: i64 = 13;
/// Width the merge heights are scaled across.
const PLOT_WIDTH: i64 = 320;
/// Room under the leaves for the distance axis.
const AXIS_HEIGHT: i64 = 34;
/// Gap between a label and its leaf.
const LABEL_GAP: i64 = 8;
/// Length of an axis tick's mark.
const TICK_LENGTH: i64 = 5;
/// Drop from a tick's mark to its label's baseline.
const TICK_LABEL_GAP: i64 = 12;

/// One pattern, at its drawn position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DendrogramLeaf {
    /// The clustered thing's identity: a pattern key in the pattern tree, a
    /// column index in the section tree.
    pub key: usize,
    /// The full name, for the leaf's tooltip.
    pub name: String,
    /// What is drawn, which is the name unless it did not fit.
    pub label: String,
    /// Vertical centre of the leaf's row.
    pub y: i64,
    /// Where the label's text ends, so it reads right up against its leaf.
    pub label_x: i64,
    /// Where the leaf's own stub starts.
    pub leaf_x: i64,
}

/// One join, as the elbow that draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DendrogramLink {
    /// `x1,y1 x2,y2 …` for one `<polyline>`: up the first child, across at the
    /// join's height, down to the second child. Formatted here because a
    /// template computing coordinates is a second place for the geometry to be
    /// got wrong.
    pub points: String,
    /// The join's distance, for the link's tooltip.
    pub distance_label: String,
    /// How many patterns sit under this join, also for the tooltip.
    pub leaves: usize,
}

/// One labelled tick on the distance axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DendrogramTick {
    pub x: i64,
    pub label: String,
}

/// The drawn tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionDendrogram {
    pub leaves: Vec<DendrogramLeaf>,
    pub links: Vec<DendrogramLink>,
    pub ticks: Vec<DendrogramTick>,
    pub width: i64,
    pub height: i64,
    /// Baseline the leaf stubs and the axis share.
    pub axis_y: i64,
    /// Where an axis tick's mark ends.
    pub tick_y: i64,
    /// Baseline of an axis tick's label.
    pub tick_label_y: i64,
    /// Entities excluded because their selected profile has no observations.
    /// Patterns with one or more observations are all retained.
    pub entities_without_observations: usize,
    /// True when fewer than two entities have any observations, so no tree was
    /// computed. The band still renders to report the missing data.
    pub insufficient_evidence: bool,
    /// The deepest join, which is the axis's right end.
    pub max_distance_label: String,
    /// True when every profile is identical, so every join sits at zero and
    /// the drawing is a comb. Worth saying rather than leaving a reader to
    /// wonder what broke.
    pub degenerate: bool,
}

/// One thing to be clustered, with the profile it is clustered on.
///
/// The two orientations differ only in what fills this: patterns profiled
/// over sections, or sections profiled over patterns.
struct Entity {
    key: usize,
    name: String,
    /// Total count across the profile's displayed dimensions.
    observations: u64,
    /// Counts against the *other* dimension, before normalization.
    counts: Vec<u64>,
}

/// A cluster during agglomeration: the leaves under it, and its drawn shape.
struct Cluster {
    size: usize,
    /// `None` for a leaf, whose position is fixed before any join.
    node: Node,
}

enum Node {
    Leaf(usize),
    Join {
        left: Box<Node>,
        right: Box<Node>,
        distance: f64,
    },
}

impl SectionDendrogram {
    /// Clusters the comparison's patterns, or `None` when there is nothing to
    /// cluster.
    ///
    /// Inherits the comparison's own refusals — a refused projection, no
    /// labelled sections, fewer than two comparable patterns — because it is
    /// built from one. Nothing else refuses: two patterns are a valid, if
    /// short, tree.
    #[must_use]
    pub fn new(comparison: &SectionComparison) -> Option<Self> {
        Self::with_distance(comparison, DistanceMetric::JensenShannon)
    }

    #[must_use]
    pub fn with_distance(comparison: &SectionComparison, metric: DistanceMetric) -> Option<Self> {
        // A pattern is profiled by its occurrence rate per million sentences
        // in each section. Not by its share of its own hits: a section holding
        // ten times the sentences collects ten times the hits from a pattern
        // with no preference at all, so a raw share would cluster patterns by
        // how the corpus is divided rather than by how they behave. Under the
        // rate, a pattern distributed in proportion to section size is
        // uniform.
        let denominators = comparison
            .columns
            .iter()
            .map(|column| column.universe)
            .collect::<Vec<_>>();
        let entities = comparison
            .rows
            .iter()
            .map(|row| Entity {
                key: row.pattern_key,
                name: row.pattern_name.clone(),
                observations: row.displayed_total,
                counts: row.cells.iter().map(|cell| cell.count).collect(),
            })
            .collect::<Vec<_>>();
        Self::cluster(entities, &denominators, metric)
    }

    /// The same comparison read down its columns: sections clustered by which
    /// patterns occur in them.
    ///
    /// The transpose of [`new`](Self::new), and normalized the same way with
    /// the roles exchanged. Each count is divided by its *pattern's* total
    /// over the displayed sections, so a frequent pattern does not dominate
    /// every section's profile, and the vector is then renormalized. A section
    /// whose composition matches the catalog's overall one is uniform, exactly
    /// as a pattern tracking section sizes is uniform in the other tree.
    #[must_use]
    pub fn by_pattern_profile(comparison: &SectionComparison) -> Option<Self> {
        Self::by_pattern_profile_with_distance(comparison, DistanceMetric::JensenShannon)
    }

    #[must_use]
    pub fn by_pattern_profile_with_distance(
        comparison: &SectionComparison,
        metric: DistanceMetric,
    ) -> Option<Self> {
        let denominators = comparison
            .rows
            .iter()
            .map(|row| row.displayed_total)
            .collect::<Vec<_>>();
        let entities = comparison
            .columns
            .iter()
            .enumerate()
            .map(|(column, section)| Entity {
                key: column,
                name: section.label.clone(),
                observations: comparison
                    .rows
                    .iter()
                    .filter_map(|row| row.cells.get(column))
                    .map(|cell| cell.count)
                    .sum(),
                counts: comparison
                    .rows
                    .iter()
                    .map(|row| row.cells.get(column).map_or(0, |cell| cell.count))
                    .collect(),
            })
            .collect::<Vec<_>>();
        Self::cluster(entities, &denominators, metric)
    }

    /// Data availability, profile, distance, tree, drawing — shared by both
    /// orientations so the two trees cannot diverge in method.
    fn cluster(
        mut entities: Vec<Entity>,
        denominators: &[u64],
        metric: DistanceMetric,
    ) -> Option<Self> {
        if entities.len() < 2 {
            return None;
        }
        // Largest first. `key` breaks the tie, so the selection is a function
        // of the run and not of the sort's stability.
        entities.sort_by(|left, right| {
            right
                .observations
                .cmp(&left.observations)
                .then_with(|| left.key.cmp(&right.key))
        });

        let with_data = entities
            .iter()
            .filter(|entity| entity.observations > 0)
            .count();
        let without_observations = entities.len() - with_data;
        entities.retain(|entity| entity.observations > 0);
        if entities.len() < 2 {
            return Some(Self::insufficient(without_observations));
        }

        let profiles = entities
            .iter()
            .map(|entity| profile(&entity.counts, denominators))
            .collect::<Vec<_>>();

        let distances = distance_matrix_with_metric(&profiles, metric);
        let tree = agglomerate(&distances);

        Self::draw(&entities, tree, without_observations)
    }

    /// The band with no tree in it, carrying only what it excluded and why.
    fn insufficient(entities_without_observations: usize) -> Self {
        Self {
            leaves: Vec::new(),
            links: Vec::new(),
            ticks: Vec::new(),
            width: 0,
            height: 0,
            axis_y: 0,
            tick_y: 0,
            tick_label_y: 0,
            entities_without_observations,
            insufficient_evidence: true,
            max_distance_label: format_distance(0.0),
            degenerate: false,
        }
    }

    fn draw(ranked: &[Entity], tree: Node, entities_without_observations: usize) -> Option<Self> {
        // In-order, so a link never crosses a leaf it does not join.
        let mut order = Vec::new();
        collect_leaves(&tree, &mut order);

        // The gutter fits the longest name it can, so a catalog of short
        // names is not drawn with an empty third of the canvas and one of long
        // ones is not clipped. Computed before any x coordinate, because every
        // one of them is measured from it.
        let widest = order
            .iter()
            .map(|leaf| text_width(&ranked[*leaf].name))
            .max()
            .unwrap_or(0);
        let label_width = (widest + LABEL_GAP).clamp(MIN_LABEL_WIDTH, MAX_LABEL_WIDTH);

        let mut positions = vec![0i64; ranked.len()];
        let leaves = order
            .iter()
            .enumerate()
            .map(|(row, leaf)| {
                let y = ROW_HEIGHT / 2 + i64::try_from(row).unwrap_or(i64::MAX) * ROW_HEIGHT;
                positions[*leaf] = y;
                let name = ranked[*leaf].name.clone();
                DendrogramLeaf {
                    key: ranked[*leaf].key,
                    label: truncate_to(&name, label_width - LABEL_GAP),
                    name,
                    y,
                    label_x: label_width - LABEL_GAP,
                    leaf_x: label_width,
                }
            })
            .collect::<Vec<_>>();

        let max_distance = max_distance(&tree);
        let degenerate = max_distance <= f64::EPSILON;
        let mut links = Vec::new();
        layout(&tree, &positions, max_distance, label_width, &mut links);

        // `layout` pushes post-order, so the links already run from the
        // shallowest join up to the root. Left as they are: the last link is
        // the one joining everything.

        let ticks = if degenerate {
            Vec::new()
        } else {
            (0..=4i32)
                .map(|step| DendrogramTick {
                    x: label_width + PLOT_WIDTH * i64::from(step) / 4,
                    label: format_distance(max_distance * f64::from(step) / 4.0),
                })
                .collect()
        };

        let axis_y = i64::try_from(leaves.len()).unwrap_or(i64::MAX) * ROW_HEIGHT;
        Some(Self {
            leaves,
            links,
            ticks,
            width: label_width + PLOT_WIDTH + 12,
            height: axis_y + AXIS_HEIGHT,
            axis_y,
            tick_y: axis_y + TICK_LENGTH,
            tick_label_y: axis_y + TICK_LENGTH + TICK_LABEL_GAP,
            entities_without_observations,
            insufficient_evidence: false,
            max_distance_label: format_distance(max_distance),
            degenerate,
        })
    }
}

/// A count vector as a profile: a rate against the other dimension's margin,
/// renormalized to unit sum.
///
/// Two steps, and both matter. Each count is first divided by the
/// corresponding denominator — a section's sentence universe when profiling a
/// pattern, a pattern's displayed total when profiling a section — which is
/// what removes the other dimension's size from the comparison. The rates are
/// then renormalized to sum to one. The renormalization makes the result
/// independent of the entity's own overall frequency, so a rare pattern and a
/// common one with the same shape are the same point.
///
/// A zero denominator cannot reach here — the comparison drops an empty
/// section, and entities with no observations are excluded — so
/// guarding it is defensive rather than expected.
fn profile(counts: &[u64], denominators: &[u64]) -> Vec<f64> {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a count or universe large enough to lose precision here is far past any corpus this draws, and the result is a drawn proportion"
    )]
    let rates = counts
        .iter()
        .zip(denominators.iter())
        .map(|(count, denominator)| {
            if *denominator == 0 {
                0.0
            } else {
                *count as f64 * f64::from(RATE_SCALE) / *denominator as f64
            }
        })
        .collect::<Vec<_>>();
    let total = rates.iter().sum::<f64>();
    if total <= 0.0 {
        return vec![0.0; counts.len()];
    }
    rates.iter().map(|rate| rate / total).collect()
}

/// Parts per million: the unit the section rates are read in before they are
/// renormalized. The scale cancels out of the renormalization and is carried
/// anyway, so the intermediate is the quantity a reader would compute by hand.
const RATE_SCALE: u32 = 1_000_000;

/// Jensen–Shannon **distance**: the square root of the divergence in bits.
///
/// The square root because that is the form obeying the triangle inequality,
/// and because its range is the one a reader can hold: 0 for two patterns
/// spread identically, 1 for two that never occur under the same section.
/// Symmetric, unlike Kullback–Leibler, which matters because a dendrogram has
/// no notion of which pattern is the reference.
fn jensen_shannon(left: &[f64], right: &[f64]) -> f64 {
    let divergence = left
        .iter()
        .zip(right.iter())
        .map(|(p, q)| {
            let mean = (p + q) / 2.0;
            entropy_term(*p, mean) + entropy_term(*q, mean)
        })
        .sum::<f64>()
        / 2.0;
    // Rounding can carry the sum a hair below zero or past one; a distance
    // outside its own range would scale the whole drawing wrong.
    divergence.clamp(0.0, 1.0).sqrt()
}

fn cosine_distance(left: &[f64], right: &[f64]) -> f64 {
    let left_norm = left.iter().map(|value| value * value).sum::<f64>().sqrt();
    let right_norm = right.iter().map(|value| value * value).sum::<f64>().sqrt();
    if left_norm <= f64::EPSILON || right_norm <= f64::EPSILON {
        return if left_norm <= f64::EPSILON && right_norm <= f64::EPSILON {
            0.0
        } else {
            1.0
        };
    }
    let dot = left
        .iter()
        .zip(right)
        .map(|(left, right)| left * right)
        .sum::<f64>();
    (1.0 - (dot / (left_norm * right_norm)).clamp(0.0, 1.0)).clamp(0.0, 1.0)
}

/// `p * log2(p / mean)`, and `0` where `p` is, since `0 log 0` is `0`.
fn entropy_term(p: f64, mean: f64) -> f64 {
    if p <= 0.0 || mean <= 0.0 {
        return 0.0;
    }
    p * (p / mean).log2()
}

fn distance_matrix_with_metric(profiles: &[Vec<f64>], metric: DistanceMetric) -> Vec<Vec<f64>> {
    profiles
        .iter()
        .map(|left| {
            profiles
                .iter()
                .map(|right| match metric {
                    DistanceMetric::JensenShannon => jensen_shannon(left, right),
                    DistanceMetric::Cosine => cosine_distance(left, right),
                })
                .collect()
        })
        .collect()
}

/// Average linkage (UPGMA) over the leaf distance matrix.
///
/// Average rather than single or complete linkage: single chains distinct
/// groups together through one intermediate pattern, complete is dominated by
/// each group's worst pair, and both make a section profile's cluster depend
/// on one row. Average reads the whole group, which is what "these patterns
/// behave alike" means.
///
/// Cluster-to-cluster distances use the average-linkage recurrence, so adding
/// leaves does not trigger a fresh cross-product scan for every candidate pair.
fn agglomerate(distances: &[Vec<f64>]) -> Node {
    let mut clusters = (0..distances.len())
        .map(|leaf| Cluster {
            size: 1,
            node: Node::Leaf(leaf),
        })
        .collect::<Vec<_>>();
    let mut distances = distances.to_vec();

    while clusters.len() > 1 {
        let mut best = (0usize, 1usize, f64::INFINITY);
        for (left, row) in distances.iter().enumerate() {
            for (right, &candidate) in row.iter().enumerate().skip(left + 1) {
                // Strictly less, so the first pair in index order wins a tie
                // and the tree is a function of the input alone.
                if candidate < best.2 {
                    best = (left, right, candidate);
                }
            }
        }
        let (left, right, distance) = best;
        let left_size = clusters[left].size;
        let right_size = clusters[right].size;
        let merged_size = left_size + right_size;
        #[expect(
            clippy::cast_precision_loss,
            reason = "cluster sizes are bounded by the number of in-memory profiles"
        )]
        let (left_weight, right_weight, merged_weight) =
            (left_size as f64, right_size as f64, merged_size as f64);
        let mut merged = Some(Cluster {
            size: merged_size,
            node: Node::Join {
                left: Box::new(std::mem::replace(
                    &mut clusters[left].node,
                    Node::Leaf(usize::MAX),
                )),
                right: Box::new(std::mem::replace(
                    &mut clusters[right].node,
                    Node::Leaf(usize::MAX),
                )),
                distance,
            },
        });

        // Preserve the active-cluster order, inserting the join at its left
        // child's slot so equal-distance ties remain deterministic.
        let old_len = clusters.len();
        let mut old_indices = Vec::with_capacity(old_len - 1);
        let mut next_clusters = Vec::with_capacity(old_len - 1);
        for (index, cluster) in clusters.iter().enumerate() {
            if index == left {
                old_indices.push(None);
                next_clusters.push(merged.take().expect("one join is inserted"));
            } else if index != right {
                old_indices.push(Some(index));
                next_clusters.push(Cluster {
                    size: cluster.size,
                    node: Node::Leaf(usize::MAX),
                });
            }
        }
        // Restore each surviving node after moving the merged children above.
        for (next, old) in next_clusters.iter_mut().zip(&old_indices) {
            if let Some(old) = old {
                next.node = std::mem::replace(&mut clusters[*old].node, Node::Leaf(usize::MAX));
            }
        }

        let next_len = next_clusters.len();
        let mut next_distances = vec![vec![0.0; next_len]; next_len];
        for row in 0..next_len {
            for column in (row + 1)..next_len {
                let value = match (old_indices[row], old_indices[column]) {
                    (Some(old_row), Some(old_column)) => distances[old_row][old_column],
                    (None, Some(other)) | (Some(other), None) => {
                        (left_weight * distances[left][other]
                            + right_weight * distances[right][other])
                            / merged_weight
                    }
                    (None, None) => 0.0,
                };
                next_distances[row][column] = value;
                next_distances[column][row] = value;
            }
        }
        clusters = next_clusters;
        distances = next_distances;
    }

    clusters.pop().expect("at least one cluster survives").node
}

fn collect_leaves(node: &Node, out: &mut Vec<usize>) {
    match node {
        Node::Leaf(leaf) => out.push(*leaf),
        Node::Join { left, right, .. } => {
            collect_leaves(left, out);
            collect_leaves(right, out);
        }
    }
}

fn max_distance(node: &Node) -> f64 {
    match node {
        Node::Leaf(_) => 0.0,
        Node::Join {
            left,
            right,
            distance,
        } => distance.max(max_distance(left)).max(max_distance(right)),
    }
}

/// Places one subtree and appends its links; returns the subtree's `(x, y)`.
///
/// A leaf sits at distance zero on the left edge of the plot; a join sits at
/// its own distance, scaled across the plot, and vertically at the mean of its
/// two children.
fn layout(
    node: &Node,
    positions: &[i64],
    max: f64,
    label_width: i64,
    links: &mut Vec<DendrogramLink>,
) -> (i64, i64) {
    match node {
        Node::Leaf(leaf) => (label_width, positions[*leaf]),
        Node::Join {
            left,
            right,
            distance,
        } => {
            let (left_x, left_y) = layout(left, positions, max, label_width, links);
            let (right_x, right_y) = layout(right, positions, max, label_width, links);
            let x = label_width + scale_distance(*distance, max);
            let y = i64::midpoint(left_y, right_y);
            let mut leaves = Vec::new();
            collect_leaves(node, &mut leaves);
            links.push(DendrogramLink {
                points: format!("{left_x},{left_y} {x},{left_y} {x},{right_y} {right_x},{right_y}"),
                distance_label: format_distance(*distance),
                leaves: leaves.len(),
            });
            (x, y)
        }
    }
}

/// A distance as pixels along the plot, `0` when every join sits at zero.
fn scale_distance(distance: f64, max: f64) -> i64 {
    if max <= f64::EPSILON {
        return 0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the ratio is clamped to the unit interval, so the product is a pixel count inside the plot"
    )]
    let scaled = ((distance / max).clamp(0.0, 1.0) * PLOT_WIDTH as f64).round() as i64;
    scaled
}

fn format_distance(distance: f64) -> String {
    format!("{distance:.2}")
}

/// Estimated drawn width of a label, in pixels.
///
/// A pattern name here is as often Japanese as ASCII, and the two differ by
/// roughly a factor of two in advance width, so a character count would
/// over-allocate for one and clip the other.
fn text_width(text: &str) -> i64 {
    text.chars()
        .map(|character| {
            if character.is_ascii() {
                HALFWIDTH_ADVANCE
            } else {
                FULLWIDTH_ADVANCE
            }
        })
        .sum()
}

/// The longest prefix of `text` fitting `width`, with an ellipsis when it had
/// to cut. The full name stays available as the leaf's tooltip, so truncation
/// costs presentation and not information.
fn truncate_to(text: &str, width: i64) -> String {
    if text_width(text) <= width {
        return text.to_owned();
    }
    let budget = width - FULLWIDTH_ADVANCE;
    let mut used = 0;
    let mut kept = String::new();
    for character in text.chars() {
        let advance = if character.is_ascii() {
            HALFWIDTH_ADVANCE
        } else {
            FULLWIDTH_ADVANCE
        };
        if used + advance > budget {
            break;
        }
        used += advance;
        kept.push(character);
    }
    kept.push('…');
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section_comparison::{ComparisonCell, ComparisonColumn, ComparisonRow};

    fn row(key: usize, name: &str, counts: &[u64]) -> ComparisonRow {
        ComparisonRow {
            pattern_key: key,
            pattern_name: name.to_owned(),
            cells: counts
                .iter()
                .map(|count| ComparisonCell {
                    count: *count,
                    intensity: 0,
                    rate_tenths: 0,
                    rate_label: String::new(),
                })
                .collect(),
            displayed_total: counts.iter().sum(),
        }
    }

    fn comparison(rows: Vec<ComparisonRow>) -> SectionComparison {
        let columns = (0..rows[0].cells.len())
            .map(|index| ComparisonColumn {
                label: format!("s{index}"),
                universe: 100,
            })
            .collect();
        SectionComparison {
            columns,
            rows,
            hidden_sections: 0,
            max_count: 0,
        }
    }

    /// A link's distance as a number. The label is what the drawing carries,
    /// but comparing two of them as strings would be lexicographic and would
    /// quietly stop meaning "deeper" the moment a distance reached 10.
    fn distance(link: &DendrogramLink) -> f64 {
        link.distance_label.parse().expect("a formatted distance")
    }

    /// The name in the leaf order, so a test reads as the tree does.
    fn order(dendrogram: &SectionDendrogram) -> Vec<&str> {
        dendrogram
            .leaves
            .iter()
            .map(|leaf| leaf.name.as_str())
            .collect()
    }

    /// Two patterns spread identically are the same point, whatever their
    /// counts. This is the property that makes the tree about *distribution*
    /// rather than about frequency.
    #[test]
    fn frequency_does_not_separate_two_identical_profiles() {
        let dendrogram = SectionDendrogram::new(&comparison(vec![
            // Two orders of magnitude below `common`: frequency alone does
            // not separate profiles with the same shape.
            row(0, "rare", &[3, 3]),
            row(1, "common", &[500, 500]),
            row(2, "skewed", &[10, 0]),
        ]))
        .expect("three comparable patterns");

        let flat = dendrogram
            .links
            .iter()
            .find(|link| link.leaves == 2)
            .expect("a two-leaf join");
        assert_eq!(
            flat.distance_label, "0.00",
            "the two flat patterns join at zero however far apart their counts are"
        );
    }

    /// The property the rate normalization buys: a pattern spread in
    /// proportion to how much corpus each section holds is *flat*, and
    /// indistinguishable from any other flat pattern — however lopsided its
    /// raw counts look. Under a share-of-own-hits profile these two would sit
    /// far apart, because `wide` puts nine tenths of its hits in the big
    /// section and `narrow` puts half of its there.
    #[test]
    fn a_pattern_tracking_section_sizes_is_flat_under_the_rate() {
        let columns = vec![
            ComparisonColumn {
                label: "big".to_owned(),
                universe: 9_000,
            },
            ComparisonColumn {
                label: "small".to_owned(),
                universe: 1_000,
            },
        ];
        let dendrogram = SectionDendrogram::new(&SectionComparison {
            columns,
            rows: vec![
                // 10 per million in each section, reached from very different
                // counts because the sections are very different sizes.
                row(0, "wide", &[90, 10]),
                // 200 per million in each: a different frequency, same shape.
                row(1, "narrow", &[1_800, 200]),
                // Concentrated in the small section, which no raw share would
                // show — 100 of 190 hits sit in the big one.
                row(2, "specialist", &[90, 100]),
            ],
            hidden_sections: 0,
            max_count: 1_800,
        })
        .expect("three comparable patterns");

        let flat = dendrogram
            .links
            .iter()
            .find(|link| link.leaves == 2)
            .expect("a two-leaf join");
        assert_eq!(
            flat.distance_label, "0.00",
            "two patterns at a constant rate across both sections are one point"
        );
        let root = dendrogram.links.last().unwrap();
        assert_eq!(root.leaves, 3);
        assert!(
            distance(root) > 0.0,
            "the specialist joins them from a distance: {}",
            root.distance_label
        );
    }

    /// Disjoint support is the far end of the scale, and the scale is the one
    /// the axis is labelled with.
    #[test]
    fn two_patterns_sharing_no_section_join_at_the_maximum() {
        let dendrogram = SectionDendrogram::new(&comparison(vec![
            row(0, "left", &[10, 0]),
            row(1, "right", &[0, 10]),
        ]))
        .expect("two comparable patterns");

        assert_eq!(dendrogram.links.len(), 1);
        assert_eq!(dendrogram.links[0].distance_label, "1.00");
        assert_eq!(dendrogram.max_distance_label, "1.00");
        assert!(!dendrogram.degenerate);
    }

    /// The shape the view exists to show: two groups that each behave alike
    /// and differ from each other.
    #[test]
    fn patterns_alike_join_before_the_group_they_differ_from() {
        let dendrogram = SectionDendrogram::new(&comparison(vec![
            row(0, "methods_a", &[90, 10]),
            row(1, "results_a", &[10, 90]),
            row(2, "methods_b", &[88, 12]),
            row(3, "results_b", &[12, 88]),
        ]))
        .expect("four comparable patterns");

        let names = order(&dendrogram);
        let position = |name: &str| names.iter().position(|leaf| *leaf == name).unwrap();
        // Neighbours in the drawing, which is what "clustered" means to a
        // reader: no results pattern sits between the two methods ones.
        assert_eq!(
            position("methods_a").abs_diff(position("methods_b")),
            1,
            "the two methods patterns are adjacent leaves: {names:?}"
        );
        assert_eq!(
            position("results_a").abs_diff(position("results_b")),
            1,
            "and so are the two results patterns: {names:?}"
        );

        // Three joins: two within-group, then the one across.
        assert_eq!(dendrogram.links.len(), 3);
        let root = dendrogram.links.last().unwrap();
        assert_eq!(root.leaves, 4, "the deepest link joins everything");
        assert!(
            dendrogram
                .links
                .iter()
                .filter(|link| link.leaves == 2)
                .all(|link| distance(link) < distance(root)),
            "the across-group join is deeper than the within-group ones: {:?}",
            dendrogram
                .links
                .iter()
                .map(|link| &link.distance_label)
                .collect::<Vec<_>>()
        );
    }

    /// A dendrogram is read by following the elbows, so a link that crosses a
    /// leaf it does not join is a wrong drawing rather than an ugly one.
    #[test]
    fn every_link_spans_only_the_leaves_beneath_it() {
        let dendrogram = SectionDendrogram::new(&comparison(vec![
            row(0, "a", &[9, 1, 0]),
            row(1, "b", &[8, 2, 0]),
            row(2, "c", &[0, 1, 9]),
            row(3, "d", &[0, 2, 8]),
            row(4, "e", &[3, 4, 3]),
        ]))
        .expect("five comparable patterns");

        let rows = dendrogram.leaves.len();
        for link in &dendrogram.links {
            let ys = link
                .points
                .split(' ')
                .map(|point| point.split(',').nth(1).unwrap().parse::<i64>().unwrap())
                .collect::<Vec<_>>();
            let span = (ys.iter().max().unwrap() - ys.iter().min().unwrap()) / ROW_HEIGHT + 1;
            assert!(
                span <= i64::try_from(rows).unwrap(),
                "a link spans more rows than the tree has"
            );
            // A join of `n` leaves is drawn across a contiguous block of `n`
            // rows exactly when the leaf order has no crossings.
            assert!(
                span <= i64::try_from(link.leaves).unwrap(),
                "a join of {} leaves is drawn across {span} rows",
                link.leaves
            );
        }
    }

    #[test]
    fn identical_profiles_everywhere_are_reported_as_degenerate() {
        let dendrogram = SectionDendrogram::new(&comparison(vec![
            row(0, "a", &[4, 6]),
            row(1, "b", &[2, 3]),
            row(2, "c", &[40, 60]),
        ]))
        .expect("three comparable patterns");

        assert!(dendrogram.degenerate);
        assert!(
            dendrogram.ticks.is_empty(),
            "an axis with no range labels nothing"
        );
        assert_eq!(dendrogram.max_distance_label, "0.00");
    }

    /// The tree includes every pattern with data, beyond forty leaves.
    #[test]
    fn more_than_forty_patterns_are_all_clustered() {
        let rows = (0..43)
            .map(|index| {
                let weight = u64::try_from(index).unwrap() + 1;
                row(index, &format!("p{index:02}"), &[weight, weight * 2])
            })
            .collect::<Vec<_>>();
        let dendrogram = SectionDendrogram::new(&comparison(rows)).expect("plenty to compare");

        assert_eq!(dendrogram.leaves.len(), 43);
        assert_eq!(dendrogram.entities_without_observations, 0);
        assert!(order(&dendrogram).contains(&"p00"));
    }

    /// A single observation still defines a profile and must not be silently
    /// discarded by an unsupported minimum-count heuristic.
    #[test]
    fn a_pattern_with_one_observation_is_retained() {
        let rows = vec![
            row(0, "plenty", &[60, 10]),
            row(1, "enough", &[10, 60]),
            row(2, "one-hit", &[1, 0]),
        ];
        let dendrogram = SectionDendrogram::new(&comparison(rows)).expect("three profiles");

        assert_eq!(dendrogram.leaves.len(), 3);
        assert!(order(&dendrogram).contains(&"one-hit"));
        assert_eq!(dendrogram.entities_without_observations, 0);
    }

    /// Profiles with no observations are excluded because their shape is
    /// undefined, and the band reports that data gap instead of drawing a tree.
    #[test]
    fn no_data_profiles_are_reported_when_fewer_than_two_can_be_clustered() {
        let rows = vec![row(0, "empty-a", &[0, 0]), row(1, "empty-b", &[0, 0])];
        let dendrogram = SectionDendrogram::new(&comparison(rows)).expect("the band still renders");

        assert!(dendrogram.insufficient_evidence);
        assert!(dendrogram.leaves.is_empty());
        assert_eq!(dendrogram.entities_without_observations, 2);
    }

    /// The transpose clusters the same table down its columns. Two sections
    /// carrying the same pattern composition are one point whatever their
    /// sizes, and a section composed differently sits apart from both.
    #[test]
    fn sections_cluster_by_which_patterns_occur_in_them() {
        // `alike` and `twin` share a composition; `apart` inverts it.
        let rows = vec![row(0, "p", &[80, 40, 10]), row(1, "q", &[20, 10, 80])];
        let mut table = comparison(rows);
        table.columns[0].label = "alike".to_owned();
        table.columns[1].label = "twin".to_owned();
        table.columns[2].label = "apart".to_owned();
        let dendrogram =
            SectionDendrogram::by_pattern_profile(&table).expect("three sections cluster");

        assert_eq!(dendrogram.leaves.len(), 3);
        let closest = dendrogram
            .links
            .iter()
            .min_by(|left, right| distance(left).total_cmp(&distance(right)))
            .expect("a first join");
        assert_eq!(closest.leaves, 2, "the first join is a pair");
        assert!(
            distance(closest) <= f64::EPSILON,
            "two sections of the same composition are one point, not {}",
            closest.distance_label
        );
        let names = order(&dendrogram);
        let alike = names.iter().position(|name| *name == "alike").unwrap();
        let twin = names.iter().position(|name| *name == "twin").unwrap();
        assert_eq!(
            alike.abs_diff(twin),
            1,
            "the pair is drawn adjacent: {names:?}"
        );
    }

    /// A section holding a fixed share of every pattern has no preference to
    /// show, and the profile says so by being flat — the transpose of the
    /// pattern tree's rate normalization.
    #[test]
    fn a_section_composed_like_the_catalog_is_flat_under_the_profile() {
        let rows = vec![row(0, "p", &[900, 100]), row(1, "q", &[90, 10])];
        let table = comparison(rows);
        let profiles = table
            .columns
            .iter()
            .enumerate()
            .map(|(column, _)| {
                profile(
                    &table
                        .rows
                        .iter()
                        .map(|row| row.cells[column].count)
                        .collect::<Vec<_>>(),
                    &table
                        .rows
                        .iter()
                        .map(|row| row.displayed_total)
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        for section in &profiles {
            for share in section {
                assert!(
                    (share - 0.5).abs() < 1e-9,
                    "a proportional section is uniform, got {section:?}"
                );
            }
        }
    }

    /// The gutter follows the names, and a name it cannot fit is cut rather
    /// than drawn off the canvas — where the viewBox would clip it with no
    /// sign that anything was lost.
    #[test]
    fn a_name_too_long_for_the_gutter_is_cut_and_kept_in_full() {
        let long = "実験手法についての非常に長いパターン名前".repeat(3);
        let dendrogram = SectionDendrogram::new(&comparison(vec![
            row(0, &long, &[9, 1]),
            row(1, "b", &[1, 9]),
        ]))
        .expect("two comparable patterns");

        let leaf = dendrogram
            .leaves
            .iter()
            .find(|leaf| leaf.name == long)
            .expect("the long-named leaf");
        assert!(leaf.label.ends_with('…'), "cut: {}", leaf.label);
        assert!(leaf.label.chars().count() < long.chars().count());
        assert_eq!(leaf.name, long, "the full name survives for a tooltip");
        assert!(
            leaf.leaf_x <= MAX_LABEL_WIDTH,
            "the gutter stops widening at its own limit"
        );
        assert!(dendrogram.width > leaf.leaf_x, "the plot is still drawn");
    }

    /// A catalog of short names should not be drawn with an empty third of a
    /// canvas.
    #[test]
    fn short_names_do_not_reserve_a_gutter_they_do_not_need() {
        let dendrogram = SectionDendrogram::new(&comparison(vec![
            row(0, "a", &[9, 1]),
            row(1, "b", &[1, 9]),
        ]))
        .expect("two comparable patterns");
        assert_eq!(dendrogram.leaves[0].leaf_x, MIN_LABEL_WIDTH);
    }

    #[test]
    fn one_comparable_pattern_is_not_a_tree() {
        let comparison = SectionComparison {
            columns: vec![ComparisonColumn {
                label: "methods".to_owned(),
                universe: 10,
            }],
            rows: vec![row(0, "a", &[3])],
            hidden_sections: 0,
            max_count: 3,
        };
        assert!(SectionDendrogram::new(&comparison).is_none());
    }

    #[test]
    fn the_same_comparison_always_draws_the_same_tree() {
        let build = || {
            SectionDendrogram::new(&comparison(vec![
                row(0, "a", &[5, 5, 0]),
                row(1, "b", &[5, 5, 0]),
                row(2, "c", &[0, 5, 5]),
                row(3, "d", &[1, 1, 1]),
            ]))
            .expect("four comparable patterns")
        };
        assert_eq!(build(), build());
    }
}
