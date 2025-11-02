-- Scans table: tracks each filesystem scan
CREATE TABLE IF NOT EXISTS scans (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    root_path TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    total_size INTEGER NOT NULL DEFAULT 0,
    total_files INTEGER NOT NULL DEFAULT 0,
    total_dirs INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'running' CHECK(status IN ('running', 'completed', 'failed'))
);

CREATE INDEX idx_scans_started_at ON scans(started_at DESC);
CREATE INDEX idx_scans_root_path ON scans(root_path);

-- File entries: stores information about each file/directory in a scan
CREATE TABLE IF NOT EXISTS file_entries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scan_id INTEGER NOT NULL,
    path TEXT NOT NULL,
    name TEXT NOT NULL,
    parent_path TEXT,
    size INTEGER NOT NULL,
    is_dir INTEGER NOT NULL,
    modified_at TEXT,
    depth INTEGER NOT NULL,
    FOREIGN KEY (scan_id) REFERENCES scans(id) ON DELETE CASCADE
);

CREATE INDEX idx_file_entries_scan_id ON file_entries(scan_id);
CREATE INDEX idx_file_entries_path ON file_entries(scan_id, path);
CREATE INDEX idx_file_entries_parent ON file_entries(scan_id, parent_path);
CREATE INDEX idx_file_entries_size ON file_entries(scan_id, size DESC);

-- Cleanup recommendations: track user-marked files for deletion
CREATE TABLE IF NOT EXISTS cleanup_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scan_id INTEGER NOT NULL,
    file_entry_id INTEGER NOT NULL,
    marked_at TEXT NOT NULL,
    reason TEXT,
    FOREIGN KEY (scan_id) REFERENCES scans(id) ON DELETE CASCADE,
    FOREIGN KEY (file_entry_id) REFERENCES file_entries(id) ON DELETE CASCADE,
    UNIQUE(scan_id, file_entry_id)
);

CREATE INDEX idx_cleanup_items_scan_id ON cleanup_items(scan_id);
