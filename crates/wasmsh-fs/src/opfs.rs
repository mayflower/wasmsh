//! OPFS-backed filesystem adapter for persistent browser storage.
//!
//! This module is only available when the `opfs` feature is enabled.
//! On non-wasm targets it provides a compile-time stub that always
//! returns errors, ensuring the core crate never becomes browser-only.
//!
//! The actual OPFS integration requires `wasm-bindgen` and the
//! `web_sys::FileSystemDirectoryHandle` API, which will be added
//! when the browser build tooling is set up.

use crate::{DirEntry, FileHandle, FsError, Metadata, OpenOptions, Vfs};

/// OPFS-backed virtual filesystem for persistent browser storage.
///
/// On non-wasm targets this is a stub that returns `FsError::Io`
/// for all operations, acting as a clear fallback.
#[derive(Debug)]
pub struct OpfsFs {
    _private: (),
}

impl OpfsFs {
    /// Create a new OPFS filesystem handle.
    ///
    /// On wasm32 targets with OPFS support, this will initialize the
    /// persistent storage. On other targets, this returns a stub.
    #[must_use]
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl Default for OpfsFs {
    fn default() -> Self {
        Self::new()
    }
}

// On non-wasm targets (or until wasm-bindgen is integrated), all
// operations return errors. This ensures the trait boundary is
// identical to MemoryFs and code compiles on all platforms.

impl Vfs for OpfsFs {
    fn open(&mut self, path: &str, _opts: OpenOptions) -> Result<FileHandle, FsError> {
        Err(FsError::Io(format!(
            "OPFS not available on this platform: {path}"
        )))
    }

    fn read_file(&self, _handle: FileHandle) -> Result<Vec<u8>, FsError> {
        Err(FsError::Io("OPFS not available on this platform".into()))
    }

    fn write_file(&mut self, _handle: FileHandle, _data: &[u8]) -> Result<(), FsError> {
        Err(FsError::Io("OPFS not available on this platform".into()))
    }

    fn close(&mut self, _handle: FileHandle) {}

    fn stat(&self, path: &str) -> Result<Metadata, FsError> {
        Err(FsError::Io(format!(
            "OPFS not available on this platform: {path}"
        )))
    }

    fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        Err(FsError::Io(format!(
            "OPFS not available on this platform: {path}"
        )))
    }

    fn create_dir(&mut self, path: &str) -> Result<(), FsError> {
        Err(FsError::Io(format!(
            "OPFS not available on this platform: {path}"
        )))
    }

    fn remove_file(&mut self, path: &str) -> Result<(), FsError> {
        Err(FsError::Io(format!(
            "OPFS not available on this platform: {path}"
        )))
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), FsError> {
        Err(FsError::Io(format!(
            "OPFS not available on this platform: {path}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opfs_stub_returns_errors() {
        let mut fs = OpfsFs::new();
        assert!(fs.open("/test", OpenOptions::read()).is_err());
        assert!(fs.stat("/test").is_err());
        assert!(fs.read_dir("/").is_err());
        assert!(fs.create_dir("/test").is_err());
    }
}
