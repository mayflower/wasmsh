#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // The lexer must never panic on any valid UTF-8 input.
        // Errors (e.g. unterminated quotes) are acceptable return values.
        let _ = wasmsh_lex::tokenize(s);
    }
});
