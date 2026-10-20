//! The Markdown reader's section-selection flags, shared by every binary that
//! reads the dialect.
//!
//! These change *what is read*, not what a query selects from it. They feed
//! [`MarkdownReaderConfig::reader_tag`], so two runs differing in any of them
//! read two different corpora and must not share a token cache. Query-time
//! selection is `--where '@section …'`, which operates on a corpus that already
//! exists and never changes which lines became sentences.
//!
//! The distinction is not academic. The default policy excludes eleven labels
//! at read time, so `--where '@section = references'` over a default-read
//! corpus can only ever be empty — and without these flags there was nothing
//! the researcher could pass to change that.
//!
//! `--md-recover-conversion-artifacts` is the same kind of flag for the same
//! reason. `lingrep-web` reads the dialect with recovery and the CLI reads it
//! without; that asymmetry is deliberate and reaches `reader_tag`, so the two
//! correctly name two artifacts for one `.md` tree. What was missing was any
//! way for a CLI caller to *choose* — a tree with one unclosed div in one file
//! of a thousand was simply unbuildable, with no flag to pass. The default
//! stays strict, so no existing artifact's identity moves; opting in names a
//! different artifact, which is the mechanism working rather than a cost.

use clap::Args;
use lingrep_core::corpus::{
    DEFAULT_EXCLUDED_SECTIONS, DEFAULT_SELF_LABELS, MarkdownReaderConfig, SectionPolicy,
};

/// Section selection for the Markdown corpus dialect.
#[derive(Args, Debug, Clone, Default)]
pub struct MarkdownReaderArgs {
    /// Include only lines carrying one of these section labels. By default,
    /// every label is included; an empty list is an error.
    #[arg(
        long = "md-include-sections",
        value_name = "LIST",
        help_heading = "Markdown input",
        hide_short_help = true
    )]
    pub md_include_sections: Option<String>,
    /// Exclude lines carrying one of these section labels. Pass "" to disable
    /// exclusions.
    #[arg(
        long = "md-exclude-sections",
        value_name = "LIST",
        help_heading = "Markdown input",
        hide_short_help = true
    )]
    pub md_exclude_sections: Option<String>,
    /// Treat these heading classes as labels for the heading line only, not
    /// for the section beneath it.
    #[arg(
        long = "md-self-labels",
        value_name = "LIST",
        help_heading = "Markdown input",
        hide_short_help = true
    )]
    pub md_self_labels: Option<String>,
    /// Recover the narrow set of artifacts the corpus converter emits:
    /// presentational HTML, `<si ...>` notation, and div imbalance at the
    /// physical document boundary. ling refuses them unless this is
    /// passed; lingrep-web always recovers and ignores it.
    #[arg(
        long = "md-recover-conversion-artifacts",
        help_heading = "Markdown input",
        hide_short_help = true
    )]
    pub md_recover_conversion_artifacts: bool,
}

impl MarkdownReaderArgs {
    /// Builds the reader configuration these flags describe.
    ///
    /// # Errors
    ///
    /// Returns a message when `--md-include-sections` is empty. An include list
    /// requires a line to carry a named label, so an empty one admits nothing —
    /// and the run would otherwise read an empty corpus and exit `0`. That is
    /// reachable from an unset `"$SECTIONS"` variable, not merely from a typo.
    pub fn resolve(&self) -> Result<MarkdownReaderConfig, String> {
        let include = match self.md_include_sections.as_deref() {
            None => None,
            Some(spec) => {
                let parsed = SectionPolicy::parse_labels(spec);
                if parsed.is_empty() {
                    return Err(
                        "--md-include-sections was given an empty list, which admits nothing; \
                         omit the flag to admit every label"
                            .to_owned(),
                    );
                }
                Some(parsed)
            }
        };
        // An empty *exclude* list is legal and means "exclude nothing", which
        // is the documented way to read unfiltered text. Only the include list
        // has an empty spelling that means the opposite of what it looks like.
        let exclude = self.md_exclude_sections.as_deref().map_or_else(
            || SectionPolicy::parse_labels(DEFAULT_EXCLUDED_SECTIONS),
            SectionPolicy::parse_labels,
        );
        let config = MarkdownReaderConfig::with_self_labels(
            SectionPolicy::new(include, exclude),
            self.self_labels(),
        );
        Ok(if self.md_recover_conversion_artifacts {
            config.recover_conversion_artifacts()
        } else {
            config
        })
    }

    /// Labels named on either list that are self-labels, and so cannot match.
    ///
    /// A self-label describes its own heading; a heading is never emitted as a
    /// sentence; so naming one selects nothing. Checkable without reading a
    /// byte of input, which is why it is a separate question from the
    /// unused-label report that needs the whole stream.
    #[must_use]
    pub fn self_label_no_ops(&self) -> Vec<String> {
        let self_labels = self.self_labels();
        let mut named =
            SectionPolicy::parse_labels(self.md_include_sections.as_deref().unwrap_or_default());
        named.append(&mut SectionPolicy::parse_labels(
            self.md_exclude_sections.as_deref().unwrap_or_default(),
        ));
        named
            .into_iter()
            .filter(|label| self_labels.contains(label))
            .collect()
    }

    fn self_labels(&self) -> std::collections::BTreeSet<String> {
        self.md_self_labels.as_deref().map_or_else(
            || SectionPolicy::parse_labels(DEFAULT_SELF_LABELS),
            SectionPolicy::parse_labels,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Harness {
        #[command(flatten)]
        markdown: MarkdownReaderArgs,
    }

    fn parse(arguments: &[&str]) -> MarkdownReaderArgs {
        let mut full = vec!["x"];
        full.extend_from_slice(arguments);
        Harness::try_parse_from(full)
            .expect("arguments parse")
            .markdown
    }

    fn labels(spec: &str) -> BTreeSet<String> {
        SectionPolicy::parse_labels(spec)
    }

    /// The failure the dialect contract calls out by name: an unset
    /// `"$SECTIONS"` expands to `""`, every line is then rejected, and the run
    /// reads an empty corpus and exits 0.
    #[test]
    fn an_empty_include_list_is_refused() {
        let error = parse(&["--md-include-sections", ""])
            .resolve()
            .expect_err("expected a refusal");
        assert!(error.contains("--md-include-sections"), "{error}");
        assert!(error.contains("admits nothing"), "{error}");
    }

    #[test]
    fn an_empty_exclude_list_disables_exclusion() {
        let config = parse(&["--md-exclude-sections", ""])
            .resolve()
            .expect("resolves");
        assert!(config.policy.admits(&labels("references")));
    }

    #[test]
    fn omitting_the_flags_reproduces_the_default_policy() {
        assert_eq!(
            parse(&[]).resolve().expect("resolves"),
            MarkdownReaderConfig::new(SectionPolicy::default())
        );
    }

    #[test]
    fn an_include_list_admits_only_its_labels() {
        let config = parse(&["--md-include-sections", "methods,results"])
            .resolve()
            .expect("resolves");
        assert!(config.policy.admits(&labels("methods")));
        assert!(!config.policy.admits(&labels("discussion")));
    }

    #[test]
    fn self_label_no_ops_follow_the_effective_self_label_set() {
        for (case, arguments, expected) in [
            (
                "default self-label on include list",
                &["--md-include-sections", "title"][..],
                vec!["title".to_owned()],
            ),
            (
                "default self-label on exclude list",
                &["--md-exclude-sections", "title"][..],
                vec!["title".to_owned()],
            ),
            (
                "ordinary section label",
                &["--md-exclude-sections", "references"][..],
                vec![],
            ),
            (
                "redeclared empty self-label set",
                &["--md-exclude-sections", "title", "--md-self-labels", ""][..],
                vec![],
            ),
        ] {
            assert_eq!(
                parse(arguments).self_label_no_ops(),
                expected,
                "case: {case}"
            );
        }
    }

    /// Recovery is opt-in, so omitting the flag must reproduce the strict read
    /// every existing artifact was built under. A default that quietly recovered
    /// would move every Markdown artifact's identity at once.
    #[test]
    fn recovery_is_off_unless_asked_for() {
        assert_eq!(
            parse(&[]).resolve().expect("resolves"),
            MarkdownReaderConfig::new(SectionPolicy::default())
        );
        assert_eq!(
            parse(&["--md-recover-conversion-artifacts"])
                .resolve()
                .expect("resolves"),
            MarkdownReaderConfig::new(SectionPolicy::default()).recover_conversion_artifacts()
        );
    }

    /// The flags must reach the cache key, or one run's corpus is served to
    /// another's query.
    #[test]
    fn differing_flags_select_differing_reader_tags() {
        let default = parse(&[]).resolve().expect("resolves").reader_tag();
        for arguments in [
            vec!["--md-exclude-sections", ""],
            vec!["--md-include-sections", "methods"],
            vec!["--md-self-labels", ""],
            vec!["--md-recover-conversion-artifacts"],
        ] {
            assert_ne!(
                parse(&arguments).resolve().expect("resolves").reader_tag(),
                default,
                "{arguments:?} did not change the reader tag"
            );
        }
    }
}
