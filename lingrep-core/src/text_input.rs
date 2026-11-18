//! Decoding of corpus source text where its bytes enter a reader.
//!
//! Every corpus text reader takes its bytes through [`TextReader`] (a stream)
//! or [`decode_document`] (a document already in memory), and both strip one
//! leading UTF-8 byte-order mark, which Windows Notepad and Excel write.

use std::io::{self, BufRead, Read};

/// The UTF-8 encoding of U+FEFF.
pub const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

const BUFFER_BYTES: usize = 64 * 1024;

/// Returns `text` without one leading U+FEFF.
#[must_use]
pub fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Decodes one in-memory corpus document as UTF-8 without its byte-order mark.
pub fn decode_document(bytes: &[u8]) -> Result<&str, std::str::Utf8Error> {
    std::str::from_utf8(bytes).map(strip_bom)
}

/// A buffered reader over corpus source bytes that omits one leading UTF-8
/// byte-order mark.
pub struct TextReader<R> {
    inner: R,
    buffer: Box<[u8]>,
    start: usize,
    end: usize,
    at_start: bool,
}

impl<R: Read> TextReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buffer: vec![0; BUFFER_BYTES].into_boxed_slice(),
            start: 0,
            end: 0,
            at_start: true,
        }
    }

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
}

impl<R: Read> BufRead for TextReader<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        while self.start == self.end {
            self.start = 0;
            self.end = 0;
            if !self.at_start {
                self.read_more()?;
                break;
            }
            // A mark split across reads is still one mark, so read until three
            // bytes are present or they can no longer begin one.
            while self.end < UTF8_BOM.len()
                && UTF8_BOM.starts_with(&self.buffer[..self.end])
                && self.read_more()? > 0
            {}
            self.at_start = false;
            if self.buffer[..self.end].starts_with(UTF8_BOM) {
                self.start = UTF8_BOM.len();
            } else {
                break;
            }
        }
        Ok(&self.buffer[self.start..self.end])
    }

    fn consume(&mut self, amount: usize) {
        self.start = (self.start + amount).min(self.end);
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

    /// Hands out one byte per read, the worst chunking a pipe can produce.
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

    fn read_all(input: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        TextReader::new(OneByte(input))
            .read_to_end(&mut output)
            .unwrap();
        output
    }

    #[test]
    fn exactly_one_leading_mark_is_omitted_however_the_bytes_arrive() {
        for (input, expected) in [
            (&b"\xEF\xBB\xBFabc"[..], &b"abc"[..]),
            (b"\xEF\xBB\xBF\xEF\xBB\xBFabc", b"\xEF\xBB\xBFabc"),
            (b"\xEF\xBB\xBF", b""),
            (b"\xEF\xBB", b"\xEF\xBB"),
            (b"a\xEF\xBB\xBF", b"a\xEF\xBB\xBF"),
            (b"", b""),
        ] {
            assert_eq!(read_all(input), expected, "{input:?}");
            let mut whole = Vec::new();
            TextReader::new(input).read_to_end(&mut whole).unwrap();
            assert_eq!(whole, expected, "{input:?}");
        }
    }

    #[test]
    fn a_document_decodes_without_its_mark() {
        assert_eq!(
            decode_document("\u{feff}\u{feff}猫".as_bytes()),
            Ok("\u{feff}猫")
        );
        assert_eq!(decode_document("猫".as_bytes()), Ok("猫"));
    }
}
