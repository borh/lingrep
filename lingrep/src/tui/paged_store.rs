use std::io::{self, Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use lingrep_core::spill::{BudgetedWriter, TempBudget, TempReservation};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tempfile::NamedTempFile;

pub(crate) const DEFAULT_PAGE_ROWS: usize = 256;
const FRAME_BYTES: u64 = std::mem::size_of::<u64>() as u64;
const INDEX_ENTRY_BYTES: u64 = std::mem::size_of::<u64>() as u64;

pub(crate) struct PagedStoreWriter<T> {
    data: BudgetedWriter<NamedTempFile>,
    index: BudgetedWriter<NamedTempFile>,
    rows: u64,
    data_offset: u64,
    _row: PhantomData<fn() -> T>,
}

impl<T> PagedStoreWriter<T>
where
    T: Serialize,
{
    pub(crate) fn new(budget: Arc<TempBudget>) -> io::Result<Self> {
        Ok(Self {
            data: BudgetedWriter::new(NamedTempFile::new()?, budget.reserve(0)?),
            index: BudgetedWriter::new(NamedTempFile::new()?, budget.reserve(0)?),
            rows: 0,
            data_offset: 0,
            _row: PhantomData,
        })
    }

    pub(crate) fn push(&mut self, row: &T) -> io::Result<()> {
        if self.rows.is_multiple_of(DEFAULT_PAGE_ROWS as u64) {
            self.index.write_all(&self.data_offset.to_le_bytes())?;
        }
        let encoded = serde_json::to_vec(row)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let encoded_len = u64::try_from(encoded.len())
            .map_err(|_| io::Error::other("paged row exceeds the addressable size"))?;
        self.data.write_all(&encoded_len.to_le_bytes())?;
        self.data.write_all(&encoded)?;
        self.data_offset = self
            .data_offset
            .checked_add(FRAME_BYTES)
            .and_then(|bytes| bytes.checked_add(encoded_len))
            .ok_or_else(|| io::Error::other("paged data offset overflow"))?;
        self.rows = self
            .rows
            .checked_add(1)
            .ok_or_else(|| io::Error::other("paged row count overflow"))?;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> io::Result<Arc<PagedStore<T>>> {
        self.data.flush()?;
        self.index.flush()?;
        let (data, data_reservation) = self.data.into_parts();
        let (index, index_reservation) = self.index.into_parts();
        Ok(Arc::new(PagedStore {
            inner: PagedStoreInner {
                data: Mutex::new(data),
                index: Mutex::new(index),
                data_reservation,
                index_reservation,
                data_len: self.data_offset,
                index_len: self
                    .rows
                    .div_ceil(DEFAULT_PAGE_ROWS as u64)
                    .checked_mul(INDEX_ENTRY_BYTES)
                    .ok_or_else(|| io::Error::other("paged index length overflow"))?,
                rows: self.rows,
            },
            _row: PhantomData,
        }))
    }
}

pub(crate) struct PagedStore<T> {
    inner: PagedStoreInner,
    _row: PhantomData<fn() -> T>,
}

struct PagedStoreInner {
    data: Mutex<NamedTempFile>,
    index: Mutex<NamedTempFile>,
    // Reservations deliberately live beside their files. Dropping the last
    // Arc closes/unlinks both files and returns both charges atomically from
    // the explorer's perspective.
    data_reservation: TempReservation,
    index_reservation: TempReservation,
    data_len: u64,
    index_len: u64,
    rows: u64,
}

impl<T> std::fmt::Debug for PagedStore<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PagedStore")
            .field("rows", &self.inner.rows)
            .field("data_bytes", &self.inner.data_reservation.held_bytes())
            .field("index_bytes", &self.inner.index_reservation.held_bytes())
            .finish()
    }
}

impl<T> PagedStore<T>
where
    T: DeserializeOwned,
{
    pub(crate) fn len(&self) -> u64 {
        self.inner.rows
    }

    #[cfg(test)]
    pub(crate) fn read_page(&self, page: u64) -> io::Result<Vec<T>> {
        let start = page
            .checked_mul(DEFAULT_PAGE_ROWS as u64)
            .ok_or_else(|| io::Error::other("page offset overflow"))?;
        self.read_window(start, DEFAULT_PAGE_ROWS)
    }

    pub(crate) fn read_window(&self, start: u64, requested: usize) -> io::Result<Vec<T>> {
        if start >= self.inner.rows || requested == 0 {
            return Ok(Vec::new());
        }
        let available = self.inner.rows - start;
        let row_count = usize::try_from(available.min(requested as u64))
            .map_err(|_| corruption("window row count exceeds usize"))?;
        let page = start / DEFAULT_PAGE_ROWS as u64;
        let skip = start % DEFAULT_PAGE_ROWS as u64;

        let mut index = self
            .inner
            .index
            .lock()
            .map_err(|_| io::Error::other("paged index lock poisoned"))?;
        if index.as_file().metadata()?.len() != self.inner.index_len {
            return Err(corruption("paged index length changed after finalization"));
        }
        let index_offset = page
            .checked_mul(INDEX_ENTRY_BYTES)
            .ok_or_else(|| corruption("paged index offset overflow"))?;
        let index_end = index_offset
            .checked_add(INDEX_ENTRY_BYTES)
            .ok_or_else(|| corruption("paged index end overflow"))?;
        if index_end > self.inner.index_len {
            return Err(corruption("paged index offset is outside the index file"));
        }
        index.as_file_mut().seek(SeekFrom::Start(index_offset))?;
        let mut offset = [0_u8; std::mem::size_of::<u64>()];
        index.as_file_mut().read_exact(&mut offset)?;
        drop(index);
        let mut cursor = u64::from_le_bytes(offset);
        if cursor > self.inner.data_len {
            return Err(corruption("paged data offset is outside the data file"));
        }

        let mut data = self
            .inner
            .data
            .lock()
            .map_err(|_| io::Error::other("paged data lock poisoned"))?;
        if data.as_file().metadata()?.len() != self.inner.data_len {
            return Err(corruption("paged data length changed after finalization"));
        }
        for _ in 0..skip {
            let (_, end) = frame_bounds(data.as_file_mut(), cursor, self.inner.data_len)?;
            cursor = end;
        }

        let mut rows = Vec::with_capacity(row_count);
        for _ in 0..row_count {
            let (length, end) = frame_bounds(data.as_file_mut(), cursor, self.inner.data_len)?;
            let length = usize::try_from(length)
                .map_err(|_| corruption("paged row is too large for this platform"))?;
            let mut encoded = vec![0_u8; length];
            data.as_file_mut().read_exact(&mut encoded)?;
            rows.push(
                serde_json::from_slice(&encoded)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            );
            cursor = end;
        }
        Ok(rows)
    }
}

#[derive(Debug)]
struct PagedStoreCorruption(&'static str);

impl std::fmt::Display for PagedStoreCorruption {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "paged result store is corrupt: {}", self.0)
    }
}

impl std::error::Error for PagedStoreCorruption {}

fn corruption(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, PagedStoreCorruption(reason))
}

fn frame_bounds(file: &mut std::fs::File, offset: u64, data_len: u64) -> io::Result<(u64, u64)> {
    let header_end = offset
        .checked_add(FRAME_BYTES)
        .ok_or_else(|| corruption("paged frame header offset overflow"))?;
    if header_end > data_len {
        return Err(corruption("paged frame header is truncated"));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut length = [0_u8; std::mem::size_of::<u64>()];
    file.read_exact(&mut length)?;
    let length = u64::from_le_bytes(length);
    // This check is deliberately before usize conversion/allocation.
    let frame_end = header_end
        .checked_add(length)
        .ok_or_else(|| corruption("paged frame length overflow"))?;
    if length > data_len || frame_end > data_len {
        return Err(corruption("paged frame extends beyond the data file"));
    }
    Ok((length, frame_end))
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::Arc;

    use lingrep_core::spill::TempBudget;
    use serde::{Deserialize, Serialize};

    use super::{DEFAULT_PAGE_ROWS, PagedStoreWriter};

    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Row {
        value: usize,
    }

    #[test]
    fn high_cardinality_store_reads_exact_bounded_pages() {
        let budget = Arc::new(TempBudget::new(u64::MAX));
        let mut writer = PagedStoreWriter::new(budget.clone()).unwrap();
        for value in 0..(DEFAULT_PAGE_ROWS * 3 + 17) {
            writer.push(&Row { value }).unwrap();
        }
        let store = writer.finish().unwrap();

        assert_eq!(store.len(), (DEFAULT_PAGE_ROWS * 3 + 17) as u64);
        let page = store.read_page(2).unwrap();
        assert_eq!(page.len(), DEFAULT_PAGE_ROWS);
        assert_eq!(
            page.first(),
            Some(&Row {
                value: DEFAULT_PAGE_ROWS * 2
            })
        );
        assert_eq!(
            page.last(),
            Some(&Row {
                value: DEFAULT_PAGE_ROWS * 3 - 1
            })
        );
        assert!(budget.peak_used_bytes() >= budget.used_bytes());
    }

    #[test]
    fn data_and_index_share_the_same_typed_budget() {
        let measured = Arc::new(TempBudget::new(u64::MAX));
        let mut writer = PagedStoreWriter::new(measured.clone()).unwrap();
        for value in 0..=DEFAULT_PAGE_ROWS {
            writer.push(&Row { value }).unwrap();
        }
        let store = writer.finish().unwrap();
        let exact_bytes = measured.used_bytes();
        drop(store);
        assert_eq!(measured.used_bytes(), 0);

        let budget = Arc::new(TempBudget::new(exact_bytes - 1));
        let mut writer = PagedStoreWriter::new(budget.clone()).unwrap();
        let error = (0..=DEFAULT_PAGE_ROWS)
            .find_map(|value| writer.push(&Row { value }).err())
            .expect("combined data and index must exhaust the shared budget");
        assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
        drop(writer);
        assert_eq!(budget.used_bytes(), 0);
    }

    #[test]
    fn windows_cross_page_boundaries_and_clamp_at_the_exact_end() {
        let budget = Arc::new(TempBudget::new(u64::MAX));
        let mut writer = PagedStoreWriter::new(budget).unwrap();
        for value in 0..(DEFAULT_PAGE_ROWS + 3) {
            writer.push(&Row { value }).unwrap();
        }
        let store = writer.finish().unwrap();

        assert_eq!(
            store.read_window(DEFAULT_PAGE_ROWS as u64 - 1, 3).unwrap(),
            [Row { value: 255 }, Row { value: 256 }, Row { value: 257 }]
        );
        assert_eq!(
            store.read_window(DEFAULT_PAGE_ROWS as u64 + 1, 99).unwrap(),
            [Row { value: 257 }, Row { value: 258 }]
        );
        assert!(store.read_window(store.len(), 4).unwrap().is_empty());
    }

    #[test]
    fn forged_lengths_offsets_and_truncation_fail_before_allocation() {
        fn store() -> Arc<super::PagedStore<Row>> {
            let budget = Arc::new(TempBudget::new(u64::MAX));
            let mut writer = PagedStoreWriter::new(budget).unwrap();
            writer.push(&Row { value: 1 }).unwrap();
            writer.finish().unwrap()
        }

        let huge = store();
        let mut data = huge.inner.data.lock().unwrap();
        data.as_file_mut().seek(SeekFrom::Start(0)).unwrap();
        data.as_file_mut()
            .write_all(&u64::MAX.to_le_bytes())
            .unwrap();
        drop(data);
        assert_eq!(
            huge.read_window(0, 1).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );

        let bad_offset = store();
        let invalid_offset = bad_offset.inner.data_len + 1;
        let mut index = bad_offset.inner.index.lock().unwrap();
        index.as_file_mut().seek(SeekFrom::Start(0)).unwrap();
        index
            .as_file_mut()
            .write_all(&invalid_offset.to_le_bytes())
            .unwrap();
        drop(index);
        assert_eq!(
            bad_offset.read_window(0, 1).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );

        let truncated = store();
        truncated
            .inner
            .data
            .lock()
            .unwrap()
            .as_file_mut()
            .set_len(truncated.inner.data_len - 1)
            .unwrap();
        assert_eq!(
            truncated.read_window(0, 1).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }
}
