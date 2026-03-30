#![no_main]
use libfuzzer_sys::fuzz_target;

use wasmsh_browser::WorkerRuntime;
use wasmsh_protocol::HostCommand;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Cap input size to keep individual runs bounded.
        if s.len() > 4096 {
            return;
        }
        // Full pipeline: Init then Run. The runtime must never panic.
        let mut rt = WorkerRuntime::new();
        let _ = rt.handle_command(HostCommand::Init { step_budget: 10_000, allowed_hosts: vec![] });
        let _ = rt.handle_command(HostCommand::Run { input: s.to_string() });
    }
});
