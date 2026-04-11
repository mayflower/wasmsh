//! Virtual filesystem abstraction for wasmsh.
//!
//! Provides the `Vfs` trait and a `MemoryFs` implementation for
//! ephemeral in-memory filesystems. No `std::fs` is used — this
//! is safe for the browser target.

#![warn(missing_docs)]

use std::io::Read;

#[cfg(feature = "emscripten")]
#[allow(unsafe_code, clippy::borrow_as_ptr)]
mod emscripten_fs;
mod memfs;
#[cfg(feature = "opfs")]
mod opfs;

#[cfg(feature = "emscripten")]
pub use emscripten_fs::EmscriptenFs;
pub use memfs::MemoryFs;
#[cfg(feature = "opfs")]
pub use opfs::OpfsFs;

/// Platform filesystem backend.
///
/// Resolves to the libc-backed [`EmscriptenFs`] when the `emscripten` feature
/// is enabled and the target is either `wasm32-unknown-emscripten` or
/// `wasm32-wasip2` (`target_os = "wasi", target_env = "p2"`). Otherwise
/// [`MemoryFs`]. The feature gate keeps native `--all-features` builds working
/// while letting both embedded WASM targets share the same libc/POSIX backend.
#[cfg(all(
    feature = "emscripten",
    target_arch = "wasm32",
    any(target_os = "emscripten", all(target_os = "wasi", target_env = "p2"))
))]
pub type BackendFs = EmscriptenFs;

/// Platform filesystem backend (default: in-memory).
#[cfg(not(all(
    feature = "emscripten",
    target_arch = "wasm32",
    any(target_os = "emscripten", all(target_os = "wasi", target_env = "p2"))
)))]
pub type BackendFs = MemoryFs;

use thiserror::Error;

/// Errors from VFS operations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FsError {
    /// The path does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// The path already exists.
    #[error("already exists: {0}")]
    AlreadyExists(String),
    /// The path exists but is not a directory.
    #[error("not a directory: {0}")]
    NotADirectory(String),
    /// The path is a directory where a file was expected.
    #[error("is a directory: {0}")]
    IsADirectory(String),
    /// The operation is not permitted.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// A low-level I/O error.
    #[error("io error: {0}")]
    Io(String),
}

/// Metadata for a filesystem entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    /// True if this entry is a directory.
    pub is_dir: bool,
    /// Size of the entry in bytes.
    pub size: u64,
}

/// A directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// File or directory name (not a full path).
    pub name: String,
    /// True if this entry is a directory.
    pub is_dir: bool,
}

/// Open options for file operations.
#[derive(Debug, Clone, Copy)]
// Five bools mirror std::fs::OpenOptions. Builder pattern is a future improvement.
#[allow(clippy::struct_excessive_bools)]
pub struct OpenOptions {
    /// Open for reading.
    pub read: bool,
    /// Open for writing.
    pub write: bool,
    /// Append writes to the end of the file.
    pub append: bool,
    /// Create the file if it does not exist.
    pub create: bool,
    /// Truncate the file to zero length on open.
    pub truncate: bool,
}

impl OpenOptions {
    /// Create options for opening a file for reading only.
    #[must_use]
    pub fn read() -> Self {
        Self {
            read: true,
            write: false,
            append: false,
            create: false,
            truncate: false,
        }
    }
    /// Create options for opening a file for writing, creating or truncating it.
    #[must_use]
    pub fn write() -> Self {
        Self {
            read: false,
            write: true,
            append: false,
            create: true,
            truncate: true,
        }
    }
    /// Create options for opening a file for appending, creating it if absent.
    #[must_use]
    pub fn append() -> Self {
        Self {
            read: false,
            write: true,
            append: true,
            create: true,
            truncate: false,
        }
    }
}

/// Virtual filesystem trait.
pub trait VfsWriteSink {
    /// Write a chunk to the sink.
    fn write(&mut self, data: &[u8]) -> Result<(), FsError>;
}

/// Virtual filesystem trait.
pub trait Vfs {
    /// Open a file at `path` with the given options, returning a handle.
    fn open(&mut self, path: &str, opts: OpenOptions) -> Result<FileHandle, FsError>;
    /// Read the entire contents of an open file.
    fn read_file(&self, handle: FileHandle) -> Result<Vec<u8>, FsError>;
    /// Create an owned streaming reader for an open file handle.
    fn stream_file(&self, handle: FileHandle) -> Result<Box<dyn Read>, FsError>;
    /// Write `data` to an open file, replacing its contents.
    fn write_file(&mut self, handle: FileHandle, data: &[u8]) -> Result<(), FsError>;
    /// Open an owned write sink for incremental writes to `path`.
    fn open_write_sink(
        &mut self,
        path: &str,
        append: bool,
    ) -> Result<Box<dyn VfsWriteSink>, FsError>;
    /// Install a single-consumer streaming reader at `path`.
    ///
    /// The next read-only open of this path may consume the installed reader
    /// instead of opening a normal file.
    fn install_stream_reader(&mut self, path: &str, reader: Box<dyn Read>) -> Result<(), FsError>;
    /// Close an open file handle.
    fn close(&mut self, handle: FileHandle);
    /// Return metadata for the entry at `path`.
    fn stat(&self, path: &str) -> Result<Metadata, FsError>;
    /// List the entries in a directory.
    fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;
    /// Create a directory at `path`.
    fn create_dir(&mut self, path: &str) -> Result<(), FsError>;
    /// Remove the file at `path`.
    fn remove_file(&mut self, path: &str) -> Result<(), FsError>;
    /// Remove the empty directory at `path`.
    fn remove_dir(&mut self, path: &str) -> Result<(), FsError>;
}

/// An opaque file handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileHandle(
    /// The raw numeric handle value.
    pub u64,
);

/// Normalize a path: resolve `.`, `..`, and redundant slashes.
pub fn normalize_path(path: &str) -> String {
    let mut components = Vec::new();
    let is_absolute = path.starts_with('/');

    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if !components.is_empty() {
                    components.pop();
                }
            }
            _ => components.push(part),
        }
    }

    let joined = components.join("/");
    if is_absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_error_display() {
        let err = FsError::NotFound("/tmp/foo".into());
        assert_eq!(err.to_string(), "not found: /tmp/foo");
    }

    #[test]
    fn normalize_basic() {
        assert_eq!(normalize_path("/a/b/c"), "/a/b/c");
        assert_eq!(normalize_path("/a//b///c"), "/a/b/c");
        assert_eq!(normalize_path("/a/./b/./c"), "/a/b/c");
        assert_eq!(normalize_path("/a/b/../c"), "/a/c");
        assert_eq!(normalize_path("/a/b/../../c"), "/c");
        assert_eq!(normalize_path("/"), "/");
    }
}
