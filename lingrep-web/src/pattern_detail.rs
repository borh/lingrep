#[cfg(any(test, feature = "testing"))]
use crate::section_tally::build;
use crate::section_tally::surface::{SurfaceRefusal, SurfaceSections};
use crate::section_tally::{SectionAvailability, SectionRefusal, SectionSlot};
use crate::snapshot::PatternKey;

pub const SURFACE_OVERVIEW_LIMIT: usize = 19;

/// Above this many section rows the list offers a search box and an order
/// control.
///
/// The list itself is never truncated. A section distribution is short by
/// construction — it is bounded by the corpus's own label inventory, not by a
/// pattern's vocabulary — so hiding its tail behind a "Show remaining N" made
/// a reader click to see a chart that already fit. The tools stay, because a
/// corpus with fifty labels is still a list worth searching, and go away when
/// there is visibly nothing to search.
pub const SECTION_TOOLS_THRESHOLD: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedHitText {
    pub reference_ordinal: usize,
    pub left: String,
    pub key: String,
    pub right: String,
    pub section_labels: Box<[String]>,
    pub in_chain: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceVariation {
    pub index: usize,
    pub form: String,
    /// Sentences the pattern matched this variation in, over the whole corpus,
    /// or retained hits when the scan could not cross-tabulate the pattern.
    /// [`PatternDetailProjection::variations_are_exact`] says which.
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceTail {
    pub variation_count: usize,
    pub match_count: usize,
    /// The tail's share of the listed total, in tenths of a percent. The
    /// denominator is the sum of every variation's count, which is the one
    /// figure the reader can check against the rows above it — and, in exact
    /// mode, is deliberately not a sentence count, since one sentence can
    /// carry two variations of the same pattern.
    pub share_percentage_tenths: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionCount {
    pub index: usize,
    pub label: String,
    pub count: usize,
    /// Sentences observed under this section, whatever matched. The
    /// denominator, carried so the client can recompute the rate when a
    /// surface filter narrows the count.
    pub universe: u64,
    /// Tenths of one occurrence per million sentences of this section.
    ///
    /// A count alone is misleading here and always was: a section holding ten
    /// times the sentences collects ten times the hits from a pattern with no
    /// preference at all, so the tallest bar reports the corpus's shape rather
    /// than the pattern's. The rate is what the comparison table and the
    /// dendrogram already read, so publishing it here is also what stops the
    /// three views disagreeing.
    pub rate_tenths: u64,
    /// `rate_tenths` as the template needs it, formatted here because a
    /// template doing its own division is a second place for the unit to be
    /// got wrong.
    pub rate_label: String,
}

/// The unlabelled slot: a section row in every respect except that it is not a
/// section, which is why it is not in `sections` and carries no filter index.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NoSectionCount {
    pub count: usize,
    pub universe: u64,
    pub rate_tenths: u64,
    pub rate_label: String,
}

/// One pattern's section counts split by matched surface, projected onto the
/// rows this panel draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceSectionMatrix {
    /// `variations × (sections + 1)`, the unlabelled slot last, so a row is
    /// positionally aligned with the rendered list.
    rows: Vec<Vec<u64>>,
    width: usize,
}

impl SurfaceSectionMatrix {
    /// One variation's counts, aligned to the rendered rows.
    #[must_use]
    pub fn row(&self, variation: usize) -> Option<&[u64]> {
        self.rows.get(variation).map(Vec::as_slice)
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    /// The whole matrix as the JSON the client reads, `{"<variation>": [...]}`.
    ///
    /// Built here rather than in the template: a template assembling JSON by
    /// hand is one missed comma away from a parse error the browser reports
    /// as nothing at all.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut json = String::from("{");
        for (variation, row) in self.rows.iter().enumerate() {
            if variation > 0 {
                json.push(',');
            }
            json.push('"');
            json.push_str(&variation.to_string());
            json.push_str("\":[");
            for (position, count) in row.iter().enumerate() {
                if position > 0 {
                    json.push(',');
                }
                json.push_str(&count.to_string());
            }
            json.push(']');
        }
        json.push('}');
        json
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedHit {
    pub has_overlaps: bool,
    pub reference_ordinal: usize,
    pub variation_index: usize,
    pub left: String,
    pub key: String,
    pub right: String,
    pub section_labels: Box<[String]>,
    pub section_indices: Box<[usize]>,
    pub in_chain: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternDetailProjection {
    pub overlap_hits: usize,
    pub overlap_names: Vec<String>,
    pub variations: Vec<SurfaceVariation>,
    /// Whether `variations` counts the corpus or the retained sample.
    ///
    /// The section distribution beside the variation list is corpus-exact, so
    /// a sampled variation list would put an inexact menu in front of exact
    /// rows. It is `true` whenever the scan
    /// cross-tabulated this pattern.
    pub variations_are_exact: bool,
    pub overview_len: usize,
    pub max_count: usize,
    pub tail: Option<SurfaceTail>,
    pub hits: Vec<ProjectedHit>,
    /// Every section this pattern occurs in, in rank order. Not truncated;
    /// see [`SECTION_TOOLS_THRESHOLD`].
    pub sections: Vec<SectionCount>,
    pub no_section: NoSectionCount,
    /// The largest rate among the drawn rows, which is what the bars are
    /// scaled against.
    pub max_section_rate_tenths: u64,
    /// The largest count among them. Not the bar's scale any more; kept
    /// because the client's re-render reports counts.
    pub max_section_count: usize,
    /// Section counts split by the matched surface, or `None` when this
    /// pattern has none.
    pub surface_sections: Option<SurfaceSectionMatrix>,
    /// Why this pattern has no cross-tabulation, when it has none and the
    /// section projection itself was available.
    pub surface_sections_refused: Option<SurfaceRefusal>,
    /// `Some` exactly when the corpus's label vocabulary would not fit the
    /// retained ceiling, in which case `sections` is empty: a refused
    /// projection lists nothing rather than falling back to the sample.
    pub section_unavailable: Option<SectionRefusal>,
    /// How many section labels the *corpus* carries, whatever this pattern
    /// matched.
    ///
    /// `sections` being empty has two unrelated causes, and a reader must be
    /// able to tell them apart: a corpus that records no section labels at all
    /// has nothing to distribute over, while a corpus that records several and
    /// gave this pattern none is a fact about the pattern. Rendering both as an
    /// absent panel is how the feature reads as broken.
    /// Counted from the tally's own slots rather than from `sections`, which is
    /// already filtered to this pattern's nonzero labels.
    pub corpus_section_labels: usize,
    /// Whether any retained hit carries a section label.
    ///
    /// Distinct from `!sections.is_empty()`, which describes the *corpus*. The
    /// KWIC section column shows each retained row's own labels,
    /// so it is this that decides whether the column exists: exact counts can
    /// be nonzero while every retained hit happens to be unlabelled, and a
    /// refused projection empties `sections` while the rows keep their labels.
    pub has_hit_sections: bool,
}

impl PatternDetailProjection {
    pub fn overview(&self) -> &[SurfaceVariation] {
        &self.variations[..self.overview_len]
    }

    /// Whether the section list is long enough to be worth searching.
    pub fn section_tools(&self) -> bool {
        self.sections.len() > SECTION_TOOLS_THRESHOLD
    }

    /// Projects one pattern's retained hits and the run's exact section counts.
    ///
    /// KWIC rows come from the retained sample. The section distribution and
    /// the surface variation list both come from `sections`, which is
    /// corpus-exact: a row's count can exceed the number of retained rows its
    /// filter selects, and the filter status line reports the latter.
    pub fn new(
        hits: Vec<RetainedHitText>,
        sections: &SectionAvailability,
        pattern: PatternKey,
    ) -> Self {
        use std::collections::HashMap;

        // Bound before `sections` is shadowed by the projected rows below, so
        // the cross-tabulation can still reach the tally it came from.
        let availability = sections;

        let mut sampled = HashMap::<String, usize>::new();
        for hit in &hits {
            *sampled.entry(hit.key.clone()).or_default() += 1;
        }
        // Corpus-exact when the scan cross-tabulated this pattern. Ranking the
        // menu on a sample while the section rows beside it are exact would,
        // under a low retention cap, make the two halves of one panel describe
        // different populations.
        let counted = match availability {
            SectionAvailability::Available(tally) => match tally.surfaces(pattern) {
                counted @ SurfaceSections::Counted { .. } => {
                    let list = counted
                        .variations()
                        .map(|(form, total)| (form.to_string(), saturate(total)))
                        .collect::<Vec<_>>();
                    // Every retained hit must have a row to filter by, so a
                    // surface the cross-tabulation does not hold falsifies the
                    // whole list rather than one entry: the two are fed from
                    // one sink, and a disagreement means the exact figures are
                    // about something else.
                    sampled
                        .keys()
                        .all(|form| list.iter().any(|(held, _)| held == form))
                        .then_some(list)
                }
                SurfaceSections::Refused(_) => None,
            },
            SectionAvailability::VocabularyTooLarge(_) => None,
        };
        let variations_are_exact = counted.is_some();
        let mut variations =
            counted.unwrap_or_else(|| sampled.iter().map(|(f, c)| (f.clone(), *c)).collect());
        variations.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        let variation_ids = variations
            .iter()
            .enumerate()
            .map(|(index, (form, _))| (form.clone(), index))
            .collect::<HashMap<_, _>>();
        let variations = variations
            .into_iter()
            .enumerate()
            .map(|(index, (form, count))| SurfaceVariation { index, form, count })
            .collect::<Vec<_>>();
        let overview_len = variations.len().min(SURFACE_OVERVIEW_LIMIT);
        // The listed total, not the retained one: in exact mode the rows count
        // corpus sentences, so dividing by the sample size would report a
        // percentage of the wrong population — occasionally above 100.
        let listed_total = variations
            .iter()
            .map(|entry| entry.count)
            .fold(0usize, usize::saturating_add);
        let tail = (variations.len() > overview_len).then(|| SurfaceTail {
            variation_count: variations.len() - overview_len,
            match_count: variations[overview_len..]
                .iter()
                .map(|entry| entry.count)
                .sum(),
            share_percentage_tenths: 0,
        });
        let tail = tail.map(|mut tail| {
            tail.share_percentage_tenths = (tail.match_count as u128 * 1_000
                + listed_total as u128 / 2)
                .checked_div(listed_total as u128)
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(0);
            tail
        });
        let max_count = variations.first().map_or(0, |entry| entry.count);

        let has_hit_sections = hits.iter().any(|hit| !hit.section_labels.is_empty());

        let corpus_section_labels = match sections {
            SectionAvailability::VocabularyTooLarge(refusal) => refusal.labels,
            SectionAvailability::Available(tally) => tally
                .slots()
                .iter()
                .filter(|descriptor| matches!(descriptor, SectionSlot::Label(_)))
                .count(),
        };

        let section_unavailable = match sections {
            SectionAvailability::VocabularyTooLarge(refusal) => Some(refusal.clone()),
            SectionAvailability::Available(_) => None,
        };
        // `(label, slot, count, universe)`. The slot is carried through the
        // ordering because the surface cross-tabulation is indexed by it and
        // the display order is not the tally's.
        let (ordered_sections, no_section) = match sections {
            // A refused projection lists nothing: falling back to the sample
            // would put a biased distribution behind an exact contract.
            SectionAvailability::VocabularyTooLarge(_) => (Vec::new(), NoSectionCount::default()),
            SectionAvailability::Available(tally) => {
                let counted = tally
                    .slots()
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, descriptor)| match descriptor {
                        SectionSlot::Label(label) => Some((
                            label.clone(),
                            slot,
                            saturate(tally.count(pattern, slot)),
                            tally.universe(slot),
                        )),
                        SectionSlot::None => None,
                    })
                    // A pattern's list holds only labels its hits touch; the
                    // tally holds the whole corpus vocabulary.
                    .filter(|(_, _, count, _)| *count > 0)
                    .collect::<Vec<_>>();
                let none = tally
                    .slots()
                    .iter()
                    .position(|descriptor| matches!(descriptor, SectionSlot::None))
                    .map(|slot| {
                        let count = saturate(tally.count(pattern, slot));
                        let universe = tally.universe(slot);
                        let rate_tenths = rate_tenths(count, universe);
                        NoSectionCount {
                            count,
                            universe,
                            rate_tenths,
                            rate_label: rate_label(rate_tenths),
                        }
                    })
                    .unwrap_or_default();
                (counted, none)
            }
        };
        // By rate, not by count: the panel answers "where does this pattern
        // occur", and a count ranks the sections by how much corpus each holds
        // as much as by anything about the pattern.
        let mut ordered_sections = ordered_sections
            .into_iter()
            .map(|(label, slot, count, universe)| {
                (label, slot, count, universe, rate_tenths(count, universe))
            })
            .collect::<Vec<_>>();
        ordered_sections
            .sort_by(|left, right| right.4.cmp(&left.4).then_with(|| left.0.cmp(&right.0)));
        let section_ids = ordered_sections
            .iter()
            .enumerate()
            .map(|(index, (label, ..))| (label.clone(), index))
            .collect::<HashMap<_, _>>();
        let section_slots = ordered_sections
            .iter()
            .map(|(_, slot, ..)| *slot)
            .collect::<Vec<_>>();
        let sections = ordered_sections
            .into_iter()
            .enumerate()
            .map(
                |(index, (label, _, count, universe, rate_tenths))| SectionCount {
                    index,
                    label,
                    count,
                    universe,
                    rate_tenths,
                    rate_label: rate_label(rate_tenths),
                },
            )
            .collect::<Vec<_>>();
        let max_section_rate_tenths = sections
            .iter()
            .map(|section| section.rate_tenths)
            .max()
            .unwrap_or(0)
            .max(no_section.rate_tenths);
        let max_section_count = sections
            .iter()
            .map(|section| section.count)
            .max()
            .unwrap_or(0)
            .max(no_section.count);

        // The cross-tabulation, projected onto the rows above: one row per
        // variation, positions aligned to `sections` with the unlabelled slot
        // last. In exact mode the variations *are* the cross-tabulation's own
        // surfaces, and in sampled mode every retained hit's surface was
        // counted, so an admitted pattern always has a row for every
        // variation — a zero here means zero rather than unknown.
        let none_slot = match availability {
            SectionAvailability::Available(tally) => tally
                .slots()
                .iter()
                .position(|descriptor| matches!(descriptor, SectionSlot::None)),
            SectionAvailability::VocabularyTooLarge(_) => None,
        };
        let (surface_sections, surface_sections_refused) = match availability {
            SectionAvailability::VocabularyTooLarge(_) => (None, None),
            SectionAvailability::Available(tally) => match tally.surfaces(pattern) {
                SurfaceSections::Refused(refusal) => (None, Some(refusal.clone())),
                counted @ SurfaceSections::Counted { .. } => {
                    let width = section_slots.len() + usize::from(none_slot.is_some());
                    let rows = variations
                        .iter()
                        .map(|variation| {
                            section_slots
                                .iter()
                                .copied()
                                .chain(none_slot)
                                .map(|slot| counted.count(&variation.form, slot).unwrap_or(0))
                                .collect::<Vec<_>>()
                        })
                        .collect::<Vec<_>>();
                    (Some(SurfaceSectionMatrix { rows, width }), None)
                }
            },
        };

        let hits = hits
            .into_iter()
            .map(|hit| {
                // Membership ids for the section filter. A label with no
                // projected row — every label when the projection is refused —
                // contributes none, rather than panicking on a missing key.
                let section_indices = hit
                    .section_labels
                    .iter()
                    .filter_map(|label| section_ids.get(label).copied())
                    .collect::<Box<[usize]>>();
                ProjectedHit {
                    has_overlaps: false,
                    reference_ordinal: hit.reference_ordinal,
                    variation_index: variation_ids[&hit.key],
                    left: hit.left,
                    key: hit.key,
                    right: hit.right,
                    section_labels: hit.section_labels,
                    section_indices,
                    in_chain: hit.in_chain,
                }
            })
            .collect();
        Self {
            overlap_hits: 0,
            overlap_names: Vec::new(),
            variations,
            variations_are_exact,
            overview_len,
            max_count,
            tail,
            hits,
            sections,
            no_section,
            max_section_rate_tenths,
            max_section_count,
            surface_sections,
            surface_sections_refused,
            section_unavailable,
            corpus_section_labels,
            has_hit_sections,
        }
    }
}

/// Exact counts are `u64`; the template's arithmetic is `usize`. Saturating is
/// right here: a count past `usize::MAX` cannot occur on any corpus this
/// supports, and clamping keeps a display path from panicking if one did.
fn saturate(count: u64) -> usize {
    usize::try_from(count).unwrap_or(usize::MAX)
}

/// Tenths of one occurrence per million sentences, rounded half up.
///
/// Zero when the section holds no sentences, which cannot happen for a slot
/// this pattern matched in and is therefore defensive rather than expected.
fn rate_tenths(count: usize, universe: u64) -> u64 {
    if universe == 0 {
        return 0;
    }
    let numerator = count as u128 * 10_000_000 + u128::from(universe) / 2;
    u64::try_from(numerator / u128::from(universe)).unwrap_or(u64::MAX)
}

/// `rate_tenths` as one decimal.
fn rate_label(tenths: u64) -> String {
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// A tally that agrees exactly with a retained sample: each hit contributes one
/// sentence under each of its labels, or to the `None` slot when it has none.
///
/// Test-only. It exists so the render and projection tests that predate exact
/// counting keep asserting what they always asserted — the sample's own
/// distribution — instead of being rewritten around a fixture tally.
#[cfg(any(test, feature = "testing"))]
#[must_use]
pub fn sample_sections(hits: &[RetainedHitText]) -> SectionAvailability {
    use std::collections::BTreeSet;

    use crate::section_tally::{SpanEvent, SpanEvents};

    let labels = hits
        .iter()
        .flat_map(|hit| hit.section_labels.iter().cloned())
        .collect::<BTreeSet<_>>();
    let vocabulary = labels
        .iter()
        .map(|label| lingrep_corpus::SectionCoverage {
            label: label.clone(),
            coverage: 1,
        })
        .collect::<Vec<_>>();
    let slot_of = labels
        .iter()
        .enumerate()
        .map(|(slot, label)| (label.clone(), slot as u32))
        .collect::<std::collections::HashMap<_, _>>();
    // One sentence per hit, so a hit's labels are exactly the labels active at
    // its ordinal. Spans are one sentence wide, which keeps starts
    // non-decreasing and never leaves two spans open for one slot.
    let mut events = Vec::new();
    for (ordinal, hit) in hits.iter().enumerate() {
        let mut slots = hit
            .section_labels
            .iter()
            .filter_map(|label| slot_of.get(label).copied())
            .collect::<Vec<_>>();
        slots.sort_unstable();
        slots.dedup();
        for slot in slots {
            events.push(SpanEvent {
                slot,
                first: ordinal as u64,
                end: ordinal as u64 + 1,
            });
        }
    }
    let mut builder = build(&vocabulary, 1, SpanEvents::from_events(events))
        .expect("a sample's own vocabulary fits the retained ceiling");
    let key = PatternKey::new(0).expect("pattern zero exists");
    for (ordinal, hit) in hits.iter().enumerate() {
        // The hit's own KWIC key is its surface, so the sample tally carries a
        // surface axis that agrees with the variation list projected beside it.
        builder
            .observe(ordinal as u64, &[key], &[(key, hit.key.clone())])
            .expect("a sample-derived stream is ordered and non-overlapping");
    }
    SectionAvailability::Available(
        builder
            .finish()
            .expect("a sample-derived stream ends cleanly"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sample-equivalent projection every pre-existing test asserts about:
    /// the tally agrees with the retained hits, so what these pin is the
    /// projection's shape, not the change in where its counts come from.
    fn project(hits: Vec<RetainedHitText>) -> PatternDetailProjection {
        let sections = sample_sections(&hits);
        PatternDetailProjection::new(hits, &sections, PatternKey::new(0).unwrap())
    }

    fn hit(ordinal: usize, key: impl Into<String>) -> RetainedHitText {
        RetainedHitText {
            reference_ordinal: ordinal,
            left: format!("left-{ordinal}"),
            key: key.into(),
            right: format!("right-{ordinal}"),
            section_labels: Box::new([]),
            in_chain: false,
        }
    }

    #[test]
    fn threads_section_labels_into_projected_hits() {
        let mut labelled = hit(0, "乙");
        labelled.section_labels = Box::new(["methods".to_string()]);
        let projected = project(vec![labelled]);
        assert_eq!(&*projected.hits[0].section_labels, &["methods".to_string()]);
    }

    #[test]
    fn preserves_retention_order_and_assigns_sorted_variation_identity() {
        let projected = project(vec![hit(0, "乙"), hit(1, "甲"), hit(2, "乙")]);
        assert_eq!(
            projected
                .variations
                .iter()
                .map(|variation| (&*variation.form, variation.count))
                .collect::<Vec<_>>(),
            vec![("乙", 2), ("甲", 1)]
        );
        assert_eq!(projected.max_count, 2);
        assert_eq!(
            projected
                .hits
                .iter()
                .map(|hit| (hit.reference_ordinal, hit.variation_index))
                .collect::<Vec<_>>(),
            vec![(0, 0), (1, 1), (2, 0)]
        );
    }

    #[test]
    fn caps_overview_and_aggregates_hundreds_of_singletons() {
        let projected = project(
            (0..250)
                .map(|index| hit(index, format!("表層{index:03}")))
                .collect(),
        );
        assert_eq!(projected.overview_len, 19);
        assert_eq!(
            projected.tail,
            Some(SurfaceTail {
                variation_count: 231,
                match_count: 231,
                share_percentage_tenths: 924,
            })
        );
        assert_eq!(projected.variations.len(), 250);
    }

    fn hit_in(ordinal: usize, key: &str, sections: &[&str]) -> RetainedHitText {
        let mut value = hit(ordinal, key);
        value.section_labels = sections.iter().map(|s| (*s).to_string()).collect();
        value
    }

    // For generated (non-literal) labels: takes owned Strings so there is no
    // `&[&format!(...)]` element-coercion to reason about.
    fn hit_sections(ordinal: usize, labels: Vec<String>) -> RetainedHitText {
        let mut value = hit(ordinal, "k");
        value.section_labels = labels.into();
        value
    }

    #[test]
    fn counts_section_membership_and_separates_the_no_section_bucket() {
        // ordinal 0 is in methods+results, ordinal 1 in methods, ordinal 2 in none.
        let projection = project(vec![
            hit_in(0, "α", &["methods", "results"]),
            hit_in(1, "β", &["methods"]),
            hit(2, "γ"),
        ]);
        let by_label = |label: &str| {
            projection
                .sections
                .iter()
                .find(|s| s.label == label)
                .map(|s| s.count)
        };
        assert_eq!(by_label("methods"), Some(2));
        assert_eq!(by_label("results"), Some(1));
        assert_eq!(projection.no_section.count, 1);
    }

    #[test]
    fn orders_sections_by_count_then_label_and_assigns_stable_indices() {
        let projection = project(vec![
            hit_in(0, "α", &["b"]),
            hit_in(1, "β", &["a"]),
            hit_in(2, "γ", &["a"]),
        ]);
        // "a" has 2, "b" has 1 → a before b; indices follow display order.
        assert_eq!(
            projection
                .sections
                .iter()
                .map(|s| (s.index, s.label.as_str(), s.count))
                .collect::<Vec<_>>(),
            vec![(0, "a", 2), (1, "b", 1)]
        );
    }

    #[test]
    fn projects_each_hits_section_indices_over_the_whole_list() {
        // Twenty sections. The lowest-ranked one is still drawn, and its hit still carries its
        // full-list index.
        let mut hits = Vec::new();
        for index in 0..19 {
            hits.push(hit_sections(index * 2, vec![format!("s{index:02}")]));
            hits.push(hit_sections(index * 2 + 1, vec![format!("s{index:02}")]));
        }
        hits.push(hit_sections(38, vec!["rarest".to_string()]));
        let projection = project(hits);
        assert_eq!(projection.sections.len(), 20);
        let rarest = projection
            .sections
            .iter()
            .find(|s| s.label == "rarest")
            .expect("every section is drawn");
        let rare_hit = projection
            .hits
            .iter()
            .find(|h| h.reference_ordinal == 38)
            .unwrap();
        assert_eq!(&*rare_hit.section_indices, &[rarest.index]);
    }

    #[test]
    fn every_section_is_drawn_and_the_tools_follow_the_list_length() {
        // A section distribution is bounded by the corpus's label inventory,
        // so it is short by construction and hiding its tail behind a button
        // made a reader click to see a chart that already fitted.
        let mut hits = Vec::new();
        for index in 0..25 {
            hits.push(hit_sections(index, vec![format!("s{index:02}")]));
        }
        let projection = project(hits);
        assert_eq!(projection.sections.len(), 25);
        assert!(projection.section_tools());

        let short = project(vec![hit_in(0, "α", &["only"])]);
        assert_eq!(short.sections.len(), 1);
        assert!(
            !short.section_tools(),
            "one row is not a list worth searching"
        );
    }

    #[test]
    fn max_section_count_spans_sections_and_no_section() {
        let projection = project(vec![
            hit_in(0, "α", &["methods"]),
            hit_in(1, "β", &["methods"]),
            hit_in(2, "γ", &["methods"]),
            hit(3, "δ"),
            hit(4, "ε"),
        ]);
        // methods = 3, no_section = 2 → max = 3.
        assert_eq!(projection.max_section_count, 3);
    }

    #[test]
    fn max_section_count_spans_every_drawn_row() {
        // 25 singleton sections: every individual count is 1, and none is
        // hidden, so the scale is 1 rather than anything the list length
        // implies.
        let hits = (0..25)
            .map(|index| hit_sections(index, vec![format!("s{index:02}")]))
            .collect();
        let projection = project(hits);
        assert_eq!(projection.sections.len(), 25);
        assert_eq!(projection.max_section_count, 1);
    }

    /// The state the KWIC section column exists for, which is not the state the
    /// distribution block exists for: exact counts can be nonzero while every
    /// retained hit happens to be unlabelled, and a refused projection empties
    /// `sections` while the rows keep their labels.
    #[test]
    fn the_kwic_section_column_follows_the_retained_hits_not_the_corpus() {
        let sections = tally_with(&["methods"], &[(0, 4_000)]);
        let unlabelled =
            PatternDetailProjection::new(vec![hit(0, "α")], &sections, PatternKey::new(0).unwrap());
        assert!(
            !unlabelled.sections.is_empty(),
            "the corpus has the section"
        );
        assert!(
            !unlabelled.has_hit_sections,
            "but no retained row has a label to show in the column"
        );

        let refused = PatternDetailProjection::new(
            vec![hit_in(0, "α", &["methods"])],
            &SectionAvailability::VocabularyTooLarge(SectionRefusal {
                labels: 9,
                patterns: 9,
            }),
            PatternKey::new(0).unwrap(),
        );
        assert!(refused.sections.is_empty());
        assert!(
            refused.has_hit_sections,
            "a refused distribution does not erase the rows' own labels"
        );
    }

    #[test]
    fn max_surface_count_excludes_a_dominant_tail() {
        let hits = (0..25)
            .map(|index| hit(index, format!("s{index:02}")))
            .collect();
        let projection = project(hits);
        assert_eq!(projection.tail.as_ref().unwrap().match_count, 6);
        assert_eq!(projection.max_count, 1);
    }

    /// A tally over `labels` in which pattern 0 matched `count` sentences under
    /// slot `slot`. Each requested count is reached by observing that many
    /// sentences inside a span for its slot; spans are laid out end to end so
    /// no two are ever active at once.
    fn tally_with(labels: &[&str], counts: &[(u32, u64)]) -> SectionAvailability {
        use crate::section_tally::{SpanEvent, SpanEvents};

        let coverage = labels
            .iter()
            .map(|label| lingrep_corpus::SectionCoverage {
                label: (*label).to_string(),
                coverage: 1,
            })
            .collect::<Vec<_>>();
        let mut events = Vec::new();
        let mut cursor = 0u64;
        for (slot, count) in counts {
            events.push(SpanEvent {
                slot: *slot,
                first: cursor,
                end: cursor + count,
            });
            cursor += count;
        }
        let mut builder = build(&coverage, 1, SpanEvents::from_events(events))
            .expect("the fixture vocabulary fits");
        for ordinal in 0..cursor {
            builder
                .observe(ordinal, &[PatternKey::new(0).unwrap()], &[])
                .unwrap();
        }
        SectionAvailability::Available(builder.finish().expect("the fixture stream ends cleanly"))
    }

    #[test]
    fn a_pattern_lists_only_the_labels_its_hits_touch() {
        // Corpus vocabulary of three; this pattern occurs under one.
        let sections = tally_with(&["methods", "results", "discussion"], &[(0, 7)]);
        let projection = PatternDetailProjection::new(
            vec![hit_in(0, "α", &["methods"])],
            &sections,
            PatternKey::new(0).unwrap(),
        );
        assert!(projection.section_unavailable.is_none());
        assert_eq!(
            projection.sections.len(),
            1,
            "zero-count slots are filtered, not listed as empty rows"
        );
        assert_eq!(projection.sections[0].label, "methods");
        assert_eq!(
            projection.sections[0].count, 7,
            "the corpus-exact count, not the one-hit sample"
        );
    }

    /// A tally where each slot holds `universe` sentences and the pattern
    /// matches the first `matched` of them, as one surface per entry. The
    /// counts and the denominators can then be set independently, which
    /// `tally_with` cannot do.
    fn tally_rates(entries: &[(&str, u64, u64, &str)]) -> SectionAvailability {
        use crate::section_tally::{SpanEvent, SpanEvents};

        let coverage = entries
            .iter()
            .map(|(label, ..)| lingrep_corpus::SectionCoverage {
                label: (*label).to_string(),
                coverage: 1,
            })
            .collect::<Vec<_>>();
        let mut events = Vec::new();
        let mut cursor = 0u64;
        for (slot, (_, universe, _, _)) in entries.iter().enumerate() {
            events.push(SpanEvent {
                slot: slot as u32,
                first: cursor,
                end: cursor + universe,
            });
            cursor += universe;
        }
        let mut builder = build(&coverage, 1, SpanEvents::from_events(events))
            .expect("the fixture vocabulary fits");
        let key = PatternKey::new(0).unwrap();
        let mut ordinal = 0u64;
        for (_, universe, matched, surface) in entries {
            let matching = [key];
            let staged = [(key, (*surface).to_string())];
            for index in 0..*universe {
                let hit = index < *matched;
                let keys: &[PatternKey] = if hit { &matching } else { &[] };
                let surfaces: &[(PatternKey, String)] = if hit { &staged } else { &[] };
                builder.observe(ordinal, keys, surfaces).unwrap();
                ordinal += 1;
            }
        }
        SectionAvailability::Available(builder.finish().expect("the fixture stream ends cleanly"))
    }

    /// Ordering is by rate, and this fixture is why. `results` holds three
    /// times the sentences of `methods`, so it collects three times the hits
    /// from a pattern matching every sentence of both. By count it would lead;
    /// by rate the two are identical and the tie falls to the label.
    #[test]
    fn sections_are_ordered_by_rate_then_label() {
        let sections = tally_with(&["methods", "results"], &[(0, 3), (1, 9)]);
        let projection = PatternDetailProjection::new(
            vec![hit_in(0, "α", &["methods"])],
            &sections,
            PatternKey::new(0).unwrap(),
        );
        assert_eq!(
            projection
                .sections
                .iter()
                .map(|s| s.label.as_str())
                .collect::<Vec<_>>(),
            vec!["methods", "results"],
        );
        assert_eq!(projection.sections[0].count, 3);
        assert_eq!(projection.sections[1].count, 9);
        for section in &projection.sections {
            assert_eq!(section.rate_label, "1000000.0", "every sentence matched");
        }
    }

    /// The count order and the rate order genuinely disagree here, which is
    /// the misleading reading the rate exists to remove: `results` has four
    /// times the hits and a quarter of the rate.
    #[test]
    fn a_bigger_section_with_more_hits_can_still_have_the_lower_rate() {
        let sections = tally_rates(&[("methods", 100, 20, "α"), ("results", 4_000, 80, "α")]);
        let projection = PatternDetailProjection::new(
            vec![hit_in(0, "α", &["methods"])],
            &sections,
            PatternKey::new(0).unwrap(),
        );
        let rows = &projection.sections;
        assert_eq!(rows[0].label, "methods");
        assert_eq!(rows[0].count, 20);
        assert_eq!(rows[0].rate_label, "200000.0");
        assert_eq!(rows[1].label, "results");
        assert_eq!(rows[1].count, 80, "four times the hits");
        assert_eq!(rows[1].rate_label, "20000.0", "and a tenth of the rate");
        assert_eq!(
            projection.max_section_rate_tenths, rows[0].rate_tenths,
            "the bars are scaled against the largest rate, not the largest count"
        );
    }

    /// The cross-tabulation is positionally aligned with the rendered rows,
    /// unlabelled slot last, and its rows sum back to the pattern's own.
    /// A one-section tally in which the pattern matched each surface in the
    /// stated number of sentences. Retained hits are supplied separately, so a
    /// test can make the sample disagree with the corpus the way a retention
    /// cap does.
    fn tally_surfaces(entries: &[(&str, u64)]) -> SectionAvailability {
        use crate::section_tally::{SpanEvent, SpanEvents};

        let total = entries.iter().map(|(_, count)| count).sum::<u64>();
        let coverage = vec![lingrep_corpus::SectionCoverage {
            label: "methods".to_string(),
            coverage: 1,
        }];
        let events = vec![SpanEvent {
            slot: 0,
            first: 0,
            end: total,
        }];
        let mut builder = build(&coverage, 1, SpanEvents::from_events(events))
            .expect("a one-label vocabulary fits");
        let key = PatternKey::new(0).unwrap();
        let mut ordinal = 0;
        for (surface, count) in entries {
            for _ in 0..*count {
                let staged = [(key, (*surface).to_string())];
                builder.observe(ordinal, &[key], &staged).unwrap();
                ordinal += 1;
            }
        }
        SectionAvailability::Available(builder.finish().expect("the stream ends cleanly"))
    }

    /// Under a retention cap the sample is a small slice of
    /// the corpus, so a list counted from it ranks the menu by something other
    /// than what the exact section rows beside it report.
    #[test]
    fn the_variation_list_counts_the_corpus_rather_than_the_retained_sample() {
        let sections = tally_surfaces(&[("控えめ", 20), ("目立つ", 4)]);
        // The sample inverts the corpus order: one hit of the frequent
        // variation survived retention and three of the rare one did.
        let hits = vec![
            hit_in(0, "控えめ", &["methods"]),
            hit_in(1, "目立つ", &["methods"]),
            hit_in(2, "目立つ", &["methods"]),
            hit_in(3, "目立つ", &["methods"]),
        ];
        let projection = PatternDetailProjection::new(hits, &sections, PatternKey::new(0).unwrap());
        assert!(projection.variations_are_exact);
        assert_eq!(
            projection
                .variations
                .iter()
                .map(|variation| (variation.form.as_str(), variation.count))
                .collect::<Vec<_>>(),
            vec![("控えめ", 20), ("目立つ", 4)],
            "corpus sentences, in corpus order"
        );
        assert_eq!(
            projection.max_count, 20,
            "the bars are scaled against the corpus maximum"
        );
        assert_eq!(
            projection
                .hits
                .iter()
                .map(|hit| hit.variation_index)
                .collect::<Vec<_>>(),
            vec![0, 1, 1, 1],
            "every retained hit still selects its own row"
        );
    }

    /// A refused cross-tabulation has no corpus figures to offer, so the list
    /// falls back to the sample and the panel says which it is showing.
    #[test]
    fn a_pattern_whose_surfaces_were_refused_falls_back_to_the_sample() {
        use crate::section_tally::SURFACE_VOCABULARY_LIMIT;

        let surfaces = (0..=SURFACE_VOCABULARY_LIMIT)
            .map(|index| format!("形{index:03}"))
            .collect::<Vec<_>>();
        let entries = surfaces
            .iter()
            .map(|surface| (surface.as_str(), 1))
            .collect::<Vec<_>>();
        let sections = tally_surfaces(&entries);
        let hits = vec![hit_in(0, "形000", &["methods"])];
        let projection = PatternDetailProjection::new(hits, &sections, PatternKey::new(0).unwrap());
        assert!(!projection.variations_are_exact);
        assert_eq!(
            projection.variations.len(),
            1,
            "the retained sample's own list"
        );
        assert_eq!(projection.variations[0].count, 1);
    }

    /// The list and the cross-tabulation are fed from one sink, so a retained
    /// surface the cross-tabulation does not hold means its figures are about
    /// something else. Falling back whole beats drawing that hit as a zero row.
    #[test]
    fn a_retained_surface_missing_from_the_cross_tabulation_falls_back_whole() {
        let sections = tally_surfaces(&[("控えめ", 20)]);
        let projection = PatternDetailProjection::new(
            vec![hit_in(0, "見えない", &["methods"])],
            &sections,
            PatternKey::new(0).unwrap(),
        );
        assert!(!projection.variations_are_exact);
        assert_eq!(
            projection
                .variations
                .iter()
                .map(|variation| variation.form.as_str())
                .collect::<Vec<_>>(),
            vec!["見えない"],
            "the sample's list, not the corpus's plus a fabricated row"
        );
    }

    #[test]
    fn the_surface_matrix_is_aligned_with_the_projected_rows() {
        let sections = tally_rates(&[("methods", 100, 20, "した"), ("results", 100, 5, "する")]);
        let projection = PatternDetailProjection::new(
            vec![
                hit_in(0, "した", &["methods"]),
                hit_in(1, "する", &["results"]),
            ],
            &sections,
            PatternKey::new(0).unwrap(),
        );
        let matrix = projection
            .surface_sections
            .as_ref()
            .expect("a two-surface pattern is well within the vocabulary limit");
        assert_eq!(matrix.width(), projection.sections.len() + 1);
        let position = |label: &str| {
            projection
                .sections
                .iter()
                .position(|section| section.label == label)
                .expect("the label is projected")
        };
        let variation = |form: &str| {
            projection
                .variations
                .iter()
                .position(|entry| entry.form == form)
                .expect("the variation is projected")
        };
        let した = matrix.row(variation("した")).expect("a row per variation");
        assert_eq!(した[position("methods")], 20);
        assert_eq!(した[position("results")], 0);
        let する = matrix.row(variation("する")).expect("a row per variation");
        assert_eq!(する[position("methods")], 0);
        assert_eq!(する[position("results")], 5);
        // The unlabelled slot is last and empty here: every fixture sentence
        // sat inside a span.
        assert_eq!(した[matrix.width() - 1], 0);
        assert_eq!(projection.no_section.count, 0);
        assert!(projection.surface_sections_refused.is_none());
    }

    /// Every variation the pane can offer came from a retained hit, and every
    /// retained hit's surface was counted during the scan, so an admitted
    /// pattern has a row for each — a zero in the matrix is a zero, never an
    /// unknown.
    #[test]
    fn every_offered_variation_has_a_matrix_row() {
        let sections = tally_rates(&[("methods", 40, 10, "α"), ("results", 40, 10, "β")]);
        let projection = PatternDetailProjection::new(
            vec![hit_in(0, "α", &["methods"]), hit_in(1, "β", &["results"])],
            &sections,
            PatternKey::new(0).unwrap(),
        );
        let matrix = projection.surface_sections.as_ref().expect("counted");
        for variation in &projection.variations {
            let row = matrix
                .row(variation.index)
                .unwrap_or_else(|| panic!("no row for {}", variation.form));
            assert!(
                row.iter().sum::<u64>() > 0,
                "{} was counted somewhere",
                variation.form
            );
        }
    }

    /// A refused pattern publishes the reason and no matrix. Zeros would be a
    /// worse answer than silence: they read as "this surface occurs nowhere".
    #[test]
    fn a_pattern_past_the_surface_limit_reaches_the_projection_as_a_reason() {
        use crate::section_tally::SURFACE_VOCABULARY_LIMIT;

        let entries = (0..=SURFACE_VOCABULARY_LIMIT)
            .map(|index| format!("s{index}"))
            .collect::<Vec<_>>();
        let fixture = entries
            .iter()
            .enumerate()
            .map(|(index, surface)| {
                (
                    if index == 0 { "methods" } else { "results" },
                    2u64,
                    1u64,
                    surface.as_str(),
                )
            })
            .collect::<Vec<_>>();
        // One label per entry would blow the section vocabulary, so collapse
        // the labels and keep the surfaces distinct.
        let sections = tally_rates(&fixture);
        let projection = PatternDetailProjection::new(
            vec![hit_in(0, "s0", &["methods"])],
            &sections,
            PatternKey::new(0).unwrap(),
        );
        assert!(projection.surface_sections.is_none());
        assert_eq!(
            projection.surface_sections_refused,
            Some(crate::section_tally::SurfaceRefusal::TooManySurfaces {
                limit: SURFACE_VOCABULARY_LIMIT
            })
        );
        assert!(
            !projection.sections.is_empty(),
            "the two-dimensional panel is unaffected by the third axis being refused"
        );
    }

    #[test]
    fn a_refused_vocabulary_reaches_the_projection_as_a_reason() {
        let refusal = SectionRefusal {
            labels: 1_200,
            patterns: 40,
        };
        let projection = PatternDetailProjection::new(
            vec![hit_in(0, "α", &["methods"])],
            &SectionAvailability::VocabularyTooLarge(refusal.clone()),
            PatternKey::new(0).unwrap(),
        );
        assert_eq!(projection.section_unavailable, Some(refusal));
        assert!(
            projection.sections.is_empty(),
            "a refused projection lists nothing rather than falling back to the sample"
        );
        assert!(
            projection.hits[0].section_indices.is_empty(),
            "a hit's membership ids resolve against the projected rows, of which \
             there are none — indexing a missing label would panic the handler"
        );
    }

    #[test]
    fn an_exact_count_may_exceed_the_retained_hits_it_filters() {
        // The point of the change: one retained hit, 12,431 corpus sentences.
        let sections = tally_with(&["methods"], &[(0, 12_431)]);
        let projection = PatternDetailProjection::new(
            vec![hit_in(0, "α", &["methods"])],
            &sections,
            PatternKey::new(0).unwrap(),
        );
        assert_eq!(projection.sections[0].count, 12_431);
        assert_eq!(projection.hits.len(), 1);
        assert_eq!(
            &*projection.hits[0].section_indices,
            &[0],
            "the row still filters to the section it belongs to"
        );
    }

    #[test]
    fn no_sections_when_no_hit_carries_a_label() {
        let projection = project(vec![hit(0, "α"), hit(1, "β")]);
        assert!(projection.sections.is_empty());
        assert_eq!(projection.no_section.count, 2);
    }
}
