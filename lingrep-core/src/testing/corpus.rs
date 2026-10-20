//! JSONL test sentences with expected matches and captures.

use crate::token_match::TokenInfo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// One source sentence and its expected matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotatedSentence {
    pub text: String,

    /// Optional pre-tokenized representation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Vec<TokenInfo>>,

    pub expected_matches: Vec<ExpectedMatch>,
}

/// An expected pattern and token span.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExpectedMatch {
    pub pattern: String,

    /// Start token index (inclusive)
    pub start: usize,

    /// End token index (exclusive)
    pub end: usize,

    /// Named capture spans.
    #[serde(default)]
    pub capture_groups: HashMap<String, MatchRange>,
}

/// Half-open token range.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatchRange {
    pub start: usize,

    pub end: usize,
}

impl AnnotatedSentence {
    /// Load nonblank JSONL records.
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Vec<Self>, Box<dyn std::error::Error>> {
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);

        let mut sentences = Vec::new();
        for line in std::io::BufRead::lines(reader) {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let annotated: AnnotatedSentence = serde_json::from_str(&line)?;
            sentences.push(annotated);
        }

        Ok(sentences)
    }
}
