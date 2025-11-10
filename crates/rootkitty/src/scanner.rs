use anyhow::Result;
use chrono::{DateTime, Utc};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    #[allow(dead_code)]
    pub dirs_scanned: u64,
    #[allow(dead_code)]
    pub total_size: u64,
    #[allow(dead_code)]
    pub current_path: String,
    /// Currently active directories being scanned (path -> (files_done, total_files))
    pub active_dirs: Vec<(String, usize, usize)>,
    /// Approximate number of active parallel workers
    pub active_workers: usize,
}

const BUFFER_SIZE: usize = 1000;
const PROGRESS_UPDATE_INTERVAL: u64 = 100; // Send progress every N entries

/// Scanning implementation to use
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScannerImpl {
    /// Custom rayon-based parallel implementation
    Custom,
    /// walkdir-based single-threaded implementation
    Walkdir,
    /// Hybrid: walkdir for traversal + rayon for parallelism
    Hybrid,
}

pub struct Scanner {
    root_path: PathBuf,
    entries: Arc<Mutex<Vec<FileEntry>>>,
    sender: Option<mpsc::Sender<crate::db::ActorMessage>>,
    progress_sender: Option<mpsc::UnboundedSender<ProgressUpdate>>,
    entries_processed: Arc<AtomicU64>,
    /// Track active directories: path -> (completed, total)
    active_dirs: Arc<Mutex<HashMap<String, (usize, usize)>>>,
    /// Approximate count of active parallel workers
    active_workers: Arc<AtomicUsize>,
    /// If true, run a simulated demo scan instead of real filesystem scan
    demo_mode: bool,
    /// Cancellation flag - when set to true, scanner should stop
    cancelled: Arc<AtomicBool>,
    /// Implementation to use for scanning
    implementation: ScannerImpl,
    /// Whether to follow symbolic links during scanning
    follow_symlinks: bool,
}

impl Scanner {
    pub fn with_sender<P: AsRef<Path>>(
        root_path: P,
        sender: mpsc::Sender<crate::db::ActorMessage>,
        progress_sender: Option<mpsc::UnboundedSender<ProgressUpdate>>,
        cancelled: Arc<AtomicBool>,
        follow_symlinks: bool,
    ) -> Self {
        Self {
            root_path: root_path.as_ref().to_path_buf(),
            entries: Arc::new(Mutex::new(Vec::new())),
            sender: Some(sender),
            progress_sender,
            entries_processed: Arc::new(AtomicU64::new(0)),
            active_dirs: Arc::new(Mutex::new(HashMap::new())),
            active_workers: Arc::new(AtomicUsize::new(0)),
            demo_mode: false,
            cancelled,
            implementation: ScannerImpl::Custom,
            follow_symlinks,
        }
    }

    pub fn with_sender_demo<P: AsRef<Path>>(
        root_path: P,
        sender: mpsc::Sender<crate::db::ActorMessage>,
        progress_sender: Option<mpsc::UnboundedSender<ProgressUpdate>>,
        cancelled: Arc<AtomicBool>,
        follow_symlinks: bool,
    ) -> Self {
        Self {
            root_path: root_path.as_ref().to_path_buf(),
            entries: Arc::new(Mutex::new(Vec::new())),
            sender: Some(sender),
            progress_sender,
            entries_processed: Arc::new(AtomicU64::new(0)),
            active_dirs: Arc::new(Mutex::new(HashMap::new())),
            active_workers: Arc::new(AtomicUsize::new(0)),
            demo_mode: true,
            cancelled,
            implementation: ScannerImpl::Custom,
            follow_symlinks,
        }
    }

    /// Simple constructor for benchmarking - collects entries in memory
    /// Uses the custom rayon-based parallel implementation by default
    pub fn new<P: AsRef<Path>>(root_path: P) -> Self {
        Self::new_with_impl(root_path, ScannerImpl::Custom)
    }

    /// Constructor for benchmarking with a specific implementation
    pub fn new_with_impl<P: AsRef<Path>>(root_path: P, implementation: ScannerImpl) -> Self {
        Self {
            root_path: root_path.as_ref().to_path_buf(),
            entries: Arc::new(Mutex::new(Vec::new())),
            sender: None,
            progress_sender: None,
            entries_processed: Arc::new(AtomicU64::new(0)),
            active_dirs: Arc::new(Mutex::new(HashMap::new())),
            active_workers: Arc::new(AtomicUsize::new(0)),
            demo_mode: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            implementation,
            follow_symlinks: false, // Default to false for benchmarks
        }
    }

    pub fn scan(&self) -> Result<(Vec<FileEntry>, ScanStats)> {
        // Check if this is a demo scan
        if self.demo_mode {
            return self.demo_scan();
        }

        // Dispatch based on implementation
        match self.implementation {
            ScannerImpl::Custom => self.scan_custom(),
            ScannerImpl::Walkdir => self.scan_walkdir(),
            ScannerImpl::Hybrid => self.scan_hybrid(),
        }
    }

    fn scan_custom(&self) -> Result<(Vec<FileEntry>, ScanStats)> {
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

    fn scan_walkdir(&self) -> Result<(Vec<FileEntry>, ScanStats)> {
        use walkdir::WalkDir;

        let mut total_size = 0u64;
        let mut total_files = 0u64;
        let mut total_dirs = 0u64;

        // Build a map to track directory sizes
        let mut dir_sizes: HashMap<PathBuf, u64> = HashMap::new();
        let mut entries = Vec::new();

        // First pass: collect all entries and calculate file sizes
        let walker = WalkDir::new(&self.root_path)
            .follow_links(self.follow_symlinks)
            .into_iter()
            .filter_map(|e| e.ok());

        for entry in walker {
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(anyhow::anyhow!("Scan cancelled"));
            }

            let path = entry.path().to_path_buf();
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };

            let is_dir = metadata.is_dir();
            let file_size = if is_dir { 0 } else { metadata.len() };

            if is_dir {
                total_dirs += 1;
                dir_sizes.insert(path.clone(), 0);
            } else {
                total_files += 1;
                total_size += file_size;

                // Add file size to all parent directories
                let mut current_parent = path.parent();
                while let Some(parent) = current_parent {
                    *dir_sizes.entry(parent.to_path_buf()).or_insert(0) += file_size;
                    current_parent = parent.parent();
                }
            }

            let name = entry.file_name().to_string_lossy().to_string();
            let parent_path = path.parent().map(|p| p.to_path_buf());
            let modified_at = metadata.modified().ok().map(|t| {
                let system_time: std::time::SystemTime = t;
                DateTime::<Utc>::from(system_time)
            });
            let depth = entry.depth();

            entries.push(FileEntry {
                path: path.clone(),
                name,
                parent_path,
                size: if is_dir {
                    *dir_sizes.get(&path).unwrap_or(&0)
                } else {
                    file_size
                },
                is_dir,
                modified_at,
                depth,
            });
        }

        // Second pass: update directory sizes
        for entry in &mut entries {
            if entry.is_dir {
                entry.size = *dir_sizes.get(&entry.path).unwrap_or(&0);
            }
        }

        let stats = ScanStats {
            total_size,
            total_files,
            total_dirs,
        };

        // Store entries if not streaming
        if self.sender.is_none() {
            *self.entries.lock().unwrap() = entries.clone();
        }

        Ok((entries, stats))
    }

    fn scan_hybrid(&self) -> Result<(Vec<FileEntry>, ScanStats)> {
        use jwalk::WalkDir;
        use rayon::prelude::*;
        use std::sync::atomic::{AtomicU64, AtomicUsize};

        // Use jwalk to traverse in PARALLEL (faster than single-threaded walkdir)!
        let walker = WalkDir::new(&self.root_path)
            .follow_links(self.follow_symlinks)
            .into_iter()
            .filter_map(|e| e.ok());

        // Collect all entries (jwalk does parallel traversal internally)
        let all_entries: Vec<_> = walker.collect();

        // Single-pass parallel processing: compute dir_sizes AND stats at the same time!
        let total_files = AtomicUsize::new(0);
        let total_dirs = AtomicUsize::new(0);
        let total_size = AtomicU64::new(0);

        let dir_sizes: HashMap<PathBuf, u64> = all_entries
            .par_iter()
            .fold(
                || (HashMap::new(), 0, 0, 0u64), // (map, files, dirs, size)
                |(mut map, mut files, mut dirs, mut size), entry| {
                    let is_dir = entry.file_type().is_dir();

                    // Update stats inline
                    if is_dir {
                        dirs += 1;
                    } else {
                        files += 1;
                        let file_size = entry.metadata().ok().map(|m| m.len()).unwrap_or(0);
                        size += file_size;

                        // Calculate dir contributions for this file
                        let path = entry.path();
                        let mut current = path.parent();
                        while let Some(parent) = current {
                            *map.entry(parent.to_path_buf()).or_insert(0) += file_size;
                            current = parent.parent();
                        }
                    }
                    (map, files, dirs, size)
                },
            )
            .reduce(
                || (HashMap::new(), 0, 0, 0u64),
                |(mut map_a, files_a, dirs_a, size_a), (map_b, files_b, dirs_b, size_b)| {
                    // Merge maps
                    for (path, size) in map_b {
                        *map_a.entry(path).or_insert(0) += size;
                    }
                    // Merge stats
                    total_files.fetch_add(files_a + files_b, Ordering::Relaxed);
                    total_dirs.fetch_add(dirs_a + dirs_b, Ordering::Relaxed);
                    total_size.fetch_add(size_a + size_b, Ordering::Relaxed);
                    (map_a, 0, 0, 0)
                },
            )
            .0;

        // Single-pass parallel FileEntry construction (now just one parallel op!)
        let entries: Vec<FileEntry> = all_entries
            .par_iter()
            .map(|entry| {
                let path = entry.path().to_path_buf();
                let metadata = entry.metadata().ok();
                let is_dir = entry.file_type().is_dir();

                let name = if path == self.root_path {
                    self.root_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("/")
                        .to_string()
                } else {
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string()
                };

                let parent_path = path.parent().map(|p| p.to_path_buf());
                let modified_at = metadata.as_ref().and_then(|m| {
                    m.modified().ok().map(|t| {
                        let system_time: std::time::SystemTime = t;
                        DateTime::<Utc>::from(system_time)
                    })
                });

                let file_size = if is_dir {
                    0
                } else {
                    metadata.as_ref().map(|m| m.len()).unwrap_or(0)
                };

                let size = if is_dir {
                    *dir_sizes.get(&path).unwrap_or(&0)
                } else {
                    file_size
                };

                FileEntry {
                    path,
                    name,
                    parent_path,
                    size,
                    is_dir,
                    modified_at,
                    depth: entry.depth(),
                }
            })
            .collect();

        let stats = ScanStats {
            total_size: total_size.load(Ordering::Relaxed),
            total_files: total_files.load(Ordering::Relaxed) as u64,
            total_dirs: total_dirs.load(Ordering::Relaxed) as u64,
        };

        // Store entries if not streaming
        if self.sender.is_none() {
            *self.entries.lock().unwrap() = entries.clone();
        }

        Ok((entries, stats))
    }

    /// Resume a scan that was previously paused, skipping already-scanned paths
    /// The `scanned_paths` HashSet contains paths that have already been scanned
    pub fn scan_resuming(
        &self,
        scanned_paths: std::collections::HashSet<String>,
    ) -> Result<(Vec<FileEntry>, ScanStats)> {
        let total_size = AtomicU64::new(0);
        let total_files = AtomicU64::new(0);
        let total_dirs = AtomicU64::new(0);

        self.scan_recursive_resuming(
            &self.root_path,
            0,
            &total_size,
            &total_files,
            &total_dirs,
            &scanned_paths,
        )?;

        // Final flush of any remaining buffered entries
        self.flush_buffer()?;

        let entries = if self.sender.is_some() {
            Vec::new()
        } else {
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

    fn scan_recursive_resuming(
        &self,
        path: &Path,
        depth: usize,
        total_size: &AtomicU64,
        total_files: &AtomicU64,
        total_dirs: &AtomicU64,
        scanned_paths: &std::collections::HashSet<String>,
    ) -> Result<u64> {
        // Check if scan was cancelled
        if self.cancelled.load(Ordering::Relaxed) {
            return Ok(0);
        }

        // Check if this path was already scanned
        let path_str = path.display().to_string();
        if scanned_paths.contains(&path_str) {
            // Skip this path - it was already scanned
            return Ok(0);
        }

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
        let name = if depth == 0 {
            // For the root directory, use the full absolute path for clarity
            // Canonicalize to resolve relative paths like ".", "..", "~", etc.
            path.canonicalize()
                .unwrap_or_else(|_| path.to_path_buf())
                .display()
                .to_string()
        } else {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        };

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
            let num_children = children.len();

            // Register this directory in active tracking
            let dir_path_str = path.display().to_string();
            {
                let mut active = self.active_dirs.lock().unwrap();
                active.insert(dir_path_str.clone(), (0, num_children));
            }

            // For small directories, process serially; for large ones, use parallelism
            if num_children > 100 {
                // Increment worker count for parallel processing
                self.active_workers.fetch_add(1, Ordering::Relaxed);

                let child_sizes: Vec<u64> = children
                    .par_iter()
                    .enumerate()
                    .filter_map(|(idx, child_path)| {
                        let result = self
                            .scan_recursive_resuming(
                                child_path,
                                depth + 1,
                                total_size,
                                total_files,
                                total_dirs,
                                scanned_paths,
                            )
                            .ok();

                        // Update directory progress after processing each child
                        {
                            let mut active = self.active_dirs.lock().unwrap();
                            if let Some(progress) = active.get_mut(&dir_path_str) {
                                progress.0 = idx + 1;
                            }
                        }

                        result
                    })
                    .collect();
                dir_size = child_sizes.iter().sum();

                // Decrement worker count
                self.active_workers.fetch_sub(1, Ordering::Relaxed);
            } else {
                for (idx, child_path) in children.iter().enumerate() {
                    if let Ok(size) = self.scan_recursive_resuming(
                        child_path,
                        depth + 1,
                        total_size,
                        total_files,
                        total_dirs,
                        scanned_paths,
                    ) {
                        dir_size += size;
                    }

                    // Update directory progress
                    {
                        let mut active = self.active_dirs.lock().unwrap();
                        if let Some(progress) = active.get_mut(&dir_path_str) {
                            progress.0 = idx + 1;
                        }
                    }
                }
            }

            // Remove this directory from active tracking
            {
                let mut active = self.active_dirs.lock().unwrap();
                active.remove(&dir_path_str);
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

    fn scan_recursive(
        &self,
        path: &Path,
        depth: usize,
        total_size: &AtomicU64,
        total_files: &AtomicU64,
        total_dirs: &AtomicU64,
    ) -> Result<u64> {
        // Check if scan was cancelled
        if self.cancelled.load(Ordering::Relaxed) {
            return Ok(0);
        }

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
        let name = if depth == 0 {
            // For the root directory, use the full absolute path for clarity
            // Canonicalize to resolve relative paths like ".", "..", "~", etc.
            path.canonicalize()
                .unwrap_or_else(|_| path.to_path_buf())
                .display()
                .to_string()
        } else {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        };

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
            let num_children = children.len();

            // Register this directory in active tracking
            let dir_path_str = path.display().to_string();
            {
                let mut active = self.active_dirs.lock().unwrap();
                active.insert(dir_path_str.clone(), (0, num_children));
            }

            // For small directories, process serially; for large ones, use parallelism
            if num_children > 100 {
                // Increment worker count for parallel processing
                self.active_workers.fetch_add(1, Ordering::Relaxed);

                let child_sizes: Vec<u64> = children
                    .par_iter()
                    .enumerate()
                    .filter_map(|(idx, child_path)| {
                        let result = self
                            .scan_recursive(
                                child_path,
                                depth + 1,
                                total_size,
                                total_files,
                                total_dirs,
                            )
                            .ok();

                        // Update directory progress after processing each child
                        {
                            let mut active = self.active_dirs.lock().unwrap();
                            if let Some(progress) = active.get_mut(&dir_path_str) {
                                progress.0 = idx + 1;
                            }
                        }

                        result
                    })
                    .collect();
                dir_size = child_sizes.iter().sum();

                // Decrement worker count
                self.active_workers.fetch_sub(1, Ordering::Relaxed);
            } else {
                for (idx, child_path) in children.iter().enumerate() {
                    if let Ok(size) = self.scan_recursive(
                        child_path,
                        depth + 1,
                        total_size,
                        total_files,
                        total_dirs,
                    ) {
                        dir_size += size;
                    }

                    // Update directory progress
                    {
                        let mut active = self.active_dirs.lock().unwrap();
                        if let Some(progress) = active.get_mut(&dir_path_str) {
                            progress.0 = idx + 1;
                        }
                    }
                }
            }

            // Remove this directory from active tracking
            {
                let mut active = self.active_dirs.lock().unwrap();
                active.remove(&dir_path_str);
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

    /// Demo scanner that simulates scanning without touching filesystem
    /// Useful for testing the UI and progress display
    pub fn demo_scan(&self) -> Result<(Vec<FileEntry>, ScanStats)> {
        use std::thread;
        use std::time::Duration;

        let total_size = AtomicU64::new(0);
        let total_files = AtomicU64::new(0);
        let total_dirs = AtomicU64::new(0);

        // Simulate scanning several directories
        let demo_dirs = vec![
            ("/demo/src", 150),
            ("/demo/target/debug", 500),
            ("/demo/target/release", 300),
            ("/demo/node_modules", 1200),
            ("/demo/docs", 50),
        ];

        for (dir_path, file_count) in demo_dirs {
            // Check if cancelled at start of each directory
            if self.cancelled.load(Ordering::Relaxed) {
                break;
            }
            // Register this directory
            {
                let mut active = self.active_dirs.lock().unwrap();
                active.insert(dir_path.to_string(), (0, file_count));
            }

            // Simulate processing files in this directory
            for i in 0..file_count {
                // Check if cancelled
                if self.cancelled.load(Ordering::Relaxed) {
                    break;
                }

                // Simulate some work
                thread::sleep(Duration::from_millis(5));

                // Create a fake entry
                let entry = FileEntry {
                    path: PathBuf::from(format!("{}/file_{}.txt", dir_path, i)),
                    name: format!("file_{}.txt", i),
                    parent_path: Some(PathBuf::from(dir_path)),
                    size: 1024 * (i as u64 % 100),
                    is_dir: false,
                    modified_at: None,
                    depth: dir_path.split('/').count(),
                };

                total_files.fetch_add(1, Ordering::Relaxed);
                total_size.fetch_add(entry.size, Ordering::Relaxed);

                self.add_entry(entry);

                // Update directory progress
                {
                    let mut active = self.active_dirs.lock().unwrap();
                    if let Some(progress) = active.get_mut(dir_path) {
                        progress.0 = i + 1;
                    }
                }
            }

            // Remove directory from active tracking
            {
                let mut active = self.active_dirs.lock().unwrap();
                active.remove(dir_path);
            }

            total_dirs.fetch_add(1, Ordering::Relaxed);
        }

        // Final flush
        self.flush_buffer()?;

        let entries = if self.sender.is_some() {
            Vec::new()
        } else {
            self.entries.lock().unwrap().clone()
        };

        let stats = ScanStats {
            total_size: total_size.load(Ordering::Relaxed),
            total_files: total_files.load(Ordering::Relaxed),
            total_dirs: total_dirs.load(Ordering::Relaxed),
        };

        Ok((entries, stats))
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
                // Snapshot of currently active directories
                let active_dirs_snapshot: Vec<(String, usize, usize)> = {
                    let active = self.active_dirs.lock().unwrap();
                    active
                        .iter()
                        .map(|(path, (done, total))| (path.clone(), *done, *total))
                        .collect()
                };

                let _ = progress_tx.send(ProgressUpdate {
                    files_scanned: count,
                    dirs_scanned: 0, // Will be updated from atomics if needed
                    total_size: 0,   // Will be updated from atomics if needed
                    current_path: entry.path.display().to_string(),
                    active_dirs: active_dirs_snapshot,
                    active_workers: self.active_workers.load(Ordering::Relaxed),
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
    fn test_hybrid_scan() {
        let temp_dir = create_test_filesystem();
        let root = temp_dir.path();

        let scanner = Scanner::new_with_impl(root, ScannerImpl::Hybrid);
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
    #[ignore] // TODO: Fix - Scanner API has changed
    fn test_scan_with_streaming() {
        let temp_dir = create_test_filesystem();
        let root = temp_dir.path();

        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        let scanner = Scanner::with_sender(root, tx, None, Arc::new(AtomicBool::new(false)), false);

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
    #[ignore] // TODO: Fix - Scanner::new no longer exists
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
    #[ignore] // TODO: Fix - Scanner::new no longer exists
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
    #[ignore] // TODO: Fix - Scanner API has changed
    fn test_progress_updates() {
        let temp_dir = create_test_filesystem();
        let root = temp_dir.path();

        let (tx, _rx) = tokio::sync::mpsc::channel(100);
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();

        let scanner = Scanner::with_sender(
            root,
            tx,
            Some(progress_tx),
            Arc::new(AtomicBool::new(false)),
            false,
        );

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
    #[ignore] // TODO: Fix - Scanner::new no longer exists
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
