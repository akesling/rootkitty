-- Add 'paused' status for resumable scans
-- SQLite doesn't support ALTER TABLE ... DROP CONSTRAINT, so we need to recreate the table

-- Create new table with updated CHECK constraint
CREATE TABLE IF NOT EXISTS scans_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    root_path TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    total_size INTEGER NOT NULL DEFAULT 0,
    total_files INTEGER NOT NULL DEFAULT 0,
    total_dirs INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'running' CHECK(status IN ('running', 'completed', 'failed', 'paused'))
);

-- Copy existing data
INSERT INTO scans_new SELECT * FROM scans;

-- Drop old table
DROP TABLE scans;

-- Rename new table
ALTER TABLE scans_new RENAME TO scans;

-- Recreate indexes
CREATE INDEX idx_scans_started_at ON scans(started_at DESC);
CREATE INDEX idx_scans_root_path ON scans(root_path);
CREATE INDEX idx_scans_status ON scans(status);
