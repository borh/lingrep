use crate::fsm::{CompiledCatalog, FsmBuilder, PatternSource};

/// Compile one pattern named `test`.
pub fn compile_test_pattern(pattern: &str) -> CompiledCatalog {
    let mut builder = FsmBuilder::new();

    // `pattern test`, not a bare `test`: `test` is a declaration
    // keyword, so a bare definition cannot be named it. The explicit
    // declaration keeps the pattern's name `test`, which callers assert on.
    let pattern_str = format!("pattern test := {pattern}.");
    builder
        .add_definition(PatternSource::named("test.lx", &pattern_str))
        .expect("Failed to add pattern");
    builder.build().expect("Failed to build matcher").matcher
}

/// Tokenize text with the shared test tokenizer.
pub fn tokenize_japanese(text: &str) -> Vec<crate::token_match::TokenInfo> {
    crate::testing::tokenize(text, crate::testing::test_tokenizer())
}

/// One token whose only attribute is its surface, so a fixture reads as a
/// string.
///
/// The same four lines were written in seven test files across two crates
/// before this existed; `TokenInfo::default()` plus one `set` is not a thing to
/// restate per file, and a fixture builder that silently gained a second
/// attribute would change what those tests match.
pub fn token(orth: &str) -> crate::token_match::TokenInfo {
    let mut token = crate::token_match::TokenInfo::default();
    token.set(crate::token_match::TokenAttributeIndex::Orth, orth.into());
    token
}

/// A sentence whose tokens are `words`, each found in `source` after the one
/// before it, carrying the source spans an analyzer would report.
///
/// # Panics
///
/// When a word does not occur in `source` after the previous word.
pub fn placed_tokens(source: &str, words: &[&str]) -> crate::token_match::TokenInfoSentence {
    let mut at = 0;
    let ranges = words
        .iter()
        .enumerate()
        .map(|(index, word)| {
            let start = at
                + source
                    .get(at..)
                    .and_then(|rest| rest.find(word))
                    .expect("word occurs in order");
            at = start + word.len();
            (start..at, index..index + 1)
        })
        .collect::<Vec<_>>();
    let spans = crate::token_match::CharSpan::from_surface_ranges(source, words.len(), ranges)
        .expect("words cover the source in order");
    crate::token_match::TokenInfoSentence::new(
        words.iter().map(|word| token(word)).collect(),
        Some(std::sync::Arc::new(spans)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_tokenizer;
    use crate::token_match::{TokenInfo, tokenize};

    #[test]
    fn compiles_one_test_pattern() {
        let matcher = compile_test_pattern("[upos=NOUN]");
        let tokenizer = test_tokenizer();
        let tokens: Vec<TokenInfo> = tokenize("名詞です。", tokenizer);
        let matches = matcher.find_matches(&tokens);

        assert!(!matches.is_empty());
    }
}
