//! Section selection policy for the Markdown corpus dialect.
//!
//! Labels are opaque strings. The pandoc class vocabulary is open — corpora
//! introduce their own classes — so nothing here enumerates class names.

use std::collections::BTreeSet;

/// Default exclusions for bibliographic and figure/table sections.
///
/// `title` and `subtitle` are self-labels rather than section labels and must
/// remain absent. See `docs/markdown-corpus-dialect.md` for the distinction and
/// the regression this prevents.
pub const DEFAULT_EXCLUDED_SECTIONS: &str = "references,caption,figure,table,notes,\
author,author_bio,keywords,submission_date,acknowledgments,toc";

/// Admits or rejects a line by the set of section labels active at that line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionPolicy {
    include: Option<BTreeSet<String>>,
    exclude: BTreeSet<String>,
}

impl SectionPolicy {
    /// `include: None` admits every label. Exclusion always wins.
    pub fn new(include: Option<BTreeSet<String>>, exclude: BTreeSet<String>) -> Self {
        Self { include, exclude }
    }

    /// Parse a comma-separated label list.
    pub fn parse_labels(spec: &str) -> BTreeSet<String> {
        spec.split(',')
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// The policy's two fields, for a caller that needs to derive a value from
    /// the whole of it. `include: None` ("admit every label") is distinct from
    /// `Some(empty)` ("admit nothing"), so the option is preserved rather than
    /// flattened.
    ///
    /// This hands back the fields rather than a rendering of them. A rendering
    /// would have to pick separators, and a separator is only unambiguous while
    /// it cannot occur in the data — labels are opaque strings and nothing here
    /// constrains their bytes, so any separator this module chose could be
    /// spelled by a label and would silently merge two fields into one. Encoding
    /// is the caller's problem because only the caller knows the whole of what it
    /// is encoding; see `markdown_dialect::reader_tag`, the only caller.
    ///
    /// `pub(crate)` rather than `pub`: the policy is one part of a reader
    /// configuration, and a cache key derived from this alone would be
    /// incomplete.
    pub(crate) fn canonical_fields(&self) -> (Option<&BTreeSet<String>>, &BTreeSet<String>) {
        (self.include.as_ref(), &self.exclude)
    }

    pub fn admits(&self, active: &BTreeSet<String>) -> bool {
        if active.iter().any(|label| self.exclude.contains(label)) {
            return false;
        }
        match &self.include {
            None => true,
            Some(include) => active.iter().any(|label| include.contains(label)),
        }
    }
}

impl Default for SectionPolicy {
    fn default() -> Self {
        Self::new(None, Self::parse_labels(DEFAULT_EXCLUDED_SECTIONS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(spec: &str) -> BTreeSet<String> {
        SectionPolicy::parse_labels(spec)
    }

    #[test]
    fn include_all_admits_unlabeled_content() {
        let policy = SectionPolicy::new(None, BTreeSet::new());
        assert!(policy.admits(&BTreeSet::new()));
    }

    #[test]
    fn exclude_rejects_when_any_active_label_matches() {
        let policy = SectionPolicy::new(None, labels("caption"));
        assert!(!policy.admits(&labels("results,caption")));
        assert!(policy.admits(&labels("results")));
    }

    #[test]
    fn include_requires_at_least_one_active_label() {
        let policy = SectionPolicy::new(Some(labels("methods")), BTreeSet::new());
        assert!(policy.admits(&labels("literature_review,methods")));
        assert!(!policy.admits(&labels("results")));
        assert!(!policy.admits(&BTreeSet::new()));
    }

    #[test]
    fn exclude_wins_over_include() {
        let policy = SectionPolicy::new(Some(labels("results")), labels("caption"));
        assert!(!policy.admits(&labels("results,caption")));
    }

    #[test]
    fn parse_labels_trims_and_drops_empties() {
        assert_eq!(labels(" a , ,b "), labels("a,b"));
        assert!(labels("").is_empty());
    }

    #[test]
    fn default_excludes_references_and_admits_prose() {
        let policy = SectionPolicy::default();
        assert!(!policy.admits(&labels("references")));
        assert!(policy.admits(&labels("methods")));
    }
}
