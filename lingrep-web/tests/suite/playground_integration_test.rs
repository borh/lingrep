//! Integration tests for playground pattern matching
//!
//! Tests the full pipeline: pattern parsing → tokenization → matching

use std::path::PathBuf;

fn get_dict_path() -> PathBuf {
    PathBuf::from(lingrep_core::testing::dictionary_path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lingrep_core::fsm::{
        CompileError, CompileLimits, CompiledCatalog, DiagnosticCode, PatternSource,
        compile_catalog,
    };

    fn compile(source: &str) -> Result<CompiledCatalog, CompileError> {
        compile_catalog(
            PatternSource::named("playground-test.lx", source),
            CompileLimits::default(),
        )
        .map(|output| output.matcher)
    }

    #[test]
    fn test_pattern_compilation() {
        // Test single token pattern
        let pattern = "pattern test := [upos=VERB].";
        let result = compile(pattern);
        assert!(result.is_ok(), "Should compile single token pattern");

        // Test multi-token pattern
        let pattern = "pattern test := [upos=VERB] [upos=AUX]+.";
        let result = compile(pattern);
        assert!(result.is_ok(), "Should compile multi-token pattern");

        // Test lemma pattern
        let pattern = "pattern test := [lemma=走る].";
        let result = compile(pattern);
        assert!(result.is_ok(), "Should compile lemma pattern");

        // Test orth pattern
        let pattern = "pattern test := [text=来た].";
        let result = compile(pattern);
        assert!(result.is_ok(), "Should compile orth pattern");
    }

    #[test]
    fn test_tokenization_and_matching() {
        use lingrep_core::dictionary::create_tokenizer_from_path;
        use lingrep_core::token_match::tokenize;

        let tokenizer = create_tokenizer_from_path(&get_dict_path().to_string_lossy())
            .expect("test dictionary must load");
        let text = "学校に行った。"; // "went to school" - should have VERB + AUX

        let tokens = tokenize(text, &tokenizer);

        // This UniDic analysis emits 行っ and た as AUX tokens.
        let pattern = "auxiliary_sequence := [upos=AUX]+.";
        let fsm = compile(pattern).expect("Should compile pattern");
        let matches = fsm.find_matches(&tokens);

        assert!(!matches.is_empty(), "Should find auxiliary matches");
    }

    #[test]
    fn test_error_handling() {
        // Invalid pattern syntax
        let pattern = "[[[invalid";
        let Err(error) = compile(pattern) else {
            panic!("malformed lingrep patterns must fail");
        };
        assert_eq!(error.diagnostics[0].code(), DiagnosticCode::Syntax);
    }

    #[test]
    fn test_edge_case_empty_text() {
        use lingrep_core::dictionary::create_tokenizer_from_path;
        use lingrep_core::token_match::tokenize;

        let tokenizer = create_tokenizer_from_path(&get_dict_path().to_string_lossy())
            .expect("test dictionary must load");
        let text = "";

        let tokens = tokenize(text, &tokenizer);

        let pattern = "pattern test := [upos=VERB].";
        let fsm = compile(pattern).expect("Should compile pattern");
        let matches = fsm.find_matches(&tokens);

        assert!(matches.is_empty(), "Should not match in empty text");
    }
}
