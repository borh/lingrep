//! `diff`: the occurrences two `ling matches --json` runs share, and those
//! only one of them has.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Write};
use std::path::Path;

use serde_json::Value;

use crate::cli::{DiffArgs, DiffLevel};
use crate::error::CliError;
use crate::presentation::OutputMode;
use crate::protocol::diff::{
    Coverage, DefinitionDiff, DiffComplete, DiffEnvelope, DiffEvent, DiffRun, Level, Occurrence,
    RunSide, Side, Span, UnpairedDefinition,
};
use crate::protocol::write_jsonl;

use super::CommandOutcome;

const QUERY_SCHEMA: &str = "lingrep.query/v2";

/// The match-record fields that do not belong to the published record:
/// positions in the stream, the sentence's address, and text derived from
/// the corpus.
const UNCOMPARED_FIELDS: [&str; 12] = [
    "sequence",
    "pattern",
    "char_span",
    "surface",
    "tokens",
    "left_context",
    "right_context",
    "evidence",
    "corpus_sentence_ordinal",
    "source_name",
    "document_ordinal",
    "document_sentence_ordinal",
];

pub fn run(args: &DiffArgs, mode: OutputMode) -> Result<CommandOutcome, CliError> {
    if mode == OutputMode::Tsv {
        return Err(CliError::UnsupportedOutput {
            command: "diff",
            output: "tsv",
        });
    }
    let a = read_stream(&args.a, "A")?;
    let b = if args.b == args.a {
        a.clone()
    } else {
        read_stream(&args.b, "B")?
    };
    let comparison = compare(&a, &b, args).map_err(CliError::InvalidArgs)?;
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    if mode == OutputMode::Jsonl {
        for event in comparison.events() {
            write_jsonl(&mut writer, &DiffEnvelope::new(event))?;
        }
    } else {
        comparison
            .write_text(&mut writer)
            .map_err(CliError::stdout)?;
    }
    Ok(CommandOutcome::Success)
}

/// One match record, reduced to what a comparison reads.
#[derive(Clone, Debug)]
struct Record {
    pattern: String,
    source_name: String,
    document_ordinal: u64,
    document_sentence_ordinal: u64,
    corpus_sentence_ordinal: Option<u64>,
    span: Span,
    /// Each capture's and graph binding's position, by name.
    roles: BTreeMap<String, String>,
    /// The published record in a canonical text form.
    published: String,
    surface: String,
    sentence: Option<String>,
}

#[derive(Clone, Debug)]
struct Stream {
    run: Value,
    complete: Value,
    patterns: Vec<String>,
    records: Vec<Record>,
}

fn read_stream(path: &Path, label: &str) -> Result<Stream, CliError> {
    let file = std::fs::File::open(path).map_err(|error| CliError::from(error).at_path(path))?;
    let mut run = None;
    let mut complete = None;
    let mut patterns = Vec::new();
    let mut records = Vec::new();
    for (index, line) in io::BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|error| CliError::from(error).at_path(path))?;
        if line.trim().is_empty() {
            continue;
        }
        let at = format!("{label} line {}", index + 1);
        let invalid = |message: &str| CliError::InvalidArgs(format!("{at}: {message}"));
        if complete.is_some() {
            return Err(invalid("an event follows the complete event"));
        }
        let mut event: Value = serde_json::from_str(&line)
            .map_err(|error| invalid(&format!("invalid JSON: {error}")))?;
        if event["schema"] != QUERY_SCHEMA {
            return Err(invalid(&format!("expected a {QUERY_SCHEMA} event")));
        }
        let kind = event["type"].as_str().unwrap_or_default().to_owned();
        let data = event["data"].take();
        if run.is_none() {
            if kind != "run" || data["mode"] != "matches" {
                return Err(CliError::InvalidArgs(format!(
                    "{label}: the stream must begin with a `ling --json matches` run event"
                )));
            }
            run = Some(data);
            continue;
        }
        match kind.as_str() {
            "definition" => {
                if data["body"]["kind"] == "pattern"
                    && let Some(name) = data["name"].as_str()
                {
                    patterns.push(name.to_owned());
                }
            }
            "match" => records.push(
                record(data).ok_or_else(|| invalid("the match record lacks a required field"))?,
            ),
            "complete" => complete = Some(data),
            other => return Err(invalid(&format!("unexpected `{other}` event"))),
        }
    }
    let run = run.ok_or_else(|| CliError::InvalidArgs(format!("{label}: empty stream")))?;
    let complete = complete.ok_or_else(|| {
        CliError::InvalidArgs(format!(
            "{label}: no complete event; the run failed or was interrupted"
        ))
    })?;
    Ok(Stream {
        run,
        complete,
        patterns,
        records,
    })
}

fn record(mut data: Value) -> Option<Record> {
    let pattern = data["pattern"].as_str()?.to_owned();
    let span = Span {
        start: data["span"]["start"].as_u64()?,
        end: data["span"]["end"].as_u64()?,
    };
    let mut roles = BTreeMap::new();
    for capture in data["captures"].as_array().into_iter().flatten() {
        let (start, end) = (
            capture["span"]["start"].as_u64()?,
            capture["span"]["end"].as_u64()?,
        );
        roles.insert(
            capture["name"].as_str()?.to_owned(),
            (start..end)
                .map(|index| (index + 1).to_string())
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    for binding in data["graph_bindings"].as_array().into_iter().flatten() {
        let position = match binding["binding_kind"].as_str()? {
            "node" => node(&binding["node"])?,
            "edge" => format!(
                "{}>{}",
                node(&binding["head"])?,
                node(&binding["dependent"])?
            ),
            _ => continue,
        };
        roles.insert(binding["name"].as_str()?.to_owned(), position);
    }
    let sentence = data["evidence"]["sentence"].as_str().map(str::to_owned);
    let reduced = Record {
        source_name: data["source_name"].as_str()?.to_owned(),
        document_ordinal: data["document_ordinal"].as_u64()?,
        document_sentence_ordinal: data["document_sentence_ordinal"].as_u64()?,
        corpus_sentence_ordinal: data["corpus_sentence_ordinal"].as_u64(),
        span,
        roles,
        surface: data["surface"].as_str().unwrap_or_default().to_owned(),
        sentence,
        published: String::new(),
        pattern: String::new(),
    };
    // A capture's path begins with its definition's name, which differs
    // between two definitions that publish the same record.
    let prefix = format!("{pattern}/");
    if let Some(object) = data.as_object_mut() {
        for field in UNCOMPARED_FIELDS {
            object.remove(field);
        }
    }
    for capture in data["captures"].as_array_mut().into_iter().flatten() {
        if let Some(object) = capture.as_object_mut() {
            object.remove("char_span");
            if let Some(path) = object.get("hygienic_path").and_then(Value::as_str)
                && let Some(local) = path.strip_prefix(&prefix)
            {
                let local = local.to_owned();
                object.insert("hygienic_path".into(), Value::String(local));
            }
        }
    }
    let mut published = String::new();
    canonical(&data, &mut published);
    Some(Record {
        pattern,
        published,
        ..reduced
    })
}

/// A word's number, or an empty node as `boundary.subindex`.
fn node(value: &Value) -> Option<String> {
    if value["node_kind"] == "word" {
        Some((value["token_index"].as_u64()? + 1).to_string())
    } else {
        Some(format!("{}.{}", value["boundary"], value["subindex"]))
    }
}

/// JSON with object keys in bytewise order, so equal values have equal text.
fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical(&object[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// The sentence bound a stream's records are complete below, or `None` for a
/// run that read every admitted sentence.
fn completed_before(stream: &Stream, label: &str) -> Result<Option<u64>, String> {
    let complete = &stream.complete;
    if !complete["retained_matches"].is_null() {
        return Err(format!(
            "{label}: the run kept a --retain sample of its matches, which another run's matches cannot be compared with; rerun without --retain"
        ));
    }
    if complete["analysis_identity"].as_str().is_none() {
        return Err(format!(
            "{label}: the run records no corpus analysis identity, as with standard input or direct input stopped at --limit; run over files with --limit all, or over a prepared corpus (`ling corpus build`)"
        ));
    }
    if complete["coverage"] == "full" && complete["termination"] == "eof" {
        return Ok(None);
    }
    // Records are published in corpus order, so the sentence holding the last
    // published record may be incomplete and every earlier one is complete.
    complete["committed_prefix"]["corpus_sentence_ordinal"]
        .as_u64()
        .map(Some)
        .ok_or_else(|| {
            format!(
                "{label}: the run stopped early without recording the sentence it reached; rerun with --limit all"
            )
        })
}

struct Pair {
    diff: DefinitionDiff,
    occurrences: Vec<Occurrence>,
}

struct Comparison {
    run: DiffRun,
    pairs: Vec<Pair>,
    complete: DiffComplete,
}

fn compare(a: &Stream, b: &Stream, args: &DiffArgs) -> Result<Comparison, String> {
    let identity = |stream: &Stream| {
        let complete = &stream.complete;
        (
            complete["corpus_kind"].clone(),
            complete["source_identity"].clone(),
            complete["analysis_identity"].clone(),
            complete["scope"].clone(),
            stream.run["sample"].clone(),
        )
    };
    let bound = match (completed_before(a, "A")?, completed_before(b, "B")?) {
        (None, None) => None,
        (Some(bound), None) | (None, Some(bound)) => Some(bound),
        (Some(left), Some(right)) => Some(left.min(right)),
    };
    if identity(a) != identity(b) {
        return Err(
            "A and B differ in corpus, analysis, --where scope or --sample; compare runs over the same corpus and selection"
                .into(),
        );
    }
    let within = |stream: &Stream, label: &str| -> Result<Vec<Record>, String> {
        let Some(bound) = bound else {
            return Ok(stream.records.clone());
        };
        stream
            .records
            .iter()
            .filter_map(|record| match record.corpus_sentence_ordinal {
                Some(ordinal) => (ordinal < bound).then(|| Ok(record.clone())),
                None => Some(Err(format!(
                    "{label}: a record has no corpus sentence ordinal, so a run stopped at --limit cannot be aligned; rerun with --limit all"
                ))),
            })
            .collect()
    };
    let (a_records, b_records) = (within(a, "A")?, within(b, "B")?);
    let coverage = match bound {
        Some(before_corpus_sentence) => Coverage::Prefix {
            before_corpus_sentence,
        },
        None => Coverage::Full {
            sentences_scanned: a.complete["counters"]["sentences_admitted"]
                .as_u64()
                .ok_or("A: the complete event has no sentences_admitted counter")?,
        },
    };

    let defined = |stream: &Stream, name: &str, label: &str| {
        if stream.patterns.iter().any(|pattern| pattern == name) {
            Ok(())
        } else {
            Err(format!("{label}: the run has no pattern `{name}`"))
        }
    };
    let mut unpaired = (Vec::new(), Vec::new());
    let names: Vec<(String, String)> = match (&args.a_pattern, &args.b_pattern) {
        (None, None) => {
            let count = |records: &[Record], name: &str| {
                records
                    .iter()
                    .filter(|record| record.pattern == name)
                    .count() as u64
            };
            for name in &a.patterns {
                if !b.patterns.contains(name) {
                    unpaired.0.push(UnpairedDefinition {
                        name: name.clone(),
                        records: count(&a_records, name),
                    });
                }
            }
            for name in &b.patterns {
                if !a.patterns.contains(name) {
                    unpaired.1.push(UnpairedDefinition {
                        name: name.clone(),
                        records: count(&b_records, name),
                    });
                }
            }
            let mut shared = a
                .patterns
                .iter()
                .filter(|name| b.patterns.contains(name))
                .map(|name| (name.clone(), name.clone()))
                .collect::<Vec<_>>();
            shared.sort();
            shared
        }
        (left, right) => {
            let left = left.clone().or_else(|| right.clone()).unwrap_or_default();
            let right = right.clone().unwrap_or_else(|| left.clone());
            defined(a, &left, "A")?;
            defined(b, &right, "B")?;
            vec![(left, right)]
        }
    };

    let level = match args.level {
        DiffLevel::Span => Level::Span,
        DiffLevel::Roles => Level::Roles,
        DiffLevel::Record => Level::Record,
    };
    let mut pairs = Vec::new();
    for (left, right) in names {
        let select = |records: &[Record], name: &str| {
            records
                .iter()
                .filter(|record| record.pattern == name)
                .cloned()
                .collect::<Vec<_>>()
        };
        pairs.push(compare_pair(
            &left,
            &right,
            &select(&a_records, &left),
            &select(&b_records, &right),
            level,
            &args.roles,
            args.examples,
        )?);
    }

    let total =
        |field: fn(&DefinitionDiff) -> u64| pairs.iter().map(|pair| field(&pair.diff)).sum();
    let (shared, a_only, b_only): (u64, u64, u64) = (
        total(|diff| diff.shared),
        total(|diff| diff.a_only),
        total(|diff| diff.b_only),
    );
    let differences_found = a_only > 0
        || b_only > 0
        || unpaired
            .0
            .iter()
            .chain(&unpaired.1)
            .any(|definition| definition.records > 0);
    let side = |stream: &Stream| RunSide {
        run_id: stream.run["run_id"].as_str().unwrap_or_default().to_owned(),
        catalog_fingerprint: stream.run["catalog_fingerprint"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    };
    let optional = |value: &Value| (!value.is_null()).then(|| value.clone());
    Ok(Comparison {
        run: DiffRun {
            level,
            a: side(a),
            b: side(b),
            corpus_kind: a.complete["corpus_kind"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            source_identity: a.complete["source_identity"].as_str().map(str::to_owned),
            analysis_identity: a.complete["analysis_identity"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            scope: optional(&a.complete["scope"]),
            sample: optional(&a.run["sample"]),
            coverage,
        },
        complete: DiffComplete {
            definitions_compared: pairs.len() as u64,
            definitions_differing: pairs
                .iter()
                .filter(|pair| pair.diff.a_only > 0 || pair.diff.b_only > 0)
                .count() as u64,
            shared,
            a_only,
            b_only,
            a_only_definitions: unpaired.0,
            b_only_definitions: unpaired.1,
            differences_found,
        },
        pairs,
    })
}

type Key = (u64, u64, String, String);

fn compare_pair(
    left: &str,
    right: &str,
    a: &[Record],
    b: &[Record],
    level: Level,
    requested: &[String],
    examples: usize,
) -> Result<Pair, String> {
    let roles: Vec<String> = if level != Level::Roles {
        Vec::new()
    } else if !requested.is_empty() {
        requested.to_vec()
    } else {
        let mut common: Option<BTreeSet<&String>> = None;
        for record in a.iter().chain(b) {
            let names = record.roles.keys().collect::<BTreeSet<_>>();
            common = Some(match common {
                Some(common) => common.intersection(&names).copied().collect(),
                None => names,
            });
        }
        let common = common.unwrap_or_default();
        if common.is_empty() && !(a.is_empty() && b.is_empty()) {
            return Err(format!(
                "`{left}` and `{right}` bind no role name in every record; name roles with --roles or compare at --level span"
            ));
        }
        common.into_iter().cloned().collect()
    };
    let unit = |record: &Record, label: &str| -> Result<String, String> {
        Ok(match level {
            Level::Span => format!("{}..{}", record.span.start, record.span.end),
            Level::Record => record.published.clone(),
            Level::Roles => roles
                .iter()
                .map(|role| {
                    record
                        .roles
                        .get(role)
                        .map(|position| format!("{role}={position}"))
                        .ok_or_else(|| {
                            format!(
                                "{label}: a record of `{}` has no role `{role}`; name roles every record binds with --roles",
                                record.pattern
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?
                .join(" "),
        })
    };
    // Keyed by sentence, then the unit: iteration is corpus order.
    let mut counts: BTreeMap<Key, ([u64; 2], &Record)> = BTreeMap::new();
    for (side, records, label) in [(0, a, "A"), (1, b, "B")] {
        for record in records {
            let key = (
                record.document_ordinal,
                record.document_sentence_ordinal,
                record.source_name.clone(),
                unit(record, label)?,
            );
            counts.entry(key).or_insert(([0, 0], record)).0[side] += 1;
        }
    }
    let mut diff = DefinitionDiff {
        a: left.to_owned(),
        b: right.to_owned(),
        roles,
        shared: 0,
        a_only: 0,
        b_only: 0,
    };
    let mut shown = (Vec::new(), Vec::new());
    for (key, (count, record)) in &counts {
        let (status, records, list) = match count {
            [0, records] => {
                diff.b_only += 1;
                (Side::BOnly, *records, &mut shown.1)
            }
            [records, 0] => {
                diff.a_only += 1;
                (Side::AOnly, *records, &mut shown.0)
            }
            _ => {
                diff.shared += 1;
                continue;
            }
        };
        if list.len() < examples {
            list.push(Occurrence {
                a: left.to_owned(),
                b: right.to_owned(),
                status,
                records,
                source_name: record.source_name.clone(),
                document_ordinal: record.document_ordinal,
                document_sentence_ordinal: record.document_sentence_ordinal,
                corpus_sentence_ordinal: record.corpus_sentence_ordinal,
                span: record.span,
                unit: match level {
                    // The canonical record is long; its span and roles name it.
                    Level::Record => {
                        std::iter::once(format!("{}..{}", record.span.start, record.span.end))
                            .chain(
                                record
                                    .roles
                                    .iter()
                                    .map(|(role, position)| format!("{role}={position}")),
                            )
                            .collect::<Vec<_>>()
                            .join(" ")
                    }
                    Level::Span | Level::Roles => key.3.clone(),
                },
                surface: record.surface.clone(),
                sentence: record.sentence.clone(),
            });
        }
    }
    let mut occurrences = shown.0;
    occurrences.append(&mut shown.1);
    Ok(Pair { diff, occurrences })
}

impl Comparison {
    fn events(self) -> impl Iterator<Item = DiffEvent> {
        std::iter::once(DiffEvent::Run(self.run))
            .chain(self.pairs.into_iter().flat_map(|pair| {
                std::iter::once(DiffEvent::Definition(pair.diff))
                    .chain(pair.occurrences.into_iter().map(DiffEvent::Occurrence))
            }))
            .chain(std::iter::once(DiffEvent::Complete(self.complete)))
    }

    /// The sentences compared, as the closing line states them.
    fn scanned(&self) -> String {
        let sampled = if self.run.sample.is_some() {
            " of the --sample selection"
        } else {
            ""
        };
        let scoped = if self.run.scope.is_some() {
            " within the --where scope"
        } else {
            ""
        };
        match self.run.coverage {
            Coverage::Full { sentences_scanned } => {
                format!("{sentences_scanned} sentences scanned{sampled}{scoped}")
            }
            Coverage::Prefix {
                before_corpus_sentence,
            } => format!(
                "the sentences before corpus sentence {before_corpus_sentence}{sampled}{scoped}, where a run stopped at --limit"
            ),
        }
    }

    fn write_text<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let level = match self.run.level {
            Level::Span => "span (the match's token span)",
            Level::Roles => "roles (the tokens each compared role occupies)",
            Level::Record => "record (the published record)",
        };
        writeln!(writer, "level: {level}")?;
        writeln!(writer, "compared: {}", self.scanned())?;
        for pair in &self.pairs {
            let diff = &pair.diff;
            let names = if diff.a == diff.b {
                diff.a.clone()
            } else {
                format!("{} (A) / {} (B)", diff.a, diff.b)
            };
            let roles = if diff.roles.is_empty() {
                String::new()
            } else {
                format!(" [roles {}]", diff.roles.join(","))
            };
            writeln!(
                writer,
                "{names}{roles}: shared {}, a_only {}, b_only {}",
                diff.shared, diff.a_only, diff.b_only
            )?;
            for occurrence in &pair.occurrences {
                let status = match occurrence.status {
                    Side::AOnly => "a_only",
                    Side::BOnly => "b_only",
                };
                writeln!(
                    writer,
                    "  {status}\t{} {}:{}\t{}\t{}\t{}",
                    occurrence.source_name,
                    occurrence.document_ordinal,
                    occurrence.document_sentence_ordinal,
                    occurrence.unit,
                    occurrence.surface,
                    occurrence.sentence.as_deref().unwrap_or_default(),
                )?;
            }
        }
        for (side, definitions) in [
            ("A", &self.complete.a_only_definitions),
            ("B", &self.complete.b_only_definitions),
        ] {
            for definition in definitions {
                writeln!(
                    writer,
                    "{} is defined only in {side}: {} record(s)",
                    definition.name, definition.records
                )?;
            }
        }
        if self.complete.differences_found {
            writeln!(
                writer,
                "{} occurrence(s) only in A and {} only in B, in {}",
                self.complete.a_only,
                self.complete.b_only,
                self.scanned()
            )
        } else {
            writeln!(writer, "no difference in {}", self.scanned())
        }
    }
}
