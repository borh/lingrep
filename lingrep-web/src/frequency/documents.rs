use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use lingrep_core::corpus::{Metadata, metadata_from_json};
use lingrep_core::spill::{SortedRunCounts, TempBudget, TempReservation};
use tempfile::NamedTempFile;

use super::SPILL_POLICY;

pub(super) struct Documents {
    data: NamedTempFile,
    index: NamedTempFile,
    _reservation: TempReservation,
}

pub(super) struct Writer {
    data: NamedTempFile,
    positions: SortedRunCounts<(u64, u64, u64, u64, u64)>,
    reservation: TempReservation,
}

impl Writer {
    pub(super) fn new(budget: Arc<TempBudget>) -> io::Result<Self> {
        Ok(Self {
            data: NamedTempFile::new()?,
            positions: SortedRunCounts::with_policy(SPILL_POLICY, Arc::clone(&budget))?,
            reservation: budget.reserve(0)?,
        })
    }

    pub(super) fn push(
        &mut self,
        id: u64,
        sentences: Range<u64>,
        metadata: &Metadata,
    ) -> io::Result<()> {
        let object = metadata
            .iter()
            .map(|(key, value)| (key.clone(), value.to_json()))
            .collect::<serde_json::Map<_, _>>();
        let bytes = serde_json::to_vec(&object)?;
        let offset = self.data.as_file_mut().stream_position()?;
        self.reservation.grow(bytes.len() as u64)?;
        self.data.write_all(&bytes)?;
        self.positions.add(
            (
                id,
                offset,
                bytes.len() as u64,
                sentences.start,
                sentences.end,
            ),
            1,
        )
    }

    pub(super) fn finish(mut self) -> io::Result<Documents> {
        let index = NamedTempFile::new()?;
        let mut writer = BufWriter::new(index.as_file());
        let mut rows = self.positions.into_sorted_drain()?;
        let mut previous = None;
        while let Some(((id, offset, len, first, end), count)) = rows.next_entry()? {
            if previous == Some(id) || count != 1 {
                return Err(io::Error::other("duplicate document metadata ID"));
            }
            previous = Some(id);
            self.reservation.grow(INDEX_RECORD as u64)?;
            for value in [id, offset, len, first, end] {
                writer.write_all(&value.to_le_bytes())?;
            }
        }
        writer.flush()?;
        drop(writer);
        Ok(Documents {
            data: self.data,
            index,
            _reservation: self.reservation,
        })
    }
}

const INDEX_RECORD: usize = 40;

pub(super) struct Reader {
    data: BufReader<File>,
    index: BufReader<File>,
}

impl Documents {
    pub(super) fn reader(&self) -> io::Result<Reader> {
        Ok(Reader {
            data: BufReader::new(self.data.reopen()?),
            index: BufReader::new(self.index.reopen()?),
        })
    }
}

impl Reader {
    fn position(&mut self) -> io::Result<Option<[u64; 5]>> {
        let mut bytes = [0u8; INDEX_RECORD];
        if self.index.read(&mut bytes[..1])? == 0 {
            return Ok(None);
        }
        self.index.read_exact(&mut bytes[1..])?;
        Ok(Some(std::array::from_fn(|index| {
            u64::from_le_bytes(
                bytes[index * 8..index * 8 + 8]
                    .try_into()
                    .expect("eight bytes"),
            )
        })))
    }

    pub(super) fn check_ids(&mut self, ids: &[u64]) -> io::Result<()> {
        for wanted in ids {
            loop {
                let Some([id, ..]) = self.position()? else {
                    return Err(io::Error::other(format!(
                        "counts document {wanted}, which the corpus does not hold"
                    )));
                };
                if id < *wanted {
                    continue;
                }
                if id != *wanted {
                    return Err(io::Error::other(format!(
                        "counts document {wanted}, which the corpus does not hold"
                    )));
                }
                break;
            }
        }
        Ok(())
    }

    pub(super) fn document(&mut self, wanted: u64) -> io::Result<Metadata> {
        self.located(wanted).map(|(_, metadata)| metadata)
    }

    /// The corpus sentence range and metadata of document `wanted`, which
    /// follows every document already read.
    pub(super) fn located(&mut self, wanted: u64) -> io::Result<(Range<u64>, Metadata)> {
        loop {
            let Some([id, offset, len, first, end]) = self.position()? else {
                return Err(io::Error::other(format!(
                    "counts document {wanted}, which the corpus does not hold"
                )));
            };
            if id < wanted {
                continue;
            }
            if id != wanted {
                return Err(io::Error::other(format!(
                    "counts document {wanted}, which the corpus does not hold"
                )));
            }
            self.data.seek(SeekFrom::Start(offset))?;
            let mut bytes = vec![0; usize::try_from(len).map_err(io::Error::other)?];
            self.data.read_exact(&mut bytes)?;
            let metadata =
                metadata_from_json(serde_json::from_slice(&bytes)?, "stored document metadata")?;
            return Ok((first..end, metadata));
        }
    }

    pub(super) fn next(&mut self) -> io::Result<Option<Metadata>> {
        let Some([_, offset, len, ..]) = self.position()? else {
            return Ok(None);
        };
        self.data.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; usize::try_from(len).map_err(io::Error::other)?];
        self.data.read_exact(&mut bytes)?;
        metadata_from_json(serde_json::from_slice(&bytes)?, "stored document metadata").map(Some)
    }
}

/// A count table's documents by position: each one's corpus sentence range,
/// and for each key the slots of its values, numbered per key in ascending
/// value order. Every lookup is a positioned read of fixed-width records.
pub(super) struct TableDocuments {
    documents: usize,
    ranges: NamedTempFile,
    values: NamedTempFile,
    value_index: NamedTempFile,
    numbers: NamedTempFile,
    offsets: NamedTempFile,
    slots: NamedTempFile,
    keys: Vec<KeySlots>,
    _reservation: TempReservation,
}

struct KeySlots {
    name: String,
    first_value: u64,
    values: u32,
}

struct Spool {
    file: NamedTempFile,
    writer: BufWriter<File>,
}

impl Spool {
    fn new() -> io::Result<Self> {
        let file = NamedTempFile::new()?;
        let writer = BufWriter::new(file.reopen()?);
        Ok(Self { file, writer })
    }

    fn write(&mut self, reservation: &mut TempReservation, bytes: &[u8]) -> io::Result<()> {
        reservation.grow(bytes.len() as u64)?;
        self.writer.write_all(bytes)
    }

    fn finish(mut self) -> io::Result<NamedTempFile> {
        self.writer.flush()?;
        Ok(self.file)
    }
}

fn read_pair(file: &NamedTempFile, record: u64) -> io::Result<[u64; 2]> {
    let mut bytes = [0u8; 16];
    file.as_file().read_exact_at(&mut bytes, record * 16)?;
    let (pair, _) = bytes.as_chunks::<8>();
    Ok([u64::from_le_bytes(pair[0]), u64::from_le_bytes(pair[1])])
}

impl TableDocuments {
    /// Reads the documents `ids`, ascending, from `reader` and numbers the
    /// scalar values of each of `keys` as `value_text` renders them.
    pub(super) fn build(
        reader: &mut Reader,
        ids: &[u64],
        keys: &[String],
        value_texts: impl Fn(&Metadata, &str) -> Vec<String>,
        budget: &Arc<TempBudget>,
    ) -> io::Result<Self> {
        let too_many = || io::Error::other("more than 2^32 keys or values");
        let mut reservation = budget.reserve(0)?;
        let mut ranges = Spool::new()?;
        let mut held = SortedRunCounts::with_policy(SPILL_POLICY, Arc::clone(budget))?;
        for (position, id) in ids.iter().enumerate() {
            let (sentences, metadata) = reader.located(*id)?;
            for bound in [sentences.start, sentences.end] {
                ranges.write(&mut reservation, &bound.to_le_bytes())?;
            }
            for (key, name) in keys.iter().enumerate() {
                let key = u32::try_from(key).map_err(|_| too_many())?;
                for text in value_texts(&metadata, name) {
                    held.add((key, text, position as u64), 1)?;
                }
            }
        }
        let mut values = Spool::new()?;
        let mut value_index = Spool::new()?;
        let mut numbers = Spool::new()?;
        let mut numbered = SortedRunCounts::with_policy(SPILL_POLICY, Arc::clone(budget))?;
        let mut key_slots = keys
            .iter()
            .map(|name| KeySlots {
                name: name.clone(),
                first_value: 0,
                values: 0,
            })
            .collect::<Vec<_>>();
        let (mut written, mut stored) = (0u64, 0u64);
        let mut previous: Option<(u32, String)> = None;
        let mut held = held.into_sorted_drain()?;
        while let Some(((key, text, position), _)) = held.next_entry()? {
            if previous
                .as_ref()
                .is_none_or(|(last, value)| *last != key || *value != text)
            {
                let slots = &mut key_slots[key as usize];
                if slots.values == 0 {
                    slots.first_value = written;
                }
                slots.values = slots.values.checked_add(1).ok_or_else(too_many)?;
                values.write(&mut reservation, text.as_bytes())?;
                for field in [stored, text.len() as u64] {
                    value_index.write(&mut reservation, &field.to_le_bytes())?;
                }
                let number = text
                    .parse::<f64>()
                    .ok()
                    .filter(|number| number.is_finite())
                    .unwrap_or(f64::NAN);
                numbers.write(&mut reservation, &number.to_le_bytes())?;
                stored += text.len() as u64;
                written += 1;
                previous = Some((key, text));
            }
            numbered.add((key, position, key_slots[key as usize].values - 1), 1)?;
        }
        drop(held);
        let per_key = ids.len() as u64 + 1;
        let mut offsets = Spool::new()?;
        let mut slots = Spool::new()?;
        let (mut entries, mut total) = (0u64, 0u64);
        let mut numbered = numbered.into_sorted_drain()?;
        while let Some(((key, position, slot), _)) = numbered.next_entry()? {
            while entries <= u64::from(key) * per_key + position {
                offsets.write(&mut reservation, &total.to_le_bytes())?;
                entries += 1;
            }
            slots.write(&mut reservation, &slot.to_le_bytes())?;
            total += 1;
        }
        while entries < keys.len() as u64 * per_key {
            offsets.write(&mut reservation, &total.to_le_bytes())?;
            entries += 1;
        }
        Ok(Self {
            documents: ids.len(),
            ranges: ranges.finish()?,
            values: values.finish()?,
            value_index: value_index.finish()?,
            numbers: numbers.finish()?,
            offsets: offsets.finish()?,
            slots: slots.finish()?,
            keys: key_slots,
            _reservation: reservation,
        })
    }

    /// The corpus sentences of the document at `position`.
    pub(super) fn sentences(&self, position: usize) -> io::Result<Range<u64>> {
        let [start, end] = read_pair(&self.ranges, position as u64)?;
        Ok(start..end)
    }

    /// The index of `name` among the numbered keys.
    pub(super) fn key(&self, name: &str) -> Option<usize> {
        self.keys.iter().position(|key| key.name == name)
    }

    /// The number of distinct values of key `key`.
    pub(super) fn value_count(&self, key: usize) -> u32 {
        self.keys[key].values
    }

    /// The slots of key `key`'s values in the document at `position`,
    /// ascending; empty for a document without a value.
    pub(super) fn slots(&self, key: usize, position: usize) -> io::Result<Vec<u32>> {
        if position >= self.documents {
            return Err(io::Error::other("document position out of range"));
        }
        let entry = key as u64 * (self.documents as u64 + 1) + position as u64;
        let mut bounds = [0u8; 16];
        self.offsets
            .as_file()
            .read_exact_at(&mut bounds, entry * 8)?;
        let (bounds, _) = bounds.as_chunks::<8>();
        let [start, end] = [bounds[0], bounds[1]].map(u64::from_le_bytes);
        let length = usize::try_from(end - start).map_err(io::Error::other)?;
        let mut bytes = vec![0u8; length * 4];
        self.slots.as_file().read_exact_at(&mut bytes, start * 4)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|slot| u32::from_le_bytes(*slot))
            .collect())
    }

    /// Each value of key `key` read as a finite number, else NaN, by slot.
    pub(super) fn numbers(&self, key: usize) -> io::Result<Vec<f64>> {
        let slots = &self.keys[key];
        let mut bytes = vec![0u8; slots.values as usize * 8];
        self.numbers
            .as_file()
            .read_exact_at(&mut bytes, slots.first_value * 8)?;
        Ok(bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|number| f64::from_le_bytes(*number))
            .collect())
    }

    /// The text of slot `slot` of key `key`.
    pub(super) fn value(&self, key: usize, slot: u32) -> io::Result<String> {
        let slots = &self.keys[key];
        if slot >= slots.values {
            return Err(io::Error::other("value slot out of range"));
        }
        let [offset, length] = read_pair(&self.value_index, slots.first_value + u64::from(slot))?;
        let mut bytes = vec![0u8; usize::try_from(length).map_err(io::Error::other)?];
        self.values.as_file().read_exact_at(&mut bytes, offset)?;
        String::from_utf8(bytes).map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::generators;

    #[hegel::test(test_cases = 40, derandomize = true)]
    fn indexed_metadata_round_trips_in_document_id_order(tc: hegel::TestCase) {
        let generated = tc.draw(generators::vecs(generators::integers::<u64>()).max_size(30));
        let expected = generated.into_iter().map(|id| {
            let text = tc.draw(generators::text());
            let text = text.chars().take(4096).collect::<String>();
            let value = serde_json::json!({"string":text,"number":id,"bool":id%2==0,"list":[text,true,id],"empty":[],"opaque":{"x":null}});
            (id, metadata_from_json(value, "test metadata").expect("metadata"))
        }).collect::<std::collections::BTreeMap<_, _>>();
        let budget = Arc::new(TempBudget::new(64 * 1024 * 1024));
        let mut writer = Writer::new(Arc::clone(&budget)).expect("writer");
        writer.positions = SortedRunCounts::with_policy(
            lingrep_core::spill::SpillPolicy {
                max_pending_keys: 2,
                max_pending_bytes: 1024,
                merge_fan_in: 2,
            },
            Arc::clone(&budget),
        )
        .expect("spill sorter");
        for (id, metadata) in expected.iter().rev() {
            writer.push(*id, 0..*id % 7, metadata).expect("write");
        }
        let store = writer.finish().expect("store");
        let mut first = store.reader().expect("first reader");
        let mut second = store.reader().expect("second reader");
        for (id, metadata) in &expected {
            assert_eq!(
                first.located(*id).expect("first document"),
                (0..*id % 7, metadata.clone())
            );
            assert_eq!(&second.document(*id).expect("second document"), metadata);
        }
        assert!(first.next().expect("end").is_none());
    }

    #[hegel::test(test_cases = 60, derandomize = true)]
    fn table_documents_number_each_keys_values_in_ascending_order(tc: hegel::TestCase) {
        let documents = tc.draw(generators::integers::<usize>().max_value(12));
        let names = ["a", "b", "c"];
        let drawn = (0..documents)
            .map(|_| {
                names
                    .iter()
                    .map(|_| {
                        tc.draw(
                            generators::vecs(generators::sampled_from(vec![
                                "x", "y", "z", "", "7", "-2.5",
                            ]))
                            .max_size(3),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let budget = Arc::new(TempBudget::new(64 * 1024 * 1024));
        let mut writer = Writer::new(Arc::clone(&budget)).expect("writer");
        let ids = (0..documents as u64).map(|id| id * 3).collect::<Vec<_>>();
        for (id, values) in ids.iter().zip(&drawn) {
            let object = names
                .iter()
                .zip(values)
                .map(|(name, values)| ((*name).to_owned(), serde_json::json!(values)))
                .collect::<serde_json::Map<_, _>>();
            let metadata = metadata_from_json(serde_json::Value::Object(object), "test metadata")
                .expect("metadata");
            writer.push(*id, *id..*id + 2, &metadata).expect("write");
        }
        let store = writer.finish().expect("store");
        let keys = names.map(str::to_owned);
        let table = TableDocuments::build(
            &mut store.reader().expect("reader"),
            &ids,
            &keys[..2],
            |metadata, key| {
                metadata
                    .iter()
                    .find(|(name, _)| name == key)
                    .and_then(|(_, value)| value.to_json().as_array().cloned())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|value| value.as_str().map(str::to_owned))
                    .collect()
            },
            &budget,
        )
        .expect("table documents");
        assert_eq!(table.key("c"), None);
        for (key, name) in keys[..2].iter().enumerate() {
            assert_eq!(table.key(name), Some(key));
            let vocabulary = drawn
                .iter()
                .flat_map(|values| values[key].iter().copied())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            assert_eq!(table.value_count(key) as usize, vocabulary.len());
            for (slot, value) in vocabulary.iter().enumerate() {
                assert_eq!(table.value(key, slot as u32).expect("value"), *value);
            }
            let numbers = table.numbers(key).expect("numbers");
            assert_eq!(numbers.len(), vocabulary.len());
            for (number, value) in numbers.iter().zip(&vocabulary) {
                match value.parse::<f64>() {
                    Ok(parsed) => assert_eq!(*number, parsed),
                    Err(_) => assert!(number.is_nan()),
                }
            }
            for (position, values) in drawn.iter().enumerate() {
                let mut expected = values[key]
                    .iter()
                    .map(|value| vocabulary.binary_search(value).expect("listed") as u32)
                    .collect::<Vec<_>>();
                expected.sort_unstable();
                expected.dedup();
                assert_eq!(table.slots(key, position).expect("slots"), expected);
            }
        }
        for (position, id) in ids.iter().enumerate() {
            assert_eq!(table.sentences(position).expect("sentences"), *id..*id + 2);
        }
        assert!(table.slots(0, documents).is_err());
        let held = budget.used_bytes();
        drop(table);
        assert!(budget.used_bytes() < held || documents == 0);
    }

    #[test]
    fn temporary_metadata_is_removed_and_its_budget_released() {
        let budget = Arc::new(TempBudget::new(1_000_000));
        let empty: Metadata = Box::new([]);
        let mut writer = Writer::new(Arc::clone(&budget)).expect("writer");
        writer.push(8, 0..1, &empty).expect("write");
        let store = writer.finish().expect("store");
        let paths = [store.data.path().to_owned(), store.index.path().to_owned()];
        assert!(budget.used_bytes() > 0);
        drop(store);
        assert_eq!(budget.used_bytes(), 0);
        assert!(paths.iter().all(|path| !path.exists()));
        let mut duplicate = Writer::new(Arc::clone(&budget)).expect("writer");
        duplicate.push(8, 0..1, &empty).expect("write");
        duplicate.push(8, 0..1, &empty).expect("write");
        assert!(duplicate.finish().is_err());
        assert_eq!(budget.used_bytes(), 0);
        let small = Arc::new(TempBudget::new(1));
        assert!(
            Writer::new(Arc::clone(&small))
                .expect("writer")
                .push(0, 0..1, &empty)
                .is_err()
        );
        assert_eq!(small.used_bytes(), 0);
    }
}
