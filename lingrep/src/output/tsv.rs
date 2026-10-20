use std::io::{self, Write};

use crate::protocol::OutputError;

pub const MATCHES_CONTRACT: &str = crate::protocol::ContractId::MatchesTsv.as_str();
pub const SUMMARY_CONTRACT: &str = crate::protocol::ContractId::SummarizeTsv.as_str();
pub const SENTENCE_FINAL_CONTRACT: &str = crate::protocol::ContractId::SentenceFinalTsv.as_str();
pub const TOKENIZE_CONTRACT: &str = crate::protocol::ContractId::TokenizeTsv.as_str();
pub const SOURCES_CONTRACT: &str = crate::protocol::ContractId::SourcesTsv.as_str();

macro_rules! columns {
    ($($name:literal),+ $(,)?) => {
        &[$(crate::protocol::discovery::ColumnSpec {
            name: $name,
            description: $name,
        }),+]
    };
}

pub(crate) fn attributes_contract() -> crate::protocol::discovery::ContractSpec {
    crate::protocol::discovery::tsv_contract(
        crate::protocol::ContractId::AttributesTsv,
        &["coverage"],
        "Headerless per-attribute presence rows.",
        columns![
            "attribute",
            "present",
            "unspecified",
            "absent",
            "admitted_tokens"
        ],
        "TokenAttributeIndex::ALL order.",
        "Every attribute, always. Exclusions need --json.",
        &["coverage/direct-root"],
    )
}

pub(crate) fn matches_contract() -> crate::protocol::discovery::ContractSpec {
    crate::protocol::discovery::tsv_contract(
        crate::protocol::ContractId::MatchesTsv,
        &["matches"],
        "Headerless per-match evidence rows.",
        columns![
            "sequence",
            "pattern",
            "source_name",
            "document_ordinal",
            "document_sentence_ordinal",
            "corpus_sentence_ordinal",
            "match_start",
            "match_end",
            "left_surface",
            "surface",
            "right_surface"
        ],
        "Canonical match order.",
        "Complete evidence or the requested first-N prefix.",
        &["matches/catalog-file-direct-root"],
    )
}

pub(crate) fn summarize_contract() -> crate::protocol::discovery::ContractSpec {
    crate::protocol::discovery::tsv_contract(
        crate::protocol::ContractId::SummarizeTsv,
        &["summarize"],
        "Headerless ranked typed-group frequency rows.",
        columns!["pattern", "rank", "group", "count"],
        "Authored definition; count descending; group kind surface, rendered unit, lexical term, projected key; \
         keyed field-name vector in authored order; keyed value vector.",
        "Exact aggregate, optionally top-N per pattern.",
        &["summarize/catalog-file-direct-root"],
    )
}

pub(crate) fn sentence_final_contract() -> crate::protocol::discovery::ContractSpec {
    crate::protocol::discovery::tsv_contract(
        crate::protocol::ContractId::SentenceFinalTsv,
        &["sentence-final"],
        "Headerless sentence-final frequency rows.",
        columns!["pattern", "surface", "count"],
        "Pattern then UTF-8 surface order.",
        "Exact aggregate after successful execution.",
        &["sentence-final/catalog-file-direct-root"],
    )
}

pub(crate) fn tokenize_contract() -> crate::protocol::discovery::ContractSpec {
    crate::protocol::discovery::tsv_contract(
        crate::protocol::ContractId::TokenizeTsv,
        &["tokenize"],
        "Headerless token rows with corpus provenance.",
        columns![
            "corpus_sentence_ordinal",
            "source_name",
            "document_ordinal",
            "document_sentence_ordinal",
            "token_index",
            "orth",
            "lemma",
            "pos",
            "cform"
        ],
        "Canonical corpus sentence and token order.",
        "Complete token evidence unless the command fails.",
        &["tokenize/direct-root"],
    )
}

pub(crate) fn sources_contract() -> crate::protocol::discovery::ContractSpec {
    crate::protocol::discovery::tsv_contract(
        crate::protocol::ContractId::SourcesTsv,
        &["corpus inventory"],
        "Headerless source inventory rows.",
        columns![
            "ordinal",
            "logical_name",
            "target_group_ordinal",
            "external_target",
            "target_multiplicity"
        ],
        "Stable source ordinal order.",
        "Live-discovered or recorded inventory as selected by the command.",
        &["corpus inventory/direct-root"],
    )
}

/// Lossy fixed-arity TSV row for a query match. Captures and token maps remain
/// available only through the authoritative JSONL event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchTsvRow {
    pub sequence: u64,
    pub pattern: String,
    pub source_name: String,
    pub document_ordinal: u64,
    pub document_sentence_ordinal: u64,
    pub corpus_sentence_ordinal: u64,
    pub match_start: u64,
    pub match_end: u64,
    pub left_surface: Option<String>,
    pub surface: String,
    pub right_surface: Option<String>,
}

/// Fixed four-column TSV projection for an exact summary row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryTsvRow {
    pub pattern: String,
    pub rank: u64,
    pub group: String,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SentenceFinalTsvRow {
    pub pattern: String,
    pub surface: String,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenizeTsvRow {
    pub corpus_sentence_ordinal: u64,
    pub source_name: String,
    pub document_ordinal: u64,
    pub document_sentence_ordinal: u64,
    pub token_index: u64,
    pub orth: String,
    pub lemma: String,
    pub pos: String,
    pub cform: String,
}

/// One attribute's presence over the admitted tokens. `admitted_tokens`
/// repeats on every row so a row stands alone as the denominator its three
/// counts partition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributesTsvRow {
    pub attribute: String,
    pub present: u64,
    pub unspecified: u64,
    pub absent: u64,
    pub admitted_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceTsvRow {
    pub ordinal: u64,
    pub logical_name: String,
    pub target_group_ordinal: u64,
    pub external_target: bool,
    pub target_multiplicity: Option<u64>,
}

/// Write one tab-delimited field using RFC-4180-style quoting. Only TAB, LF,
/// CR, and quote require quoting; the field is otherwise emitted unchanged.
pub fn write_tsv_field<W: Write>(writer: &mut W, value: &str) -> io::Result<()> {
    if value.contains(['\t', '\n', '\r', '"']) {
        writer.write_all(b"\"")?;
        for chunk in value.split_inclusive('"') {
            writer.write_all(chunk.as_bytes())?;
            if chunk.ends_with('"') {
                writer.write_all(b"\"")?;
            }
        }
        writer.write_all(b"\"")
    } else {
        writer.write_all(value.as_bytes())
    }
}

/// Write the exactly eleven match TSV columns in their public order.
pub fn write_match_tsv<W: Write>(writer: &mut W, row: &MatchTsvRow) -> Result<(), OutputError> {
    let sequence = row.sequence.to_string();
    let document_ordinal = row.document_ordinal.to_string();
    let document_sentence_ordinal = row.document_sentence_ordinal.to_string();
    let corpus_sentence_ordinal = row.corpus_sentence_ordinal.to_string();
    let match_start = row.match_start.to_string();
    let match_end = row.match_end.to_string();
    let fields = [
        sequence.as_str(),
        row.pattern.as_str(),
        row.source_name.as_str(),
        document_ordinal.as_str(),
        document_sentence_ordinal.as_str(),
        corpus_sentence_ordinal.as_str(),
        match_start.as_str(),
        match_end.as_str(),
        row.left_surface.as_deref().unwrap_or(""),
        row.surface.as_str(),
        row.right_surface.as_deref().unwrap_or(""),
    ];
    for (index, field) in fields.iter().enumerate() {
        if index != 0 {
            writer.write_all(b"\t")?;
        }
        write_tsv_field(writer, field)?;
    }
    writer.write_all(b"\n")?;
    Ok(())
}

/// Write `pattern`, `rank`, `group`, and `count` in the fixed summary TSV
/// order. It intentionally has no header row.
pub fn write_summary_tsv<W: Write>(writer: &mut W, row: &SummaryTsvRow) -> Result<(), OutputError> {
    let rank = row.rank.to_string();
    let count = row.count.to_string();
    for (index, field) in [
        row.pattern.as_str(),
        rank.as_str(),
        row.group.as_str(),
        count.as_str(),
    ]
    .iter()
    .enumerate()
    {
        if index != 0 {
            writer.write_all(b"\t")?;
        }
        write_tsv_field(writer, field)?;
    }
    writer.write_all(b"\n")?;
    Ok(())
}

pub fn write_sentence_final_tsv<W: Write>(
    writer: &mut W,
    row: &SentenceFinalTsvRow,
) -> Result<(), OutputError> {
    let count = row.count.to_string();
    write_tsv_row(writer, &[&row.pattern, &row.surface, &count])
}

pub fn write_tokenize_tsv<W: Write>(
    writer: &mut W,
    row: &TokenizeTsvRow,
) -> Result<(), OutputError> {
    let corpus_sentence_ordinal = row.corpus_sentence_ordinal.to_string();
    let document_ordinal = row.document_ordinal.to_string();
    let document_sentence_ordinal = row.document_sentence_ordinal.to_string();
    let token_index = row.token_index.to_string();
    write_tsv_row(
        writer,
        &[
            &corpus_sentence_ordinal,
            &row.source_name,
            &document_ordinal,
            &document_sentence_ordinal,
            &token_index,
            &row.orth,
            &row.lemma,
            &row.pos,
            &row.cform,
        ],
    )
}

pub fn write_attributes_tsv<W: Write>(
    writer: &mut W,
    row: &AttributesTsvRow,
) -> Result<(), OutputError> {
    let present = row.present.to_string();
    let unspecified = row.unspecified.to_string();
    let absent = row.absent.to_string();
    let admitted_tokens = row.admitted_tokens.to_string();
    write_tsv_row(
        writer,
        &[
            &row.attribute,
            &present,
            &unspecified,
            &absent,
            &admitted_tokens,
        ],
    )
}

pub fn write_source_tsv<W: Write>(writer: &mut W, row: &SourceTsvRow) -> Result<(), OutputError> {
    let ordinal = row.ordinal.to_string();
    let target_group_ordinal = row.target_group_ordinal.to_string();
    let external_target = row.external_target.to_string();
    let target_multiplicity = row.target_multiplicity.map(|value| value.to_string());
    write_tsv_row(
        writer,
        &[
            &ordinal,
            &row.logical_name,
            &target_group_ordinal,
            &external_target,
            target_multiplicity.as_deref().unwrap_or(""),
        ],
    )
}

fn write_tsv_row<W: Write>(writer: &mut W, fields: &[&str]) -> Result<(), OutputError> {
    for (index, field) in fields.iter().enumerate() {
        if index != 0 {
            writer.write_all(b"\t")?;
        }
        write_tsv_field(writer, field)?;
    }
    writer.write_all(b"\n")?;
    Ok(())
}
