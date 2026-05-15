//! Streaming `cut` stage: slice each line by fields, characters, or bytes with
//! optional complement / only-delimited / custom output-delimiter.

use std::io::Read;

use crate::{streaming_read_next_line, take_pending_output};

pub(crate) struct StreamingCutParseState {
    pub(crate) delim: char,
    pub(crate) mode: Option<StreamingCutMode>,
    pub(crate) complement: bool,
    pub(crate) only_delimited: bool,
    pub(crate) output_delim: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) enum StreamingCutMode {
    Fields(Vec<StreamingCutRange>),
    Chars(Vec<StreamingCutRange>),
    Bytes(Vec<StreamingCutRange>),
}

#[derive(Clone, Debug)]
pub(crate) struct StreamingCutStage {
    pub(crate) mode: StreamingCutMode,
    pub(crate) delim: char,
    pub(crate) complement: bool,
    pub(crate) only_delimited: bool,
    pub(crate) output_delim: String,
}

#[derive(Clone, Debug)]
pub(crate) struct StreamingCutRange {
    pub(crate) start: Option<usize>,
    pub(crate) end: Option<usize>,
}

pub(crate) struct CutStreamReader<R> {
    inner: R,
    stage: StreamingCutStage,
    input_pending: Vec<u8>,
    output_pending: Vec<u8>,
    output_offset: usize,
    finished: bool,
}

fn streaming_cut_range_includes(ranges: &[StreamingCutRange], idx: usize) -> bool {
    ranges.iter().any(|range| {
        let start = range.start.unwrap_or(1);
        let end = range.end.unwrap_or(usize::MAX);
        idx >= start && idx <= end
    })
}

fn apply_streaming_cut(line: &str, stage: &StreamingCutStage) -> Option<Vec<u8>> {
    match &stage.mode {
        StreamingCutMode::Fields(ranges) => {
            if stage.only_delimited && !line.contains(stage.delim) {
                return None;
            }
            let parts: Vec<&str> = line.split(stage.delim).collect();
            let selected: Vec<&str> = parts
                .iter()
                .enumerate()
                .filter(|(idx, _)| {
                    let included = streaming_cut_range_includes(ranges, idx + 1);
                    if stage.complement {
                        !included
                    } else {
                        included
                    }
                })
                .map(|(_, part)| *part)
                .collect();
            Some(selected.join(&stage.output_delim).into_bytes())
        }
        StreamingCutMode::Chars(ranges) | StreamingCutMode::Bytes(ranges) => {
            let chars: Vec<char> = line.chars().collect();
            let selected: String = chars
                .iter()
                .enumerate()
                .filter(|(idx, _)| {
                    let included = streaming_cut_range_includes(ranges, idx + 1);
                    if stage.complement {
                        !included
                    } else {
                        included
                    }
                })
                .map(|(_, ch)| *ch)
                .collect();
            Some(selected.into_bytes())
        }
    }
}

impl<R> CutStreamReader<R> {
    pub(crate) fn new(inner: R, stage: StreamingCutStage) -> Self {
        Self {
            inner,
            stage,
            input_pending: Vec::new(),
            output_pending: Vec::new(),
            output_offset: 0,
            finished: false,
        }
    }
}

impl<R: Read> Read for CutStreamReader<R> {
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

            match streaming_read_next_line(&mut self.inner, &mut self.input_pending)? {
                Some((line, _had_newline)) => {
                    if let Some(mut out) = apply_streaming_cut(&line, &self.stage) {
                        out.push(b'\n');
                        self.output_pending.extend_from_slice(&out);
                    }
                }
                None => {
                    self.finished = true;
                }
            }
        }
    }
}
