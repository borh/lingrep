use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
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
    positions: SortedRunCounts<(u64, u64, u64)>,
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

    pub(super) fn push(&mut self, id: u64, metadata: &Metadata) -> io::Result<()> {
        let object = metadata
            .iter()
            .map(|(key, value)| (key.clone(), value.to_json()))
            .collect::<serde_json::Map<_, _>>();
        let bytes = serde_json::to_vec(&object)?;
        let offset = self.data.as_file_mut().stream_position()?;
        self.reservation.grow(bytes.len() as u64)?;
        self.data.write_all(&bytes)?;
        self.positions.add((id, offset, bytes.len() as u64), 1)
    }

    pub(super) fn finish(mut self) -> io::Result<Documents> {
        let index = NamedTempFile::new()?;
        let mut writer = BufWriter::new(index.as_file());
        let mut rows = self.positions.into_sorted_drain()?;
        let mut previous = None;
        while let Some(((id, offset, len), count)) = rows.next_entry()? {
            if previous == Some(id) || count != 1 {
                return Err(io::Error::other("duplicate document metadata ID"));
            }
            previous = Some(id);
            self.reservation.grow(24)?;
            for value in [id, offset, len] {
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
    fn position(&mut self) -> io::Result<Option<[u64; 3]>> {
        let mut bytes = [0u8; 24];
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
                let Some([id, _, _]) = self.position()? else {
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
        loop {
            let Some([id, offset, len]) = self.position()? else {
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
            return metadata_from_json(serde_json::from_slice(&bytes)?, "stored document metadata");
        }
    }

    pub(super) fn next(&mut self) -> io::Result<Option<Metadata>> {
        let Some([_, offset, len]) = self.position()? else {
            return Ok(None);
        };
        self.data.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; usize::try_from(len).map_err(io::Error::other)?];
        self.data.read_exact(&mut bytes)?;
        metadata_from_json(serde_json::from_slice(&bytes)?, "stored document metadata").map(Some)
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
            writer.push(*id, metadata).expect("write");
        }
        let store = writer.finish().expect("store");
        let mut first = store.reader().expect("first reader");
        let mut second = store.reader().expect("second reader");
        for (id, metadata) in &expected {
            assert_eq!(&first.document(*id).expect("first document"), metadata);
            assert_eq!(&second.document(*id).expect("second document"), metadata);
        }
        assert!(first.next().expect("end").is_none());
    }

    #[test]
    fn temporary_metadata_is_removed_and_its_budget_released() {
        let budget = Arc::new(TempBudget::new(1_000_000));
        let empty: Metadata = Box::new([]);
        let mut writer = Writer::new(Arc::clone(&budget)).expect("writer");
        writer.push(8, &empty).expect("write");
        let store = writer.finish().expect("store");
        let paths = [store.data.path().to_owned(), store.index.path().to_owned()];
        assert!(budget.used_bytes() > 0);
        drop(store);
        assert_eq!(budget.used_bytes(), 0);
        assert!(paths.iter().all(|path| !path.exists()));
        let mut duplicate = Writer::new(Arc::clone(&budget)).expect("writer");
        duplicate.push(8, &empty).expect("write");
        duplicate.push(8, &empty).expect("write");
        assert!(duplicate.finish().is_err());
        assert_eq!(budget.used_bytes(), 0);
        let small = Arc::new(TempBudget::new(1));
        assert!(
            Writer::new(Arc::clone(&small))
                .expect("writer")
                .push(0, &empty)
                .is_err()
        );
        assert_eq!(small.used_bytes(), 0);
    }
}
