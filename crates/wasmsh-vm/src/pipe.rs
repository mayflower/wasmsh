//! Pipe buffer for connecting pipeline stages.
//!
//! A bounded, in-memory byte buffer with producer/consumer semantics.
//! The producer writes data and eventually closes the write end.
//! The consumer reads data until the buffer is empty and closed.

use std::collections::VecDeque;

/// A bounded pipe buffer connecting two pipeline stages.
#[derive(Debug)]
pub struct PipeBuffer {
    data: VecDeque<u8>,
    capacity: usize,
    write_closed: bool,
    read_closed: bool,
}

/// Result of a write attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteResult {
    /// All bytes were written.
    Written(usize),
    /// Buffer is full — only `n` bytes were written. Caller should yield.
    WouldBlock(usize),
    /// The read end was closed (broken pipe).
    BrokenPipe,
}

/// Result of a read attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadResult {
    /// Read `n` bytes into the output buffer.
    Read(usize),
    /// No data available and writer hasn't closed yet. Caller should yield.
    WouldBlock,
    /// No data available and writer has closed. End of stream.
    Eof,
}

impl PipeBuffer {
    /// Create a new pipe buffer with the given capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            data: VecDeque::with_capacity(capacity),
            capacity,
            write_closed: false,
            read_closed: false,
        }
    }

    /// Default pipe buffer (64 KiB).
    #[must_use]
    pub fn default_size() -> Self {
        Self::new(64 * 1024)
    }

    /// Write data into the pipe. Returns how many bytes were accepted.
    pub fn write(&mut self, buf: &[u8]) -> WriteResult {
        if self.read_closed {
            return WriteResult::BrokenPipe;
        }
        if buf.is_empty() {
            return WriteResult::Written(0);
        }

        let available = self.capacity.saturating_sub(self.data.len());
        if available == 0 {
            return WriteResult::WouldBlock(0);
        }

        let to_write = buf.len().min(available);
        self.data.extend(&buf[..to_write]);

        if to_write < buf.len() {
            WriteResult::WouldBlock(to_write)
        } else {
            WriteResult::Written(to_write)
        }
    }

    /// Maximum total data that `write_all` will accept (64 MiB).
    const MAX_PIPE_TOTAL: usize = 64 * 1024 * 1024;

    /// Write all data into the pipe, ignoring per-write capacity limits
    /// but enforcing a total size cap.
    /// Used for the "run to completion then feed" model.
    pub fn write_all(&mut self, buf: &[u8]) {
        let remaining = Self::MAX_PIPE_TOTAL.saturating_sub(self.data.len());
        let to_write = buf.len().min(remaining);
        self.data.extend(&buf[..to_write]);
    }

    /// Read available data from the pipe into a Vec.
    pub fn read_all(&mut self) -> ReadResult {
        if self.data.is_empty() {
            if self.write_closed {
                return ReadResult::Eof;
            }
            return ReadResult::WouldBlock;
        }
        ReadResult::Read(self.data.len())
    }

    /// Read up to `buf.len()` bytes from the pipe into `buf`.
    pub fn read(&mut self, buf: &mut [u8]) -> ReadResult {
        if buf.is_empty() {
            return ReadResult::Read(0);
        }
        if self.data.is_empty() {
            if self.write_closed {
                return ReadResult::Eof;
            }
            return ReadResult::WouldBlock;
        }
        let to_read = buf.len().min(self.data.len());
        for slot in &mut buf[..to_read] {
            *slot = self
                .data
                .pop_front()
                .expect("pipe read length exceeded available data");
        }
        ReadResult::Read(to_read)
    }

    /// Drain all available data from the pipe.
    pub fn drain(&mut self) -> Vec<u8> {
        self.data.drain(..).collect()
    }

    /// Close the write end of the pipe.
    pub fn close_write(&mut self) {
        self.write_closed = true;
    }

    /// Close the read end of the pipe so future writes observe broken pipe.
    pub fn close_read(&mut self) {
        self.read_closed = true;
    }

    /// Check if the write end is closed.
    #[must_use]
    pub fn is_write_closed(&self) -> bool {
        self.write_closed
    }

    /// Check if the read end is closed.
    #[must_use]
    pub fn is_read_closed(&self) -> bool {
        self.read_closed
    }

    /// Check if there's data available to read.
    #[must_use]
    pub fn has_data(&self) -> bool {
        !self.data.is_empty()
    }

    /// Check if there's space available to write.
    #[must_use]
    pub fn has_space(&self) -> bool {
        self.data.len() < self.capacity
    }

    /// Number of bytes currently buffered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Check if the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_drain() {
        let mut pipe = PipeBuffer::new(1024);
        assert!(matches!(pipe.write(b"hello"), WriteResult::Written(5)));
        assert_eq!(pipe.len(), 5);
        let data = pipe.drain();
        assert_eq!(data, b"hello");
        assert!(pipe.is_empty());
    }

    #[test]
    fn write_would_block_at_capacity() {
        let mut pipe = PipeBuffer::new(4);
        assert!(matches!(pipe.write(b"abcd"), WriteResult::Written(4)));
        assert!(matches!(pipe.write(b"x"), WriteResult::WouldBlock(0)));
    }

    #[test]
    fn partial_write() {
        let mut pipe = PipeBuffer::new(4);
        assert!(matches!(pipe.write(b"abcdef"), WriteResult::WouldBlock(4)));
        assert_eq!(pipe.len(), 4);
    }

    #[test]
    fn read_eof_after_close() {
        let mut pipe = PipeBuffer::new(1024);
        pipe.write_all(b"data");
        pipe.close_write();
        assert!(matches!(pipe.read_all(), ReadResult::Read(4)));
        pipe.drain();
        assert!(matches!(pipe.read_all(), ReadResult::Eof));
    }

    #[test]
    fn read_would_block_when_empty_and_open() {
        let mut pipe = PipeBuffer::new(1024);
        assert!(matches!(pipe.read_all(), ReadResult::WouldBlock));
    }

    #[test]
    fn write_all_ignores_capacity() {
        let mut pipe = PipeBuffer::new(4);
        pipe.write_all(b"hello world"); // exceeds capacity
        assert_eq!(pipe.len(), 11);
    }

    #[test]
    fn incremental_read_drains_prefix_only() {
        let mut pipe = PipeBuffer::new(1024);
        pipe.write_all(b"hello");
        let mut buf = [0u8; 2];
        assert!(matches!(pipe.read(&mut buf), ReadResult::Read(2)));
        assert_eq!(&buf, b"he");
        assert_eq!(pipe.drain(), b"llo");
    }

    #[test]
    fn close_read_makes_future_writes_broken_pipe() {
        let mut pipe = PipeBuffer::new(1024);
        pipe.close_read();
        assert!(matches!(pipe.write(b"hello"), WriteResult::BrokenPipe));
    }
}
