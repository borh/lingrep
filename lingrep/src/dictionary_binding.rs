//! Comparing a catalog's declared dictionaries against the one a run loaded.
//!
//! A lingrep patterns catalog's predicates are written against a tagset. Nothing else in
//! the system records which: `token_schema_identity` digests the structural
//! attribute names and value kinds, which do not distinguish one dictionary's
//! tagset from another's, and `dictionary_identity` describes the run rather
//! than the catalog. So a catalog authored for one dictionary and run against
//! another agrees on every recorded identity while meaning
//! something different: one catalog over one set of sentences reports 18
//! matches under both `unidic-cwj-202512` and `unidic-kinsei-edo-202512` while
//! 37 token analyses differ underneath.
//!
//! **This is a notice, never an error.** Running a catalog against a second
//! dictionary is a deliberate experiment as often as it is a mistake. Report
//! divergence for the researcher to adjudicate without changing exit status.
//!
//! # What is compared, and what deliberately is not
//!
//! Compatibility is a spectrum, not a match on a file name:
//!
//! * `unidic-cwj` and `unidic-csj` differ in **weights only** — same
//!   vocabulary, same tagset. A catalog is genuinely valid for both.
//! * `unidic-kinsei-edo`, `unidic-kindai-bungo` and the other historical
//!   variants differ in vocabulary **and** in closed-class grammatical and
//!   morphological analysis, by varying amounts.
//! * Sudachi's A/B/C splitting modes change the **unit size** — what counts as
//!   one token — which is a different axis again.
//!
//! Two consequences for this module. The catalog declares a **set**, because
//! only the researcher can say which dictionaries their predicates survive;
//! encoding an equivalence table here would be an unmaintainable claim about
//! dictionaries this workspace does not ship. And the **release version is
//! dropped before comparing**, because it is the least significant part: a
//! catalog written for `unidic-cwj-202512` is not thereby wrong under a later
//! `unidic-cwj`. A version-sensitive check would fire on every dictionary bump
//! and be muted within a week.
//!
//! Sudachi's unit size is **not** yet expressible. It is not derivable from a
//! dictionary path — it is a runtime mode — so the declaration will need to
//! carry it explicitly when Sudachi support lands. Recorded here rather than
//! approximated, because a check that silently ignores unit size would be worse
//! than none: A-mode and C-mode disagree about what a token is, so every span
//! in a catalog means something different between them.

use lingrep_core::fsm::CompiledCatalog;

/// The dictionary name a path denotes, by file name.
///
/// **Stated plainly: this is the file name, not an authority.** vibrato's
/// dictionary bytes carry no name the loader exposes, and the only exact
/// identity available is a BLAKE3 digest of those bytes — which changes on
/// every rebuild and cannot be authored into a catalog by hand. A file name is
/// what remains, and the flake's dictionaries are named after their releases
/// (`unidic-cwj-202512.dic.zst`), so it is right in the case that matters and
/// wrong only for a renamed or copied file.
///
/// A renamed file therefore produces a spurious notice or misses a real one.
/// That is survivable precisely because the outcome is a notice: nothing is
/// blocked either way.
#[must_use]
pub fn dictionary_name_from_path(path: &str) -> Option<&str> {
    let file_name = path.rsplit('/').next()?;
    let stem = file_name
        .strip_suffix(".dic.zst")
        .or_else(|| file_name.strip_suffix(".dic"))
        .unwrap_or(file_name);
    (!stem.is_empty()).then_some(stem)
}

/// Drops a trailing release version, so `unidic-cwj-202512` compares as
/// `unidic-cwj`.
///
/// A version is a run of digits in the final `-`-separated component. Only a
/// final component is considered, so `unidic-kindai-bungo` keeps every part of
/// its name and a hypothetical `unidic-2020-cwj` is left alone.
#[must_use]
pub fn strip_release_version(name: &str) -> &str {
    match name.rsplit_once('-') {
        Some((head, tail))
            if !head.is_empty() && !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) =>
        {
            head
        }
        _ => name,
    }
}

/// The notice to publish, or `None` when there is nothing to say.
///
/// Silent when the catalog declares nothing, including every inline
/// `--pattern`, which has no file to declare in. A notice that always fires is
/// a notice nobody reads, so absence is not reported.
#[must_use]
pub fn binding_mismatch(matcher: &CompiledCatalog, dictionary_path: &str) -> Option<String> {
    let loaded = dictionary_name_from_path(dictionary_path)?;
    let loaded_family = strip_release_version(loaded);
    match matcher.analyzer_compatibility("vibrato", Some(loaded_family), None, "input") {
        lingrep_core::fsm::AnalyzerCompatibility::Unspecified
        | lingrep_core::fsm::AnalyzerCompatibility::Compatible => return None,
        lingrep_core::fsm::AnalyzerCompatibility::Mismatch => {}
    }
    Some(format!(
        "catalog analyzer declarations do not admit Vibrato resource `{loaded_family}` \
         (loaded as `{loaded}`), so counts may agree while meaning differs"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};

    fn matcher(text: &str) -> CompiledCatalog {
        compile_catalog(
            PatternSource::named("test.lx", text),
            CompileLimits::default(),
        )
        .expect("catalog compiles")
        .matcher
    }

    const CWJ: &str = "/nix/store/x/share/vibrato/unidic-cwj-202512.dic.zst";
    const CSJ: &str = "/nix/store/x/share/vibrato/unidic-csj-202512.dic.zst";
    const EDO: &str = "/nix/store/x/share/vibrato/unidic-kinsei-edo-202512.dic.zst";

    #[test]
    fn a_nix_store_dictionary_path_yields_its_release_name() {
        assert_eq!(dictionary_name_from_path(CWJ), Some("unidic-cwj-202512"));
    }

    #[test]
    fn an_uncompressed_dictionary_and_a_bare_name_both_resolve() {
        assert_eq!(
            dictionary_name_from_path("/tmp/unidic-csj-202512.dic"),
            Some("unidic-csj-202512")
        );
        assert_eq!(
            dictionary_name_from_path("unidic-cwj-202512"),
            Some("unidic-cwj-202512")
        );
    }

    #[test]
    fn a_path_with_no_file_name_yields_nothing() {
        assert_eq!(dictionary_name_from_path(""), None);
        assert_eq!(dictionary_name_from_path("/some/dir/"), None);
    }

    #[test]
    fn a_release_version_is_dropped_but_a_name_component_is_not() {
        assert_eq!(strip_release_version("unidic-cwj-202512"), "unidic-cwj");
        assert_eq!(
            strip_release_version("unidic-kinsei-edo-202512"),
            "unidic-kinsei-edo"
        );
        assert_eq!(
            strip_release_version("unidic-kindai-bungo"),
            "unidic-kindai-bungo"
        );
        assert_eq!(strip_release_version("sudachi-c"), "sudachi-c");
    }

    #[test]
    fn an_undeclared_catalog_is_silent() {
        assert_eq!(binding_mismatch(&matcher("p := [text=猫].\n"), CWJ), None);
    }

    #[test]
    fn a_matching_declaration_is_silent() {
        let pattern =
            "analyzer: { family: \"vibrato\", resource: \"unidic-cwj\" }\np := [text=猫].\n";
        assert_eq!(binding_mismatch(&matcher(pattern), CWJ), None);
    }

    /// The version is the least significant part of the name, so a catalog
    /// written for one release is not reported against the next one.
    #[test]
    fn a_later_release_of_the_declared_dictionary_is_silent() {
        let pattern =
            "analyzer: { family: \"vibrato\", resource: \"unidic-cwj\" }\np := [text=猫].\n";
        assert_eq!(
            binding_mismatch(&matcher(pattern), "/d/unidic-cwj-209901.dic.zst"),
            None
        );
    }

    /// cwj and csj differ in weights only, so a catalog valid for both says so
    /// and is not nagged about either.
    #[test]
    fn a_declared_set_covers_every_dictionary_in_it() {
        let pattern = "analyzer: { family: \"vibrato\", resource: \"unidic-cwj\" }\n\
                   analyzer: { family: \"vibrato\", resource: \"unidic-csj\" }\n\
                   p := [text=猫].\n";
        let compiled = matcher(pattern);
        assert_eq!(binding_mismatch(&compiled, CWJ), None);
        assert_eq!(binding_mismatch(&compiled, CSJ), None);
        let notice = binding_mismatch(&compiled, EDO).expect("edo is outside the declared set");
        assert!(notice.contains("unidic-kinsei-edo-202512"), "{notice}");
        assert!(notice.contains("unidic-kinsei-edo"), "{notice}");
    }

    #[test]
    fn a_historical_variant_is_reported_against_a_modern_catalog() {
        let pattern =
            "analyzer: { family: \"vibrato\", resource: \"unidic-cwj\" }\np := [text=猫].\n";
        let notice = binding_mismatch(&matcher(pattern), EDO).expect("a mismatch is reported");
        assert!(notice.contains("meaning differs"), "{notice}");
    }
}
