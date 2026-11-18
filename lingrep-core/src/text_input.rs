//! UTF-8 decoding for corpus sources and pattern catalogs, with diagnostics
//! located in the original input bytes.

use std::io::{self, BufRead, Read};

/// The UTF-8 encoding of U+FEFF.
pub const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// The signature of a ZIP local file header, the first bytes of a ZIP archive.
const ZIP_MAGIC: &[u8] = b"PK\x03\x04";

const BUFFER_BYTES: usize = 64 * 1024;

/// Bytes after the first invalid one that the Shift_JIS check decodes.
const SHIFT_JIS_WINDOW_BYTES: usize = 64 * 1024;

/// Text that cannot be read as UTF-8.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TextDecodeError {
    /// `byte_offset` counts from the start of the file, byte-order mark
    /// included, and `line` is one-based.
    #[error(
        "invalid UTF-8 at line {line}, byte offset {byte_offset}{hint}",
        hint = if *looks_like_shift_jis { SHIFT_JIS_HINT } else { "" }
    )]
    InvalidUtf8 {
        line: u64,
        byte_offset: u64,
        /// The input before the invalid byte is ASCII, and the diagnostic window
        /// decodes as [WHATWG Shift_JIS](https://encoding.spec.whatwg.org/#shift_jis).
        looks_like_shift_jis: bool,
    },
    #[error(
        "ZIP archive detected. To read its text, unzip it first. For a Soranoha archive, use \
         --input-format soranoha."
    )]
    ZipArchive,
}

const SHIFT_JIS_HINT: &str = ". The file looks like Shift_JIS (CP932); convert it to UTF-8 with \
     `iconv -f CP932 -t UTF-8 INPUT > OUTPUT` or `nkf -w INPUT > OUTPUT`";

impl From<TextDecodeError> for io::Error {
    fn from(error: TextDecodeError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, error)
    }
}

/// Decodes `bytes` as UTF-8, keeping a leading byte-order mark.
pub fn decode_utf8(bytes: &[u8]) -> Result<&str, TextDecodeError> {
    std::str::from_utf8(bytes).map_err(|error| {
        let valid = error.valid_up_to();
        let window_end = valid
            .saturating_add(SHIFT_JIS_WINDOW_BYTES)
            .min(bytes.len());
        // The mark is counted in the offset but is not text before the error.
        let mut position = Position::default();
        let mark = if bytes.starts_with(UTF8_BOM) {
            position.byte_offset = UTF8_BOM.len() as u64;
            UTF8_BOM.len()
        } else {
            0
        };
        invalid_utf8(
            position,
            &bytes[mark..valid],
            &bytes[valid..window_end],
            window_end == bytes.len(),
        )
    })
}

/// Reads the file at `path` as UTF-8, keeping a leading byte-order mark, as
/// [`decode_utf8`] decodes it.
pub fn read_utf8_file(path: &std::path::Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(decode_utf8(&bytes)?.to_owned())
}

/// Decodes one in-memory corpus document as UTF-8 without its byte-order mark.
pub fn decode_document(bytes: &[u8]) -> Result<&str, TextDecodeError> {
    decode_utf8(bytes).map(|text| text.strip_prefix('\u{feff}').unwrap_or(text))
}

/// Where a buffer starts within its file.
#[derive(Clone, Copy)]
struct Position {
    line: u64,
    byte_offset: u64,
    ascii: bool,
}

impl Default for Position {
    fn default() -> Self {
        Self {
            line: 1,
            byte_offset: 0,
            ascii: true,
        }
    }
}

impl Position {
    fn advance(&mut self, bytes: &[u8]) {
        self.line += bytes.iter().filter(|byte| **byte == b'\n').count() as u64;
        self.byte_offset += bytes.len() as u64;
        self.ascii = self.ascii && bytes.is_ascii();
    }
}

fn invalid_utf8(
    mut position: Position,
    valid: &[u8],
    window: &[u8],
    complete: bool,
) -> TextDecodeError {
    position.advance(valid);
    TextDecodeError::InvalidUtf8 {
        line: position.line,
        byte_offset: position.byte_offset,
        looks_like_shift_jis: position.ascii && decodes_as_shift_jis(window, complete),
    }
}

fn decodes_as_shift_jis(bytes: &[u8], complete: bool) -> bool {
    let mut decoder = encoding_rs::SHIFT_JIS.new_decoder_without_bom_handling();
    let Some(capacity) = decoder.max_utf8_buffer_length_without_replacement(bytes.len()) else {
        return false;
    };
    let mut decoded = String::with_capacity(capacity);
    let (result, _) = decoder.decode_to_string_without_replacement(bytes, &mut decoded, complete);
    result == encoding_rs::DecoderResult::InputEmpty
}

/// A buffered reader over corpus source bytes that omits one leading UTF-8
/// byte-order mark and hands out only valid UTF-8.
///
/// Invalid UTF-8 or a ZIP archive is an [`io::ErrorKind::InvalidData`] error
/// carrying a [`TextDecodeError`]; after it the reader reports end of input.
pub struct TextReader<R> {
    inner: R,
    buffer: Box<[u8]>,
    /// `buffer[start..valid]` is validated and not yet consumed, and
    /// `buffer[valid..end]` is the start of a character still incomplete.
    start: usize,
    valid: usize,
    end: usize,
    /// The position of `buffer[0]`.
    position: Position,
    at_start: bool,
    failed: bool,
}

impl<R: Read> TextReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: vec![0; BUFFER_BYTES].into_boxed_slice(),
            start: 0,
            valid: 0,
            end: 0,
            position: Position::default(),
            at_start: true,
            failed: false,
        }
    }

    /// Reads into the free end of the buffer; zero means end of input.
    fn read_more(&mut self) -> io::Result<usize> {
        loop {
            match self.inner.read(&mut self.buffer[self.end..]) {
                Ok(read) => {
                    self.end += read;
                    return Ok(read);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// Reads until the first bytes can be told apart from a byte-order mark
    /// and a ZIP signature, then omits a mark. Returns whether input ended.
    fn read_start(&mut self) -> io::Result<bool> {
        self.at_start = false;
        let mut ended = false;
        while self.end < ZIP_MAGIC.len()
            && (ZIP_MAGIC.starts_with(&self.buffer[..self.end])
                || UTF8_BOM.starts_with(&self.buffer[..self.end]))
        {
            if self.read_more()? == 0 {
                ended = true;
                break;
            }
        }
        if self.buffer[..self.end].starts_with(ZIP_MAGIC) {
            self.failed = true;
            return Err(TextDecodeError::ZipArchive.into());
        }
        if self.buffer[..self.end].starts_with(UTF8_BOM) {
            self.buffer.copy_within(UTF8_BOM.len()..self.end, 0);
            self.end -= UTF8_BOM.len();
            self.position.byte_offset = UTF8_BOM.len() as u64;
        }
        Ok(ended)
    }

    fn fail(&mut self, valid: usize) -> io::Error {
        self.failed = true;
        let mut window = self.buffer[valid..self.end].to_vec();
        let mut complete = false;
        let mut chunk = [0; 8 * 1024];
        while window.len() < SHIFT_JIS_WINDOW_BYTES {
            match self.inner.read(&mut chunk) {
                Ok(0) => {
                    complete = true;
                    break;
                }
                Ok(read) => window.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        if window.len() > SHIFT_JIS_WINDOW_BYTES {
            window.truncate(SHIFT_JIS_WINDOW_BYTES);
            complete = false;
        }
        invalid_utf8(self.position, &self.buffer[..valid], &window, complete).into()
    }
}

impl<R: Read> BufRead for TextReader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.start == self.valid && !self.failed {
            self.position.advance(&self.buffer[..self.valid]);
            self.buffer.copy_within(self.valid..self.end, 0);
            self.end -= self.valid;
            self.start = 0;
            self.valid = 0;
            let mut ended = if self.at_start {
                self.read_start()?
            } else {
                false
            };
            loop {
                match std::str::from_utf8(&self.buffer[..self.end]) {
                    Ok(_) => self.valid = self.end,
                    Err(error) if error.error_len().is_none() && !ended => {
                        self.valid = error.valid_up_to();
                    }
                    Err(error) => return Err(self.fail(error.valid_up_to())),
                }
                if self.valid > 0 || ended {
                    break;
                }
                ended = self.read_more()? == 0;
            }
        }
        Ok(&self.buffer[self.start..self.valid])
    }

    fn consume(&mut self, amount: usize) {
        self.start = (self.start + amount).min(self.valid);
    }
}

impl<R: Read> Read for TextReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let read = available.len().min(output.len());
        output[..read].copy_from_slice(&available[..read]);
        self.consume(read);
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Limits each read to one byte.
    struct OneByte<'a>(&'a [u8]);

    impl Read for OneByte<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let Some((first, rest)) = self.0.split_first() else {
                return Ok(0);
            };
            if output.is_empty() {
                return Ok(0);
            }
            output[0] = *first;
            self.0 = rest;
            Ok(1)
        }
    }

    #[expect(
        clippy::panic_in_result_fn,
        reason = "the result is the value under test, and the assertion compares two reads"
    )]
    fn read_all(input: &[u8]) -> Result<Vec<u8>, TextDecodeError> {
        let read = |reader: &mut dyn Read| {
            let mut output = Vec::new();
            reader
                .read_to_end(&mut output)
                .map(|_| output)
                .map_err(|error| {
                    error
                        .into_inner()
                        .and_then(|inner| inner.downcast::<TextDecodeError>().ok())
                        .map(|error| *error)
                        .expect("a decode error")
                })
        };
        let whole = read(&mut TextReader::new(input));
        assert_eq!(
            read(&mut TextReader::new(OneByte(input))),
            whole,
            "{input:?}"
        );
        whole
    }

    #[test]
    fn exactly_one_leading_mark_is_omitted_however_the_bytes_arrive() {
        for (input, expected) in [
            (&b"\xEF\xBB\xBFabc"[..], &b"abc"[..]),
            (b"\xEF\xBB\xBF\xEF\xBB\xBFabc", b"\xEF\xBB\xBFabc"),
            (b"\xEF\xBB\xBF", b""),
            (b"a\xEF\xBB\xBF", b"a\xEF\xBB\xBF"),
            ("猫\n犬".as_bytes(), "猫\n犬".as_bytes()),
            (b"PK\x03", b"PK\x03"),
            (b"", b""),
        ] {
            assert_eq!(read_all(input), Ok(expected.to_vec()), "{input:?}");
        }
    }

    #[test]
    fn invalid_utf8_names_its_line_and_file_byte_offset() {
        let invalid = |line, byte_offset, looks_like_shift_jis| {
            Err(TextDecodeError::InvalidUtf8 {
                line,
                byte_offset,
                looks_like_shift_jis,
            })
        };
        // CP932 猫 is 94 4C; 81 60 is ～ and 87 40 is ①, both CP932 extensions.
        for (input, expected) in [
            (&b"ab\n\x94\x4c\n"[..], invalid(2, 3, true)),
            (b"\xEF\xBB\xBF\x94\x4c\x81\x60\x87\x40", invalid(1, 3, true)),
            // A lead byte cut off by end of input is not Shift_JIS.
            (b"a\n\x94", invalid(2, 2, false)),
            // An incomplete UTF-8 sequence at end of input; E7 8C is also one
            // Shift_JIS character.
            (b"a\xE7\x8C", invalid(1, 1, true)),
            // Valid non-ASCII UTF-8 before the error rules out Shift_JIS.
            ("猫\n".as_bytes(), Ok("猫\n".as_bytes().to_vec())),
            (b"\xE7\x8C\xAB\x94\x4c", invalid(1, 3, false)),
            // 0xFF is not a Shift_JIS byte.
            (b"x\xFF", invalid(1, 1, false)),
        ] {
            assert_eq!(read_all(input), expected, "{input:?}");
            if let Err(error) = &expected {
                assert_eq!(
                    decode_document(input).map(str::as_bytes),
                    Err(error.clone()),
                    "{input:?}"
                );
            }
        }
    }

    #[test]
    fn a_zip_signature_at_the_start_is_reported_as_an_archive() {
        assert_eq!(
            read_all(b"PK\x03\x04rest"),
            Err(TextDecodeError::ZipArchive)
        );
        assert_eq!(read_all(b"xPK\x03\x04"), Ok(b"xPK\x03\x04".to_vec()));
    }

    #[test]
    fn a_line_after_the_buffer_is_counted_from_every_earlier_newline() {
        let mut input = "猫\n".repeat(BUFFER_BYTES).into_bytes();
        let offset = input.len() as u64;
        input.extend_from_slice(b"\xFF");
        assert_eq!(
            read_all(&input),
            Err(TextDecodeError::InvalidUtf8 {
                line: BUFFER_BYTES as u64 + 1,
                byte_offset: offset,
                looks_like_shift_jis: false,
            })
        );
    }

    #[test]
    fn a_document_decodes_without_its_mark_and_a_catalog_with_it() {
        let text = "\u{feff}\u{feff}猫";
        assert_eq!(decode_document(text.as_bytes()), Ok("\u{feff}猫"));
        assert_eq!(decode_utf8(text.as_bytes()), Ok(text));
    }
}
