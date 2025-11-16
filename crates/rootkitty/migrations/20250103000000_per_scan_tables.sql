-- Migration: Move to per-scan tables for easy garbage collection
-- Each scan gets its own table for entries, referenced in scans.entries_table
-- Note: User OK'd blowing away old database, no backward compat needed

-- Step 1: Add entries_table column to scans
ALTER TABLE scans ADD COLUMN entries_table TEXT;

-- Step 2: Change cleanup_items to reference paths instead of file_entry_id
-- (since per-scan tables have independent ID sequences)
DROP TABLE IF EXISTS cleanup_items;

CREATE TABLE IF NOT EXISTS cleanup_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scan_id INTEGER NOT NULL,
    entry_path TEXT NOT NULL,
    marked_at TEXT NOT NULL,
    reason TEXT,
    FOREIGN KEY (scan_id) REFERENCES scans(id) ON DELETE CASCADE,
    UNIQUE(scan_id, entry_path)
);

CREATE INDEX idx_cleanup_items_scan_id ON cleanup_items(scan_id);
