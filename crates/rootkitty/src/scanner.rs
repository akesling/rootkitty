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

#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    pub files_scanned: u64,
    pub dirs_scanned: u64,
    pub total_size: u64,
    pub current_path: String,
}

const BUFFER_SIZE: usize = 1000;
const PROGRESS_UPDATE_INTERVAL: u64 = 100; // Send progress every N entries

pub struct Scanner {
    root_path: PathBuf,
    entries: Arc<Mutex<Vec<FileEntry>>>,
    stats: Arc<ScanStats>,
    sender: Option<mpsc::Sender<crate::db::ActorMessage>>,
    progress_sender: Option<mpsc::UnboundedSender<ProgressUpdate>>,
    entries_processed: Arc<AtomicU64>,
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
            progress_sender: None,
            entries_processed: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn with_sender<P: AsRef<Path>>(
        root_path: P,
        sender: mpsc::Sender<crate::db::ActorMessage>,
        progress_sender: Option<mpsc::UnboundedSender<ProgressUpdate>>,
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
            progress_sender,
            entries_processed: Arc::new(AtomicU64::new(0)),
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
            entries.push(entry.clone());
            self.sender.is_some() && entries.len() >= BUFFER_SIZE
        };

        if should_flush {
            // Flush outside the lock to avoid holding it during send
            let _ = self.flush_buffer();
        }

        // Send progress update periodically
        let count = self.entries_processed.fetch_add(1, Ordering::Relaxed);
        if let Some(progress_tx) = &self.progress_sender {
            if count % PROGRESS_UPDATE_INTERVAL == 0 {
                let _ = progress_tx.send(ProgressUpdate {
                    files_scanned: count,
                    dirs_scanned: 0, // Will be updated from atomics if needed
                    total_size: 0,   // Will be updated from atomics if needed
                    current_path: entry.path.display().to_string(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_filesystem() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create a nested directory structure
        fs::write(root.join("file1.txt"), b"hello").unwrap();
        fs::write(root.join("file2.txt"), b"world").unwrap();

        fs::create_dir(root.join("subdir")).unwrap();
        fs::write(root.join("subdir/file3.txt"), b"test").unwrap();
        fs::write(root.join("subdir/file4.txt"), b"data").unwrap();

        fs::create_dir(root.join("subdir/nested")).unwrap();
        fs::write(root.join("subdir/nested/deep.txt"), b"deep file").unwrap();

        fs::create_dir(root.join("empty_dir")).unwrap();

        temp_dir
    }

    #[test]
    fn test_basic_scan() {
        let temp_dir = create_test_filesystem();
        let root = temp_dir.path();

        let scanner = Scanner::new(root);
        let (entries, stats) = scanner.scan().unwrap();

        // Should have at least 5 files (file1, file2, file3, file4, deep.txt)
        assert!(
            stats.total_files >= 5,
            "Expected at least 5 files, got {}",
            stats.total_files
        );

        // Should have at least 4 directories (root, subdir, nested, empty_dir)
        assert!(
            stats.total_dirs >= 4,
            "Expected at least 4 dirs, got {}",
            stats.total_dirs
        );

        // Total size should be sum of file contents
        assert!(
            stats.total_size >= 23,
            "Expected at least 23 bytes, got {}",
            stats.total_size
        );

        // Should return all entries when not streaming
        assert!(!entries.is_empty());
        assert_eq!(entries.len() as u64, stats.total_files + stats.total_dirs);
    }

    #[test]
    fn test_scan_with_streaming() {
        let temp_dir = create_test_filesystem();
        let root = temp_dir.path();

        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        let scanner = Scanner::with_sender(root, tx, None);

        // Spawn a task to collect entries from the channel
        let collect_handle = std::thread::spawn(move || {
            let mut total_entries = 0;
            while let Some(msg) = rx.blocking_recv() {
                match msg {
                    crate::db::ActorMessage::InsertBatch(entries) => {
                        total_entries += entries.len();
                    }
                    crate::db::ActorMessage::Shutdown => break,
                }
            }
            total_entries
        });

        let (entries, stats) = scanner.scan().unwrap();

        // When streaming, should return empty vec
        assert!(entries.is_empty());

        // Stats should still be correct
        assert!(stats.total_files >= 5);
        assert!(stats.total_dirs >= 4);

        // Wait for collection to finish
        let collected = collect_handle.join().unwrap();
        assert!(collected > 0, "Should have collected some entries");
    }

    #[test]
    fn test_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let scanner = Scanner::new(root);
        let (entries, stats) = scanner.scan().unwrap();

        // Root directory itself
        assert_eq!(stats.total_dirs, 1);
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.total_size, 0);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_large_directory_uses_parallelism() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create > 100 files to trigger parallel scanning
        for i in 0..150 {
            fs::write(root.join(format!("file{}.txt", i)), format!("content{}", i)).unwrap();
        }

        let scanner = Scanner::new(root);
        let (entries, stats) = scanner.scan().unwrap();

        assert_eq!(stats.total_files, 150);
        assert_eq!(stats.total_dirs, 1); // Just the root
        assert!(entries.len() == 151); // 150 files + 1 dir
    }

    #[test]
    fn test_progress_updates() {
        let temp_dir = create_test_filesystem();
        let root = temp_dir.path();

        let (tx, _rx) = tokio::sync::mpsc::channel(100);
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();

        let scanner = Scanner::with_sender(root, tx, Some(progress_tx));

        // Spawn task to collect progress updates
        let progress_handle = std::thread::spawn(move || {
            let mut updates = Vec::new();
            while let Some(update) = progress_rx.blocking_recv() {
                updates.push(update);
            }
            updates
        });

        let (_entries, stats) = scanner.scan().unwrap();

        // Give progress collector time to finish
        std::thread::sleep(std::time::Duration::from_millis(100));

        let updates = progress_handle.join().unwrap();

        // Should have received at least one progress update
        // (depends on PROGRESS_UPDATE_INTERVAL and number of entries)
        assert!(
            !updates.is_empty() || stats.total_files + stats.total_dirs < PROGRESS_UPDATE_INTERVAL
        );
    }

    #[test]
    fn test_directory_size_calculation() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        fs::create_dir(root.join("dir1")).unwrap();
        fs::write(root.join("dir1/a.txt"), b"12345").unwrap();
        fs::write(root.join("dir1/b.txt"), b"67890").unwrap();

        let scanner = Scanner::new(root);
        let (entries, _stats) = scanner.scan().unwrap();

        // Find the dir1 entry
        let dir1_entry = entries
            .iter()
            .find(|e| e.name == "dir1" && e.is_dir)
            .expect("Should find dir1");

        // Directory size should be sum of its contents
        assert_eq!(dir1_entry.size, 10, "dir1 should have size 10 (5+5)");
    }
}
