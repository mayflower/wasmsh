#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // The parser must never panic on any valid UTF-8 input.
        // Parse errors are acceptable; panics are not.
        let _ = wasmsh_parse::parse(s);
    }
});
