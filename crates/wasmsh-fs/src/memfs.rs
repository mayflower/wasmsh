//! In-memory filesystem implementation.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::rc::Rc;
use std::sync::Arc;

use crate::{DirEntry, FileHandle, FsError, Metadata, OpenOptions, Vfs, VfsWriteSink};

/// Maximum file size (64 MiB).
const MAX_FILE_SIZE: usize = 64 * 1024 * 1024;

/// An entry in the memory filesystem.
#[derive(Debug, Clone)]
enum FsNode {
    File(Arc<[u8]>),
    Dir,
}

enum OpenFileSource {
    Path(String),
    Virtual(Rc<RefCell<Box<dyn Read>>>),
}

struct OpenFile {
    source: OpenFileSource,
    opts: OpenOptions,
}

impl std::fmt::Debug for OpenFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source = match &self.source {
            OpenFileSource::Path(path) => path.as_str(),
            OpenFileSource::Virtual(_) => "<virtual>",
        };
        f.debug_struct("OpenFile")
            .field("source", &source)
            .field("opts", &self.opts)
            .finish()
    }
}

struct SharedReadHandle {
    reader: Rc<RefCell<Box<dyn Read>>>,
}

enum EitherReadSource {
    Path(String),
    Virtual(Rc<RefCell<Box<dyn Read>>>),
}

impl Read for SharedReadHandle {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.borrow_mut().read(buf)
    }
}

struct MemoryFsInner {
    nodes: HashMap<String, FsNode>,
    virtual_readers: HashMap<String, Rc<RefCell<Box<dyn Read>>>>,
    handles: HashMap<u64, OpenFile>,
    next_handle: u64,
}

impl std::fmt::Debug for MemoryFsInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryFsInner")
            .field("nodes", &self.nodes)
            .field("virtual_reader_count", &self.virtual_readers.len())
            .field("handles", &self.handles)
            .field("next_handle", &self.next_handle)
            .finish()
    }
}

/// In-memory virtual filesystem. No persistence, no `std::fs`.
#[derive(Debug, Clone)]
pub struct MemoryFs {
    inner: Rc<RefCell<MemoryFsInner>>,
}

struct MemoryWriteSink {
    inner: Rc<RefCell<MemoryFsInner>>,
    path: String,
    append: bool,
}

impl MemoryFs {
    /// Create a new empty filesystem with a root directory.
    #[must_use]
    pub fn new() -> Self {
        let mut nodes = HashMap::new();
        nodes.insert("/".to_string(), FsNode::Dir);
        Self {
            inner: Rc::new(RefCell::new(MemoryFsInner {
                nodes,
                virtual_readers: HashMap::new(),
                handles: HashMap::new(),
                next_handle: 1,
            })),
        }
    }

    fn alloc_handle(&mut self) -> u64 {
        let mut inner = self.inner.borrow_mut();
        let h = inner.next_handle;
        inner.next_handle = inner
            .next_handle
            .checked_add(1)
            .expect("file handle counter overflow");
        h
    }

    /// Ensure all parent directories exist for a given path.
    fn ensure_parents(&mut self, path: &str) {
        let mut inner = self.inner.borrow_mut();
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut current = String::new();
        for part in &parts[..parts.len().saturating_sub(1)] {
            current.push('/');
            current.push_str(part);
            inner.nodes.entry(current.clone()).or_insert(FsNode::Dir);
        }
    }
}

impl MemoryWriteSink {
    fn write_chunk(&mut self, data: &[u8]) -> Result<(), FsError> {
        let mut inner = self.inner.borrow_mut();
        match inner.nodes.get_mut(&self.path) {
            Some(FsNode::File(contents)) => {
                let new_size = if self.append {
                    contents.len() + data.len()
                } else {
                    data.len()
                };
                if new_size > MAX_FILE_SIZE {
                    return Err(FsError::Io("file size limit exceeded".into()));
                }
                if self.append {
                    let mut combined = contents.as_ref().to_vec();
                    combined.extend_from_slice(data);
                    *contents = Arc::from(combined);
                } else {
                    *contents = Arc::from(data.to_vec());
                    self.append = true;
                }
                Ok(())
            }
            _ => Err(FsError::NotFound(self.path.clone())),
        }
    }
}

impl VfsWriteSink for MemoryWriteSink {
    fn write(&mut self, data: &[u8]) -> Result<(), FsError> {
        self.write_chunk(data)
    }
}

impl Default for MemoryFs {
    fn default() -> Self {
        Self::new()
    }
}

impl Vfs for MemoryFs {
    fn open(&mut self, path: &str, opts: OpenOptions) -> Result<FileHandle, FsError> {
        let norm = crate::normalize_path(path);
        let mut inner = self.inner.borrow_mut();

        if opts.read && !opts.write && !opts.append && !opts.create && !opts.truncate {
            if let Some(reader) = inner.virtual_readers.remove(&norm) {
                inner.nodes.remove(&norm);
                drop(inner);
                let h = self.alloc_handle();
                self.inner.borrow_mut().handles.insert(
                    h,
                    OpenFile {
                        source: OpenFileSource::Virtual(reader),
                        opts,
                    },
                );
                return Ok(FileHandle(h));
            }
        }

        match inner.nodes.get(&norm) {
            Some(FsNode::Dir) => {
                return Err(FsError::IsADirectory(norm));
            }
            Some(FsNode::File(_)) => {
                if opts.write && opts.truncate && !opts.append {
                    inner
                        .nodes
                        .insert(norm.clone(), FsNode::File(Arc::from([])));
                }
            }
            None => {
                if opts.create {
                    drop(inner);
                    self.ensure_parents(&norm);
                    inner = self.inner.borrow_mut();
                    inner
                        .nodes
                        .insert(norm.clone(), FsNode::File(Arc::from([])));
                } else {
                    return Err(FsError::NotFound(norm));
                }
            }
        }
        drop(inner);

        let h = self.alloc_handle();
        self.inner.borrow_mut().handles.insert(
            h,
            OpenFile {
                source: OpenFileSource::Path(norm),
                opts,
            },
        );
        Ok(FileHandle(h))
    }

    fn read_file(&self, handle: FileHandle) -> Result<Vec<u8>, FsError> {
        let source = {
            let inner = self.inner.borrow();
            let of = inner
                .handles
                .get(&handle.0)
                .ok_or_else(|| FsError::Io("invalid handle".into()))?;
            if !of.opts.read {
                return Err(FsError::PermissionDenied("not opened for reading".into()));
            }
            match &of.source {
                OpenFileSource::Path(path) => EitherReadSource::Path(path.clone()),
                OpenFileSource::Virtual(reader) => EitherReadSource::Virtual(Rc::clone(reader)),
            }
        };
        match source {
            EitherReadSource::Path(path) => {
                let inner = self.inner.borrow();
                match inner.nodes.get(&path) {
                    Some(FsNode::File(data)) => Ok(data.as_ref().to_vec()),
                    _ => Err(FsError::NotFound(path)),
                }
            }
            EitherReadSource::Virtual(reader) => {
                let mut result = Vec::new();
                let mut reader = reader.borrow_mut();
                reader
                    .read_to_end(&mut result)
                    .map_err(|err| FsError::Io(err.to_string()))?;
                Ok(result)
            }
        }
    }

    fn stream_file(&self, handle: FileHandle) -> Result<Box<dyn Read>, FsError> {
        let source = {
            let inner = self.inner.borrow();
            let of = inner
                .handles
                .get(&handle.0)
                .ok_or_else(|| FsError::Io("invalid handle".into()))?;
            if !of.opts.read {
                return Err(FsError::PermissionDenied("not opened for reading".into()));
            }
            match &of.source {
                OpenFileSource::Path(path) => EitherReadSource::Path(path.clone()),
                OpenFileSource::Virtual(reader) => EitherReadSource::Virtual(Rc::clone(reader)),
            }
        };
        match source {
            EitherReadSource::Path(path) => {
                let inner = self.inner.borrow();
                match inner.nodes.get(&path) {
                    Some(FsNode::File(data)) => Ok(Box::new(Cursor::new(Arc::clone(data)))),
                    _ => Err(FsError::NotFound(path)),
                }
            }
            EitherReadSource::Virtual(reader) => Ok(Box::new(SharedReadHandle { reader })),
        }
    }

    fn write_file(&mut self, handle: FileHandle, data: &[u8]) -> Result<(), FsError> {
        let inner = self.inner.borrow();
        let of = inner
            .handles
            .get(&handle.0)
            .ok_or_else(|| FsError::Io("invalid handle".into()))?;
        if !of.opts.write {
            return Err(FsError::PermissionDenied("not opened for writing".into()));
        }
        let path = match &of.source {
            OpenFileSource::Path(path) => path.clone(),
            OpenFileSource::Virtual(_) => {
                return Err(FsError::PermissionDenied(
                    "virtual reader is read-only".into(),
                ))
            }
        };
        let append = of.opts.append;
        drop(inner);

        MemoryWriteSink {
            inner: Rc::clone(&self.inner),
            path,
            append,
        }
        .write_chunk(data)
    }

    fn open_write_sink(
        &mut self,
        path: &str,
        append: bool,
    ) -> Result<Box<dyn VfsWriteSink>, FsError> {
        let norm = crate::normalize_path(path);
        {
            let mut inner = self.inner.borrow_mut();
            inner.virtual_readers.remove(&norm);
            match inner.nodes.get(&norm) {
                Some(FsNode::Dir) => return Err(FsError::IsADirectory(norm)),
                Some(FsNode::File(_)) => {}
                None => {
                    drop(inner);
                    self.ensure_parents(&norm);
                    self.inner
                        .borrow_mut()
                        .nodes
                        .insert(norm.clone(), FsNode::File(Arc::from([])));
                }
            }
        }
        if !append {
            self.inner
                .borrow_mut()
                .nodes
                .insert(norm.clone(), FsNode::File(Arc::from([])));
        }
        Ok(Box::new(MemoryWriteSink {
            inner: Rc::clone(&self.inner),
            path: norm,
            append,
        }))
    }

    fn install_stream_reader(&mut self, path: &str, reader: Box<dyn Read>) -> Result<(), FsError> {
        let norm = crate::normalize_path(path);
        {
            let inner = self.inner.borrow();
            if matches!(inner.nodes.get(&norm), Some(FsNode::Dir)) {
                return Err(FsError::IsADirectory(norm));
            }
        }
        self.ensure_parents(&norm);
        let mut inner = self.inner.borrow_mut();
        inner
            .nodes
            .insert(norm.clone(), FsNode::File(Arc::from([])));
        inner
            .virtual_readers
            .insert(norm, Rc::new(RefCell::new(reader)));
        Ok(())
    }

    fn close(&mut self, handle: FileHandle) {
        self.inner.borrow_mut().handles.remove(&handle.0);
    }

    fn stat(&self, path: &str) -> Result<Metadata, FsError> {
        let norm = crate::normalize_path(path);
        match self.inner.borrow().nodes.get(&norm) {
            Some(FsNode::File(data)) => Ok(Metadata {
                is_dir: false,
                size: data.len() as u64,
            }),
            Some(FsNode::Dir) => Ok(Metadata {
                is_dir: true,
                size: 0,
            }),
            None => Err(FsError::NotFound(norm)),
        }
    }

    fn read_dir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let norm = crate::normalize_path(path);
        let inner = self.inner.borrow();
        match inner.nodes.get(&norm) {
            Some(FsNode::Dir) => {}
            Some(FsNode::File(_)) => return Err(FsError::NotADirectory(norm)),
            None => return Err(FsError::NotFound(norm)),
        }

        let prefix = if norm == "/" {
            "/".to_string()
        } else {
            format!("{norm}/")
        };

        let mut entries = Vec::new();
        for (k, v) in &inner.nodes {
            if let Some(rest) = k.strip_prefix(&prefix) {
                if !rest.contains('/') && !rest.is_empty() {
                    entries.push(DirEntry {
                        name: rest.to_string(),
                        is_dir: matches!(v, FsNode::Dir),
                    });
                }
            }
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn create_dir(&mut self, path: &str) -> Result<(), FsError> {
        let norm = crate::normalize_path(path);
        if self.inner.borrow().nodes.contains_key(&norm) {
            return Err(FsError::AlreadyExists(norm));
        }
        self.ensure_parents(&norm);
        self.inner.borrow_mut().nodes.insert(norm, FsNode::Dir);
        Ok(())
    }

    fn remove_file(&mut self, path: &str) -> Result<(), FsError> {
        let norm = crate::normalize_path(path);
        let kind = {
            let inner = self.inner.borrow();
            inner.nodes.get(&norm).cloned()
        };
        match kind {
            Some(FsNode::Dir) => Err(FsError::IsADirectory(norm)),
            Some(FsNode::File(_)) => {
                let mut inner = self.inner.borrow_mut();
                inner.nodes.remove(&norm);
                inner.virtual_readers.remove(&norm);
                Ok(())
            }
            None => Err(FsError::NotFound(norm)),
        }
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), FsError> {
        let norm = crate::normalize_path(path);
        {
            let inner = self.inner.borrow();
            match inner.nodes.get(&norm) {
                Some(FsNode::File(_)) => return Err(FsError::NotADirectory(norm)),
                Some(FsNode::Dir) => {}
                None => return Err(FsError::NotFound(norm)),
            }
        }
        let prefix = if norm == "/" {
            "/".to_string()
        } else {
            format!("{norm}/")
        };
        let has_children = {
            let inner = self.inner.borrow();
            inner.nodes.keys().any(|k| k.starts_with(&prefix))
        };
        if has_children {
            return Err(FsError::Io(format!("directory not empty: {norm}")));
        }
        self.inner.borrow_mut().nodes.remove(&norm);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_read_file() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/hello.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello world").unwrap();
        fs.close(h);

        let h = fs.open("/hello.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        assert_eq!(data, b"hello world");
    }

    #[test]
    fn stream_file_reads_incrementally() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/hello.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"hello world").unwrap();
        fs.close(h);

        let h = fs.open("/hello.txt", OpenOptions::read()).unwrap();
        let mut reader = fs.stream_file(h).unwrap();
        let mut buf = [0u8; 5];
        assert_eq!(reader.read(&mut buf).unwrap(), 5);
        assert_eq!(&buf, b"hello");
        assert_eq!(reader.read(&mut buf).unwrap(), 5);
        assert_eq!(&buf, b" worl");
        assert_eq!(reader.read(&mut buf).unwrap(), 1);
        assert_eq!(buf[0], b'd');
        fs.close(h);
    }

    #[test]
    fn install_stream_reader_registers_single_consumer_path() {
        let mut fs = MemoryFs::new();
        fs.install_stream_reader("/stream.txt", Box::new(Cursor::new(b"hello".to_vec())))
            .unwrap();

        let h = fs.open("/stream.txt", OpenOptions::read()).unwrap();
        let mut reader = fs.stream_file(h).unwrap();
        let mut data = Vec::new();
        reader.read_to_end(&mut data).unwrap();
        assert_eq!(data, b"hello");
        fs.close(h);

        assert!(matches!(
            fs.open("/stream.txt", OpenOptions::read()),
            Err(FsError::NotFound(_))
        ));
    }

    #[test]
    fn append_to_file() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/log.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"line1\n").unwrap();
        fs.close(h);

        let h = fs.open("/log.txt", OpenOptions::append()).unwrap();
        fs.write_file(h, b"line2\n").unwrap();
        fs.close(h);

        let h = fs.open("/log.txt", OpenOptions::read()).unwrap();
        let data = fs.read_file(h).unwrap();
        assert_eq!(data, b"line1\nline2\n");
    }

    #[test]
    fn truncate_on_write() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/f.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"original").unwrap();
        fs.close(h);

        let h = fs.open("/f.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"new").unwrap();
        fs.close(h);

        let h = fs.open("/f.txt", OpenOptions::read()).unwrap();
        assert_eq!(fs.read_file(h).unwrap(), b"new");
    }

    #[test]
    fn stat_file_and_dir() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/test.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"abc").unwrap();
        fs.close(h);

        let meta = fs.stat("/test.txt").unwrap();
        assert!(!meta.is_dir);
        assert_eq!(meta.size, 3);

        let meta = fs.stat("/").unwrap();
        assert!(meta.is_dir);
    }

    #[test]
    fn create_and_list_dir() {
        let mut fs = MemoryFs::new();
        fs.create_dir("/mydir").unwrap();
        let h = fs.open("/mydir/file.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"").unwrap();
        fs.close(h);

        let entries = fs.read_dir("/mydir").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "file.txt");
    }

    #[test]
    fn remove_file_and_dir() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/tmp.txt", OpenOptions::write()).unwrap();
        fs.close(h);
        fs.remove_file("/tmp.txt").unwrap();
        assert!(fs.stat("/tmp.txt").is_err());

        fs.create_dir("/empty").unwrap();
        fs.remove_dir("/empty").unwrap();
        assert!(fs.stat("/empty").is_err());
    }

    #[test]
    fn not_found_errors() {
        let fs = MemoryFs::new();
        assert!(matches!(fs.stat("/nope"), Err(FsError::NotFound(_))));
    }

    #[test]
    fn auto_create_parents() {
        let mut fs = MemoryFs::new();
        let h = fs.open("/a/b/c/file.txt", OpenOptions::write()).unwrap();
        fs.write_file(h, b"deep").unwrap();
        fs.close(h);
        assert!(fs.stat("/a").unwrap().is_dir);
        assert!(fs.stat("/a/b").unwrap().is_dir);
        assert!(fs.stat("/a/b/c").unwrap().is_dir);
    }

    #[test]
    fn write_sink_streams_incrementally() {
        let mut fs = MemoryFs::new();
        let mut sink = fs.open_write_sink("/log.txt", false).unwrap();
        sink.write(b"line1\n").unwrap();
        sink.write(b"line2\n").unwrap();

        let h = fs.open("/log.txt", OpenOptions::read()).unwrap();
        assert_eq!(fs.read_file(h).unwrap(), b"line1\nline2\n");
    }
}
