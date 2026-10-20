//! Soranoha bulk plaintext archives: one ZIP per release selection, with the
//! bibliography beside the texts.
//!
//! Soranoha publishes a bulk archive for the whole corpus and for each
//! smaller selection (one author, one NDC class or subclass, the children's
//! material). Every archive has the same shape: `catalog.csv` at the root and
//! one plaintext file per work beside it, so the archive is self-describing
//! and a work can be found, sized and cited without opening a text.
//!
//! The archive is one discovered source, exactly as a JSONL file is; each
//! data entry is one admitted document. Entry order is byte-sorted by member
//! name, so a document's ordinal is a property of the set of works rather than
//! of the ZIP's physical order. The archive's bytes are therefore the whole
//! source identity, bibliography included: a catalog edit that leaves every
//! text unchanged still names a different corpus.
//!
//! The joined catalog is held for the archive's lifetime — one metadata record
//! per work, never per sentence — while each work is inflated, split and
//! dropped one at a time, so text-bearing memory follows the largest work and
//! only the metadata table grows with the work count.
//!
//! # The catalog
//!
//! `catalog.csv` is written with a UTF-8 byte-order mark, CRLF records and
//! every field quoted ([`CATALOG_COLUMNS`] is the pinned header). Each row is
//! joined to its entry by `filename`. The row's columns become the document's
//! `@document.*` metadata verbatim, so `--where '@document.author = …'` and
//! `--source-metadata @document.identifier` reach the bibliography without a
//! second file.
//!
//! - An empty field is **omitted**, not stored as an empty string: `subtitle`,
//!   `first_published`, `ndc`, the source-edition fields and `doi` are absent
//!   from many works, and "not recorded" is not "recorded as empty".
//! - `body_character_count` and `source_edition_year` are typed as numbers;
//!   every other column, `identifier` included, is a string. The identifier
//!   keeps its leading zeros.
//! - `identifier`, `filename` and `release` are required on every row, and
//!   the release is one value across the archive: an archive is a release
//!   selection, so a mixed one is corrupt rather than merely odd.
//! - Columns added by a later catalog version are carried as string metadata
//!   instead of being refused, so a reader of this release keeps working on
//!   the next one. A *missing* pinned column is an error, because then the
//!   join or a citation key could not be read.
//!
//! # What the text is
//!
//! The plaintext projection is reading text only: ruby readings, editorial
//! notes and apparatus are absent, and source indentation is preserved as
//! U+3000. Lines are Aozora Bunko's physical line breaks rather than authored
//! paragraphs, so `paragraph` structure is reported unavailable and sentences
//! are framed from each line with the same Japanese delimiter rule as the
//! `plain` format. Reading the same member as `plain` and as part of a
//! Soranoha archive therefore admits the same sentence texts.
//!
//! The TEI archive is not read here: its members are refused by name, and the
//! plaintext archive is the projection this format reads.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Read, Seek};

use super::structured::{CorpusEvent, Metadata, metadata_from_json};
use super::zip::zip_entry_names;
use crate::utils::split_sentence_ja;
use zip::ZipArchive;

/// The archive member holding the bibliography.
pub const CATALOG_MEMBER: &str = "catalog.csv";

/// The pinned `catalog.csv` header: the columns a row must carry for its work
/// to be joined, sized and cited.
pub const CATALOG_COLUMNS: [&str; 17] = [
    "identifier",
    "title",
    "subtitle",
    "title_reading",
    "author",
    "orthographic_style",
    "ndc",
    "first_published",
    "body_character_count",
    "source_edition_title",
    "source_edition_publisher",
    "source_edition_year",
    "filename",
    "source_content_hash",
    "url",
    "release",
    "doi",
];

/// Columns whose values are non-negative integers. They are typed for
/// ordered comparison; a value that does not parse is a catalog error.
const NUMERIC_COLUMNS: [&str; 2] = ["body_character_count", "source_edition_year"];

/// The bound on `catalog.csv` itself.
///
/// The catalog is metadata *about* the archive's records, not one of them, so
/// the per-record bound the run carries cannot apply: a whole-corpus catalog
/// is several megabytes on its own. The bound still exists so a decompression
/// bomb cannot arrive disguised as a catalog, and it is generous enough for an
/// order of magnitude more works than the corpus holds.
const MAX_CATALOG_BYTES: u64 = 64 * 1024 * 1024;

/// The per-archive `CorpusEvent` stream over a caller-owned handle.
///
/// Construction reads and validates the catalog and joins it to the entry
/// list, so an archive that cannot be read fails before the first sentence is
/// tokenized. It opens nothing and never reopens: the seam drains the file for
/// its content observation first, records the file stat, rewinds, and hands
/// the same handle over.
pub struct SoranohaEvents<R: Read + Seek> {
    zip: ZipArchive<R>,
    /// The data entries as `(name, index)`, byte-sorted by name.
    entries: Vec<(String, usize)>,
    position: usize,
    pending: VecDeque<CorpusEvent>,
    /// The joined catalog, keyed by member name.
    rows: HashMap<String, Metadata>,
    source_name: String,
    /// The run's `max_input_record_bytes`, applied per work.
    max_entry_bytes: usize,
}

impl<R: Read + Seek> SoranohaEvents<R> {
    /// Opens one archive and validates its bibliography.
    ///
    /// `max_entry_bytes` is the run's `--max-record-bytes`; a work whose
    /// declared uncompressed size exceeds it is refused before a byte is
    /// decompressed, and the read itself is bounded so an entry that
    /// under-declares its size cannot outgrow the limit either.
    ///
    /// # Errors
    ///
    /// The handle is not a ZIP container, the archive has no `catalog.csv` or
    /// no work entries, the catalog is malformed, a work has no row, two rows
    /// name one file or one identifier, rows disagree about the release, or a
    /// member belongs to a TEI archive.
    pub fn new(reader: R, source_name: String, max_entry_bytes: usize) -> io::Result<Self> {
        let mut zip = ZipArchive::new(reader).map_err(|error| {
            invalid(format!(
                "soranoha archive {source_name}: not a readable ZIP archive: {error}"
            ))
        })?;
        let names = zip_entry_names(&zip);
        if !names.iter().any(|name| name == CATALOG_MEMBER) {
            return Err(invalid(format!(
                "soranoha archive {source_name}: no {CATALOG_MEMBER} at the root"
            )));
        }
        let catalog = read_catalog(&mut zip, &source_name)?;
        let rows = parse_catalog(&catalog, &source_name)?;
        let entries: Vec<(String, usize)> = names
            .into_iter()
            .filter(|name| name != CATALOG_MEMBER)
            .map(|name| {
                let index = zip
                    .index_for_name(&name)
                    .expect("the name came from this archive");
                (name, index)
            })
            .collect();
        if entries.is_empty() {
            return Err(invalid(format!(
                "soranoha archive {source_name}: only {CATALOG_MEMBER} is present, so there is no work to read"
            )));
        }
        for (name, _) in &entries {
            if name.to_ascii_lowercase().ends_with(".xml") {
                return Err(invalid(format!(
                    "soranoha archive {source_name}: entry {name} is TEI, which this reader does not read; use the archive built from the plaintext projection"
                )));
            }
            if !rows.contains_key(name) {
                return Err(invalid(format!(
                    "soranoha archive {source_name}: entry {name} has no {CATALOG_MEMBER} row"
                )));
            }
        }
        Ok(Self {
            zip,
            entries,
            position: 0,
            pending: VecDeque::new(),
            rows,
            source_name,
            max_entry_bytes,
        })
    }

    fn load_document(&mut self, name: &str, index: usize) -> io::Result<CorpusEvent> {
        let context = |message: String| {
            invalid(format!(
                "soranoha archive {}: entry {name}: {message}",
                self.source_name
            ))
        };
        let limit = self.max_entry_bytes;
        let mut entry = self
            .zip
            .by_index(index)
            .map_err(|error| context(format!("unreadable ZIP entry: {error}")))?;
        // The declared uncompressed size refuses an oversized work — and a
        // decompression bomb — before anything is inflated. The bounded read
        // then covers an entry that under-declares its size.
        let declared = entry.size();
        if declared > limit as u64 {
            return Err(context(oversized(declared.to_string(), limit)));
        }
        let mut bytes = Vec::with_capacity(usize::try_from(declared).unwrap_or(0));
        entry
            .by_ref()
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| context(format!("unreadable ZIP entry: {error}")))?;
        if bytes.len() > limit {
            return Err(context(oversized(format!("more than {limit}"), limit)));
        }
        drop(entry);
        let text =
            String::from_utf8(bytes).map_err(|error| context(format!("is not UTF-8: {error}")))?;
        let metadata = self
            .rows
            .get(name)
            .cloned()
            .expect("every data entry was joined at construction");
        let mut events = Vec::new();
        events.push(CorpusEvent::DocumentStart {
            source_name: name.to_owned(),
            metadata,
        });
        events.push(CorpusEvent::DivisionObservation {
            kind: "paragraph".into(),
            observation: super::DivisionObservation::Unavailable {
                reason: "soranoha plaintext preserves Aozora Bunko line breaks and declares no authored paragraph boundaries".into(),
            },
        });
        for line in text.lines() {
            for segment in split_sentence_ja(line) {
                events.push(CorpusEvent::Sentence {
                    text: segment,
                    words: None,
                    enhanced_graph: None,
                    active_labels: Box::new([]),
                    metadata: Box::new([]),
                });
            }
        }
        events.push(CorpusEvent::DocumentEnd);
        self.pending.extend(events);
        Ok(self
            .pending
            .pop_front()
            .expect("the document events were just queued"))
    }
}

impl<R: Read + Seek> Iterator for SoranohaEvents<R> {
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(event) = self.pending.pop_front() {
            return Some(Ok(event));
        }
        if self.position >= self.entries.len() {
            return None;
        }
        let (name, index) = self.entries[self.position].clone();
        self.position += 1;
        Some(self.load_document(&name, index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        // One document is at least two events (Start/End); the exact count is
        // not knowable before the entries parse.
        (
            self.entries
                .len()
                .saturating_sub(self.position)
                .saturating_mul(2),
            None,
        )
    }
}

fn oversized(observed: String, limit: usize) -> String {
    format!(
        "is {observed} bytes, over the {limit}-byte input-record limit; raise --max-record-bytes to read it"
    )
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn catalog_error(source_name: &str, message: String) -> io::Error {
    invalid(format!(
        "soranoha archive {source_name}: {CATALOG_MEMBER} {message}"
    ))
}

fn read_catalog<R: Read + Seek>(zip: &mut ZipArchive<R>, source_name: &str) -> io::Result<Vec<u8>> {
    let index = zip
        .index_for_name(CATALOG_MEMBER)
        .expect("the catalog presence was checked");
    let mut entry = zip
        .by_index(index)
        .map_err(|error| catalog_error(source_name, format!("is unreadable: {error}")))?;
    let declared = entry.size();
    if declared > MAX_CATALOG_BYTES {
        return Err(catalog_error(
            source_name,
            format!("is {declared} bytes, over the {MAX_CATALOG_BYTES}-byte catalog limit"),
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(declared).unwrap_or(0));
    entry
        .by_ref()
        .take(MAX_CATALOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| catalog_error(source_name, format!("is unreadable: {error}")))?;
    if bytes.len() as u64 > MAX_CATALOG_BYTES {
        return Err(catalog_error(
            source_name,
            format!("is more than {MAX_CATALOG_BYTES} bytes, over the catalog limit"),
        ));
    }
    Ok(bytes)
}

/// Parses the bibliography into one metadata record per member name.
fn parse_catalog(bytes: &[u8], source_name: &str) -> io::Result<HashMap<String, Metadata>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| catalog_error(source_name, format!("is not UTF-8: {error}")))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut records = parse_records(text, source_name)?.into_iter();
    let header = records
        .next()
        .ok_or_else(|| catalog_error(source_name, "is empty".into()))?;
    let mut index: HashMap<&str, usize> = HashMap::new();
    for (position, column) in header.iter().enumerate() {
        if column.is_empty() {
            return Err(catalog_error(
                source_name,
                "has an empty column name".into(),
            ));
        }
        if index.insert(column.as_str(), position).is_some() {
            return Err(catalog_error(
                source_name,
                format!("repeats the {column:?} column"),
            ));
        }
    }
    for column in CATALOG_COLUMNS {
        if !index.contains_key(column) {
            return Err(catalog_error(
                source_name,
                format!("is missing the {column:?} column"),
            ));
        }
    }
    let mut rows: HashMap<String, Metadata> = HashMap::new();
    let mut identifiers: HashSet<String> = HashSet::new();
    let mut release: Option<String> = None;
    for (offset, record) in records.enumerate() {
        let row = offset + 1;
        if record.len() != header.len() {
            return Err(catalog_error(
                source_name,
                format!(
                    "row {row} has {} fields; the header has {}",
                    record.len(),
                    header.len()
                ),
            ));
        }
        let value = |column: &str| record[index[column]].as_str();
        let required = |column: &str| {
            let value = value(column);
            if value.is_empty() {
                Err(catalog_error(
                    source_name,
                    format!("row {row} has an empty {column:?}"),
                ))
            } else {
                Ok(value)
            }
        };
        let identifier = required("identifier")?;
        let filename = required("filename")?;
        let row_release = required("release")?;
        if !identifiers.insert(identifier.to_owned()) {
            return Err(catalog_error(
                source_name,
                format!("row {row} repeats the identifier {identifier:?}"),
            ));
        }
        match &release {
            Some(release) if release != row_release => {
                return Err(catalog_error(
                    source_name,
                    format!(
                        "row {row} declares release {row_release:?}, but the archive declares {release:?}; an archive is one release"
                    ),
                ));
            }
            Some(_) => {}
            None => release = Some(row_release.to_owned()),
        }
        let mut object = serde_json::Map::new();
        for (position, column) in header.iter().enumerate() {
            let value = record[position].as_str();
            if value.is_empty() {
                continue;
            }
            let value = if NUMERIC_COLUMNS.contains(&column.as_str()) {
                let number = value.parse::<u64>().map_err(|_| {
                    catalog_error(
                        source_name,
                        format!(
                            "row {row} has {column}={value:?}, which is not a non-negative integer"
                        ),
                    )
                })?;
                serde_json::Value::from(number)
            } else {
                serde_json::Value::from(value)
            };
            object.insert(column.clone(), value);
        }
        let metadata = metadata_from_json(
            serde_json::Value::Object(object),
            &format!("soranoha archive {source_name}: {CATALOG_MEMBER} row {row}"),
        )?;
        if rows.insert(filename.to_owned(), metadata).is_some() {
            return Err(catalog_error(
                source_name,
                format!("row {row} repeats the filename {filename:?}"),
            ));
        }
    }
    Ok(rows)
}

/// Splits CSV text into records of fields, following RFC 4180 closely enough
/// for a spreadsheet round trip: quoted fields, `""` for a literal quote, and
/// CRLF, LF or CR record terminators. A UTF-8 byte-order mark is stripped by
/// the caller.
fn parse_records(text: &str, source_name: &str) -> io::Result<Vec<Vec<String>>> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut quoted_closed = false;
    let malformed = |records: &[Vec<String>], reason: &str| {
        catalog_error(
            source_name,
            format!("row {} is not readable CSV: {reason}", records.len() + 1),
        )
    };
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if in_quotes {
            if character == '"' {
                if characters.peek() == Some(&'"') {
                    characters.next();
                    field.push('"');
                } else {
                    in_quotes = false;
                    quoted_closed = true;
                }
            } else {
                field.push(character);
            }
            continue;
        }
        match character {
            '"' => {
                if quoted_closed || !field.is_empty() {
                    return Err(malformed(&records, "a quote inside an unquoted field"));
                }
                in_quotes = true;
            }
            ',' => {
                quoted_closed = false;
                record.push(std::mem::take(&mut field));
            }
            '\r' | '\n' => {
                if character == '\r' && characters.peek() == Some(&'\n') {
                    characters.next();
                }
                quoted_closed = false;
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            other => {
                if quoted_closed {
                    return Err(malformed(&records, "text after a closing quote"));
                }
                field.push(other);
            }
        }
    }
    if in_quotes {
        return Err(malformed(&records, "an unterminated quoted field"));
    }
    if quoted_closed || !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::structured::{MetadataScalar, MetadataValue};
    use std::io::{Cursor, Write};

    const HEADER: &str = "identifier,title,subtitle,title_reading,author,orthographic_style,ndc,first_published,body_character_count,source_edition_title,source_edition_publisher,source_edition_year,filename,source_content_hash,url,release,doi";

    fn csv<S: AsRef<str>>(rows: &[S]) -> String {
        let rows: Vec<&str> = rows.iter().map(AsRef::as_ref).collect();
        format!("\u{feff}{HEADER}\r\n{}\r\n", rows.join("\r\n"))
    }

    /// One valid row, quoted the way the exporter writes every field.
    fn row(identifier: &str, filename: &str, release: &str) -> String {
        [
            identifier,
            "蜘蛛の糸",
            "",
            "くものいと",
            "芥川 竜之介",
            "新字新仮名",
            "NDC 913",
            "「赤い鳥」1918（大正7）年7月",
            "1234",
            "或阿呆の一生・侏儒の言葉",
            "角川文庫、角川書店",
            "1969",
            filename,
            "sha256:397232c46b0784bf6a5844ecbddd58a08130295267146d7b6d648f69e45c34ec",
            "https://soranoha.org/works/000092_000879/",
            release,
            "10.5281/zenodo.23469644",
        ]
        .iter()
        .map(|value| format!("\"{}\"", value.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(",")
    }

    fn archive<S: AsRef<str>>(entries: &[(&str, S)]) -> Cursor<Vec<u8>> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .expect("start entry");
            writer
                .write_all(content.as_ref().as_bytes())
                .expect("entry");
        }
        let cursor = writer.finish().expect("finish zip");
        Cursor::new(cursor.into_inner())
    }

    fn events(events: impl Iterator<Item = io::Result<CorpusEvent>>) -> Vec<CorpusEvent> {
        events.map(|event| event.expect("event")).collect()
    }

    fn document_metadata(events: &[CorpusEvent]) -> Vec<(&str, &Metadata)> {
        events
            .iter()
            .filter_map(|event| match event {
                CorpusEvent::DocumentStart {
                    source_name,
                    metadata,
                } => Some((source_name.as_str(), metadata)),
                _ => None,
            })
            .collect()
    }

    fn sentences(events: &[CorpusEvent]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| match event {
                CorpusEvent::Sentence { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    fn scalar<'a>(metadata: &'a Metadata, key: &str) -> Option<&'a MetadataValue> {
        metadata
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    #[test]
    fn the_pinned_header_is_the_reader_contract() {
        assert_eq!(CATALOG_COLUMNS.len(), 17);
        assert_eq!(CATALOG_COLUMNS[0], "identifier");
        assert_eq!(CATALOG_COLUMNS[12], "filename");
        assert_eq!(CATALOG_COLUMNS[15], "release");
    }

    #[test]
    fn a_row_becomes_typed_document_metadata_with_empty_fields_omitted() {
        let text = csv(&[row("000092_000879", "kumo.txt", "c373bb")]);
        let rows = parse_catalog(text.as_bytes(), "test.zip").expect("catalog");
        let metadata = rows.get("kumo.txt").expect("row");
        assert!(matches!(
            scalar(metadata, "identifier"),
            Some(MetadataValue::Scalar(MetadataScalar::String(value))) if value == "000092_000879"
        ));
        assert!(matches!(
            scalar(metadata, "body_character_count"),
            Some(MetadataValue::Scalar(MetadataScalar::Number(value))) if value == "1234"
        ));
        assert!(matches!(
            scalar(metadata, "source_edition_year"),
            Some(MetadataValue::Scalar(MetadataScalar::Number(value))) if value == "1969"
        ));
        assert!(scalar(metadata, "subtitle").is_none(), "empty is absent");
        assert!(scalar(metadata, "doi").is_some());
        assert!(scalar(metadata, "filename").is_some());
    }

    #[test]
    fn a_comma_and_a_quote_survive_a_quoted_field() {
        let text = format!(
            "\u{feff}{HEADER}\r\n{}\r\n",
            row("000092_000879", "kumo.txt", "c373bb").replace("蜘蛛の糸", "蜘蛛の糸, \"\"糸\"\"")
        );
        let rows = parse_catalog(text.as_bytes(), "test.zip").expect("catalog");
        let metadata = rows.get("kumo.txt").expect("row");
        assert!(matches!(
            scalar(metadata, "title"),
            Some(MetadataValue::Scalar(MetadataScalar::String(value))) if value == "蜘蛛の糸, \"糸\""
        ));
    }

    #[test]
    fn an_extra_column_is_carried_rather_than_refused() {
        let header = format!("{HEADER},provenance_note");
        let text = format!(
            "\u{feff}{header}\r\n{},\"from a later catalog version\"\r\n",
            row("000092_000879", "kumo.txt", "c373bb")
        );
        let rows = parse_catalog(text.as_bytes(), "test.zip").expect("catalog");
        let metadata = rows.get("kumo.txt").expect("row");
        assert!(matches!(
            scalar(metadata, "provenance_note"),
            Some(MetadataValue::Scalar(MetadataScalar::String(value))) if value == "from a later catalog version"
        ));
    }

    #[test]
    fn a_missing_pinned_column_is_refused_by_name() {
        let header = HEADER.replace("source_content_hash,", "");
        let text = format!("\u{feff}{header}\r\n");
        let error = parse_catalog(text.as_bytes(), "test.zip").err().unwrap();
        assert!(
            error
                .to_string()
                .contains("missing the \"source_content_hash\""),
            "{error}"
        );
    }

    #[test]
    fn the_catalog_refusals_name_the_row_and_the_defect() {
        let duplicate = csv(&[
            row("000092_000879", "kumo.txt", "c373bb"),
            row("000093_000879", "kumo.txt", "c373bb"),
        ]);
        let error = parse_catalog(duplicate.as_bytes(), "test.zip")
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("repeats the filename"),
            "{error}"
        );

        let duplicate = csv(&[
            row("000092_000879", "a.txt", "c373bb"),
            row("000092_000879", "b.txt", "c373bb"),
        ]);
        let error = parse_catalog(duplicate.as_bytes(), "test.zip")
            .err()
            .unwrap();
        assert!(
            error.to_string().contains("repeats the identifier"),
            "{error}"
        );

        let mixed = csv(&[
            row("000092_000879", "a.txt", "c373bb"),
            row("000093_000879", "b.txt", "deadbeef"),
        ]);
        let error = parse_catalog(mixed.as_bytes(), "test.zip").err().unwrap();
        assert!(
            error.to_string().contains("declares release \"deadbeef\""),
            "{error}"
        );

        let number = csv(&[
            row("000092_000879", "a.txt", "c373bb").replace("\"1234\"", "\"twelve hundred\"")
        ]);
        let error = parse_catalog(number.as_bytes(), "test.zip").err().unwrap();
        assert!(
            error
                .to_string()
                .contains("body_character_count=\"twelve hundred\""),
            "{error}"
        );

        let short = csv(&["\"000092_000879\",\"only two\""]);
        let error = parse_catalog(short.as_bytes(), "test.zip").err().unwrap();
        assert!(error.to_string().contains("row 1 has 2 fields"), "{error}");
    }

    #[test]
    fn the_stream_admits_sorted_works_with_the_catalog_metadata() {
        let entries = [
            (
                "catalog.csv",
                csv(&[
                    row("000092_000879", "kumo.txt", "c373bb"),
                    row("000013_000879", "hari.txt", "c373bb"),
                ]),
            ),
            ("kumo.txt", "これは文です。二つ目です。\n".to_owned()),
            ("hari.txt", "十本の針。\n".to_owned()),
        ];
        let archive = archive(&entries);
        let events =
            events(SoranohaEvents::new(archive, "a.zip".into(), 1024 * 1024).expect("open"));
        let documents = document_metadata(&events);
        assert_eq!(documents.len(), 2);
        // Byte-sorted names, not catalog order.
        assert_eq!(documents[0].0, "hari.txt");
        assert_eq!(documents[1].0, "kumo.txt");
        assert!(matches!(
            scalar(documents[1].1, "identifier"),
            Some(MetadataValue::Scalar(MetadataScalar::String(value))) if value == "000092_000879"
        ));
        let starts = events
            .iter()
            .filter(|event| matches!(event, CorpusEvent::DocumentStart { .. }))
            .count();
        let ends = events
            .iter()
            .filter(|event| matches!(event, CorpusEvent::DocumentEnd))
            .count();
        assert_eq!((starts, ends), (2, 2));
        assert_eq!(
            sentences(&events),
            ["十本の針。", "これは文です。", "二つ目です。"]
        );
    }

    #[test]
    fn the_stream_reports_line_breaks_rather_than_paragraphs() {
        let entries = [
            (
                "catalog.csv",
                csv(&[row("000092_000879", "kumo.txt", "c373bb")]),
            ),
            ("kumo.txt", "一行目。\n二行目。\n".to_owned()),
        ];
        let archive = archive(&entries);
        let events =
            events(SoranohaEvents::new(archive, "a.zip".into(), 1024 * 1024).expect("open"));
        let observations: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                CorpusEvent::DivisionObservation { kind, observation } => {
                    Some((kind.as_str(), observation))
                }
                _ => None,
            })
            .collect();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].0, "paragraph");
        assert!(matches!(
            observations[0].1,
            super::super::DivisionObservation::Unavailable { .. }
        ));
    }

    #[test]
    fn the_stream_refuses_what_it_cannot_join_or_read() {
        // No catalog at all.
        let error = SoranohaEvents::new(
            archive(&[("kumo.txt", "文です。\n")]),
            "a.zip".into(),
            1024 * 1024,
        )
        .err()
        .unwrap();
        assert!(
            error.to_string().contains("no catalog.csv at the root"),
            "{error}"
        );

        // Catalog only.
        let error = SoranohaEvents::new(
            archive(&[(
                "catalog.csv",
                csv(&[row("000092_000879", "kumo.txt", "c373bb")]),
            )]),
            "a.zip".into(),
            1024 * 1024,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("no work to read"), "{error}");

        // A member without a row.
        let error = SoranohaEvents::new(
            archive(&[
                (
                    "catalog.csv",
                    csv(&[row("000092_000879", "kumo.txt", "c373bb")]),
                ),
                ("kumo.txt", "文です。\n".to_owned()),
                ("extra.txt", "余分です。\n".to_owned()),
            ]),
            "a.zip".into(),
            1024 * 1024,
        )
        .err()
        .unwrap();
        assert!(
            error
                .to_string()
                .contains("entry extra.txt has no catalog.csv row"),
            "{error}"
        );

        // The TEI projection.
        let error = SoranohaEvents::new(
            archive(&[
                (
                    "catalog.csv",
                    csv(&[row("000092_000879", "kumo.xml", "c373bb")]),
                ),
                ("kumo.xml", "<TEI/>".to_owned()),
            ]),
            "a.zip".into(),
            1024 * 1024,
        )
        .err()
        .unwrap();
        assert!(
            error.to_string().contains("entry kumo.xml is TEI"),
            "{error}"
        );

        // An entry over the run's record limit is refused before inflation.
        let mut limited = SoranohaEvents::new(
            archive(&[
                (
                    "catalog.csv",
                    csv(&[row("000092_000879", "kumo.txt", "c373bb")]),
                ),
                ("kumo.txt", "長い文です。\n".to_owned()),
            ]),
            "a.zip".into(),
            8,
        )
        .expect("the catalog itself fits");
        let error = loop {
            match limited.next() {
                Some(Ok(_)) => {}
                Some(Err(error)) => break error,
                None => panic!("the oversized entry must be refused"),
            }
        };
        assert!(
            error.to_string().contains("raise --max-record-bytes"),
            "{error}"
        );
    }

    #[test]
    fn a_soranoha_member_admits_the_same_sentences_as_the_plain_reader() {
        let text = "\u{3000}\u{3000}これは文です。二つ目です。\n\u{3000}三つ目です。\n";
        let entries = [
            (
                "catalog.csv",
                csv(&[row("000092_000879", "kumo.txt", "c373bb")]),
            ),
            ("kumo.txt", text.to_owned()),
        ];
        let archive = archive(&entries);
        let events =
            events(SoranohaEvents::new(archive, "a.zip".into(), 1024 * 1024).expect("open"));
        let soranoha: Vec<String> = sentences(&events).into_iter().map(str::to_owned).collect();
        let plain = crate::corpus::stream_input_reader(
            crate::corpus::InputFormat::Plaintext,
            io::BufReader::new(Cursor::new(text.as_bytes())),
            None,
        )
        .collect::<io::Result<Vec<_>>>()
        .expect("plain");
        assert_eq!(soranoha, plain);
    }
}
