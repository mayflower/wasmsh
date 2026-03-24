#![no_main]
use libfuzzer_sys::fuzz_target;

use wasmsh_ast::{Command, CompleteCommand, Word};
use wasmsh_expand::expand_word;
use wasmsh_state::ShellState;

/// Walk every `Word` reachable from a `CompleteCommand` slice and expand it.
fn expand_all_words(cmds: &[CompleteCommand], state: &mut ShellState) {
    for cc in cmds {
        for aol in &cc.list {
            expand_pipeline_words(&aol.first.commands, state);
            for (_, pl) in &aol.rest {
                expand_pipeline_words(&pl.commands, state);
            }
        }
    }
}

fn expand_pipeline_words(commands: &[Command], state: &mut ShellState) {
    for cmd in commands {
        expand_command_words(cmd, state);
    }
}

fn expand_word_list(words: &[Word], state: &mut ShellState) {
    for w in words {
        let _ = expand_word(w, state);
    }
}

fn expand_command_words(cmd: &Command, state: &mut ShellState) {
    match cmd {
        Command::Simple(sc) => {
            expand_word_list(&sc.words, state);
        }
        Command::Subshell(s) => expand_all_words(&s.body, state),
        Command::Group(g) => expand_all_words(&g.body, state),
        Command::If(ic) => {
            expand_all_words(&ic.condition, state);
            expand_all_words(&ic.then_body, state);
            for elif in &ic.elifs {
                expand_all_words(&elif.condition, state);
                expand_all_words(&elif.then_body, state);
            }
            if let Some(eb) = &ic.else_body {
                expand_all_words(eb, state);
            }
        }
        Command::While(w) => {
            expand_all_words(&w.condition, state);
            expand_all_words(&w.body, state);
        }
        Command::Until(u) => {
            expand_all_words(&u.condition, state);
            expand_all_words(&u.body, state);
        }
        Command::For(f) => {
            if let Some(ws) = &f.words {
                expand_word_list(ws, state);
            }
            expand_all_words(&f.body, state);
        }
        Command::ArithFor(af) => {
            expand_all_words(&af.body, state);
        }
        Command::FunctionDef(fd) => {
            expand_command_words(&fd.body, state);
        }
        Command::Case(c) => {
            let _ = expand_word(&c.word, state);
            for item in &c.items {
                expand_word_list(&item.patterns, state);
                expand_all_words(&item.body, state);
            }
        }
        Command::DoubleBracket(db) => {
            expand_word_list(&db.words, state);
        }
        Command::ArithCommand(_) => {}
        Command::Select(s) => {
            if let Some(ws) = &s.words {
                expand_word_list(ws, state);
            }
            expand_all_words(&s.body, state);
        }
    }
}

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Parse may fail; that's fine. Only successful parses are expanded.
        if let Ok(program) = wasmsh_parse::parse(s) {
            let mut state = ShellState::new();
            // The expansion engine must never panic on any parsed AST.
            expand_all_words(&program.commands, &mut state);
        }
    }
});
