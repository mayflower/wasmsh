//! Virtual filesystem abstraction for wasmsh.
//!
//! Provides the `Vfs` trait and a `MemoryFs` implementation for
//! ephemeral in-memory filesystems. No `std::fs` is used — this
//! is safe for the browser target.

mod memfs;
#[cfg(feature = "opfs")]
mod opfs;

pub use memfs::MemoryFs;
#[cfg(feature = "opfs")]
pub use opfs::OpfsFs;

use thiserror::Error;

/// Errors from VFS operations.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FsError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("already exists: {0}")]
    AlreadyExists(String),
    #[error("not a directory: {0}")]
    NotADirectory(String),
    #[error("is a directory: {0}")]
    IsADirectory(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("io error: {0}")]
    Io(String),
}

/// Metadata for a filesystem entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    pub is_dir: bool,
    pub size: u64,
}

/// A directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
}

/// Open options for file operations.
#[derive(Debug, Clone, Copy)]
pub struct OpenOptions {
    pub read: bool,
    pub write: bool,
    pub append: bool,
    pub create: bool,
    pub truncate: bool,
}

impl OpenOptions {
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
pub trait Vfs {
    fn open(&mut self, path: &str, opts: OpenOptions) -> Result<FileHandle, FsError>;
    fn read_file(&self, handle: FileHandle) -> Result<Vec<u8>, FsError>;
    fn write_file(&mut self, handle: FileHandle, data: &[u8]) -> Result<(), FsError>;
    fn close(&mut self, handle: FileHandle);
    fn stat(&self, path: &str) -> Result<Metadata, FsError>;
    fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;
    fn create_dir(&mut self, path: &str) -> Result<(), FsError>;
    fn remove_file(&mut self, path: &str) -> Result<(), FsError>;
    fn remove_dir(&mut self, path: &str) -> Result<(), FsError>;
}

/// An opaque file handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileHandle(pub u64);

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
