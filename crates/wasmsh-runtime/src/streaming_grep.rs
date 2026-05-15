//! Streaming `grep` stage: POSIX BRE/ERE matching, context windows, counts.
//!
//! Argument parsing (`parse_streaming_grep_stage`) lives in the parent module;
//! this file owns the stage types, the line-matching primitives, and the
//! `GrepStreamReader` that drives match output with `-A`/`-B`/`-c`/`-q`
//! semantics.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io::Read;
use std::rc::Rc;

use crate::{streaming_read_next_line, take_pending_output};

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct StreamingGrepFlags {
    pub(crate) ignore_case: bool,
    pub(crate) invert: bool,
    pub(crate) count_only: bool,
    pub(crate) show_line_numbers: bool,
    pub(crate) files_only: bool,
    pub(crate) word_match: bool,
    pub(crate) only_matching: bool,
    pub(crate) quiet: bool,
    pub(crate) extended: bool,
    pub(crate) fixed: bool,
    pub(crate) after_context: usize,
    pub(crate) before_context: usize,
    pub(crate) max_count: Option<usize>,
    pub(crate) show_filename: Option<bool>,
}

#[derive(Clone, Debug)]
pub(crate) struct StreamingGrepStage {
    pub(crate) flags: StreamingGrepFlags,
    pub(crate) patterns: Vec<String>,
}

#[derive(Copy, Clone, Debug)]
pub(crate) enum StreamingGrepStep {
    Advance(usize),
    NotMatched,
}

fn streaming_grep_match_single(line: &str, pattern: &str, flags: &StreamingGrepFlags) -> bool {
    use posix_regex::compile::PosixRegexBuilder;

    if flags.word_match {
        return line
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|word| word == pattern);
    }
    if flags.fixed {
        return line.contains(pattern);
    }
    // Try POSIX regex first; fall back to literal substring on compile error.
    match PosixRegexBuilder::new(pattern.as_bytes())
        .with_default_classes()
        .compile()
    {
        Ok(re) => !re.matches(line.as_bytes(), Some(1)).is_empty(),
        Err(_) => line.contains(pattern),
    }
}

fn streaming_grep_match_pattern(line: &str, pattern: &str, flags: &StreamingGrepFlags) -> bool {
    let (line_cmp, pattern_cmp) = if flags.ignore_case {
        (line.to_lowercase(), pattern.to_lowercase())
    } else {
        (line.to_string(), pattern.to_string())
    };
    if flags.extended && pattern_cmp.contains('|') {
        return pattern_cmp
            .split('|')
            .any(|alt| streaming_grep_match_single(&line_cmp, alt.trim(), flags));
    }
    streaming_grep_match_single(&line_cmp, &pattern_cmp, flags)
}

fn streaming_grep_find_match<'a>(
    line: &'a str,
    pattern: &str,
    flags: &StreamingGrepFlags,
) -> Option<&'a str> {
    let (line_cmp, pattern_cmp) = if flags.ignore_case {
        (line.to_lowercase(), pattern.to_lowercase())
    } else {
        (line.to_string(), pattern.to_string())
    };
    if flags.word_match {
        let start = line_cmp.find(&pattern_cmp)?;
        if start > 0 && line_cmp.as_bytes()[start - 1].is_ascii_alphanumeric() {
            return None;
        }
        let end = start + pattern_cmp.len();
        if end < line_cmp.len() && line_cmp.as_bytes()[end].is_ascii_alphanumeric() {
            return None;
        }
        Some(&line[start..start + pattern_cmp.len()])
    } else {
        let idx = line_cmp.find(&pattern_cmp)?;
        Some(&line[idx..idx + pattern_cmp.len()])
    }
}

fn streaming_grep_line_matches(
    line: &str,
    flags: &StreamingGrepFlags,
    patterns: &[String],
) -> bool {
    let matched = patterns
        .iter()
        .any(|pattern| streaming_grep_match_pattern(line, pattern, flags));
    matched != flags.invert
}

fn emit_streaming_grep_one(
    output: &mut Vec<u8>,
    line: &str,
    line_num: usize,
    flags: &StreamingGrepFlags,
    patterns: &[String],
) {
    let mut prefix = String::new();
    if flags.show_filename == Some(true) {
        prefix.push_str("(standard input):");
    }
    if flags.show_line_numbers {
        use std::fmt::Write;
        let _ = write!(prefix, "{line_num}:");
    }
    if flags.only_matching {
        for pattern in patterns {
            if let Some(matched) = streaming_grep_find_match(line, pattern, flags) {
                output.extend_from_slice(prefix.as_bytes());
                output.extend_from_slice(matched.as_bytes());
                output.push(b'\n');
            }
        }
    } else {
        output.extend_from_slice(prefix.as_bytes());
        output.extend_from_slice(line.as_bytes());
        output.push(b'\n');
    }
}

#[allow(clippy::struct_excessive_bools)]
pub(crate) struct GrepStreamReader<R> {
    inner: R,
    stage: StreamingGrepStage,
    status: Rc<RefCell<i32>>,
    input_pending: Vec<u8>,
    output_pending: Vec<u8>,
    output_offset: usize,
    finished: bool,
    match_count: u64,
    found: bool,
    remaining_after: usize,
    printed_separator: bool,
    before_buf: VecDeque<(usize, String)>,
    line_num: usize,
    emitted_count_summary: bool,
}

impl<R> GrepStreamReader<R> {
    pub(crate) fn new(inner: R, stage: StreamingGrepStage, status: Rc<RefCell<i32>>) -> Self {
        Self {
            inner,
            stage,
            status,
            input_pending: Vec::new(),
            output_pending: Vec::new(),
            output_offset: 0,
            finished: false,
            match_count: 0,
            found: false,
            remaining_after: 0,
            printed_separator: false,
            before_buf: VecDeque::new(),
            line_num: 0,
            emitted_count_summary: false,
        }
    }

    fn emit_count_summary(&mut self) {
        if self.stage.flags.count_only && !self.stage.flags.quiet && !self.emitted_count_summary {
            if self.stage.flags.show_filename == Some(true) {
                self.output_pending.extend_from_slice(
                    format!("(standard input):{}\n", self.match_count).as_bytes(),
                );
            } else {
                self.output_pending
                    .extend_from_slice(format!("{}\n", self.match_count).as_bytes());
            }
            self.emitted_count_summary = true;
        }
    }
}

impl<R: Read> Read for GrepStreamReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            let copied =
                take_pending_output(&mut self.output_pending, &mut self.output_offset, buf);
            if copied > 0 {
                return Ok(copied);
            }
            if self.finished {
                return Ok(0);
            }
            self.pump_one_line()?;
        }
    }
}

impl<R: Read> GrepStreamReader<R> {
    fn pump_one_line(&mut self) -> std::io::Result<()> {
        let Some((line, _had_newline)) =
            streaming_read_next_line(&mut self.inner, &mut self.input_pending)?
        else {
            self.emit_count_summary();
            if !self.found {
                *self.status.borrow_mut() = 1;
            }
            self.finished = true;
            return Ok(());
        };
        self.line_num += 1;
        if streaming_grep_line_matches(&line, &self.stage.flags, &self.stage.patterns) {
            self.on_match(&line);
        } else {
            self.on_nonmatch(line);
        }
        Ok(())
    }

    fn on_match(&mut self, line: &str) {
        self.found = true;
        *self.status.borrow_mut() = 0;
        self.match_count += 1;

        if self.stage.flags.quiet || self.stage.flags.files_only {
            self.check_max_count();
            return;
        }

        if !self.stage.flags.count_only {
            self.flush_before_context();
            emit_streaming_grep_one(
                &mut self.output_pending,
                line,
                self.line_num,
                &self.stage.flags,
                &self.stage.patterns,
            );
            self.remaining_after = self.stage.flags.after_context;
            self.printed_separator = true;
        }

        if self.should_stop_for_max_count() {
            self.emit_count_summary();
            self.finished = true;
        }
    }

    fn on_nonmatch(&mut self, line: String) {
        if self.remaining_after > 0 && !self.stage.flags.count_only {
            emit_streaming_grep_one(
                &mut self.output_pending,
                &line,
                self.line_num,
                &self.stage.flags,
                &self.stage.patterns,
            );
            self.remaining_after -= 1;
            return;
        }
        if self.stage.flags.before_context > 0 {
            self.before_buf.push_back((self.line_num, line));
            if self.before_buf.len() > self.stage.flags.before_context {
                self.before_buf.pop_front();
            }
        }
    }

    fn flush_before_context(&mut self) {
        if self.stage.flags.before_context == 0 || self.before_buf.is_empty() {
            self.before_buf.clear();
            return;
        }
        if self.printed_separator {
            self.output_pending.extend_from_slice(b"--\n");
        }
        for (before_line_num, before_line) in &self.before_buf {
            emit_streaming_grep_one(
                &mut self.output_pending,
                before_line,
                *before_line_num,
                &self.stage.flags,
                &self.stage.patterns,
            );
        }
        self.before_buf.clear();
    }

    fn should_stop_for_max_count(&self) -> bool {
        self.stage
            .flags
            .max_count
            .is_some_and(|m| self.match_count >= m as u64)
    }

    fn check_max_count(&mut self) {
        if self.should_stop_for_max_count() {
            self.finished = true;
        }
    }
}
