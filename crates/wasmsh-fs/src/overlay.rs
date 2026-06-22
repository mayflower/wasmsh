//! Lazy copy-on-write overlay filesystem.
//!
//! [`OverlayFs`] composes a read-only, lazily-read [`LazyBase`] with a writable
//! [`MemoryFs`] upper layer. Reads fall through to the base with no copy; the
//! first write to a base-backed path materializes it into the upper layer
//! (copy-on-write). Deletions of base entries are recorded as *whiteouts* so the
//! base entry stays hidden without mutating the base.
//!
//! An overlay constructed with an empty base behaves identically to its upper
//! `MemoryFs`, which is what lets it stand in for `MemoryFs` as the default
//! [`crate::BackendFs`] on non-emscripten targets.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::rc::Rc;
use std::sync::Arc;

use crate::{
    normalize_path, DirEntry, FileHandle, FsChangeLog, FsError, MemoryFs, Metadata, OpenOptions,
    Vfs, VfsWriteSink,
};

/// A read-only, lazily-read filesystem layer used as the base of an
/// [`OverlayFs`].
///
/// Implementations expose only what an overlay needs to read through: metadata,
/// whole-file contents, and directory listings. The base is never mutated by the
/// overlay. An implementation is free to fetch and decode bytes on demand in
/// [`LazyBase::read`] (that is the "lazy" in lazy copy-on-write).
pub trait LazyBase {
    /// Return metadata for the entry at `path`, or [`FsError::NotFound`].
    fn stat(&self, path: &str) -> Result<Metadata, FsError>;
    /// Read the entire contents of the file at `path`.
    fn read(&self, path: &str) -> Result<Arc<[u8]>, FsError>;
    /// List the direct children of the directory at `path`.
    fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, FsError>;
}

/// An in-memory [`LazyBase`]: a fixed set of read-only files and the directories
/// implied by their paths. Cheap to clone (`Rc`-backed); clones share storage.
#[derive(Debug, Clone, Default)]
pub struct InMemoryBase {
    inner: Rc<InMemoryBaseData>,
}

#[derive(Debug, Default)]
struct InMemoryBaseData {
    files: HashMap<String, Arc<[u8]>>,
    dirs: HashSet<String>,
}

impl InMemoryBase {
    /// Create an empty base (root directory only).
    #[must_use]
    pub fn new() -> Self {
        let mut dirs = HashSet::new();
        dirs.insert("/".to_string());
        Self {
            inner: Rc::new(InMemoryBaseData {
                files: HashMap::new(),
                dirs,
            }),
        }
    }

    /// Build a base from an iterator of `(path, contents)` pairs. Paths are
    /// normalized; the directories implied by each path are synthesized.
    pub fn from_files<I, P, D>(files: I) -> Self
    where
        I: IntoIterator<Item = (P, D)>,
        P: AsRef<str>,
        D: Into<Vec<u8>>,
    {
        let mut file_map: HashMap<String, Arc<[u8]>> = HashMap::new();
        let mut dirs: HashSet<String> = HashSet::new();
        dirs.insert("/".to_string());
        for (path, data) in files {
            let norm = normalize_path(path.as_ref());
            if norm == "/" {
                continue;
            }
            for ancestor in ancestors(&norm) {
                dirs.insert(ancestor);
            }
            file_map.insert(norm, Arc::from(data.into()));
        }
        Self {
            inner: Rc::new(InMemoryBaseData {
                files: file_map,
                dirs,
            }),
        }
    }
}

/// Return the proper ancestor directories of a normalized absolute path,
/// excluding the path itself, including `/`.
fn ancestors(norm: &str) -> Vec<String> {
    let parts: Vec<&str> = norm.split('/').filter(|s| !s.is_empty()).collect();
    let mut out = vec!["/".to_string()];
    let mut current = String::new();
    for part in &parts[..parts.len().saturating_sub(1)] {
        current.push('/');
        current.push_str(part);
        out.push(current.clone());
    }
    out
}

/// Return the parent directory of a normalized absolute path (`/` for top-level
/// entries and for `/` itself).
fn parent_of(norm: &str) -> String {
    match norm.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(idx) => norm[..idx].to_string(),
    }
}

/// Join a normalized directory path with a child name.
fn join_child(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

impl LazyBase for InMemoryBase {
    fn stat(&self, path: &str) -> Result<Metadata, FsError> {
        let norm = normalize_path(path);
        if let Some(data) = self.inner.files.get(&norm) {
            return Ok(Metadata {
                is_dir: false,
                size: data.len() as u64,
            });
        }
        if self.inner.dirs.contains(&norm) {
            return Ok(Metadata {
                is_dir: true,
                size: 0,
            });
        }
        Err(FsError::NotFound(norm))
    }

    fn read(&self, path: &str) -> Result<Arc<[u8]>, FsError> {
        let norm = normalize_path(path);
        if let Some(data) = self.inner.files.get(&norm) {
            return Ok(Arc::clone(data));
        }
        if self.inner.dirs.contains(&norm) {
            return Err(FsError::IsADirectory(norm));
        }
        Err(FsError::NotFound(norm))
    }

    fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let norm = normalize_path(path);
        if self.inner.files.contains_key(&norm) {
            return Err(FsError::NotADirectory(norm));
        }
        if !self.inner.dirs.contains(&norm) {
            return Err(FsError::NotFound(norm));
        }
        let mut entries: HashMap<String, bool> = HashMap::new();
        for dir in &self.inner.dirs {
            if dir != "/" && parent_of(dir) == norm {
                if let Some(name) = dir.rsplit('/').next() {
                    entries.insert(name.to_string(), true);
                }
            }
        }
        for file in self.inner.files.keys() {
            if parent_of(file) == norm {
                if let Some(name) = file.rsplit('/').next() {
                    entries.insert(name.to_string(), false);
                }
            }
        }
        Ok(entries
            .into_iter()
            .map(|(name, is_dir)| DirEntry { name, is_dir })
            .collect())
    }
}

/// How an open [`FileHandle`] in an overlay is backed.
enum OverlayHandleKind {
    /// Delegates to a handle in the upper `MemoryFs`.
    Upper(FileHandle),
    /// A read-only snapshot of base file contents (zero-copy `Arc`).
    BaseRead(Arc<[u8]>),
}

impl std::fmt::Debug for OverlayHandleKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Upper(h) => f.debug_tuple("Upper").field(h).finish(),
            Self::BaseRead(data) => f.debug_tuple("BaseRead").field(&data.len()).finish(),
        }
    }
}

#[derive(Debug, Default)]
struct OverlayState {
    whiteouts: HashSet<String>,
    handles: HashMap<u64, OverlayHandleKind>,
    next_handle: u64,
}

/// A lazy copy-on-write filesystem layering a writable [`MemoryFs`] over a
/// read-only [`LazyBase`].
///
/// See the module docs for the resolution and copy-on-write semantics. Cloning
/// is shallow: the upper layer, the base, and the overlay's own state
/// (whiteouts + handle table) are all shared with the clone, matching
/// `MemoryFs`'s clone behavior.
#[derive(Debug, Clone)]
pub struct OverlayFs<B: LazyBase> {
    upper: MemoryFs,
    base: B,
    state: Rc<RefCell<OverlayState>>,
}

impl<B: LazyBase + Default> Default for OverlayFs<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B: LazyBase + Default> OverlayFs<B> {
    /// Create an overlay with an empty (default) base. Behaves identically to a
    /// fresh `MemoryFs`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_base(B::default())
    }
}

impl<B: LazyBase> OverlayFs<B> {
    /// Create an overlay over the given read-only base.
    #[must_use]
    pub fn with_base(base: B) -> Self {
        Self {
            upper: MemoryFs::new(),
            base,
            state: Rc::new(RefCell::new(OverlayState {
                whiteouts: HashSet::new(),
                handles: HashMap::new(),
                next_handle: 1,
            })),
        }
    }

    /// Replace the read-only base and clear all whiteouts. The upper layer is
    /// left intact (it always takes priority over the base).
    pub fn replace_base(&mut self, base: B) {
        self.base = base;
        self.state.borrow_mut().whiteouts.clear();
    }

    fn alloc_handle(&self, kind: OverlayHandleKind) -> FileHandle {
        let mut state = self.state.borrow_mut();
        let h = state.next_handle;
        state.next_handle = state
            .next_handle
            .checked_add(1)
            .expect("file handle counter overflow");
        state.handles.insert(h, kind);
        FileHandle(h)
    }

    fn is_whiteout(&self, norm: &str) -> bool {
        self.state.borrow().whiteouts.contains(norm)
    }

    fn add_whiteout(&self, norm: &str) {
        self.state.borrow_mut().whiteouts.insert(norm.to_string());
    }

    fn clear_whiteout(&self, norm: &str) {
        self.state.borrow_mut().whiteouts.remove(norm);
    }

    /// Record a change on the upper layer's change log (shared with the host
    /// drain). Used for base-only mutations that never touch the upper layer.
    fn record_change(&self, norm: &str) {
        if let Some(log) = self.upper.change_log() {
            log.record(norm);
        }
    }

    /// Copy base contents into the upper layer so a subsequent write mutates a
    /// private copy. No-op if the upper layer already has the path.
    fn materialize(&mut self, norm: &str, data: &[u8]) -> Result<(), FsError> {
        let h = self.upper.open(norm, OpenOptions::write())?;
        let res = self.upper.write_file(h, data);
        self.upper.close(h);
        res
    }
}

impl<B: LazyBase> Vfs for OverlayFs<B> {
    fn open(&mut self, path: &str, opts: OpenOptions) -> Result<FileHandle, FsError> {
        let norm = normalize_path(path);
        let read_only = opts.read && !opts.write && !opts.append && !opts.create && !opts.truncate;

        // The upper layer is authoritative for anything it already knows about
        // (materialized files, created dirs, virtual readers).
        if self.upper.stat(&norm).is_ok() {
            let h = self.upper.open(&norm, opts)?;
            return Ok(self.alloc_handle(OverlayHandleKind::Upper(h)));
        }

        if read_only {
            if self.is_whiteout(&norm) {
                return Err(FsError::NotFound(norm));
            }
            return match self.base.stat(&norm) {
                Ok(meta) if meta.is_dir => Err(FsError::IsADirectory(norm)),
                Ok(_) => {
                    let data = self.base.read(&norm)?;
                    Ok(self.alloc_handle(OverlayHandleKind::BaseRead(data)))
                }
                Err(e) => Err(e),
            };
        }

        // Write-intent open with nothing in the upper layer yet.
        if self.is_whiteout(&norm) {
            // The base entry was deleted; recreate fresh in the upper layer.
            self.clear_whiteout(&norm);
            let h = self.upper.open(&norm, opts)?;
            return Ok(self.alloc_handle(OverlayHandleKind::Upper(h)));
        }

        match self.base.stat(&norm) {
            Ok(meta) if meta.is_dir => Err(FsError::IsADirectory(norm)),
            Ok(_) => {
                // Copy-on-write: materialize the base file unless this open
                // fully truncates it (in which case the old content is gone).
                let fully_truncates = opts.truncate && !opts.append;
                if !fully_truncates {
                    let data = self.base.read(&norm)?;
                    self.materialize(&norm, &data)?;
                }
                let h = self.upper.open(&norm, opts)?;
                Ok(self.alloc_handle(OverlayHandleKind::Upper(h)))
            }
            Err(FsError::NotFound(_)) => {
                let h = self.upper.open(&norm, opts)?;
                Ok(self.alloc_handle(OverlayHandleKind::Upper(h)))
            }
            Err(e) => Err(e),
        }
    }

    fn read_file(&self, handle: FileHandle) -> Result<Vec<u8>, FsError> {
        let upper_handle = {
            let state = self.state.borrow();
            match state.handles.get(&handle.0) {
                Some(OverlayHandleKind::Upper(h)) => *h,
                Some(OverlayHandleKind::BaseRead(data)) => return Ok(data.as_ref().to_vec()),
                None => return Err(FsError::Io("invalid handle".into())),
            }
        };
        self.upper.read_file(upper_handle)
    }

    fn stream_file(&self, handle: FileHandle) -> Result<Box<dyn Read>, FsError> {
        let upper_handle = {
            let state = self.state.borrow();
            match state.handles.get(&handle.0) {
                Some(OverlayHandleKind::Upper(h)) => *h,
                Some(OverlayHandleKind::BaseRead(data)) => {
                    return Ok(Box::new(Cursor::new(Arc::clone(data))))
                }
                None => return Err(FsError::Io("invalid handle".into())),
            }
        };
        self.upper.stream_file(upper_handle)
    }

    fn write_file(&mut self, handle: FileHandle, data: &[u8]) -> Result<(), FsError> {
        let upper_handle = {
            let state = self.state.borrow();
            match state.handles.get(&handle.0) {
                Some(OverlayHandleKind::Upper(h)) => *h,
                Some(OverlayHandleKind::BaseRead(_)) => {
                    return Err(FsError::PermissionDenied("not opened for writing".into()))
                }
                None => return Err(FsError::Io("invalid handle".into())),
            }
        };
        self.upper.write_file(upper_handle, data)
    }

    fn open_write_sink(
        &mut self,
        path: &str,
        append: bool,
    ) -> Result<Box<dyn VfsWriteSink>, FsError> {
        let norm = normalize_path(path);
        if self.upper.stat(&norm).is_ok() {
            return self.upper.open_write_sink(&norm, append);
        }
        if self.is_whiteout(&norm) {
            self.clear_whiteout(&norm);
            return self.upper.open_write_sink(&norm, append);
        }
        match self.base.stat(&norm) {
            Ok(meta) if meta.is_dir => Err(FsError::IsADirectory(norm)),
            Ok(_) => {
                // Appending continues from the base content, so materialize it
                // first; truncating discards it (a fresh empty upper file
                // shadows the base).
                if append {
                    let data = self.base.read(&norm)?;
                    self.materialize(&norm, &data)?;
                }
                self.upper.open_write_sink(&norm, append)
            }
            Err(FsError::NotFound(_)) => self.upper.open_write_sink(&norm, append),
            Err(e) => Err(e),
        }
    }

    fn install_stream_reader(&mut self, path: &str, reader: Box<dyn Read>) -> Result<(), FsError> {
        let norm = normalize_path(path);
        self.clear_whiteout(&norm);
        self.upper.install_stream_reader(&norm, reader)
    }

    fn close(&mut self, handle: FileHandle) {
        let kind = self.state.borrow_mut().handles.remove(&handle.0);
        if let Some(OverlayHandleKind::Upper(h)) = kind {
            self.upper.close(h);
        }
    }

    fn stat(&self, path: &str) -> Result<Metadata, FsError> {
        let norm = normalize_path(path);
        if let Ok(meta) = self.upper.stat(&norm) {
            return Ok(meta);
        }
        if self.is_whiteout(&norm) {
            return Err(FsError::NotFound(norm));
        }
        self.base.stat(&norm)
    }

    fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let norm = normalize_path(path);

        let upper_meta = self.upper.stat(&norm);
        if let Ok(meta) = &upper_meta {
            if !meta.is_dir {
                return Err(FsError::NotADirectory(norm));
            }
        }
        let whiteout = self.is_whiteout(&norm);
        let base_dir = if whiteout {
            None
        } else {
            match self.base.stat(&norm) {
                Ok(meta) if meta.is_dir => Some(()),
                Ok(_) => return Err(FsError::NotADirectory(norm)),
                Err(_) => None,
            }
        };

        if upper_meta.is_err() && base_dir.is_none() {
            if whiteout {
                return Err(FsError::NotFound(norm));
            }
            // Surface the base's own error (NotFound / NotADirectory).
            return self.base.read_dir(&norm).map(|_| Vec::new());
        }

        let mut merged: HashMap<String, bool> = HashMap::new();
        if base_dir.is_some() {
            if let Ok(entries) = self.base.read_dir(&norm) {
                for entry in entries {
                    let child = join_child(&norm, &entry.name);
                    if !self.is_whiteout(&child) {
                        merged.insert(entry.name, entry.is_dir);
                    }
                }
            }
        }
        if upper_meta.is_ok() {
            if let Ok(entries) = self.upper.read_dir(&norm) {
                for entry in entries {
                    merged.insert(entry.name, entry.is_dir);
                }
            }
        }

        let mut out: Vec<DirEntry> = merged
            .into_iter()
            .map(|(name, is_dir)| DirEntry { name, is_dir })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn create_dir(&mut self, path: &str) -> Result<(), FsError> {
        let norm = normalize_path(path);
        if self.upper.stat(&norm).is_ok() {
            return Err(FsError::AlreadyExists(norm));
        }
        if self.is_whiteout(&norm) {
            self.clear_whiteout(&norm);
        } else if self.base.stat(&norm).is_ok() {
            return Err(FsError::AlreadyExists(norm));
        }
        self.upper.create_dir(&norm)
    }

    fn remove_file(&mut self, path: &str) -> Result<(), FsError> {
        let norm = normalize_path(path);
        let in_upper = self.upper.stat(&norm);
        match in_upper {
            Ok(meta) if meta.is_dir => return Err(FsError::IsADirectory(norm)),
            Ok(_) => {
                self.upper.remove_file(&norm)?;
                // If the base also has this path, hide it so it does not
                // reappear after the upper copy is gone.
                if matches!(self.base.stat(&norm), Ok(m) if !m.is_dir) {
                    self.add_whiteout(&norm);
                }
                return Ok(());
            }
            Err(_) => {}
        }
        if self.is_whiteout(&norm) {
            return Err(FsError::NotFound(norm));
        }
        match self.base.stat(&norm) {
            Ok(meta) if meta.is_dir => Err(FsError::IsADirectory(norm)),
            Ok(_) => {
                self.add_whiteout(&norm);
                self.record_change(&norm);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), FsError> {
        let norm = normalize_path(path);
        let upper_meta = self.upper.stat(&norm);
        if let Ok(meta) = &upper_meta {
            if !meta.is_dir {
                return Err(FsError::NotADirectory(norm));
            }
        }
        let whiteout = self.is_whiteout(&norm);
        let base_is_dir = if whiteout {
            false
        } else {
            matches!(self.base.stat(&norm), Ok(m) if m.is_dir)
        };
        let base_is_file = !whiteout && matches!(self.base.stat(&norm), Ok(m) if !m.is_dir);
        if base_is_file {
            return Err(FsError::NotADirectory(norm));
        }
        if upper_meta.is_err() && !base_is_dir {
            return Err(FsError::NotFound(norm));
        }

        // Directory must be empty in the merged view.
        if !self.read_dir(&norm)?.is_empty() {
            return Err(FsError::Io(format!("directory not empty: {norm}")));
        }

        if upper_meta.is_ok() {
            self.upper.remove_dir(&norm)?;
        }
        if base_is_dir {
            self.add_whiteout(&norm);
        }
        self.record_change(&norm);
        Ok(())
    }

    fn change_log(&self) -> Option<&FsChangeLog> {
        self.upper.change_log()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_with(files: &[(&str, &[u8])]) -> InMemoryBase {
        InMemoryBase::from_files(files.iter().map(|(p, d)| ((*p).to_string(), (*d).to_vec())))
    }

    fn overlay(files: &[(&str, &[u8])]) -> OverlayFs<InMemoryBase> {
        OverlayFs::with_base(base_with(files))
    }

    fn read_path<B: LazyBase>(fs: &mut OverlayFs<B>, path: &str) -> Result<Vec<u8>, FsError> {
        let h = fs.open(path, OpenOptions::read())?;
        let data = fs.read_file(h);
        fs.close(h);
        data
    }

    fn write_path<B: LazyBase>(fs: &mut OverlayFs<B>, path: &str, data: &[u8]) {
        let h = fs.open(path, OpenOptions::write()).unwrap();
        fs.write_file(h, data).unwrap();
        fs.close(h);
    }

    #[test]
    fn empty_base_behaves_like_memfs() {
        let mut fs = OverlayFs::<InMemoryBase>::new();
        assert!(fs.stat("/").unwrap().is_dir);
        assert!(matches!(fs.stat("/nope"), Err(FsError::NotFound(_))));
        write_path(&mut fs, "/a.txt", b"hello");
        assert_eq!(read_path(&mut fs, "/a.txt").unwrap(), b"hello");
        assert!(fs.read_dir("/").unwrap().iter().any(|e| e.name == "a.txt"));
    }

    #[test]
    fn reads_through_to_base() {
        let mut fs = overlay(&[("/seed.txt", b"from base")]);
        assert_eq!(read_path(&mut fs, "/seed.txt").unwrap(), b"from base");
        let meta = fs.stat("/seed.txt").unwrap();
        assert!(!meta.is_dir);
        assert_eq!(meta.size, b"from base".len() as u64);
    }

    #[test]
    fn read_through_does_not_materialize() {
        let mut fs = overlay(&[("/seed.txt", b"from base")]);
        let _ = read_path(&mut fs, "/seed.txt").unwrap();
        // Nothing was recorded as changed by a pure read.
        let log = fs.change_log().unwrap().clone();
        assert!(log.take().is_empty());
        // Upper layer has no private copy yet.
        assert!(fs.upper.stat("/seed.txt").is_err());
    }

    #[test]
    fn write_materializes_and_isolates_base() {
        let base = base_with(&[("/seed.txt", b"original")]);
        let mut fs = OverlayFs::with_base(base.clone());
        write_path(&mut fs, "/seed.txt", b"changed");
        assert_eq!(read_path(&mut fs, "/seed.txt").unwrap(), b"changed");
        // Base is untouched.
        assert_eq!(base.read("/seed.txt").unwrap().as_ref(), b"original");
        // Upper now holds a private copy.
        assert!(fs.upper.stat("/seed.txt").is_ok());
    }

    #[test]
    fn append_materializes_base_content() {
        let mut fs = overlay(&[("/log.txt", b"line1\n")]);
        let h = fs.open("/log.txt", OpenOptions::append()).unwrap();
        fs.write_file(h, b"line2\n").unwrap();
        fs.close(h);
        assert_eq!(read_path(&mut fs, "/log.txt").unwrap(), b"line1\nline2\n");
    }

    #[test]
    fn truncating_open_discards_base_content() {
        let mut fs = overlay(&[("/f.txt", b"old content")]);
        write_path(&mut fs, "/f.txt", b"new");
        assert_eq!(read_path(&mut fs, "/f.txt").unwrap(), b"new");
    }

    #[test]
    fn remove_base_file_records_whiteout() {
        let base = base_with(&[("/seed.txt", b"data")]);
        let mut fs = OverlayFs::with_base(base.clone());
        fs.remove_file("/seed.txt").unwrap();
        assert!(matches!(fs.stat("/seed.txt"), Err(FsError::NotFound(_))));
        assert!(matches!(
            read_path(&mut fs, "/seed.txt"),
            Err(FsError::NotFound(_))
        ));
        // Base entry still exists, just hidden.
        assert!(base.stat("/seed.txt").is_ok());
    }

    #[test]
    fn remove_then_recreate_starts_empty() {
        let mut fs = overlay(&[("/seed.txt", b"base data")]);
        fs.remove_file("/seed.txt").unwrap();
        let h = fs.open("/seed.txt", OpenOptions::append()).unwrap();
        fs.write_file(h, b"fresh").unwrap();
        fs.close(h);
        assert_eq!(read_path(&mut fs, "/seed.txt").unwrap(), b"fresh");
    }

    #[test]
    fn read_dir_merges_base_and_upper() {
        let mut fs = overlay(&[("/a.txt", b"a"), ("/b.txt", b"b")]);
        write_path(&mut fs, "/c.txt", b"c");
        let names: Vec<String> = fs
            .read_dir("/")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn read_dir_hides_whiteouts() {
        let mut fs = overlay(&[("/a.txt", b"a"), ("/b.txt", b"b")]);
        fs.remove_file("/a.txt").unwrap();
        let names: Vec<String> = fs
            .read_dir("/")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["b.txt"]);
    }

    #[test]
    fn read_dir_dedupes_materialized_file() {
        let mut fs = overlay(&[("/a.txt", b"base")]);
        write_path(&mut fs, "/a.txt", b"upper");
        let entries = fs.read_dir("/").unwrap();
        assert_eq!(entries.iter().filter(|e| e.name == "a.txt").count(), 1);
    }

    #[test]
    fn nested_base_dirs_are_listable() {
        let mut fs = overlay(&[("/dir/sub/file.txt", b"deep")]);
        assert!(fs.stat("/dir").unwrap().is_dir);
        assert!(fs.stat("/dir/sub").unwrap().is_dir);
        let names: Vec<String> = fs
            .read_dir("/dir")
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["sub"]);
        assert_eq!(read_path(&mut fs, "/dir/sub/file.txt").unwrap(), b"deep");
    }

    #[test]
    fn create_dir_over_existing_base_dir_fails() {
        let mut fs = overlay(&[("/dir/file.txt", b"x")]);
        assert!(matches!(
            fs.create_dir("/dir"),
            Err(FsError::AlreadyExists(_))
        ));
    }

    #[test]
    fn remove_base_dir_requires_empty_then_whiteouts() {
        let mut fs = overlay(&[("/dir/file.txt", b"x")]);
        assert!(fs.remove_dir("/dir").is_err());
        fs.remove_file("/dir/file.txt").unwrap();
        fs.remove_dir("/dir").unwrap();
        assert!(matches!(fs.stat("/dir"), Err(FsError::NotFound(_))));
    }

    #[test]
    fn change_log_forwards_for_base_only_removal() {
        let mut fs = overlay(&[("/seed.txt", b"data")]);
        let log = fs.change_log().unwrap().clone();
        fs.remove_file("/seed.txt").unwrap();
        assert_eq!(log.take(), vec!["/seed.txt"]);
    }

    #[test]
    fn change_log_records_cow_write() {
        let mut fs = overlay(&[("/seed.txt", b"data")]);
        let log = fs.change_log().unwrap().clone();
        write_path(&mut fs, "/seed.txt", b"new");
        assert_eq!(log.take(), vec!["/seed.txt"]);
    }

    #[test]
    fn open_directory_for_read_errors() {
        let fs = overlay(&[("/dir/file.txt", b"x")]);
        let mut fs = fs;
        assert!(matches!(
            fs.open("/dir", OpenOptions::read()),
            Err(FsError::IsADirectory(_))
        ));
    }

    #[test]
    fn clone_shares_state() {
        let mut fs = overlay(&[("/seed.txt", b"data")]);
        let mut clone = fs.clone();
        write_path(&mut fs, "/new.txt", b"x");
        // Shallow clone: the write is visible through the clone.
        assert_eq!(read_path(&mut clone, "/new.txt").unwrap(), b"x");
        // Whiteouts are shared too.
        fs.remove_file("/seed.txt").unwrap();
        assert!(matches!(clone.stat("/seed.txt"), Err(FsError::NotFound(_))));
    }

    #[test]
    fn mv_over_overlay_via_primitives() {
        // Emulate `mv /seed.txt /dest.txt`: copy contents, then remove source.
        let mut fs = overlay(&[("/seed.txt", b"payload")]);
        let contents = read_path(&mut fs, "/seed.txt").unwrap();
        write_path(&mut fs, "/dest.txt", &contents);
        fs.remove_file("/seed.txt").unwrap();
        assert_eq!(read_path(&mut fs, "/dest.txt").unwrap(), b"payload");
        assert!(matches!(fs.stat("/seed.txt"), Err(FsError::NotFound(_))));
    }

    #[test]
    fn replace_base_clears_whiteouts() {
        let mut fs = overlay(&[("/seed.txt", b"data")]);
        fs.remove_file("/seed.txt").unwrap();
        fs.replace_base(base_with(&[("/other.txt", b"x")]));
        assert!(matches!(fs.stat("/seed.txt"), Err(FsError::NotFound(_))));
        assert_eq!(read_path(&mut fs, "/other.txt").unwrap(), b"x");
    }

    /// Which read-only operation a [`CountingBase`] observed, for laziness
    /// assertions.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Access {
        Stat,
        Read,
        ReadDir,
    }

    /// A [`LazyBase`] that records every access so tests can assert *when* (and
    /// whether) the base layer was consulted. Clones share the access log.
    #[derive(Clone, Default)]
    struct CountingBase {
        inner: InMemoryBase,
        log: Rc<RefCell<Vec<(Access, String)>>>,
    }

    impl CountingBase {
        fn new(files: &[(&str, &[u8])]) -> Self {
            Self {
                inner: base_with(files),
                log: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn count(&self, access: Access, path: &str) -> usize {
            self.log
                .borrow()
                .iter()
                .filter(|(a, p)| *a == access && p.as_str() == path)
                .count()
        }

        fn total(&self, path: &str) -> usize {
            self.log
                .borrow()
                .iter()
                .filter(|(_, p)| p.as_str() == path)
                .count()
        }
    }

    impl LazyBase for CountingBase {
        fn stat(&self, path: &str) -> Result<Metadata, FsError> {
            self.log
                .borrow_mut()
                .push((Access::Stat, normalize_path(path)));
            self.inner.stat(path)
        }

        fn read(&self, path: &str) -> Result<Arc<[u8]>, FsError> {
            self.log
                .borrow_mut()
                .push((Access::Read, normalize_path(path)));
            self.inner.read(path)
        }

        fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
            self.log
                .borrow_mut()
                .push((Access::ReadDir, normalize_path(path)));
            self.inner.read_dir(path)
        }
    }

    #[test]
    fn mounting_does_not_touch_base() {
        let base = CountingBase::new(&[("/a.txt", b"a"), ("/b.txt", b"b")]);
        let probe = base.clone();
        let _fs = OverlayFs::with_base(base);
        // Constructing the overlay must not eagerly read or enumerate the base.
        assert!(probe.log.borrow().is_empty());
    }

    #[test]
    fn untouched_base_file_is_never_accessed() {
        let base = CountingBase::new(&[("/a.txt", b"a"), ("/b.txt", b"b")]);
        let probe = base.clone();
        let mut fs = OverlayFs::with_base(base);

        let _ = read_path(&mut fs, "/a.txt").unwrap();

        // Per-path, on-demand resolution: the file we never touched is never
        // read or stat'd in the base.
        assert_eq!(probe.total("/b.txt"), 0);
        assert!(probe.count(Access::Read, "/a.txt") >= 1);
    }

    #[test]
    fn repeated_reads_never_materialize() {
        let base = CountingBase::new(&[("/seed.txt", b"data")]);
        let probe = base.clone();
        let mut fs = OverlayFs::with_base(base);

        let _ = read_path(&mut fs, "/seed.txt").unwrap();
        let _ = read_path(&mut fs, "/seed.txt").unwrap();

        // Each read is served from the base (no copy into the upper layer), so
        // the base is read both times and the upper layer stays empty.
        assert_eq!(probe.count(Access::Read, "/seed.txt"), 2);
        assert!(fs.upper.stat("/seed.txt").is_err());
    }

    #[test]
    fn first_write_copies_base_once_then_serves_from_upper() {
        let base = CountingBase::new(&[("/seed.txt", b"base")]);
        let probe = base.clone();
        let mut fs = OverlayFs::with_base(base);

        // Append is a copy-on-write trigger: the base is read exactly once to
        // materialize it into the upper layer.
        let h = fs.open("/seed.txt", OpenOptions::append()).unwrap();
        fs.write_file(h, b"!").unwrap();
        fs.close(h);
        assert_eq!(probe.count(Access::Read, "/seed.txt"), 1);

        // After materialization, reads are served from the upper layer and the
        // base is never consulted again.
        let _ = read_path(&mut fs, "/seed.txt").unwrap();
        assert_eq!(probe.count(Access::Read, "/seed.txt"), 1);
    }

    #[test]
    fn truncating_write_never_reads_base() {
        let base = CountingBase::new(&[("/seed.txt", b"base")]);
        let probe = base.clone();
        let mut fs = OverlayFs::with_base(base);

        // A full-truncate open discards the base content, so it is never read.
        write_path(&mut fs, "/seed.txt", b"new");
        assert_eq!(probe.count(Access::Read, "/seed.txt"), 0);
    }

    #[test]
    fn whiteout_short_circuits_base_lookups() {
        let base = CountingBase::new(&[("/seed.txt", b"data")]);
        let probe = base.clone();
        let mut fs = OverlayFs::with_base(base);

        fs.remove_file("/seed.txt").unwrap();
        let after_delete = probe.total("/seed.txt");

        let _ = fs.stat("/seed.txt");
        let _ = read_path(&mut fs, "/seed.txt");

        // Once a path is whiteouted, resolution returns NotFound before ever
        // consulting the base again.
        assert_eq!(probe.total("/seed.txt"), after_delete);
    }
}
