//! The entries of one pattern whose compound key has given values in given
//! fields, and their summed occurrences under a document mask.

use std::collections::BTreeSet;

use lingrep_core::execution::CancellationFlag;

use super::{CountTable, CountTableError, DocumentMask, FieldValue, Group, check_cancellation};

/// Exact equality on named scalar fields of a compound key: `$n.lemma = 坂`
/// admits the key `{$n.lemma: 坂, …}` and not `{$n.lemma: 寺坂, …}`. Fields
/// the predicate does not name are unrestricted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyEquals {
    fields: Vec<(String, String)>,
}

/// Why a [`KeyEquals`] cannot select entries of a pattern.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum KeyFieldError {
    #[error("pattern {pattern} has no key field {field}; its key fields are {fields:?}")]
    UnknownField {
        pattern: String,
        field: String,
        fields: Vec<String>,
    },
    #[error(
        "key field {field} of pattern {pattern} holds a list of values; exact key predicates compare single values"
    )]
    ListField { pattern: String, field: String },
}

/// The occurrences of the entries a [`KeyEquals`] admits under a mask.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeySubset {
    pub occurrences: u64,
    /// Admitted documents holding at least one admitted entry.
    pub works: u64,
    /// Admitted entries with a nonzero count.
    pub entries: u64,
}

impl KeyEquals {
    #[must_use]
    pub fn new(fields: Vec<(String, String)>) -> Self {
        Self { fields }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    #[must_use]
    pub fn fields(&self) -> &[(String, String)] {
        &self.fields
    }

    /// Whether every named field of `group` equals its value. A group that
    /// is not a compound key, or lacks a named field, is not admitted.
    fn admits(&self, pattern: &str, group: &Group<'_>) -> Result<bool, KeyFieldError> {
        let Group::Key(fields) = group else {
            return Ok(self.fields.is_empty());
        };
        for (name, value) in &self.fields {
            match fields.iter().find(|(field, _)| field == name) {
                Some((_, FieldValue::Scalar(found))) if found == value => {}
                Some((_, FieldValue::Scalar(_))) | None => return Ok(false),
                Some((_, FieldValue::List(_))) => {
                    return Err(KeyFieldError::ListField {
                        pattern: pattern.to_owned(),
                        field: name.clone(),
                    });
                }
            }
        }
        Ok(true)
    }
}

impl CountTable {
    /// The field names of `pattern`'s keys, in schema then field order;
    /// empty for a pattern without entries or without compound keys.
    ///
    /// # Errors
    ///
    /// A corrupt table.
    pub fn pattern_key_fields(&self, pattern: usize) -> Result<Vec<String>, CountTableError> {
        let mut schemas = BTreeSet::new();
        for entry in self.pattern_range(pattern)? {
            schemas.insert(self.entry_key(entry as u32)?.0);
        }
        let mut fields = Vec::new();
        for schema in schemas {
            for name in &self
                .header
                .schemas
                .get(schema as usize)
                .ok_or_else(|| super::corrupt("schema out of range"))?
                .names
            {
                if !fields.contains(name) {
                    fields.push(name.clone());
                }
            }
        }
        Ok(fields)
    }

    /// Checks that every field `key` names is a field of `pattern`'s keys.
    /// A pattern without entries has no fields to check, and its subset is
    /// empty.
    ///
    /// # Errors
    ///
    /// The outer error is the table's own; the inner one names the field.
    pub fn check_key_equals(
        &self,
        pattern: usize,
        key: &KeyEquals,
    ) -> Result<Result<(), KeyFieldError>, CountTableError> {
        if key.is_empty() || self.pattern_range(pattern)?.is_empty() {
            return Ok(Ok(()));
        }
        let fields = self.pattern_key_fields(pattern)?;
        for (field, _) in &key.fields {
            if !fields.contains(field) {
                return Ok(Err(KeyFieldError::UnknownField {
                    pattern: self.pattern_name(pattern)?.to_owned(),
                    field: field.clone(),
                    fields,
                }));
            }
        }
        Ok(Ok(()))
    }

    /// Whether `entry` of `pattern` is in `key`'s subset.
    ///
    /// # Errors
    ///
    /// The outer error is the table's own; the inner one is a list field.
    pub fn key_admits(
        &self,
        pattern: usize,
        entry: u32,
        key: &KeyEquals,
    ) -> Result<Result<bool, KeyFieldError>, CountTableError> {
        if key.is_empty() {
            return Ok(Ok(true));
        }
        let group = self.group(entry)?;
        Ok(key.admits(self.pattern_name(pattern)?, &group))
    }

    /// The occurrences, documents and entries of `pattern` under `mask`
    /// whose keys `key` admits, summed over every entry before any ranking
    /// or limit.
    ///
    /// # Errors
    ///
    /// The outer error is the table's own, including
    /// [`CountTableError::Cancelled`]; the inner one names the field.
    pub fn key_subset(
        &self,
        pattern: usize,
        mask: &DocumentMask,
        key: &KeyEquals,
        cancellation: &CancellationFlag,
    ) -> Result<Result<KeySubset, KeyFieldError>, CountTableError> {
        if let Err(error) = self.check_key_equals(pattern, key)? {
            return Ok(Err(error));
        }
        let mut subset = KeySubset::default();
        let mut holds = vec![false; mask.admitted.len()];
        for row in self.unranked_pattern_rows(pattern, mask, cancellation)? {
            let row = row?;
            match self.key_admits(pattern, row.entry, key)? {
                Ok(false) => continue,
                Ok(true) => {}
                Err(error) => return Ok(Err(error)),
            }
            check_cancellation(cancellation)?;
            subset.occurrences += row.count;
            subset.entries += 1;
            let (documents, counts) = self.entry_documents(row.entry)?;
            for (document, count) in documents.iter().zip(counts) {
                let index = *document as usize;
                if *count > 0
                    && mask.admits(index)
                    && let Some(held) = holds.get_mut(index)
                {
                    *held = true;
                }
            }
        }
        subset.works = holds.iter().filter(|held| **held).count() as u64;
        Ok(Ok(subset))
    }

    fn pattern_name(&self, pattern: usize) -> Result<&str, CountTableError> {
        self.pattern_names()
            .nth(pattern)
            .ok_or_else(|| super::corrupt("pattern out of range"))
    }
}
