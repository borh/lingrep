//! Slots for counting per document-metadata value.

use std::collections::HashMap;

/// Document-metadata values numbered in the order they first occur over a
/// sequence of documents, with each document's slots. Slot `values().len()`
/// holds the documents without a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValueSlots {
    values: Vec<String>,
    documents: Vec<Box<[u32]>>,
}

#[derive(Debug, thiserror::Error)]
#[error("metadata value count exceeds u32")]
pub struct TooManyValues;

impl ValueSlots {
    /// Numbers the values of `documents`, each document's values in document
    /// order. A document's slots are ascending and distinct, and a document
    /// that lists no value has the one slot after every value's.
    ///
    /// # Errors
    ///
    /// More values than a `u32` slot can number.
    pub fn number(documents: impl IntoIterator<Item = Vec<String>>) -> Result<Self, TooManyValues> {
        let mut slot_of = HashMap::<String, u32>::new();
        let mut values = Vec::new();
        let mut numbered = Vec::new();
        for document_values in documents {
            let mut slots = Vec::with_capacity(document_values.len());
            for value in document_values {
                let next = u32::try_from(values.len()).map_err(|_| TooManyValues)?;
                slots.push(*slot_of.entry(value).or_insert_with_key(|value| {
                    values.push(value.clone());
                    next
                }));
            }
            slots.sort_unstable();
            slots.dedup();
            numbered.push(slots);
        }
        let no_value = u32::try_from(values.len()).map_err(|_| TooManyValues)?;
        Ok(Self {
            values,
            documents: numbered
                .into_iter()
                .map(|slots| {
                    if slots.is_empty() {
                        Box::new([no_value]) as Box<[u32]>
                    } else {
                        slots.into_boxed_slice()
                    }
                })
                .collect(),
        })
    }

    /// The numbered values; a slot past the last indexes no value.
    #[must_use]
    pub fn values(&self) -> &[String] {
        &self.values
    }

    /// The value of `slot`, or `None` for the slot of documents without one.
    #[must_use]
    pub fn value(&self, slot: usize) -> Option<&str> {
        self.values.get(slot).map(String::as_str)
    }

    /// Values plus the slot for documents without one.
    #[must_use]
    pub fn slot_count(&self) -> usize {
        self.values.len() + 1
    }

    /// The documents in numbering order.
    #[must_use]
    pub fn documents(&self) -> &[Box<[u32]>] {
        &self.documents
    }

    /// Keeps the documents `select` returns, in its order: `Some(index)` is
    /// the numbered document at `index`, and `None` a document without a
    /// value.
    #[must_use]
    pub fn select(&self, select: impl IntoIterator<Item = Option<usize>>) -> Self {
        let no_value: Box<[u32]> =
            Box::new([u32::try_from(self.values.len()).expect("number bounded the value count")]);
        Self {
            values: self.values.clone(),
            documents: select
                .into_iter()
                .map(|index| {
                    index
                        .and_then(|index| self.documents.get(index))
                        .unwrap_or(&no_value)
                        .clone()
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn values_are_numbered_by_first_occurrence_with_the_empty_slot_last() {
        let slots = ValueSlots::number([
            strings(&["b", "a", "b"]),
            strings(&[]),
            strings(&["c", "a"]),
        ])
        .unwrap();
        assert_eq!(slots.values(), ["b", "a", "c"]);
        assert_eq!(slots.slot_count(), 4);
        assert_eq!(
            slots.documents(),
            [
                Box::from([0, 1].as_slice()),
                Box::from([3].as_slice()),
                Box::from([1, 2].as_slice()),
            ]
        );
        assert_eq!(slots.value(2), Some("c"));
        assert_eq!(slots.value(3), None);
        let selected = slots.select([Some(2), None, Some(0)]);
        assert_eq!(selected.values(), slots.values());
        assert_eq!(
            selected.documents(),
            [
                Box::from([1, 2].as_slice()),
                Box::from([3].as_slice()),
                Box::from([0, 1].as_slice()),
            ]
        );
    }
}
