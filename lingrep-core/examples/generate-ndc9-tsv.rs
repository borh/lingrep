//! One-way regeneration of `src/corpus/bccwj/data/ndc9.tsv` from the NDC9
//! Linked Data map (`ndc9.edn.xz` in the `corpus-utils` checkout).
//!
//! The table is NDL/JLA's NDC9 classification scheme — not BCCWJ text — and is
//! checked in so the reader has no runtime data dependency. The generator is
//! the validation: it refuses to write on any malformed input and prints the
//! provenance numbers recorded beside this example in
//! `lingrep-core/src/corpus/bccwj/mod.rs`.
//!
//! Usage:
//! ```text
//! xz -dc /path/to/corpus-utils/data/ndc9.edn.xz \
//!   | cargo run -p lingrep-core --example generate-ndc9-tsv -- - OUT.tsv
//! ```

use std::fs::File;
use std::io::{self, Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("generate-ndc9-tsv: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [source, destination] = args.as_slice() else {
        return Err("usage: generate-ndc9-tsv SOURCE.edn|- OUT.tsv".into());
    };
    let (edn, source_digest) = if source == "-" {
        let mut buffer = String::new();
        io::stdin()
            .lock()
            .read_to_string(&mut buffer)
            .map_err(|error| format!("reading stdin: {error}"))?;
        // Digest the decompressed EDN bytes; the xz wrapper is not part of
        // the table's provenance.
        let digest = blake3::hash(buffer.as_bytes());
        (buffer, *digest.as_bytes())
    } else {
        let bytes = std::fs::read(source).map_err(|error| format!("reading {source}: {error}"))?;
        let digest = blake3::hash(&bytes);
        let text = String::from_utf8(bytes).map_err(|_| "source is not valid UTF-8".to_owned())?;
        (text, *digest.as_bytes())
    };

    let table = parse_edn_map(&edn)?;
    let label_count: usize = table.values().map(Vec::len).sum();

    let mut out = String::with_capacity(edn.len() / 2);
    for (notation, labels) in &table {
        out.push_str(notation);
        for label in labels {
            out.push('\t');
            out.push_str(label);
        }
        out.push('\n');
    }

    let bytes = out.as_bytes();
    let out_digest = *blake3::hash(bytes).as_bytes();
    if destination == "-" {
        io::stdout()
            .write_all(bytes)
            .map_err(|error| error.to_string())?;
    } else {
        File::create(destination)
            .and_then(|mut file| file.write_all(bytes))
            .map_err(|error| format!("writing {destination}: {error}"))?;
    }

    println!("source blake3:  {}", hex(&source_digest));
    println!("output blake3:  {}", hex(&out_digest));
    println!("rows:           {}", table.len());
    println!("labels:         {label_count}");
    Ok(())
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The length in bytes of the UTF-8 character starting with `lead`.
fn utf8_len(lead: u8) -> Option<usize> {
    if lead < 0x80 {
        Some(1)
    } else if (0xc2..=0xdf).contains(&lead) {
        Some(2)
    } else if (0xe0..=0xef).contains(&lead) {
        Some(3)
    } else if (0xf0..=0xf4).contains(&lead) {
        Some(4)
    } else {
        None
    }
}

/// Parses the strict EDN subset the NDC9 map uses: a flat map of
/// double-quoted strings to vectors of one or more double-quoted strings.
/// Anything else is a refused input, not a warning. The returned map is
/// sorted by notation, which fixes the TSV row order.
fn parse_edn_map(text: &str) -> Result<std::collections::BTreeMap<String, Vec<String>>, String> {
    let mut cursor = Scanner {
        text: text.as_bytes(),
        pos: 0,
    };
    cursor.ws()?;
    cursor.byte(b'{')?;
    let mut entries: Vec<(String, Vec<String>)> = Vec::new();
    loop {
        cursor.ws()?;
        if cursor.peek() == Some(b'}') {
            cursor.advance();
            break;
        }
        let key = cursor.quoted_string()?;
        cursor.ws()?;
        cursor.byte(b'[')?;
        cursor.ws()?;
        let mut labels = Vec::new();
        loop {
            cursor.ws()?;
            if cursor.peek() == Some(b']') {
                cursor.advance();
                break;
            }
            // EDN vector elements are separated by whitespace; a comma is
            // tolerated but not required.
            if !labels.is_empty() && cursor.peek() == Some(b',') {
                cursor.advance();
                cursor.ws()?;
            }
            labels.push(cursor.quoted_string()?);
        }
        entries.push((key, labels));
        cursor.ws()?;
        if cursor.peek() == Some(b',') {
            cursor.advance();
        }
    }
    cursor.ws()?;
    if !cursor.at_end() {
        return Err(format!("trailing content at byte {}", cursor.pos));
    }
    if entries.is_empty() {
        return Err("map is empty".into());
    }
    let mut table = std::collections::BTreeMap::new();
    for (key, labels) in entries {
        if key.is_empty() {
            return Err("empty notation key".into());
        }
        if table.contains_key(&key) {
            return Err(format!("duplicate notation key {key:?}"));
        }
        if key.contains('\t') || key.contains('\n') {
            return Err(format!("notation key {key:?} contains a tab or newline"));
        }
        if labels.is_empty() {
            return Err(format!("notation {key:?} has no labels"));
        }
        for label in &labels {
            if label.contains('\t') || label.contains('\n') {
                return Err(format!("a label of {key:?} contains a tab or newline"));
            }
        }
        if labels.iter().any(String::is_empty) {
            // Known heading gaps in the source (astronomy sub-headings),
            // carried by the reference map verbatim. Any other empty label
            // means the source changed; inspect before committing.
            let known = matches!(key.as_str(), "440.12" | "440.13" | "440.14" | "440.19");
            if !known {
                return Err(format!("notation {key:?} carries an unknown empty label"));
            }
        }
        table.insert(key, labels);
    }
    Ok(table)
}

struct Scanner<'a> {
    text: &'a [u8],
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn peek(&self) -> Option<u8> {
        self.text.get(self.pos).copied()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.text.len()
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn byte(&mut self, expected: u8) -> Result<(), String> {
        let found = self
            .peek()
            .ok_or_else(|| format!("unexpected end of input, expected {expected:?}"))?;
        if found != expected {
            return Err(format!(
                "byte {}: expected {expected:?}, found {found:?}",
                self.pos
            ));
        }
        self.advance();
        Ok(())
    }

    fn ws(&mut self) -> Result<(), String> {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.advance();
        }
        Ok(())
    }

    fn quoted_string(&mut self) -> Result<String, String> {
        self.byte(b'"')?;
        let mut value = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err("unterminated string".into());
            };
            self.advance();
            match byte {
                b'"' => break,
                b'\\' => value.push(self.escaped()?),
                _ => {
                    // Consume the full UTF-8 character starting at `byte`.
                    let start = self.pos - 1;
                    let length = utf8_len(byte).ok_or("invalid UTF-8 in string")?;
                    let end = start
                        .checked_add(length)
                        .filter(|end| *end <= self.text.len())
                        .ok_or("invalid UTF-8 in string")?;
                    let chars = std::str::from_utf8(&self.text[start..end])
                        .map_err(|_| "invalid UTF-8 in string".to_owned())?;
                    value.push_str(chars);
                    self.pos = end;
                }
            }
        }
        Ok(value)
    }

    fn escaped(&mut self) -> Result<char, String> {
        let Some(code) = self.peek() else {
            return Err("dangling escape".into());
        };
        self.advance();
        match code {
            b'"' => Ok('"'),
            b'\\' => Ok('\\'),
            b'/' => Ok('/'),
            b'n' => Ok('\n'),
            b't' => Ok('\t'),
            b'r' => Ok('\r'),
            b'b' => Ok('\u{0008}'),
            b'f' => Ok('\u{000C}'),
            b'u' => {
                let mut code = 0u32;
                for _ in 0..4 {
                    let hex = self
                        .peek()
                        .and_then(|byte| (byte as char).to_digit(16))
                        .ok_or_else(|| "bad \\u escape".to_owned())?;
                    self.advance();
                    code = code
                        .checked_mul(16)
                        .and_then(|value| value.checked_add(hex))
                        .ok_or_else(|| "bad \\u escape".to_owned())?;
                }
                char::from_u32(code).ok_or_else(|| "bad \\u escape".to_owned())
            }
            other => Err(format!("unsupported escape \\{other:?}")),
        }
    }
}
