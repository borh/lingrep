#![no_main]
use libfuzzer_sys::fuzz_target;
use lingrep_core::fsm::{compile_catalog, CompileLimits, PatternSource};

// Fuzzer for pattern parser
// Tests the .lx syntax parser with malformed and edge case inputs
fuzz_target!(|data: &[u8]| {
    // Convert bytes to UTF-8 string, ignore invalid UTF-8
    if let Ok(s) = std::str::from_utf8(data) {
        // We don't care if compilation fails, only that it never crashes.
        let _ = compile_catalog(PatternSource::named("fuzz.lx", s), CompileLimits::default());
    }
});
