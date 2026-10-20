//! Frozen observations of the shared corpus-reading primitives that the web
//! source and the CLIs build upon.
//!
//! These characterize `lingrep_core::corpus::stream_input_reader` for the plain and
//! JSONL formats — the extraction surface a later task will route through a
//! shared `CorpusSource`. Pinning the byte-level behavior here keeps that
//! migration behavior-preserving.

use std::io::{BufReader, Cursor};

use lingrep_core::corpus::text_source::{InputFormat, stream_input_reader};

/// Pinned: plain-text reading splits each physical line on Japanese
/// sentence delimiters (`。！？` plus a following closing quotation),
/// matching the legacy `SentenceFramer` byte-framing, and the shared admission
/// layer drops blank lines. A line carrying multiple sentences is admitted as
/// one sentence per delimiter; a line carrying one sentence with no delimiter
/// is admitted as one sentence. A blank physical line never surfaces.
#[test]
fn plain_text_reader_splits_lines_on_japanese_sentence_delimiters() {
    let input = "一\n\n二\n";
    let sentences = stream_input_reader(
        InputFormat::Plaintext,
        BufReader::new(Cursor::new(input)),
        None,
    )
    .collect::<std::io::Result<Vec<_>>>()
    .unwrap();

    assert_eq!(sentences, ["一".to_owned(), "二".to_owned()]);

    let multi = "猫がいる。猫が寝る。\n";
    let sentences = stream_input_reader(
        InputFormat::Plaintext,
        BufReader::new(Cursor::new(multi)),
        None,
    )
    .collect::<std::io::Result<Vec<_>>>()
    .unwrap();
    assert_eq!(
        sentences,
        ["猫がいる。".to_owned(), "猫が寝る。".to_owned()]
    );
}

/// Pinned: JSONL reading extracts the `sentences` array from each document in
/// order, across multiple physical lines. This is exactly the extraction the
/// web source and the matchers rely on.
#[test]
fn jsonl_reader_extracts_sentence_arrays_from_each_document() {
    let input = "{\"sentences\":[\"一つ目。\",\"二つ目。\"]}\n{\"sentences\":[\"三つ目。\"]}\n";
    let sentences =
        stream_input_reader(InputFormat::Jsonl, BufReader::new(Cursor::new(input)), None)
            .collect::<std::io::Result<Vec<_>>>()
            .unwrap();

    assert_eq!(
        sentences,
        [
            "一つ目。".to_owned(),
            "二つ目。".to_owned(),
            "三つ目。".to_owned()
        ]
    );
}
