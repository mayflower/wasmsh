//! In-memory filesystem implementation.

use std::collections::HashMap;

use crate::{DirEntry, FileHandle, FsError, Metadata, OpenOptions, Vfs};

/// An entry in the memory filesystem.
#[derive(Debug, Clone)]
enum FsNode {
    File(Vec<u8>),
    Dir,
}

/// Open file state.
#[derive(Debug)]
struct OpenFile {
    path: String,
    opts: OpenOptions,
    /// Byte offset for future seek support.
    _cursor: usize,
}

/// In-memory virtual filesystem. No persistence, no `std::fs`.
#[derive(Debug)]
pub struct MemoryFs {
    nodes: HashMap<String, FsNode>,
    handles: HashMap<u64, OpenFile>,
    next_handle: u64,
}

impl MemoryFs {
    /// Create a new empty filesystem with a root directory.
    #[must_use]
    pub fn new() -> Self {
        let mut nodes = HashMap::new();
        nodes.insert("/".to_string(), FsNode::Dir);
        Self {
            nodes,
            handles: HashMap::new(),
            next_handle: 1,
        }
    }

    fn alloc_handle(&mut self) -> u64 {
        let h = self.next_handle;
        self.next_handle += 1;
        h
    }

    /// Ensure all parent directories exist for a given path.
    fn ensure_parents(&mut self, path: &str) {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut current = String::new();
        for part in &parts[..parts.len().saturating_sub(1)] {
            current.push('/');
            current.push_str(part);
            self.nodes
                .entry(current.clone())
                .or_insert(FsNode::Dir);
        }
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

        match self.nodes.get(&norm) {
            Some(FsNode::Dir) => {
                return Err(FsError::IsADirectory(norm));
            }
            Some(FsNode::File(_)) => {
                if opts.write && opts.truncate && !opts.append {
                    self.nodes.insert(norm.clone(), FsNode::File(Vec::new()));
                }
            }
            None => {
                if opts.create {
                    self.ensure_parents(&norm);
                    self.nodes.insert(norm.clone(), FsNode::File(Vec::new()));
                } else {
                    return Err(FsError::NotFound(norm));
                }
            }
        }

        let h = self.alloc_handle();
        self.handles.insert(
            h,
            OpenFile {
                path: norm,
                opts,
                _cursor: 0,
            },
        );
        Ok(FileHandle(h))
    }

    fn read_file(&self, handle: FileHandle) -> Result<Vec<u8>, FsError> {
        let of = self
            .handles
            .get(&handle.0)
            .ok_or_else(|| FsError::Io("invalid handle".into()))?;
        if !of.opts.read {
            return Err(FsError::PermissionDenied("not opened for reading".into()));
        }
        match self.nodes.get(&of.path) {
            Some(FsNode::File(data)) => Ok(data.clone()),
            _ => Err(FsError::NotFound(of.path.clone())),
        }
    }

    fn write_file(&mut self, handle: FileHandle, data: &[u8]) -> Result<(), FsError> {
        let of = self
            .handles
            .get(&handle.0)
            .ok_or_else(|| FsError::Io("invalid handle".into()))?;
        if !of.opts.write {
            return Err(FsError::PermissionDenied("not opened for writing".into()));
        }
        let path = of.path.clone();
        let append = of.opts.append;

        match self.nodes.get_mut(&path) {
            Some(FsNode::File(contents)) => {
                if append {
                    contents.extend_from_slice(data);
                } else {
                    *contents = data.to_vec();
                }
                Ok(())
            }
            _ => Err(FsError::NotFound(path)),
        }
    }

    fn close(&mut self, handle: FileHandle) {
        self.handles.remove(&handle.0);
    }

    fn stat(&self, path: &str) -> Result<Metadata, FsError> {
        let norm = crate::normalize_path(path);
        match self.nodes.get(&norm) {
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
        match self.nodes.get(&norm) {
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
        for (k, v) in &self.nodes {
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
        if self.nodes.contains_key(&norm) {
            return Err(FsError::AlreadyExists(norm));
        }
        self.ensure_parents(&norm);
        self.nodes.insert(norm, FsNode::Dir);
        Ok(())
    }

    fn remove_file(&mut self, path: &str) -> Result<(), FsError> {
        let norm = crate::normalize_path(path);
        match self.nodes.get(&norm) {
            Some(FsNode::Dir) => Err(FsError::IsADirectory(norm)),
            Some(FsNode::File(_)) => {
                self.nodes.remove(&norm);
                Ok(())
            }
            None => Err(FsError::NotFound(norm)),
        }
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), FsError> {
        let norm = crate::normalize_path(path);
        match self.nodes.get(&norm) {
            Some(FsNode::File(_)) => Err(FsError::NotADirectory(norm)),
            Some(FsNode::Dir) => {
                // Check if directory is empty
                let prefix = if norm == "/" {
                    "/".to_string()
                } else {
                    format!("{norm}/")
                };
                let has_children = self.nodes.keys().any(|k| k.starts_with(&prefix));
                if has_children {
                    return Err(FsError::Io(format!("directory not empty: {norm}")));
                }
                self.nodes.remove(&norm);
                Ok(())
            }
            None => Err(FsError::NotFound(norm)),
        }
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
        assert!(matches!(
            fs.stat("/nope"),
            Err(FsError::NotFound(_))
        ));
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
}
