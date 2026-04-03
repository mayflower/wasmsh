//! Emscripten-backed filesystem for `wasm32-unknown-emscripten`.
//!
//! Delegates to libc POSIX calls which go through Emscripten's virtual
//! filesystem. This shares the same FS that Python sees inside Pyodide.

use std::collections::HashMap;
use std::ffi::CString;

use crate::{DirEntry, FileHandle, FsError, Metadata, OpenOptions, Vfs};

/// A filesystem backed by Emscripten's POSIX layer.
///
/// All operations use libc `fopen`/`fread`/`fwrite`/`fclose`/`stat`/`opendir`
/// etc., which route through Emscripten's in-process VFS.
#[derive(Debug)]
pub struct EmscriptenFs {
    next_handle: u64,
    /// Maps our `FileHandle` to (libc `FILE*`, path, open-for-write).
    open_files: HashMap<u64, OpenFile>,
}

#[derive(Debug)]
struct OpenFile {
    fp: *mut libc::FILE,
    path: String,
    writable: bool,
    append: bool,
}

fn to_cstring(path: &str) -> Result<CString, FsError> {
    CString::new(path).map_err(|_| FsError::Io("path contains null byte".into()))
}

/// Map the current libc errno to the closest `FsError` variant.
fn errno_to_fs_error(path: &str) -> FsError {
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::ENOENT) => FsError::NotFound(path.to_string()),
        Some(libc::EACCES | libc::EPERM) => FsError::PermissionDenied(path.to_string()),
        Some(libc::EEXIST) => FsError::AlreadyExists(path.to_string()),
        Some(libc::EISDIR) => FsError::IsADirectory(path.to_string()),
        Some(libc::ENOTDIR) => FsError::NotADirectory(path.to_string()),
        Some(libc::ENOTEMPTY) => FsError::Io(format!("directory not empty: {path}")),
        _ => FsError::Io(format!("{err}: {path}")),
    }
}

/// Recursively create parent directories (like `mkdir -p`).
fn ensure_parents(dir: &str) {
    let mut acc = String::new();
    for part in dir.split('/') {
        if part.is_empty() {
            acc.push('/');
            continue;
        }
        if !acc.ends_with('/') {
            acc.push('/');
        }
        acc.push_str(part);
        if let Ok(cpath) = CString::new(acc.as_str()) {
            unsafe { libc::mkdir(cpath.as_ptr(), 0o755) };
            // Ignore errors — dir may already exist.
        }
    }
}

impl EmscriptenFs {
    /// Create a new Emscripten filesystem handle.
    pub fn new() -> Self {
        Self {
            next_handle: 1,
            open_files: HashMap::new(),
        }
    }
}

impl Default for EmscriptenFs {
    fn default() -> Self {
        Self::new()
    }
}

impl Vfs for EmscriptenFs {
    fn open(&mut self, path: &str, opts: OpenOptions) -> Result<FileHandle, FsError> {
        // Auto-create parent directories for write operations (matches MemoryFs).
        if opts.write || opts.append || opts.create {
            if let Some(parent) = path.rsplit_once('/') {
                if !parent.0.is_empty() {
                    ensure_parents(parent.0);
                }
            }
        }

        let cpath = to_cstring(path)?;
        let mode = if opts.append {
            c"a+"
        } else if opts.write && opts.read {
            if opts.truncate {
                c"w+"
            } else {
                c"r+"
            }
        } else if opts.write {
            c"w"
        } else {
            c"r"
        };

        let fp = unsafe { libc::fopen(cpath.as_ptr(), mode.as_ptr()) };
        if fp.is_null() {
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::stat(cpath.as_ptr(), &mut st) } != 0 {
                return Err(FsError::NotFound(path.to_string()));
            }
            return Err(FsError::PermissionDenied(path.to_string()));
        }

        let h = self.next_handle;
        self.next_handle += 1;
        self.open_files.insert(
            h,
            OpenFile {
                fp,
                path: path.to_string(),
                writable: opts.write || opts.append,
                append: opts.append,
            },
        );
        Ok(FileHandle(h))
    }

    fn read_file(&self, handle: FileHandle) -> Result<Vec<u8>, FsError> {
        let of = self
            .open_files
            .get(&handle.0)
            .ok_or_else(|| FsError::Io("invalid handle".into()))?;

        // Seek to start, read all
        unsafe { libc::fseek(of.fp, 0, libc::SEEK_SET) };
        let mut buf = vec![0u8; 65536];
        let mut result = Vec::new();
        loop {
            let n = unsafe { libc::fread(buf.as_mut_ptr().cast(), 1, buf.len(), of.fp) };
            if n == 0 {
                break;
            }
            result.extend_from_slice(&buf[..n]);
        }
        Ok(result)
    }

    fn write_file(&mut self, handle: FileHandle, data: &[u8]) -> Result<(), FsError> {
        let of = self
            .open_files
            .get(&handle.0)
            .ok_or_else(|| FsError::Io("invalid handle".into()))?;
        if !of.writable {
            return Err(FsError::PermissionDenied(of.path.clone()));
        }
        if of.append {
            // Append mode: seek to end, write there.
            unsafe { libc::fseek(of.fp, 0, libc::SEEK_END) };
        } else {
            // Overwrite mode: truncate to the written length.
            unsafe { libc::fseek(of.fp, 0, libc::SEEK_SET) };
        }
        let written = unsafe { libc::fwrite(data.as_ptr().cast(), 1, data.len(), of.fp) };
        if !of.append {
            // Truncate any leftover content from a previous longer write.
            let pos = unsafe { libc::ftell(of.fp) };
            if pos >= 0 {
                unsafe { libc::ftruncate(libc::fileno(of.fp), pos as libc::off_t) };
            }
        }
        unsafe { libc::fflush(of.fp) };
        if written != data.len() {
            return Err(FsError::Io("short write".into()));
        }
        Ok(())
    }

    fn close(&mut self, handle: FileHandle) {
        if let Some(of) = self.open_files.remove(&handle.0) {
            unsafe { libc::fclose(of.fp) };
        }
    }

    fn stat(&self, path: &str) -> Result<Metadata, FsError> {
        let cpath = to_cstring(path)?;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::stat(cpath.as_ptr(), &mut st) } != 0 {
            return Err(FsError::NotFound(path.to_string()));
        }
        Ok(Metadata {
            is_dir: (st.st_mode & libc::S_IFMT) == libc::S_IFDIR,
            size: st.st_size as u64,
        })
    }

    fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let cpath = to_cstring(path)?;
        let dp = unsafe { libc::opendir(cpath.as_ptr()) };
        if dp.is_null() {
            return Err(FsError::NotFound(path.to_string()));
        }
        let mut entries = Vec::new();
        loop {
            let ent = unsafe { libc::readdir(dp) };
            if ent.is_null() {
                break;
            }
            let name_ptr = unsafe { (*ent).d_name.as_ptr() };
            let name = unsafe { std::ffi::CStr::from_ptr(name_ptr) }
                .to_string_lossy()
                .into_owned();
            if name == "." || name == ".." {
                continue;
            }
            let is_dir = unsafe { (*ent).d_type } == libc::DT_DIR;
            entries.push(DirEntry { name, is_dir });
        }
        unsafe { libc::closedir(dp) };
        Ok(entries)
    }

    fn create_dir(&mut self, path: &str) -> Result<(), FsError> {
        let cpath = to_cstring(path)?;
        let rc = unsafe { libc::mkdir(cpath.as_ptr(), 0o755) };
        if rc != 0 {
            return Err(errno_to_fs_error(path));
        }
        Ok(())
    }

    fn remove_file(&mut self, path: &str) -> Result<(), FsError> {
        let cpath = to_cstring(path)?;
        if unsafe { libc::unlink(cpath.as_ptr()) } != 0 {
            return Err(errno_to_fs_error(path));
        }
        Ok(())
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), FsError> {
        let cpath = to_cstring(path)?;
        if unsafe { libc::rmdir(cpath.as_ptr()) } != 0 {
            return Err(errno_to_fs_error(path));
        }
        Ok(())
    }
}
