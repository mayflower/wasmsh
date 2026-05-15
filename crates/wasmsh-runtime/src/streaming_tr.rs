//! Streaming `tr` stage: character-set translation / deletion / squeezing
//! over a UTF-8 byte stream.
//!
//! Pipeline glue (argument parsing into a `StreamingTrStage`) lives in the
//! parent module; this module owns the stage type, reader, and set-expansion
//! helpers.

use std::io::Read;

use crate::take_pending_output;

#[derive(Clone, Debug)]
pub(crate) struct StreamingTrStage {
    pub(crate) delete: bool,
    pub(crate) squeeze: bool,
    pub(crate) complement: bool,
    pub(crate) from_chars: Vec<char>,
    pub(crate) to_chars: Vec<char>,
}

pub(crate) struct TrStreamReader<R> {
    inner: R,
    stage: StreamingTrStage,
    input_pending: Vec<u8>,
    output_pending: Vec<u8>,
    output_offset: usize,
    finished: bool,
    prev: Option<char>,
}

fn streaming_tr_expand_posix_class(class_name: &str, chars: &mut Vec<char>) {
    match class_name {
        "upper" => chars.extend('A'..='Z'),
        "lower" => chars.extend('a'..='z'),
        "digit" => chars.extend('0'..='9'),
        "alpha" => {
            chars.extend('A'..='Z');
            chars.extend('a'..='z');
        }
        "alnum" => {
            chars.extend('0'..='9');
            chars.extend('A'..='Z');
            chars.extend('a'..='z');
        }
        "space" => chars.extend([' ', '\t', '\n', '\r', '\x0b', '\x0c']),
        "blank" => chars.extend([' ', '\t']),
        "punct" => chars.extend("!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~".chars()),
        _ => {}
    }
}

pub(crate) fn streaming_tr_expand_set(s: &str) -> Vec<char> {
    let mut chars = Vec::new();
    let mut iter = s.chars().peekable();
    while let Some(ch) = iter.next() {
        if ch == '[' && iter.peek() == Some(&':') {
            iter.next();
            let class_name: String = iter.by_ref().take_while(|&c| c != ':').collect();
            let _ = iter.next();
            streaming_tr_expand_posix_class(&class_name, &mut chars);
        } else if iter.peek() == Some(&'-') {
            streaming_tr_expand_range(ch, &mut iter, &mut chars);
        } else if ch == '\\' {
            chars.push(streaming_tr_unescape(&mut iter));
        } else {
            chars.push(ch);
        }
    }
    chars
}

fn streaming_tr_expand_range(
    ch: char,
    iter: &mut std::iter::Peekable<std::str::Chars<'_>>,
    chars: &mut Vec<char>,
) {
    let saved = iter.clone();
    iter.next(); // consume '-'
    if let Some(&end_ch) = iter.peek() {
        if end_ch > ch {
            chars.extend(ch..=end_ch);
            iter.next();
        } else {
            chars.push(ch);
            *iter = saved;
            iter.next();
            chars.push('-');
        }
    } else {
        chars.push(ch);
        chars.push('-');
    }
}

fn streaming_tr_unescape(iter: &mut std::iter::Peekable<std::str::Chars<'_>>) -> char {
    match iter.next() {
        Some('n') => '\n',
        Some('t') => '\t',
        Some('r') => '\r',
        Some('\\') | None => '\\',
        Some(other) => other,
    }
}

fn streaming_tr_process_utf8_chunk(pending: &mut Vec<u8>, chunk: &[u8], mut f: impl FnMut(char)) {
    pending.extend_from_slice(chunk);
    while streaming_tr_drain_once(pending, &mut f) {}
}

/// Consumes one contiguous UTF-8 slice (or one invalid byte) from `pending`.
/// Returns true if more work may remain, false if `pending` is either empty,
/// fully consumed, or ends in an incomplete UTF-8 sequence that must wait.
fn streaming_tr_drain_once(pending: &mut Vec<u8>, f: &mut impl FnMut(char)) -> bool {
    match std::str::from_utf8(pending) {
        Ok(text) => {
            for ch in text.chars() {
                f(ch);
            }
            pending.clear();
            false
        }
        Err(err) => streaming_tr_consume_invalid_prefix(pending, &err, f),
    }
}

fn streaming_tr_consume_invalid_prefix(
    pending: &mut Vec<u8>,
    err: &std::str::Utf8Error,
    f: &mut impl FnMut(char),
) -> bool {
    let valid = err.valid_up_to();
    if valid > 0 {
        let text = String::from_utf8_lossy(&pending[..valid]).to_string();
        for ch in text.chars() {
            f(ch);
        }
        pending.drain(..valid);
        return true;
    }
    if err.error_len().is_some() {
        let text = String::from_utf8_lossy(&pending[..1]).to_string();
        for ch in text.chars() {
            f(ch);
        }
        pending.drain(..1);
        return true;
    }
    false
}

fn streaming_tr_flush_pending_lossy(pending: &mut Vec<u8>, mut f: impl FnMut(char)) {
    if pending.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(pending).to_string();
    pending.clear();
    for ch in text.chars() {
        f(ch);
    }
}

impl<R> TrStreamReader<R> {
    pub(crate) fn new(inner: R, stage: StreamingTrStage) -> Self {
        Self {
            inner,
            stage,
            input_pending: Vec::new(),
            output_pending: Vec::new(),
            output_offset: 0,
            finished: false,
            prev: None,
        }
    }

    fn emit_char(&mut self, ch: char) {
        let mut buffer = [0u8; 4];
        self.output_pending
            .extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
        self.prev = Some(ch);
    }

    fn is_in_from_set(&self, ch: char) -> bool {
        let in_set = self.stage.from_chars.contains(&ch);
        if self.stage.complement {
            !in_set
        } else {
            in_set
        }
    }

    fn process_char(&mut self, ch: char) {
        if self.stage.delete {
            self.process_char_delete(ch);
            return;
        }
        if self.stage.squeeze && self.stage.to_chars.is_empty() {
            if self.stage.from_chars.contains(&ch) && self.prev == Some(ch) {
                return;
            }
            self.emit_char(ch);
            return;
        }
        self.process_char_translate(ch);
    }

    fn process_char_delete(&mut self, ch: char) {
        if self.is_in_from_set(ch) {
            return;
        }
        if self.stage.squeeze && self.stage.to_chars.contains(&ch) && self.prev == Some(ch) {
            return;
        }
        self.emit_char(ch);
    }

    fn process_char_translate(&mut self, ch: char) {
        let from_set = if self.stage.complement {
            (0u8..=127)
                .map(|b| b as char)
                .filter(|candidate| !self.stage.from_chars.contains(candidate))
                .collect::<Vec<_>>()
        } else {
            self.stage.from_chars.clone()
        };
        let translated = from_set
            .iter()
            .position(|&source| source == ch)
            .and_then(|pos| {
                self.stage
                    .to_chars
                    .get(pos)
                    .or(self.stage.to_chars.last())
                    .copied()
            })
            .unwrap_or(ch);
        if self.stage.squeeze
            && self.stage.to_chars.contains(&translated)
            && self.prev == Some(translated)
        {
            return;
        }
        self.emit_char(translated);
    }
}

impl<R: Read> Read for TrStreamReader<R> {
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

            let mut scratch = [0u8; 4096];
            let read = self.inner.read(&mut scratch)?;
            if read == 0 {
                let mut pending = std::mem::take(&mut self.input_pending);
                let mut chars = Vec::new();
                streaming_tr_flush_pending_lossy(&mut pending, |ch| chars.push(ch));
                self.input_pending = pending;
                for ch in chars {
                    self.process_char(ch);
                }
                self.finished = true;
                continue;
            }
            let mut pending = std::mem::take(&mut self.input_pending);
            let mut chars = Vec::new();
            streaming_tr_process_utf8_chunk(&mut pending, &scratch[..read], |ch| chars.push(ch));
            self.input_pending = pending;
            for ch in chars {
                self.process_char(ch);
            }
        }
    }
}
