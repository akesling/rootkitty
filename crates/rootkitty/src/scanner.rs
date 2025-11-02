use anyhow::Result;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub parent_path: Option<PathBuf>,
    pub size: u64,
    pub is_dir: bool,
    pub modified_at: Option<DateTime<Utc>>,
    pub depth: usize,
}

#[derive(Debug, Clone)]
pub struct ScanStats {
    pub total_size: u64,
    pub total_files: u64,
    pub total_dirs: u64,
}

const BUFFER_SIZE: usize = 1000;

pub struct Scanner {
    root_path: PathBuf,
    entries: Arc<Mutex<Vec<FileEntry>>>,
    stats: Arc<ScanStats>,
    sender: Option<mpsc::Sender<crate::db::ActorMessage>>,
}

impl Scanner {
    pub fn new<P: AsRef<Path>>(root_path: P) -> Self {
        Self {
            root_path: root_path.as_ref().to_path_buf(),
            entries: Arc::new(Mutex::new(Vec::new())),
            stats: Arc::new(ScanStats {
                total_size: 0,
                total_files: 0,
                total_dirs: 0,
            }),
            sender: None,
        }
    }

    pub fn with_sender<P: AsRef<Path>>(
        root_path: P,
        sender: mpsc::Sender<crate::db::ActorMessage>,
    ) -> Self {
        Self {
            root_path: root_path.as_ref().to_path_buf(),
            entries: Arc::new(Mutex::new(Vec::new())),
            stats: Arc::new(ScanStats {
                total_size: 0,
                total_files: 0,
                total_dirs: 0,
            }),
            sender: Some(sender),
        }
    }

    pub fn scan(&self) -> Result<(Vec<FileEntry>, ScanStats)> {
        let total_size = AtomicU64::new(0);
        let total_files = AtomicU64::new(0);
        let total_dirs = AtomicU64::new(0);

        self.scan_recursive(&self.root_path, 0, &total_size, &total_files, &total_dirs)?;

        // Final flush of any remaining buffered entries
        self.flush_buffer()?;

        let entries = if self.sender.is_some() {
            // If streaming, we already sent everything, return empty vec
            Vec::new()
        } else {
            // If not streaming, return collected entries
            self.entries.lock().unwrap().clone()
        };

        let stats = ScanStats {
            total_size: total_size.load(Ordering::Relaxed),
            total_files: total_files.load(Ordering::Relaxed),
            total_dirs: total_dirs.load(Ordering::Relaxed),
        };

        Ok((entries, stats))
    }

    fn flush_buffer(&self) -> Result<()> {
        if let Some(sender) = &self.sender {
            let mut entries = self.entries.lock().unwrap();
            if !entries.is_empty() {
                let batch = entries.drain(..).collect();
                sender.blocking_send(crate::db::ActorMessage::InsertBatch(batch))?;
            }
        }
        Ok(())
    }

    fn scan_recursive(
        &self,
        path: &Path,
        depth: usize,
        total_size: &AtomicU64,
        total_files: &AtomicU64,
        total_dirs: &AtomicU64,
    ) -> Result<u64> {
        let metadata = match fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return Ok(0), // Skip inaccessible files
        };

        let modified_at = metadata.modified().ok().and_then(|t| {
            DateTime::from_timestamp(
                t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64,
                0,
            )
        });

        let is_dir = metadata.is_dir();
        let file_size = if is_dir { 0 } else { metadata.len() };

        let parent_path = path.parent().map(|p| p.to_path_buf());
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let mut dir_size = file_size;

        if is_dir {
            total_dirs.fetch_add(1, Ordering::Relaxed);

            // Read directory entries
            let read_dir = match fs::read_dir(path) {
                Ok(rd) => rd,
                Err(_) => {
                    // Still record the directory even if we can't read it
                    self.add_entry(FileEntry {
                        path: path.to_path_buf(),
                        name,
                        parent_path,
                        size: 0,
                        is_dir: true,
                        modified_at,
                        depth,
                    });
                    return Ok(0);
                }
            };

            let children: Vec<_> = read_dir.filter_map(|e| e.ok()).map(|e| e.path()).collect();

            // For small directories, process serially; for large ones, use parallelism
            if children.len() > 100 {
                let child_sizes: Vec<u64> = children
                    .par_iter()
                    .filter_map(|child_path| {
                        self.scan_recursive(
                            child_path,
                            depth + 1,
                            total_size,
                            total_files,
                            total_dirs,
                        )
                        .ok()
                    })
                    .collect();
                dir_size = child_sizes.iter().sum();
            } else {
                for child_path in children {
                    if let Ok(size) = self.scan_recursive(
                        &child_path,
                        depth + 1,
                        total_size,
                        total_files,
                        total_dirs,
                    ) {
                        dir_size += size;
                    }
                }
            }
        } else {
            total_files.fetch_add(1, Ordering::Relaxed);
            total_size.fetch_add(file_size, Ordering::Relaxed);
        }

        self.add_entry(FileEntry {
            path: path.to_path_buf(),
            name,
            parent_path,
            size: dir_size,
            is_dir,
            modified_at,
            depth,
        });

        Ok(dir_size)
    }

    fn add_entry(&self, entry: FileEntry) {
        let should_flush = {
            let mut entries = self.entries.lock().unwrap();
            entries.push(entry);
            self.sender.is_some() && entries.len() >= BUFFER_SIZE
        };

        if should_flush {
            // Flush outside the lock to avoid holding it during send
            let _ = self.flush_buffer();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_basic_scan() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        fs::write(root.join("file1.txt"), b"hello").unwrap();
        fs::write(root.join("file2.txt"), b"world").unwrap();
        fs::create_dir(root.join("subdir")).unwrap();
        fs::write(root.join("subdir/file3.txt"), b"test").unwrap();

        let scanner = Scanner::new(root);
        let (entries, stats) = scanner.scan().unwrap();

        assert!(stats.total_files >= 3);
        assert!(stats.total_dirs >= 2);
        assert!(stats.total_size >= 14);
        assert!(!entries.is_empty());
    }
}
