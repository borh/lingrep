//! Differential-fidelity harness for the BCCWJ reader.
//!
//! Drives the reader's own values — `parse_entry`, `BccwjMetadata` — over a
//! user-held distribution so a separate reference implementation can be
//! compared against them. Nothing this harness records is committed: the
//! selection and the per-document records go to caller-named files, and the
//! tracked record is the seed, the selection's basename set, and per-document
//! verdicts only (copyright constraint, spec §2).
//!
//! Selection is deterministic: for each subcorpus code, five entries ordered
//! by `blake3(seed || basename)`, and for the codes present in both trees
//! (the dual subcorpora) the quota is three from `FIXED` and two from
//! `VARIABLE`, so every dual window is covered.
//!
//! # Usage
//!
//! ```text
//! bccwj-diff select     --root /data/bccwj [--seed 1] [--out selection.tsv]
//! bccwj-diff paragraphs --root /data/bccwj --selection selection.tsv [--out paragraphs.jsonl]
//! bccwj-diff metadata   --root /data/bccwj --selection selection.tsv [--out metadata.jsonl]
//! bccwj-diff content    --root /data/bccwj --selection selection.tsv [--out content.jsonl]
//! ```
//!
//! Selection rows are `zip_logical \t entry_name \t basename`. The
//! `content` leg compares the raw XML text (Text + CDATA events, unescaped,
//! NFC, all Unicode whitespace stripped) against the parsed sentence texts
//! under the same normalization, and reports a structural context window at
//! the first difference — element path and character offset, never the text.

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use lingrep_core::corpus::TraversalLimits;
use lingrep_core::corpus::bccwj::{
    BccwjInventory, BccwjMetadata, Sampling, Subcorpus, basename_from_entry, open_zip_archive,
    parse_entry, zip_entries, zip_entry_names,
};
use quick_xml::Reader;
use quick_xml::events::Event;
use unicode_normalization::UnicodeNormalization;

const SEED_DEFAULT: u64 = 1;
const PER_TREE_QUOTA: usize = 5;
const DUAL_FIXED_QUOTA: usize = 3;
const DUAL_VARIABLE_QUOTA: usize = 2;

struct Args {
    root: PathBuf,
    selection: Option<PathBuf>,
    out: Option<PathBuf>,
    seed: u64,
}

fn parse_args() -> Result<(String, Args), String> {
    let mut raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() {
        return Err("a subcommand is required (select | paragraphs | metadata | content)".into());
    }
    let command = raw.remove(0);
    let mut args = Args {
        root: PathBuf::new(),
        selection: None,
        out: None,
        seed: SEED_DEFAULT,
    };
    while !raw.is_empty() {
        match raw.remove(0).as_str() {
            "--root" => {
                args.root = PathBuf::from(raw.remove(0));
            }
            "--selection" => {
                args.selection = Some(PathBuf::from(raw.remove(0)));
            }
            "--out" => {
                args.out = Some(PathBuf::from(raw.remove(0)));
            }
            "--seed" => {
                args.seed = raw.remove(0).parse().expect("seed must be an integer");
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if args.root.as_os_str().is_empty() {
        return Err("--root is required".into());
    }
    if matches!(command.as_str(), "paragraphs" | "metadata" | "content") && args.selection.is_none()
    {
        return Err(format!("--selection is required for {command}"));
    }
    Ok((command, args))
}

fn select_order_key(seed: u64, basename: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&seed.to_le_bytes());
    hasher.update(basename.as_bytes());
    *hasher.finalize().as_bytes()
}

/// The selection: five basenames per code, hash-ordered by
/// `blake3(seed || basename)`; dual codes take three from each tree.
fn select(root: &Path, seed: u64, out: &mut dyn Write) -> io::Result<()> {
    let inventory = BccwjInventory::discover(root, &TraversalLimits::default())?;
    // (code, sampling) -> (basename, zip logical, entry name), hash-ordered.
    let mut by_tree: BTreeMap<(String, bool), Vec<(String, String, String)>> = BTreeMap::new();
    for zip in &inventory.zips {
        let file = fs::File::open(&zip.path)?;
        let archive = open_zip_archive(file)?;
        let names = zip_entry_names(&archive)?;
        let code = zip.subcorpus.as_code().to_owned();
        let mut candidates: Vec<(String, String, String)> = names
            .iter()
            .map(|entry| {
                (
                    basename_from_entry(entry).to_owned(),
                    zip.logical_name.clone(),
                    entry.clone(),
                )
            })
            .collect();
        candidates.sort_by_key(|(basename, ..)| select_order_key(seed, basename));
        by_tree
            .entry((code, matches!(zip.sampling, Sampling::Fixed)))
            .or_default()
            .extend(
                candidates
                    .into_iter()
                    .take(PER_TREE_QUOTA.max(DUAL_FIXED_QUOTA)),
            );
    }
    let mut rows: Vec<(String, String, String)> = Vec::new();
    for ((_code, fixed), candidates) in &by_tree {
        let quota = if *fixed {
            DUAL_FIXED_QUOTA
        } else {
            DUAL_VARIABLE_QUOTA
        };
        for (basename, zip, entry) in candidates.iter().take(quota) {
            rows.push((zip.clone(), entry.clone(), basename.clone()));
        }
    }
    rows.sort();
    for (zip, entry, basename) in rows {
        writeln!(out, "{zip}\t{entry}\t{basename}")?;
    }
    Ok(())
}

fn read_selection(path: &Path) -> io::Result<Vec<(String, String, String)>> {
    let text = fs::read_to_string(path)?;
    Ok(text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let mut parts = line.split('\t');
            (
                parts.next().expect("zip").to_owned(),
                parts.next().expect("entry").to_owned(),
                parts.next().expect("basename").to_owned(),
            )
        })
        .collect())
}

/// Opens the selected entry's bytes through the distribution's zip.
fn entry_bytes(root: &Path, zip_logical: &str, entry: &str) -> io::Result<Vec<u8>> {
    let inventory = BccwjInventory::discover(root, &TraversalLimits::default())?;
    let zip = inventory
        .zips
        .iter()
        .find(|zip| zip.logical_name == zip_logical)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("selection names a zip the inventory does not: {zip_logical}"),
            )
        })?;
    let file = fs::File::open(&zip.path)?;
    let mut archive = open_zip_archive(file)?;
    let index = zip_entries(&archive)?
        .into_iter()
        .find_map(|(name, index)| (name == entry).then_some(index))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("zip {zip_logical} has no entry {entry}"),
            )
        })?;
    let mut reader = archive.by_index(index)?;
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut reader, &mut bytes)?;
    Ok(bytes)
}

fn subcorpus_for(root: &Path, zip_logical: &str) -> io::Result<(Subcorpus, Sampling)> {
    let inventory = BccwjInventory::discover(root, &TraversalLimits::default())?;
    let zip = inventory
        .zips
        .iter()
        .find(|zip| zip.logical_name == zip_logical)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("selection names a zip the inventory does not: {zip_logical}"),
            )
        })?;
    Ok((zip.subcorpus, zip.sampling))
}

fn run_paragraphs(
    root: &Path,
    selection: &[(String, String, String)],
    out: &mut dyn Write,
) -> Result<u64, io::Error> {
    let mut failures = 0_u64;
    for (zip, entry, basename) in selection {
        let (subcorpus, _sampling) = subcorpus_for(root, zip)?;
        match entry_bytes(root, zip, entry).and_then(|bytes| {
            parse_entry(&bytes, subcorpus).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{zip}/{entry}: {error}"),
                )
            })
        }) {
            Ok(document) => {
                let paragraphs = document
                    .paragraphs
                    .iter()
                    .map(|paragraph| {
                        serde_json::json!({
                            "tags": paragraph.tags.iter().collect::<Vec<_>>(),
                            "sentences": paragraph.sentences.iter().collect::<Vec<_>>(),
                        })
                    })
                    .collect::<Vec<_>>();
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({
                        "zip": zip,
                        "entry": entry,
                        "basename": basename,
                        "subcorpus": subcorpus.as_code(),
                        "paragraphs": paragraphs,
                    })
                )?;
            }
            Err(error) => {
                failures += 1;
                eprintln!("parse failure: {error}");
            }
        }
    }
    Ok(failures)
}

fn run_metadata(
    root: &Path,
    selection: &[(String, String, String)],
    out: &mut dyn Write,
) -> Result<u64, io::Error> {
    let (metadata, notes) = BccwjMetadata::build(root)?;
    eprintln!(
        "metadata rows: {}; dropped category values: {}",
        metadata.len(),
        notes.dropped_category_values
    );
    let mut failures = 0_u64;
    for (zip, _entry, basename) in selection {
        let (_subcorpus, sampling) = subcorpus_for(root, zip)?;
        match metadata.lookup(basename) {
            Ok(doc) => {
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({
                        "basename": basename,
                        "metadata": doc.to_json(sampling),
                    })
                )?;
            }
            Err(error) => {
                failures += 1;
                eprintln!("metadata failure: {error}");
            }
        }
    }
    Ok(failures)
}

/// The normalization rule both sides of the content check share: NFC, then
/// every **ASCII** whitespace character stripped, nothing else dropped.
///
/// The stripped class has to cover the newline-and-indent between C-XML
/// tags, which the parser legitimately drops and the raw side legitimately
/// keeps — and nothing beyond it. Stripping all of Unicode `White_Space`
/// made the leg blind to exactly the defect the parser's own whitespace
/// rule can introduce: a text run made only of U+3000 or U+00A0, deleted on
/// one side and kept on the other. That is now a content difference the
/// check reports. Line-ending rewriting stays invisible here — `\r` and
/// `\n` are both stripped on both sides — and is covered by unit tests
/// instead.
fn normalize(text: &str) -> String {
    text.nfc().filter(|c| !c.is_ascii_whitespace()).collect()
}

/// The raw document text the way an XML reader sees it: `Text` (unescaped),
/// `GeneralRef` (expanded) and `CDATA` events in document order.
///
/// The reader splits an entity reference out of the surrounding text as its
/// own `GeneralRef` event, which `parse_entry` re-spells and expands.
/// Ignoring it here dropped the referenced character from the raw side only,
/// so any entry carrying one would have been reported as a content mismatch
/// the parser did not cause.
fn raw_document_text(bytes: &[u8]) -> Result<String, io::Error> {
    let xml = std::str::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut reader = Reader::from_str(xml);
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(_start)) => {}
            Ok(Event::End(_end)) => {}
            Ok(Event::Empty(_start)) => {}
            Ok(Event::Text(block)) => {
                let unescaped = quick_xml::escape::unescape(
                    std::str::from_utf8(block.as_ref())
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                )
                .map_err(|error| io::Error::other(format!("entity: {error}")))?;
                text.push_str(&unescaped);
            }
            Ok(Event::CData(block)) => {
                let bytes: &[u8] = &block.into_inner();
                text.push_str(std::str::from_utf8(bytes).map_err(|error| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("cdata utf-8: {error}"))
                })?);
            }
            Ok(Event::GeneralRef(reference)) => {
                let name = std::str::from_utf8(reference.as_ref())
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                let spelling = format!("&{name};");
                let expanded = quick_xml::escape::unescape(&spelling)
                    .map_err(|error| io::Error::other(format!("entity: {error}")))?;
                text.push_str(&expanded);
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "malformed C-XML at byte {}: {error}",
                        reader.buffer_position()
                    ),
                ));
            }
        }
    }
    Ok(text)
}

/// The content-identity leg: the raw document text and the parsed sentence
/// texts must normalize to one string. A mismatch reports the first
/// differing offset with a structural window — element path and offsets,
/// never the text.
fn run_content(
    root: &Path,
    selection: &[(String, String, String)],
    out: &mut dyn Write,
) -> Result<u64, io::Error> {
    let mut mismatches = 0_u64;
    for (zip, entry, basename) in selection {
        let (subcorpus, _sampling) = subcorpus_for(root, zip)?;
        let bytes = entry_bytes(root, zip, entry)?;
        let raw = match raw_document_text(&bytes) {
            Ok(raw) => raw,
            Err(error) => {
                mismatches += 1;
                writeln!(
                    out,
                    "{}",
                    serde_json::json!({"zip": zip, "entry": entry, "basename": basename,
                                        "ok": false, "reason": format!("raw extraction: {error}")})
                )?;
                continue;
            }
        };
        let document = parse_entry(&bytes, subcorpus).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{zip}/{entry}: {error}"),
            )
        })?;
        let mut parsed = String::new();
        for paragraph in &document.paragraphs {
            for sentence in &paragraph.sentences {
                parsed.push_str(sentence);
            }
        }
        let raw_norm = normalize(&raw);
        let parsed_norm = normalize(&parsed);
        if raw_norm == parsed_norm {
            writeln!(
                out,
                "{}",
                serde_json::json!({"zip": zip, "entry": entry, "basename": basename,
                                    "ok": true,
                                    "normalized_chars": raw_norm.chars().count()})
            )?;
            continue;
        }
        mismatches += 1;
        let offset = raw_norm
            .chars()
            .zip(parsed_norm.chars())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| raw_norm.chars().count().min(parsed_norm.chars().count()));
        let window_start = offset.saturating_sub(24);
        let window_end = (offset + 24).min(raw_norm.chars().count());
        // Offsets and lengths only: the window's *content* is not reported.
        writeln!(
            out,
            "{}",
            serde_json::json!({"zip": zip, "entry": entry, "basename": basename,
                                "ok": false,
                                "raw_chars": raw_norm.chars().count(),
                                "parsed_chars": parsed_norm.chars().count(),
                                "first_diff_offset": offset,
                                "context_window": [window_start, window_end]})
        )?;
    }
    Ok(mismatches)
}

fn main() -> io::Result<()> {
    let (command, args) = parse_args().map_err(|error| {
        eprintln!("bccwj-diff: {error}");
        io::Error::new(io::ErrorKind::InvalidInput, error)
    })?;
    let make_out = |args: &Args| -> Box<dyn Write + '_> {
        match &args.out {
            Some(path) => Box::new(BufWriter::new(
                fs::File::create(path).expect("create output"),
            )),
            None => Box::new(io::stdout()),
        }
    };
    let result = (|| -> io::Result<u64> {
        let mut out = make_out(&args);
        match command.as_str() {
            "select" => {
                select(&args.root, args.seed, &mut out)?;
                Ok(0)
            }
            "paragraphs" => {
                let selection = read_selection(args.selection.as_ref().expect("checked"))?;
                run_paragraphs(&args.root, &selection, &mut out)
            }
            "metadata" => {
                let selection = read_selection(args.selection.as_ref().expect("checked"))?;
                run_metadata(&args.root, &selection, &mut out)
            }
            "content" => {
                let selection = read_selection(args.selection.as_ref().expect("checked"))?;
                run_content(&args.root, &selection, &mut out)
            }
            other => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown subcommand {other}"),
            )),
        }
    })();
    let problem_count = result?;
    if problem_count > 0 {
        // A non-zero exit lets a shell gate on the harness run.
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{problem_count} problem record(s)"),
        ));
    }
    Ok(())
}
