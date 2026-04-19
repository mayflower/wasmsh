//! Libc-backed filesystem for the Pyodide embedding.
//!
//! Delegates to libc POSIX calls which route through Emscripten's in-process
//! VFS — the same filesystem Python sees inside Pyodide.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::io::Read;
use std::rc::Rc;

use crate::{DirEntry, FileHandle, FsError, Metadata, OpenOptions, Vfs, VfsWriteSink};

/// A filesystem backed by Emscripten's POSIX layer.
///
/// All operations use libc `fopen`/`fread`/`fwrite`/`fclose`/`stat`/`opendir`
/// etc., which route through Emscripten's in-process VFS.
pub struct EmscriptenFs {
    next_handle: u64,
    virtual_readers: HashMap<String, Rc<RefCell<Box<dyn Read>>>>,
    /// Maps our `FileHandle` to (libc `FILE*`, path, open-for-write).
    open_files: HashMap<u64, OpenFile>,
}

impl std::fmt::Debug for EmscriptenFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmscriptenFs")
            .field("next_handle", &self.next_handle)
            .field("virtual_reader_count", &self.virtual_readers.len())
            .field("open_files", &self.open_files)
            .finish()
    }
}

struct OpenFile {
    source: OpenFileSource,
    opts: OpenOptions,
}

impl std::fmt::Debug for OpenFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source = match &self.source {
            OpenFileSource::Path { path, .. } => path.as_str(),
            OpenFileSource::Virtual(_) => "<virtual>",
        };
        f.debug_struct("OpenFile")
            .field("source", &source)
            .field("opts", &self.opts)
            .finish()
    }
}

enum OpenFileSource {
    Path {
        fp: *mut libc::FILE,
        path: String,
        writable: bool,
        append: bool,
    },
    Virtual(Rc<RefCell<Box<dyn Read>>>),
}

struct EmscriptenFileReader {
    fp: *mut libc::FILE,
}

struct EmscriptenWriteSink {
    fp: *mut libc::FILE,
}

struct EmscriptenSharedReadHandle {
    reader: Rc<RefCell<Box<dyn Read>>>,
}

impl Read for EmscriptenFileReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = unsafe { libc::fread(buf.as_mut_ptr().cast(), 1, buf.len(), self.fp) };
        if read == 0 {
            // fread returns 0 on both EOF and error.  We must use ferror()
            // to distinguish — checking errno/last_os_error() is incorrect
            // because errno may be stale from a completely unrelated libc
            // call (e.g. a failed fopen during command resolution).
            if unsafe { libc::ferror(self.fp) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            return Ok(0);
        }
        if read > buf.len() {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(read)
        }
    }
}

impl Read for EmscriptenSharedReadHandle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.borrow_mut().read(buf)
    }
}

impl Drop for EmscriptenFileReader {
    fn drop(&mut self) {
        unsafe { libc::fclose(self.fp) };
    }
}

impl VfsWriteSink for EmscriptenWriteSink {
    fn write(&mut self, data: &[u8]) -> Result<(), FsError> {
        let written = unsafe { libc::fwrite(data.as_ptr().cast(), 1, data.len(), self.fp) };
        unsafe { libc::fflush(self.fp) };
        if written != data.len() {
            return Err(FsError::Io("short write".into()));
        }
        Ok(())
    }
}

impl Drop for EmscriptenWriteSink {
    fn drop(&mut self) {
        unsafe { libc::fclose(self.fp) };
    }
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
            virtual_readers: HashMap::new(),
            open_files: HashMap::new(),
        }
    }
}

impl Clone for EmscriptenFs {
    fn clone(&self) -> Self {
        Self {
            next_handle: 1,
            virtual_readers: self.virtual_readers.clone(),
            open_files: HashMap::new(),
        }
    }
}

impl Drop for EmscriptenFs {
    fn drop(&mut self) {
        for of in self.open_files.drain() {
            if let OpenFileSource::Path { fp, .. } = of.1.source {
                unsafe { libc::fclose(fp) };
            }
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
        if opts.read && !opts.write && !opts.append && !opts.create && !opts.truncate {
            if let Some(reader) = self.virtual_readers.remove(path) {
                let h = self.next_handle;
                self.next_handle += 1;
                self.open_files.insert(
                    h,
                    OpenFile {
                        source: OpenFileSource::Virtual(reader),
                        opts,
                    },
                );
                return Ok(FileHandle(h));
            }
        }

        // Auto-create parent directories for write operations (matches MemoryFs).
        if opts.write || opts.append || opts.create {
            self.virtual_readers.remove(path);
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
                source: OpenFileSource::Path {
                    fp,
                    path: path.to_string(),
                    writable: opts.write || opts.append,
                    append: opts.append,
                },
                opts,
            },
        );
        Ok(FileHandle(h))
    }

    fn read_file(&self, handle: FileHandle) -> Result<Vec<u8>, FsError> {
        let of = self
            .open_files
            .get(&handle.0)
            .ok_or_else(|| FsError::Io("invalid handle".into()))?;
        if !of.opts.read {
            return Err(FsError::PermissionDenied("not opened for reading".into()));
        }

        match &of.source {
            OpenFileSource::Path { fp, .. } => {
                unsafe { libc::fseek(*fp, 0, libc::SEEK_SET) };
                let mut buf = vec![0u8; 65536];
                let mut result = Vec::new();
                loop {
                    let n = unsafe { libc::fread(buf.as_mut_ptr().cast(), 1, buf.len(), *fp) };
                    if n == 0 {
                        break;
                    }
                    result.extend_from_slice(&buf[..n]);
                }
                Ok(result)
            }
            OpenFileSource::Virtual(reader) => {
                let mut result = Vec::new();
                reader
                    .borrow_mut()
                    .read_to_end(&mut result)
                    .map_err(|err| FsError::Io(err.to_string()))?;
                Ok(result)
            }
        }
    }

    fn stream_file(&self, handle: FileHandle) -> Result<Box<dyn Read>, FsError> {
        let of = self
            .open_files
            .get(&handle.0)
            .ok_or_else(|| FsError::Io("invalid handle".into()))?;
        if !of.opts.read {
            return Err(FsError::PermissionDenied("not opened for reading".into()));
        }
        match &of.source {
            OpenFileSource::Path { path, .. } => {
                let cpath = to_cstring(path)?;
                let fp = unsafe { libc::fopen(cpath.as_ptr(), c"r".as_ptr()) };
                if fp.is_null() {
                    return Err(errno_to_fs_error(path));
                }
                Ok(Box::new(EmscriptenFileReader { fp }))
            }
            OpenFileSource::Virtual(reader) => Ok(Box::new(EmscriptenSharedReadHandle {
                reader: Rc::clone(reader),
            })),
        }
    }

    fn write_file(&mut self, handle: FileHandle, data: &[u8]) -> Result<(), FsError> {
        let of = self
            .open_files
            .get(&handle.0)
            .ok_or_else(|| FsError::Io("invalid handle".into()))?;
        match &of.source {
            OpenFileSource::Path {
                fp,
                path,
                writable,
                append,
            } => {
                if !writable {
                    return Err(FsError::PermissionDenied(path.clone()));
                }
                if *append {
                    unsafe { libc::fseek(*fp, 0, libc::SEEK_END) };
                } else {
                    unsafe { libc::fseek(*fp, 0, libc::SEEK_SET) };
                }
                let written = unsafe { libc::fwrite(data.as_ptr().cast(), 1, data.len(), *fp) };
                if !append {
                    let pos = unsafe { libc::ftell(*fp) };
                    if pos >= 0 {
                        unsafe { libc::ftruncate(libc::fileno(*fp), pos as libc::off_t) };
                    }
                }
                unsafe { libc::fflush(*fp) };
                if written != data.len() {
                    return Err(FsError::Io("short write".into()));
                }
                Ok(())
            }
            OpenFileSource::Virtual(_) => Err(FsError::PermissionDenied(
                "virtual reader is read-only".into(),
            )),
        }
    }

    fn open_write_sink(
        &mut self,
        path: &str,
        append: bool,
    ) -> Result<Box<dyn VfsWriteSink>, FsError> {
        if let Some(parent) = path.rsplit_once('/') {
            if !parent.0.is_empty() {
                ensure_parents(parent.0);
            }
        }
        let cpath = to_cstring(path)?;
        let mode = if append { c"a" } else { c"w" };
        let fp = unsafe { libc::fopen(cpath.as_ptr(), mode.as_ptr()) };
        if fp.is_null() {
            return Err(errno_to_fs_error(path));
        }
        Ok(Box::new(EmscriptenWriteSink { fp }))
    }

    fn install_stream_reader(&mut self, path: &str, reader: Box<dyn Read>) -> Result<(), FsError> {
        self.virtual_readers
            .insert(path.to_string(), Rc::new(RefCell::new(reader)));
        Ok(())
    }

    fn close(&mut self, handle: FileHandle) {
        if let Some(of) = self.open_files.remove(&handle.0) {
            if let OpenFileSource::Path { fp, .. } = of.source {
                unsafe { libc::fclose(fp) };
            }
        }
    }

    fn stat(&self, path: &str) -> Result<Metadata, FsError> {
        if self.virtual_readers.contains_key(path) {
            return Ok(Metadata {
                is_dir: false,
                size: 0,
            });
        }
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
        if self.virtual_readers.remove(path).is_some() {
            return Ok(());
        }
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
