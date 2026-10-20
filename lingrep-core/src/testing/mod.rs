//! Shared fixtures and helpers for in-process tests.
//!
//! The dictionary-backed tokenizer is initialized once per test process.

use std::sync::OnceLock;
use std::time::Instant;

use crate::dictionary::create_tokenizer_from_path;
use crate::fsm::{FsmBuilder, PatternSource};

pub use vibrato_rkyv::Tokenizer;

pub use crate::token_match::{TokenInfo, tokenize};

pub mod campaign_sentences;
pub mod catalogs;
pub mod corpus;
pub mod helpers;
mod mini_dictionary;

pub use mini_dictionary::MiniDictionary;

pub use corpus::{AnnotatedSentence, ExpectedMatch, MatchRange};

pub use helpers::{compile_test_pattern, token, tokenize_japanese};

static TEST_TOKENIZER: OnceLock<Tokenizer> = OnceLock::new();

/// Path to the Nix-provided UniDic dictionary artifact.
///
/// The `nix develop` shell always exports `LINGREP_DICTIONARY_PATH` (see `flake.nix`),
/// so every test and benchmark that needs a real dictionary resolves it here.
/// Absence means the suite is running outside `nix develop`, which is
/// unsupported — so this panics rather than skipping, keeping "the dictionary is
/// always available" a hard invariant instead of a per-test guess.
pub fn dictionary_path() -> String {
    std::env::var("LINGREP_DICTIONARY_PATH").expect(
        "LINGREP_DICTIONARY_PATH is unset — run tests inside `nix develop`, which exports it",
    )
}

/// Aggregate measurements from one matching test run.
#[derive(Debug, Clone, PartialEq)]
pub struct TestStats {
    pub sentences_processed: usize,
    pub matches_found: usize,
    pub tokens_processed: usize,
    /// Elapsed wall time in milliseconds.
    pub processing_time_ms: u64,
}

/// Return the process-wide test tokenizer.
pub fn test_tokenizer() -> &'static Tokenizer {
    TEST_TOKENIZER.get_or_init(|| {
        create_tokenizer_from_path(&dictionary_path())
            .expect("default test dictionary must be available")
    })
}

/// Compile named patterns and match sentences with the shared tokenizer.
pub fn run_matching_test(patterns: &[(&str, &str)], sentences: &[&str]) -> TestStats {
    let start = Instant::now();

    let mut builder = FsmBuilder::new();

    for (name, pattern) in patterns {
        // Formatted as `pattern NAME := …`, with the declaration explicit
        // rather than bare: a caller is free to name a pattern `test`, and
        // `test` is a declaration keyword a *bare* definition cannot take.
        let pattern_str = format!("pattern {name} := {pattern}.");
        builder
            .add_definition(PatternSource::named(name, &pattern_str))
            .expect("Failed to add pattern");
    }
    let matcher = builder.build().expect("Failed to build matcher").matcher;

    let tokenizer = test_tokenizer();

    let mut total_matches = 0;
    let mut total_tokens = 0;

    for sentence in sentences {
        let tokens: Vec<TokenInfo> = crate::token_match::tokenize(sentence, tokenizer);
        total_tokens += tokens.len();

        let matches = matcher.find_matches(&tokens);
        total_matches += matches.len();
    }

    let elapsed = start.elapsed();

    TestStats {
        sentences_processed: sentences.len(),
        matches_found: total_matches,
        tokens_processed: total_tokens,
        processing_time_ms: elapsed.as_millis() as u64,
    }
}

#[cfg(feature = "testing")]
pub mod strategies {
    use proptest::collection::vec;
    use proptest::prelude::*;

    /// Strategy for generating valid Japanese test sentences
    ///
    /// Uses pre-defined Japanese sentence templates to generate
    /// linguistically valid input (not random ASCII).
    ///
    /// # Example
    /// ```rust
    /// use proptest::proptest;
    /// use lingrep_core::testing::strategies::japanese_sentence;
    ///
    /// proptest! {
    ///     fn test_with_japanese(s in japanese_sentence()) {
    ///         println!("Testing with: {}", s);
    ///     }
    /// }
    /// ```
    pub fn japanese_sentence() -> impl Strategy<Value = String> {
        static TEMPLATES: &[&str] = &[
            "これはテストです。",
            "日本語の文章をテストしています。",
            "形態素解析の確認です。",
            "文末表現の研究データです。",
            "科学論文からのサンプル文です。",
            "データセットの検証用テキストです。",
            "システムの動作確認をします。",
            "パフォーマンスの計測です。",
            "正解率を検証します。",
            "エッジケースのテストです。",
        ];

        prop_oneof![
            Just(TEMPLATES[0].to_string()),
            Just(TEMPLATES[1].to_string()),
            Just(TEMPLATES[2].to_string()),
            Just(TEMPLATES[3].to_string()),
            Just(TEMPLATES[4].to_string()),
            Just(TEMPLATES[5].to_string()),
            Just(TEMPLATES[6].to_string()),
            Just(TEMPLATES[7].to_string()),
            Just(TEMPLATES[8].to_string()),
            Just(TEMPLATES[9].to_string()),
        ]
    }

    /// Strategy for generating a corpus of Japanese sentences
    ///
    /// # Example
    /// ```rust
    /// use proptest::proptest;
    /// use lingrep_core::testing::strategies::japanese_corpus;
    ///
    /// proptest! {
    ///     fn test_with_corpus(corpus in japanese_corpus(1..100)) {
    ///         assert!(corpus.len() < 100);
    ///         for sentence in corpus {
    ///             println!("{}", sentence);
    ///         }
    ///     }
    /// }
    /// ```
    pub fn japanese_corpus(
        size: impl Into<prop::sample::SizeRange>,
    ) -> impl Strategy<Value = Vec<String>> {
        vec(japanese_sentence(), size)
    }

    /// Strategy for generating pattern names
    ///
    /// Generates valid pattern identifier names.
    pub fn pattern_name() -> impl Strategy<Value = String> {
        "[a-z_][a-z0-9_]*"
    }

    /// Generates pattern definitions.
    ///
    /// # Example
    /// ```rust
    /// use proptest::proptest;
    /// use lingrep_core::testing::strategies::{simple_pattern, pattern_name};
    ///
    /// proptest! {
    ///     fn test_pattern(name in pattern_name(), pattern in simple_pattern()) {
    ///         println!("{} := {}", name, pattern);
    ///     }
    /// }
    /// ```
    pub fn simple_pattern() -> impl Strategy<Value = String> {
        static PATTERNS: &[&str] = &[
            "[upos=NOUN]",
            "[upos=VERB]",
            "[upos=AUX]",
            "[upos=ADJ]",
            "[unidic.pos1=形容詞]",
            "[unidic.pos2=終助詞]",
            "[unidic.cform=終止形]",
            "[lemma=テスト]",
            "[upos=VERB & unidic.cform=終止形]",
            "[upos=NOUN] [upos=AUX]",
            "[upos~/VERB|AUX/]+",
            "[upos=PART]*",
            "[unidic.pos1=形容詞]?",
        ];

        prop_oneof![
            Just(PATTERNS[0].to_string()),
            Just(PATTERNS[1].to_string()),
            Just(PATTERNS[2].to_string()),
            Just(PATTERNS[3].to_string()),
            Just(PATTERNS[4].to_string()),
            Just(PATTERNS[5].to_string()),
            Just(PATTERNS[6].to_string()),
            Just(PATTERNS[7].to_string()),
            Just(PATTERNS[8].to_string()),
            Just(PATTERNS[9].to_string()),
            Just(PATTERNS[10].to_string()),
            Just(PATTERNS[11].to_string()),
            Just(PATTERNS[12].to_string()),
        ]
    }

    /// Strategy for generating token count ranges for benchmarks
    pub fn token_count() -> impl Strategy<Value = usize> {
        prop_oneof![Just(100), Just(500), Just(1000), Just(5000), Just(10000),]
    }

    /// Strategy for generating thread counts for parallel tests
    pub fn thread_count() -> impl Strategy<Value = usize> {
        prop_oneof![Just(1), Just(2), Just(4), Just(8), Just(16),]
    }
}
