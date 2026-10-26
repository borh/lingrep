use crate::section_comparison::{SectionComparison, scale};
use crate::templates::format_count;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContrastSelection {
    pub reference: Option<String>,
    pub comparison: Option<String>,
    pub order: ContrastOrder,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ContrastOrder {
    #[default]
    DifferenceDescending,
    DifferenceAscending,
    NameAscending,
}

impl ContrastOrder {
    #[must_use]
    pub fn from_form_value(value: Option<&str>) -> Self {
        match value {
            Some("difference-asc") => Self::DifferenceAscending,
            Some("name-asc") => Self::NameAscending,
            _ => Self::DifferenceDescending,
        }
    }

    #[must_use]
    pub const fn form_value(self) -> &'static str {
        match self {
            Self::DifferenceDescending => "difference-desc",
            Self::DifferenceAscending => "difference-asc",
            Self::NameAscending => "name-asc",
        }
    }
}

impl ContrastSelection {
    pub fn from_form(
        reference: Option<String>,
        comparison: Option<String>,
        order: Option<&str>,
    ) -> Self {
        let present = |label: Option<String>| label.filter(|label| !label.is_empty());
        Self {
            reference: present(reference),
            comparison: present(comparison),
            order: ContrastOrder::from_form_value(order),
        }
    }

    #[must_use]
    pub fn reference_label(&self) -> &str {
        self.reference.as_deref().unwrap_or("")
    }

    #[must_use]
    pub fn comparison_label(&self) -> &str {
        self.comparison.as_deref().unwrap_or("")
    }

    #[must_use]
    pub fn is_same_section(&self) -> bool {
        self.reference.is_some() && self.reference == self.comparison
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContrastSection {
    pub label: String,
    pub universe: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContrastRow {
    pub pattern_key: usize,
    pub pattern_name: String,
    pub reference_count: u64,
    pub comparison_count: u64,
    pub reference_ppm: u64,
    pub comparison_ppm: u64,
    pub reference_ppm_label: String,
    pub comparison_ppm_label: String,
    pub difference_ppm: i64,
    pub difference_label: String,
    pub ratio_hundredths: Option<u64>,
    pub ratio_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionContrast {
    pub reference: ContrastSection,
    pub comparison: ContrastSection,
    pub rows: Vec<ContrastRow>,
}

impl SectionContrast {
    #[must_use]
    pub fn new(comparison: &SectionComparison, selection: &ContrastSelection) -> Option<Self> {
        let (reference_label, comparison_label) = (
            selection.reference.as_ref()?,
            selection.comparison.as_ref()?,
        );
        if reference_label == comparison_label {
            return None;
        }
        let column_index = |label: &String| {
            comparison
                .columns
                .iter()
                .position(|column| column.label == *label)
        };
        let reference_index = column_index(reference_label)?;
        let comparison_index = column_index(comparison_label)?;
        let reference_universe = comparison.columns[reference_index].universe;
        let comparison_universe = comparison.columns[comparison_index].universe;

        let mut rows = comparison
            .rows
            .iter()
            .map(|row| {
                let reference_count = row.cells[reference_index].count;
                let comparison_count = row.cells[comparison_index].count;
                let reference_ppm = scale(reference_count, reference_universe, 1_000_000);
                let comparison_ppm = scale(comparison_count, comparison_universe, 1_000_000);
                let difference_ppm = i64::try_from(comparison_ppm)
                    .unwrap_or(i64::MAX)
                    .saturating_sub(i64::try_from(reference_ppm).unwrap_or(i64::MAX));
                let ratio_hundredths = (reference_count > 0).then(|| {
                    let numerator = u128::from(comparison_count) * u128::from(reference_universe);
                    let denominator = u128::from(reference_count) * u128::from(comparison_universe);
                    u64::try_from((numerator * 100 + denominator / 2) / denominator)
                        .unwrap_or(u64::MAX)
                });
                ContrastRow {
                    pattern_key: row.pattern_key,
                    pattern_name: row.pattern_name.clone(),
                    reference_count,
                    comparison_count,
                    reference_ppm,
                    comparison_ppm,
                    reference_ppm_label: format_count(reference_ppm),
                    comparison_ppm_label: format_count(comparison_ppm),
                    difference_ppm,
                    difference_label: signed_count(difference_ppm),
                    ratio_hundredths,
                    ratio_label: ratio_hundredths
                        .map(|hundredths| format!("{}.{:02}", hundredths / 100, hundredths % 100)),
                }
            })
            .collect::<Vec<_>>();

        match selection.order {
            ContrastOrder::DifferenceDescending => rows.sort_by(|left, right| {
                right
                    .difference_ppm
                    .cmp(&left.difference_ppm)
                    .then_with(|| left.pattern_name.cmp(&right.pattern_name))
            }),
            ContrastOrder::DifferenceAscending => rows.sort_by(|left, right| {
                left.difference_ppm
                    .cmp(&right.difference_ppm)
                    .then_with(|| left.pattern_name.cmp(&right.pattern_name))
            }),
            ContrastOrder::NameAscending => {
                rows.sort_by(|left, right| left.pattern_name.cmp(&right.pattern_name));
            }
        }

        Some(Self {
            reference: ContrastSection {
                label: reference_label.clone(),
                universe: reference_universe,
            },
            comparison: ContrastSection {
                label: comparison_label.clone(),
                universe: comparison_universe,
            },
            rows,
        })
    }
}

fn signed_count(value: i64) -> String {
    let magnitude = format_count(value.unsigned_abs());
    match value {
        0 => magnitude,
        value if value > 0 => format!("+{magnitude}"),
        _ => format!("\u{2212}{magnitude}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section_tally::{SectionAvailability, SpanEvent, SpanEvents, build};
    use crate::snapshot::{PatternDescriptor, PatternKey, PatternOrigin};

    fn descriptor(index: usize, name: &str) -> PatternDescriptor {
        PatternDescriptor {
            key: PatternKey::new(index).unwrap(),
            name: name.into(),
            exact_total: 0,
            metadata: Box::default(),
            origin: PatternOrigin::Authored { chained_total: 0 },
        }
    }

    fn comparison() -> SectionComparison {
        let labels = ["methods", "results"].map(|label| lingrep_corpus::SectionCoverage {
            label: label.into(),
            coverage: 1,
        });
        let events = SpanEvents::from_events(vec![
            SpanEvent {
                slot: 0,
                first: 0,
                end: 4,
            },
            SpanEvent {
                slot: 1,
                first: 4,
                end: 6,
            },
        ]);
        let mut builder = build(&labels, 3, events).expect("the fixture vocabulary fits");
        let matches: [&[usize]; 6] = [&[0, 2], &[0], &[], &[], &[0, 1], &[1]];
        for (ordinal, matched) in matches.iter().enumerate() {
            let keys = matched
                .iter()
                .map(|index| PatternKey::new(*index).unwrap())
                .collect::<Vec<_>>();
            builder.observe(ordinal as u64, &keys, &[]).unwrap();
        }
        let sections = SectionAvailability::Available(builder.finish().unwrap());
        SectionComparison::new(
            &sections,
            &[descriptor(0, "a"), descriptor(1, "b"), descriptor(2, "c")],
        )
        .expect("three patterns with labelled counts")
    }

    fn selection(reference: &str, comparison: &str, order: ContrastOrder) -> ContrastSelection {
        ContrastSelection {
            reference: Some(reference.into()),
            comparison: Some(comparison.into()),
            order,
        }
    }

    #[test]
    fn difference_is_always_defined_and_a_zero_reference_rate_has_no_ratio() {
        let contrast = SectionContrast::new(
            &comparison(),
            &selection("methods", "results", ContrastOrder::NameAscending),
        )
        .expect("both sections are displayed columns");
        assert_eq!(contrast.reference.universe, 4);
        assert_eq!(contrast.comparison.universe, 2);
        let [a, b, c] = contrast.rows.as_slice() else {
            panic!("three rows, by name: {:?}", contrast.rows);
        };

        assert_eq!((a.reference_ppm, a.comparison_ppm), (500_000, 500_000));
        assert_eq!(a.difference_ppm, 0);
        assert_eq!(a.difference_label, "0");
        assert_eq!(a.ratio_hundredths, Some(100));
        assert_eq!(a.ratio_label.as_deref(), Some("1.00"));

        assert_eq!(b.reference_count, 0);
        assert_eq!(b.difference_ppm, 1_000_000);
        assert_eq!(b.difference_label, "+1,000,000");
        assert_eq!(b.ratio_hundredths, None);
        assert_eq!(b.ratio_label, None);

        assert_eq!(c.difference_ppm, -250_000);
        assert_eq!(c.difference_label, "\u{2212}250,000");
        assert_eq!(c.ratio_hundredths, Some(0));
        assert_eq!(c.ratio_label.as_deref(), Some("0.00"));
    }

    #[test]
    fn rows_order_by_signed_difference_in_either_direction() {
        let names = |order| {
            SectionContrast::new(&comparison(), &selection("methods", "results", order))
                .unwrap()
                .rows
                .iter()
                .map(|row| row.pattern_name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(ContrastOrder::DifferenceDescending), ["b", "a", "c"]);
        assert_eq!(names(ContrastOrder::DifferenceAscending), ["c", "a", "b"]);
    }

    #[test]
    fn swapping_the_sections_negates_the_difference_and_inverts_the_ratio() {
        let forward = SectionContrast::new(
            &comparison(),
            &selection("methods", "results", ContrastOrder::NameAscending),
        )
        .unwrap();
        let backward = SectionContrast::new(
            &comparison(),
            &selection("results", "methods", ContrastOrder::NameAscending),
        )
        .unwrap();
        for (f, b) in forward.rows.iter().zip(&backward.rows) {
            assert_eq!(f.difference_ppm, -b.difference_ppm, "{}", f.pattern_name);
        }
        assert_eq!(forward.rows[2].ratio_hundredths, Some(0));
        assert_eq!(backward.rows[2].ratio_hundredths, None);
    }

    #[test]
    fn an_incomplete_or_same_section_selection_has_no_contrast() {
        let table = comparison();
        assert!(SectionContrast::new(&table, &ContrastSelection::default()).is_none());
        assert!(
            SectionContrast::new(
                &table,
                &selection("methods", "methods", ContrastOrder::DifferenceDescending)
            )
            .is_none()
        );
        assert!(
            SectionContrast::new(
                &table,
                &selection("methods", "appendix", ContrastOrder::DifferenceDescending)
            )
            .is_none(),
            "a label that is not a displayed column is not contrasted"
        );
        let selection = ContrastSelection::from_form(
            Some(String::new()),
            Some("results".into()),
            Some("difference-asc"),
        );
        assert_eq!(selection.reference, None, "an empty option is no selection");
        assert_eq!(selection.order, ContrastOrder::DifferenceAscending);
    }
}
