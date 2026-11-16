use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataPattern {
    pub ordinal: u64,
    pub name: String,
    pub strings: BTreeMap<String, String>,
    pub ambiguous: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PatternMetadataSelection {
    pub filter: Option<(String, String)>,
    pub group_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataRow {
    pub name: String,
    pub members: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetadataProjection {
    pub rows: Vec<MetadataRow>,
    pub selected: BTreeSet<u64>,
    pub omitted: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternMetadataError {
    UnknownStringKey(String),
    ConflictingAlias { key: String, name: String },
}

impl std::fmt::Display for PatternMetadataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownStringKey(key) => write!(
                formatter,
                "No published pattern has string metadata {key:?}. Choose a string metadata key from this catalog."
            ),
            Self::ConflictingAlias { key, name } => write!(
                formatter,
                "Metadata {key:?} differs between definitions published as {name:?}. Give those definitions consistent annotations or distinct output names."
            ),
        }
    }
}
impl std::error::Error for PatternMetadataError {}

/// Retains the first ordinal per published name and records string-metadata disagreements.
pub fn coalesce_metadata(
    patterns: impl IntoIterator<Item = MetadataPattern>,
) -> Vec<MetadataPattern> {
    let mut positions = BTreeMap::<String, usize>::new();
    let mut aliases: Vec<MetadataPattern> = Vec::new();
    for pattern in patterns {
        if let Some(index) = positions.get(&pattern.name) {
            let alias = &mut aliases[*index];
            for key in alias.strings.keys().chain(pattern.strings.keys()) {
                if alias.strings.get(key) != pattern.strings.get(key) {
                    alias.ambiguous.insert(key.clone());
                }
            }
            alias.ambiguous.extend(pattern.ambiguous);
        } else {
            positions.insert(pattern.name.clone(), aliases.len());
            aliases.push(pattern);
        }
    }
    aliases
}

impl PatternMetadataSelection {
    /// Parses an exact string comparison; everything after the first `=` is the value.
    ///
    /// # Errors
    /// Returns an error for a comparison without a key or `=`.
    pub fn parse(filter: Option<&str>, group_by: Option<&str>) -> Result<Self, String> {
        let filter = filter.map(|filter| {
            let (key, value) = filter.split_once('=').ok_or_else(|| "Use KEY=VALUE for a pattern metadata filter, for example aspect=progressive".to_owned())?;
            if key.trim().is_empty() { return Err("The pattern metadata filter has no key".to_owned()); }
            Ok((key.trim().to_owned(), value.to_owned()))
        }).transpose()?;
        let group_by = group_by.map(str::trim).map(str::to_owned);
        if group_by.as_deref() == Some("") {
            return Err("Choose a pattern metadata key to group by".to_owned());
        }
        Ok(Self { filter, group_by })
    }

    /// Selected keys exclude missing and non-string metadata; grouping retains each published-name member.
    ///
    /// # Errors
    /// Returns an error if a requested key has no string values or conflicting annotations under one published name.
    pub fn project(
        &self,
        patterns: &[MetadataPattern],
    ) -> Result<MetadataProjection, PatternMetadataError> {
        for key in self
            .filter
            .iter()
            .map(|(key, _)| key)
            .chain(self.group_by.iter())
        {
            if let Some(pattern) = patterns
                .iter()
                .find(|pattern| pattern.ambiguous.contains(key))
            {
                return Err(PatternMetadataError::ConflictingAlias {
                    key: key.clone(),
                    name: pattern.name.clone(),
                });
            }
            if !patterns
                .iter()
                .any(|pattern| pattern.strings.contains_key(key))
            {
                return Err(PatternMetadataError::UnknownStringKey(key.clone()));
            }
        }
        let mut rows = Vec::new();
        let mut groups = BTreeMap::<String, Vec<u64>>::new();
        let mut selected = BTreeSet::new();
        for pattern in patterns {
            if self
                .filter
                .as_ref()
                .is_some_and(|(key, value)| pattern.strings.get(key) != Some(value))
            {
                continue;
            }
            if let Some(key) = &self.group_by {
                let Some(value) = pattern.strings.get(key) else {
                    continue;
                };
                groups
                    .entry(value.clone())
                    .or_default()
                    .push(pattern.ordinal);
            } else {
                rows.push(MetadataRow {
                    name: pattern.name.clone(),
                    members: vec![pattern.ordinal],
                });
            }
            selected.insert(pattern.ordinal);
        }
        if self.group_by.is_some() {
            rows = groups
                .into_iter()
                .map(|(name, members)| MetadataRow { name, members })
                .collect();
        }
        Ok(MetadataProjection {
            omitted: patterns.len() - selected.len(),
            rows,
            selected,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_require_agreement_in_requested_string_metadata() {
        let entry = |ordinal, value: Option<&str>| MetadataPattern {
            ordinal,
            name: "shared".into(),
            strings: value
                .map(|value| BTreeMap::from([("aspect".into(), value.into())]))
                .unwrap_or_default(),
            ambiguous: BTreeSet::new(),
        };
        let selection = PatternMetadataSelection::parse(None, Some("aspect")).unwrap();
        for value in [None, Some("progressive")] {
            let aliases = coalesce_metadata([entry(0, Some("perfect")), entry(1, value)]);
            assert_eq!(aliases.len(), 1);
            assert!(
                selection
                    .project(&aliases)
                    .unwrap_err()
                    .to_string()
                    .contains("shared")
            );
            assert!(
                PatternMetadataSelection::default()
                    .project(&aliases)
                    .is_ok()
            );
        }
        let aliases = coalesce_metadata([entry(0, Some("perfect")), entry(1, Some("perfect"))]);
        assert_eq!(selection.project(&aliases).unwrap().rows[0].members, [0]);
    }

    #[test]
    fn filtering_and_grouping_keep_distinct_members_and_report_missing_values() {
        let patterns = vec![
            MetadataPattern {
                ordinal: 0,
                name: "a".into(),
                ambiguous: BTreeSet::new(),
                strings: BTreeMap::from([("aspect".into(), "perfect".into())]),
            },
            MetadataPattern {
                ordinal: 1,
                name: "b".into(),
                ambiguous: BTreeSet::new(),
                strings: BTreeMap::from([("aspect".into(), "perfect".into())]),
            },
            MetadataPattern {
                ordinal: 2,
                name: "c".into(),
                ambiguous: BTreeSet::new(),
                strings: BTreeMap::from([("aspect".into(), "progressive".into())]),
            },
            MetadataPattern {
                ordinal: 3,
                name: "missing".into(),
                ambiguous: BTreeSet::new(),
                strings: BTreeMap::new(),
            },
        ];
        let selected = PatternMetadataSelection::parse(Some("aspect=perfect"), Some("aspect"))
            .unwrap()
            .project(&patterns)
            .unwrap();
        assert_eq!(selected.omitted, 2);
        assert_eq!(
            selected.rows,
            vec![MetadataRow {
                name: "perfect".into(),
                members: vec![0, 1]
            }]
        );
        assert_eq!(selected.selected, BTreeSet::from([0, 1]));
        let none = PatternMetadataSelection::parse(Some("aspect=absent"), None)
            .unwrap()
            .project(&patterns)
            .unwrap();
        assert!(none.rows.is_empty());
        assert_eq!(none.omitted, 4);
        assert!(
            PatternMetadataSelection::parse(None, Some("unknown"))
                .unwrap()
                .project(&patterns)
                .is_err()
        );
        assert!(PatternMetadataSelection::parse(Some("aspect"), None).is_err());
        assert_eq!(
            PatternMetadataSelection::parse(Some("a=x=y"), None)
                .unwrap()
                .filter,
            Some(("a".into(), "x=y".into()))
        );
    }
}
