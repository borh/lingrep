#![no_main]
use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;
use lingrep_core::fsm::{compile_catalog, CompileLimits, CompiledCatalog, PatternSource};
use lingrep_core::token_match::TokenInfo;

/// Pre-built compiled matchers for fuzzing.
/// Using OnceLock for thread-safe lazy initialization
static PATTERNS: OnceLock<[CompiledCatalog; 3]> = OnceLock::new();

fn get_patterns() -> &'static [CompiledCatalog; 3] {
    PATTERNS.get_or_init(|| {
        let compile = |name, source| {
            compile_catalog(PatternSource::named(name, source), CompileLimits::default())
                .unwrap()
                .matcher
        };
        let fsm_noun = compile("noun.lx", "noun := [unidic.pos1=NOUN].");
        let fsm_verb = compile("verb.lx", "verb := [unidic.pos1=VERB].");
        let fsm_seq = compile("seq.lx", "seq := [unidic.pos1=NOUN] [unidic.pos1=VERB].");

        [fsm_noun, fsm_verb, fsm_seq]
    })
}

/// Generate TokenInfo from fuzz input bytes
/// Handles odd byte lengths by padding or truncating
fn generate_tokens(data: &[u8]) -> Vec<TokenInfo> {
    let mut tokens = Vec::new();

    // Each token needs at least 2 bytes (pos1 length, orth length)
    let chunk_size = 2;
    for chunk in data.chunks(chunk_size) {
        let mut token = TokenInfo::default();

        // Use first byte as pos1 tag (cycle through common tags)
        if let Some(&byte) = chunk.first() {
            let pos_tags = ["NOUN", "VERB", "AUX", "ADJ", "PART"];
            let tag = pos_tags[byte as usize % pos_tags.len()];
            token.set_pos1(tag);
        }

        // Use second byte as orth (generate a string)
        if let Some(&byte) = chunk.get(1) {
            let orth = format!("token_{}", byte);
            token.set_orth(orth);
        }

        tokens.push(token);
    }

    tokens
}

// Fuzzer for the compiled matcher.
// Tests pattern matching with edge case token sequences
fuzz_target!(|data: &[u8]| {
    // Use first byte to select pattern, rest as token data
    if data.is_empty() {
        return;
    }

    let pattern_idx = data[0] % 3; // Select from 3 pre-built patterns
    let fsm = &get_patterns()[pattern_idx as usize];

    // Generate tokens from remaining bytes
    let tokens = generate_tokens(&data[1..]);

    let _ = fsm.find_matches(&tokens);
});
