//! Public output projections for `ling`.
//!
//! JSONL is the authoritative protocol. TSV and aligned output are deliberately
//! separate projections so neither terminal escaping nor lossy TSV fallbacks
//! can alter machine evidence.

mod aligned;
mod human;
mod style;
mod tsv;

#[cfg(test)]
mod human_tests;
#[cfg(test)]
mod tsv_tests;

pub use aligned::format_kwic_aligned_line;
pub use human::{
    HumanKwicRow, HumanMatchLayout, HumanRenderer, MIN_HUMAN_MATCH_WIDTH, visible_width,
};
pub use lingrep_cli_common::sanitize_terminal;
pub use style::{
    ColorPolicy, ContextColorPalette, ContextColorTheme, ContextStyle, ResolvedContextColorTheme,
    TerminalColorCapability,
};
pub use tsv::{
    AttributesTsvRow, MATCHES_CONTRACT, MatchTsvRow, SENTENCE_FINAL_CONTRACT, SOURCES_CONTRACT,
    SUMMARY_CONTRACT, SentenceFinalTsvRow, SourceTsvRow, SummaryTsvRow, TOKENIZE_CONTRACT,
    TokenizeTsvRow, write_attributes_tsv, write_match_tsv, write_sentence_final_tsv,
    write_source_tsv, write_summary_tsv, write_tokenize_tsv, write_tsv_field,
};

/// A single key-word-in-context output row.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct KwicLine {
    pub pre: String,
    pub key: String,
    pub post: String,
    pub match_start: usize,
    pub match_end: usize,
}

pub(crate) use tsv::{
    attributes_contract, matches_contract, sentence_final_contract, sources_contract,
    summarize_contract, tokenize_contract,
};
