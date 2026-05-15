//! Streaming `uniq` stage: suppress adjacent duplicates with `-c`/`-d`/`-u`
//! semantics and field/char-skipping for the compare key.

use std::io::Read;

use crate::{streaming_read_next_line, take_pending_output};

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct StreamingUniqFlags {
    pub(crate) count: bool,
    pub(crate) duplicates_only: bool,
    pub(crate) unique_only: bool,
    pub(crate) ignore_case: bool,
    pub(crate) skip_fields: usize,
    pub(crate) skip_chars: usize,
    pub(crate) compare_chars: Option<usize>,
}

pub(crate) struct UniqStreamReader<R> {
    inner: R,
    flags: StreamingUniqFlags,
    input_pending: Vec<u8>,
    output_pending: Vec<u8>,
    output_offset: usize,
    finished: bool,
    prev: Option<(String, String)>,
    count: usize,
}

fn streaming_uniq_compare_key(line: &str, flags: &StreamingUniqFlags) -> String {
    let mut slice = line;
    for _ in 0..flags.skip_fields {
        slice = slice.trim_start();
        if let Some(pos) = slice.find(char::is_whitespace) {
            slice = &slice[pos..];
        } else {
            slice = "";
            break;
        }
    }
    if flags.skip_chars > 0 {
        let chars: Vec<char> = slice.chars().collect();
        slice = if flags.skip_chars < chars.len() {
            &slice[chars[..flags.skip_chars]
                .iter()
                .map(|ch| ch.len_utf8())
                .sum::<usize>()..]
        } else {
            ""
        };
    }
    let mut key = slice.to_string();
    if let Some(limit) = flags.compare_chars {
        key = key.chars().take(limit).collect();
    }
    if flags.ignore_case {
        key = key.to_lowercase();
    }
    key
}

fn emit_streaming_uniq_line(
    output: &mut Vec<u8>,
    line: &str,
    count: usize,
    flags: &StreamingUniqFlags,
) {
    if flags.duplicates_only && count < 2 {
        return;
    }
    if flags.unique_only && count > 1 {
        return;
    }
    if flags.count {
        output.extend_from_slice(format!("{count:>7} {line}\n").as_bytes());
    } else {
        output.extend_from_slice(line.as_bytes());
        output.push(b'\n');
    }
}

impl<R> UniqStreamReader<R> {
    pub(crate) fn new(inner: R, flags: StreamingUniqFlags) -> Self {
        Self {
            inner,
            flags,
            input_pending: Vec::new(),
            output_pending: Vec::new(),
            output_offset: 0,
            finished: false,
            prev: None,
            count: 0,
        }
    }
}

impl<R: Read> Read for UniqStreamReader<R> {
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
            self.pump_next_uniq_line()?;
        }
    }
}

impl<R: Read> UniqStreamReader<R> {
    fn pump_next_uniq_line(&mut self) -> std::io::Result<()> {
        if let Some((line, _had_newline)) =
            streaming_read_next_line(&mut self.inner, &mut self.input_pending)?
        {
            self.handle_uniq_line(line);
        } else {
            self.flush_uniq_prev();
            self.finished = true;
        }
        Ok(())
    }

    fn handle_uniq_line(&mut self, line: String) {
        let key = streaming_uniq_compare_key(&line, &self.flags);
        if self
            .prev
            .as_ref()
            .is_some_and(|(_, prev_key)| *prev_key == key)
        {
            self.count += 1;
            return;
        }
        self.flush_uniq_prev();
        self.prev = Some((line, key));
        self.count = 1;
    }

    fn flush_uniq_prev(&mut self) {
        if let Some((prev_line, _)) = self.prev.take() {
            emit_streaming_uniq_line(
                &mut self.output_pending,
                &prev_line,
                self.count,
                &self.flags,
            );
        }
    }
}
