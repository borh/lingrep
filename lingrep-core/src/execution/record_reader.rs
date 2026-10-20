//! A bounded reader over newline-delimited physical input records.
//!
//! One *record* is one line of a corpus source: a JSONL document, a plaintext
//! line, or a Markdown line. Its size is a property of the corpus a researcher
//! was given, and is independent of
//! [`ExecutionLimits::max_stage_input_bytes`](super::ExecutionLimits::max_stage_input_bytes),
//! which budgets how many raw bytes of *already admitted sentences* one
//! physical execution batch retains. A document larger than one batch is
//! ordinary; it simply feeds several batches. The two bounds are therefore
//! carried separately, and the record bound defaults to
//! [`DEFAULT_MAX_INPUT_RECORD_BYTES`](super::DEFAULT_MAX_INPUT_RECORD_BYTES).

use std::io::{self, BufRead, Read};
use std::sync::{Arc, Mutex};

/// A physical input record proved larger than its configured bound.
///
/// The reader stops as soon as the bound is exceeded, so `actual_at_least` is a
/// proved lower bound rather than the record's true length: reading further to
/// report an exact size is precisely the unbounded allocation the limit exists
/// to prevent.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "input record {record_ordinal} from {source_name} is at least {actual_at_least} bytes; \
     limit is {limit}"
)]
pub struct BoundedRecordError {
    /// Logical name of the source document the record came from.
    pub source_name: String,
    /// Zero-based ordinal of the rejected record within that document.
    pub record_ordinal: u64,
    /// Smallest byte length the reader proved before stopping.
    pub actual_at_least: usize,
    /// The configured upper bound.
    pub limit: usize,
}

/// The high-water mark of bytes one [`BoundedRecordReader`] retained.
///
/// Shared with the reader's owner so a run can report observed residency
/// without borrowing the reader itself.
#[derive(Clone, Default)]
pub struct BoundedRecordStats {
    max_retained_bytes: Arc<Mutex<usize>>,
}

impl BoundedRecordStats {
    /// Returns the largest record buffer observed so far, or `0` if the
    /// statistics lock was poisoned.
    #[must_use]
    pub fn max_retained_bytes(&self) -> usize {
        self.max_retained_bytes
            .lock()
            .map_or(0, |retained| *retained)
    }
}

/// A newline-record buffer that proves oversize with at most `limit + 1`
/// retained bytes before any part of that record reaches [`BufRead::lines`].
pub struct BoundedRecordReader<R> {
    reader: R,
    record: Vec<u8>,
    position: usize,
    limit: usize,
    source_name: String,
    next_record_ordinal: u64,
    terminal_error: Option<BoundedRecordError>,
    stats: BoundedRecordStats,
}

impl<R: BufRead> BoundedRecordReader<R> {
    /// Wraps `reader`, bounding each newline-delimited record at `limit` bytes.
    ///
    /// `source_name` is the logical document name a rejection names, so a
    /// failure identifies the file as well as the record within it.
    pub fn new(
        reader: R,
        limit: usize,
        source_name: impl Into<String>,
    ) -> (Self, BoundedRecordStats) {
        let stats = BoundedRecordStats::default();
        (
            Self {
                reader,
                record: Vec::with_capacity(limit.min(8 * 1024).saturating_add(1)),
                position: 0,
                limit,
                source_name: source_name.into(),
                next_record_ordinal: 0,
                terminal_error: None,
                stats: stats.clone(),
            },
            stats,
        )
    }

    fn fill_record(&mut self) -> io::Result<()> {
        if self.position < self.record.len() {
            return Ok(());
        }
        if let Some(error) = &self.terminal_error {
            return Err(io::Error::new(io::ErrorKind::InvalidData, error.clone()));
        }
        self.record.clear();
        self.position = 0;

        loop {
            let (copy, newline, oversized) = {
                let available = self.reader.fill_buf()?;
                if available.is_empty() {
                    if !self.record.is_empty() {
                        self.finish_record()?;
                    }
                    return Ok(());
                }
                let remaining = self.limit.saturating_sub(self.record.len());
                let inspected = available.len().min(remaining.saturating_add(1));
                match available[..inspected]
                    .iter()
                    .position(|byte| *byte == b'\n')
                {
                    Some(index) => (index, true, false),
                    None if available.len() > remaining => {
                        (remaining.saturating_add(1), false, true)
                    }
                    None => (available.len(), false, false),
                }
            };

            let available = self.reader.fill_buf()?;
            self.record.extend_from_slice(&available[..copy]);
            self.reader.consume(copy + usize::from(newline));
            self.observe_retained()?;
            if oversized {
                let error = BoundedRecordError {
                    source_name: self.source_name.clone(),
                    record_ordinal: self.next_record_ordinal,
                    actual_at_least: self.limit.saturating_add(1),
                    limit: self.limit,
                };
                self.terminal_error = Some(error.clone());
                return Err(io::Error::new(io::ErrorKind::InvalidData, error));
            }
            if newline {
                self.record.push(b'\n');
                self.observe_retained()?;
                self.finish_record()?;
                return Ok(());
            }
        }
    }

    fn finish_record(&mut self) -> io::Result<()> {
        self.next_record_ordinal = self
            .next_record_ordinal
            .checked_add(1)
            .ok_or_else(|| io::Error::other("input record ordinal overflow"))?;
        Ok(())
    }

    fn observe_retained(&self) -> io::Result<()> {
        let mut max_retained = self
            .stats
            .max_retained_bytes
            .lock()
            .map_err(|_| io::Error::other("bounded record statistics lock poisoned"))?;
        *max_retained = (*max_retained).max(self.record.len());
        Ok(())
    }
}

impl<R: BufRead> Read for BoundedRecordReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let available = self.fill_buf()?;
        let read = available.len().min(output.len());
        output[..read].copy_from_slice(&available[..read]);
        self.consume(read);
        Ok(read)
    }
}

impl<R: BufRead> BufRead for BoundedRecordReader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.fill_record()?;
        Ok(&self.record[self.position..])
    }

    fn consume(&mut self, amount: usize) {
        self.position = self.position.saturating_add(amount).min(self.record.len());
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor, Read};

    use super::{BoundedRecordError, BoundedRecordReader};

    fn typed_error(error: &std::io::Error) -> &BoundedRecordError {
        error
            .get_ref()
            .and_then(|error| error.downcast_ref::<BoundedRecordError>())
            .expect("typed bounded-record error")
    }

    #[test]
    fn rejects_unterminated_records_without_retaining_them() {
        for source_name in ["plain.txt", "document.md", "documents.jsonl"] {
            let input = vec![b'x'; 64];
            let reader = BufReader::with_capacity(3, Cursor::new(input));
            let (mut bounded, stats) = BoundedRecordReader::new(reader, 8, source_name);
            let mut exposed = String::new();

            let error = bounded.read_to_string(&mut exposed).unwrap_err();
            let error = typed_error(&error);

            assert_eq!(error.source_name, source_name);
            assert_eq!(error.record_ordinal, 0);
            assert_eq!(error.actual_at_least, 9);
            assert_eq!(error.limit, 8);
            assert!(exposed.is_empty());
            assert!(stats.max_retained_bytes() <= 9);
        }
    }

    #[test]
    fn preserves_normal_multi_sentence_records() {
        let input = "猫。犬。\n鳥。\n";
        let reader = BufReader::with_capacity(2, Cursor::new(input.as_bytes()));
        let (mut bounded, stats) = BoundedRecordReader::new(reader, 16, "plain.txt");
        let mut output = String::new();

        bounded.read_to_string(&mut output).unwrap();

        assert_eq!(output, input);
        assert!(stats.max_retained_bytes() <= 17);
    }

    #[test]
    fn names_the_ordinal_of_the_oversized_record_not_the_first() {
        let input = "ok\nok\nxxxxxxxxxxxx\n";
        let reader = BufReader::with_capacity(4, Cursor::new(input.as_bytes()));
        let (mut bounded, _) = BoundedRecordReader::new(reader, 8, "documents.jsonl");
        let mut output = String::new();

        let error = bounded.read_to_string(&mut output).unwrap_err();
        let error = typed_error(&error);

        assert_eq!(error.record_ordinal, 2);
        assert_eq!(error.source_name, "documents.jsonl");
    }

    #[test]
    fn admits_a_record_larger_than_a_stage_batch_budget() {
        // The bound a record is measured against is its own, not the batch
        // budget: a document may exceed one execution batch and simply feed
        // several. `limit` here is larger than DEFAULT_MAX_STAGE_INPUT_BYTES
        // would be, relative to the record.
        let record = "あ".repeat(4096);
        let input = format!("{record}\n");
        let reader = BufReader::with_capacity(64, Cursor::new(input.clone().into_bytes()));
        let (mut bounded, stats) = BoundedRecordReader::new(reader, 64 * 1024, "big.jsonl");
        let mut output = String::new();

        bounded.read_to_string(&mut output).unwrap();

        assert_eq!(output, input);
        assert!(stats.max_retained_bytes() >= record.len());
    }
}
