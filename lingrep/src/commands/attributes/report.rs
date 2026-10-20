//! Coverage buckets and the exposure join.
//!
//! Pure values with no I/O, so every rule below is testable without a
//! dictionary, a corpus, or a terminal.

use lingrep_core::execution::ExecutionError;
use lingrep_core::fsm::ExclusionSite;
use lingrep_core::token_match::{AttributeKey, AttributeValueRef, TokenSchema, TokenSequence};
use std::collections::BTreeMap;

/// UniDic writes this for "this attribute does not apply here". It is a
/// supplied value, not an absence — see
/// `a_dictionary_supplied_unspecified_value_is_not_replaced`.
const UNSPECIFIED: &str = "*";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CoverageBuckets {
    /// Set to a value other than `*`.
    pub present: u64,
    /// Set to `*`.
    pub unspecified: u64,
    /// Genuinely unset.
    pub absent: u64,
    /// Present but empty; lexicon projection treats this like a run boundary.
    pub empty: u64,
}

/// Per-attribute presence over the admitted tokens.
///
/// Keyed by the selected analyzer's schema; the report emits every attribute
/// that producer declares, including dynamic provider keys.
#[derive(Clone, Debug)]
pub(crate) struct AttributeCoverage {
    buckets: BTreeMap<AttributeKey, CoverageBuckets>,
    admitted_tokens: u64,
}

impl AttributeCoverage {
    pub(crate) fn new(schema: &TokenSchema) -> Self {
        Self {
            buckets: schema
                .keys()
                .iter()
                .cloned()
                .map(|key| (key, CoverageBuckets::default()))
                .collect(),
            admitted_tokens: 0,
        }
    }
}

/// Corpus-wide counters are checked, as elsewhere in `commands`: a saturating
/// or wrapping count would report a number rather than a failure, and this
/// command exists to be believed.
fn add(counter: &mut u64) -> Result<(), ExecutionError> {
    *counter = counter
        .checked_add(1)
        .ok_or(ExecutionError::ResourceAccounting)?;
    Ok(())
}

#[cfg(test)]
impl Default for AttributeCoverage {
    fn default() -> Self {
        Self::new(&TokenSchema::unidic_morphology())
    }
}

impl AttributeCoverage {
    pub(crate) fn observe(&mut self, tokens: &dyn TokenSequence) -> Result<(), ExecutionError> {
        for token in 0..tokens.len() {
            add(&mut self.admitted_tokens)?;
            for (attribute, bucket) in &mut self.buckets {
                match tokens.value_key(token, attribute) {
                    None => add(&mut bucket.absent)?,
                    Some(AttributeValueRef::Scalar(value)) if value == UNSPECIFIED => {
                        add(&mut bucket.unspecified)?
                    }
                    Some(AttributeValueRef::Scalar("")) => {
                        add(&mut bucket.present)?;
                        add(&mut bucket.empty)?;
                    }
                    Some(AttributeValueRef::StringSet([])) => {
                        add(&mut bucket.present)?;
                        add(&mut bucket.empty)?;
                    }
                    Some(_) => add(&mut bucket.present)?,
                }
            }
        }
        Ok(())
    }

    pub(crate) fn buckets(&self, attribute: &AttributeKey) -> CoverageBuckets {
        self.buckets
            .get(attribute)
            .copied()
            .unwrap_or(CoverageBuckets {
                absent: self.admitted_tokens,
                ..CoverageBuckets::default()
            })
    }

    pub(crate) fn attributes(&self) -> impl Iterator<Item = &AttributeKey> {
        self.buckets.keys()
    }

    pub(crate) fn admitted_tokens(&self) -> u64 {
        self.admitted_tokens
    }
}

/// What this run can say about one exclusion. Deliberately never says a match
/// was lost: see the spec's *Exposure is not impact*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExclusionStatus {
    /// Evaluated, and its attribute is absent on no admitted token.
    Clean,
    /// Evaluated, and its attribute is absent somewhere. A risk, not a defect.
    Exposed,
    /// Never evaluated, so no absent token can reach it.
    Inert,
    /// Nothing was admitted, so there is no evidence either way.
    Unknown,
}

impl ExclusionStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Exposed => "exposed",
            Self::Inert => "inert",
            Self::Unknown => "unknown",
        }
    }
}

pub(crate) struct ExclusionRecord {
    pub definition: String,
    pub attribute: String,
    pub source_name: String,
    pub declaration_line: usize,
    pub site_count: usize,
    pub reachable: bool,
    pub absent_in_corpus: u64,
    pub status: ExclusionStatus,
}

/// Joins one authored exclusion to what this corpus measured.
///
/// Order matters. `Inert` outranks `Unknown` because unreachability holds
/// whatever the corpus contains, and both outrank the absent-count test
/// because a count of zero means different things when nothing was admitted.
pub(crate) fn exclusion_record(
    site: &ExclusionSite,
    coverage: &AttributeCoverage,
) -> ExclusionRecord {
    let absent_in_corpus = coverage.buckets(&site.attribute).absent;
    let status = if site.reachable {
        if coverage.admitted_tokens() == 0 {
            ExclusionStatus::Unknown
        } else if absent_in_corpus > 0 {
            ExclusionStatus::Exposed
        } else {
            ExclusionStatus::Clean
        }
    } else {
        ExclusionStatus::Inert
    };
    ExclusionRecord {
        definition: site.definition.to_string(),
        attribute: site.attribute.pattern_key().to_owned(),
        source_name: site.source_name.to_string(),
        declaration_line: site.declaration_line,
        site_count: site.site_count,
        reachable: site.reachable,
        absent_in_corpus,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

    fn token(pairs: &[(TokenAttributeIndex, &str)]) -> TokenInfo {
        let mut token = TokenInfo::default();
        for (attribute, value) in pairs {
            token.set(*attribute, (*value).into());
        }
        token
    }

    /// `*` is a supplied value, so it is neither `present` nor
    /// `absent`. A two-bucket implementation is indistinguishable without this.
    #[test]
    fn an_unspecified_value_is_its_own_bucket() {
        let mut coverage = AttributeCoverage::default();
        coverage
            .observe(&[
                token(&[(TokenAttributeIndex::CType, "五段")]),
                token(&[(TokenAttributeIndex::CType, "*")]),
                token(&[(TokenAttributeIndex::Orth, "x")]),
            ])
            .expect("three tokens cannot overflow");

        assert_eq!(
            coverage.buckets(&TokenAttributeIndex::CType.into()),
            CoverageBuckets {
                present: 1,
                unspecified: 1,
                absent: 1,
                empty: 0,
            }
        );
        assert_eq!(coverage.admitted_tokens(), 3);
    }

    /// Every bucket sums to the admitted token count for every attribute, or a
    /// rate computed from them is meaningless.
    #[test]
    fn the_three_buckets_partition_every_attribute() {
        let mut coverage = AttributeCoverage::default();
        coverage
            .observe(&[token(&[(TokenAttributeIndex::Orth, "x")]), token(&[])])
            .expect("two tokens cannot overflow");

        for attribute in TokenAttributeIndex::ALL {
            let buckets = coverage.buckets(&attribute.into());
            assert_eq!(
                buckets.present + buckets.unspecified + buckets.absent,
                coverage.admitted_tokens(),
                "{attribute:?} does not partition"
            );
        }
    }

    fn site(reachable: bool) -> lingrep_core::fsm::ExclusionSite {
        lingrep_core::fsm::ExclusionSite {
            definition: "p".into(),
            attribute: TokenAttributeIndex::Pron.into(),
            source_name: "t.lx".into(),
            declaration_line: 1,
            site_count: 1,
            reachable,
        }
    }

    fn coverage_of(tokens: &[TokenInfo]) -> AttributeCoverage {
        let mut coverage = AttributeCoverage::default();
        coverage.observe(&tokens).expect("fixture cannot overflow");
        coverage
    }

    #[test]
    fn an_attribute_absent_nowhere_is_clean() {
        let coverage = coverage_of(&[token(&[(TokenAttributeIndex::Pron, "x")])]);

        assert_eq!(
            exclusion_record(&site(true), &coverage).status,
            ExclusionStatus::Clean
        );
    }

    #[test]
    fn an_attribute_absent_somewhere_is_exposed() {
        let coverage = coverage_of(&[token(&[(TokenAttributeIndex::Orth, "x")])]);

        let record = exclusion_record(&site(true), &coverage);
        assert_eq!(record.status, ExclusionStatus::Exposed);
        assert_eq!(record.absent_in_corpus, 1);
    }

    /// An unreachable definition is never evaluated, so no absent token can
    /// reach it. Reporting `exposed` would claim a risk that cannot exist.
    #[test]
    fn an_unreachable_definition_is_inert_however_absent_the_attribute() {
        let coverage = coverage_of(&[token(&[(TokenAttributeIndex::Orth, "x")])]);

        assert_eq!(
            exclusion_record(&site(false), &coverage).status,
            ExclusionStatus::Inert
        );
    }

    /// Zero admitted tokens makes every count zero. Reporting that as `clean`
    /// would claim zero exposure on the strength of no evidence.
    #[test]
    fn an_empty_admitted_scope_is_unknown_not_clean() {
        let coverage = AttributeCoverage::default();

        assert_eq!(
            exclusion_record(&site(true), &coverage).status,
            ExclusionStatus::Unknown
        );
    }

    /// `inert` outranks `unknown`: unreachability holds whatever the corpus
    /// contains, so it is the more specific true statement.
    #[test]
    fn an_unreachable_definition_under_an_empty_scope_is_inert_not_unknown() {
        let coverage = AttributeCoverage::default();

        assert_eq!(
            exclusion_record(&site(false), &coverage).status,
            ExclusionStatus::Inert
        );
    }
}
