# Database Schema and Queries

This document describes the SQLite database schema used by rootkitty.

## Overview

Rootkitty uses SQLite with the following configuration:
- **Journal Mode**: WAL (Write-Ahead Logging)
- **Synchronous**: NORMAL
- **Connection Pool**: Max 5 connections
- **Location**: `~/.config/rootkitty/rootkitty.db` (default)

## Schema

### Table: `scans`

Stores metadata about each filesystem scan.

```sql
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
```

**Columns**:
- `id`: Auto-incrementing primary key
- `root_path`: Absolute path to scanned directory
- `started_at`: ISO 8601 timestamp (RFC 3339 format)
- `completed_at`: Timestamp when scan finished (NULL if still running)
- `total_size`: Total bytes scanned
- `total_files`: Count of files (not directories)
- `total_dirs`: Count of directories
- `status`: 'running', 'completed', or 'failed'

**Indices**:
- `idx_scans_started_at`: For listing scans newest-first
- `idx_scans_root_path`: For finding scans by path

**Typical Queries**:
```sql
-- List all scans, newest first
SELECT * FROM scans ORDER BY started_at DESC;

-- Find scans for a specific path
SELECT * FROM scans WHERE root_path = '/home/user' ORDER BY started_at DESC;

-- Get scan by ID
SELECT * FROM scans WHERE id = ?;
```

### Table: `file_entries`

Stores information about each file or directory in a scan.

```sql
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
```

**Columns**:
- `id`: Auto-incrementing primary key
- `scan_id`: Foreign key to `scans.id`
- `path`: Absolute path to file/directory
- `name`: Filename or directory name
- `parent_path`: Path to parent directory (NULL for root)
- `size`: Bytes (for files) or cumulative size (for directories)
- `is_dir`: 1 if directory, 0 if file
- `modified_at`: ISO 8601 timestamp of last modification
- `depth`: Depth in directory tree (0 = root)

**Indices**:
- `idx_file_entries_scan_id`: For filtering by scan
- `idx_file_entries_path`: For exact path lookups
- `idx_file_entries_parent`: For tree navigation
- `idx_file_entries_size`: For "largest files" queries

**Typical Queries**:
```sql
-- Get largest entries in a scan
SELECT * FROM file_entries
WHERE scan_id = ?
ORDER BY size DESC
LIMIT 1000;

-- Get entries by parent (tree navigation)
SELECT * FROM file_entries
WHERE scan_id = ? AND parent_path = ?
ORDER BY size DESC;

-- Get specific entry by path
SELECT * FROM file_entries
WHERE scan_id = ? AND path = ?;

-- Count files vs directories
SELECT is_dir, COUNT(*)
FROM file_entries
WHERE scan_id = ?
GROUP BY is_dir;

-- Total size by depth
SELECT depth, SUM(size)
FROM file_entries
WHERE scan_id = ?
GROUP BY depth
ORDER BY depth;
```

### Table: `cleanup_items`

Tracks files/directories marked for cleanup by the user.

```sql
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
```

**Columns**:
- `id`: Auto-incrementing primary key
- `scan_id`: Foreign key to `scans.id`
- `file_entry_id`: Foreign key to `file_entries.id`
- `marked_at`: Timestamp when marked for cleanup
- `reason`: Optional user-provided reason

**Constraints**:
- `UNIQUE(scan_id, file_entry_id)`: Can't mark same file twice in same scan

**Indices**:
- `idx_cleanup_items_scan_id`: For querying cleanup items by scan

**Typical Queries**:
```sql
-- Get all cleanup items for a scan
SELECT fe.* FROM file_entries fe
INNER JOIN cleanup_items ci ON fe.id = ci.file_entry_id
WHERE ci.scan_id = ?
ORDER BY fe.size DESC;

-- Mark item for cleanup
INSERT OR IGNORE INTO cleanup_items (scan_id, file_entry_id, marked_at, reason)
VALUES (?, ?, ?, ?);

-- Remove item from cleanup list
DELETE FROM cleanup_items
WHERE scan_id = ? AND file_entry_id = ?;

-- Total size of cleanup items
SELECT SUM(fe.size) FROM file_entries fe
INNER JOIN cleanup_items ci ON fe.id = ci.file_entry_id
WHERE ci.scan_id = ?;
```

## Query Performance

### Expected Query Times

On a scan with 1 million files (approximate):

| Query | Time | Notes |
|-------|------|-------|
| List all scans | <1ms | Scans table is small |
| Get top 1000 files by size | 5-10ms | Uses size index |
| Get files by parent | 1-5ms | Uses parent index |
| Exact path lookup | <1ms | Uses path index |
| Total size calculation | 50-100ms | Full table scan |
| Join cleanup items | <10ms | Small join table |

### Index Usage

Query plans for common operations:

```sql
-- Verify index usage
EXPLAIN QUERY PLAN
SELECT * FROM file_entries
WHERE scan_id = 1
ORDER BY size DESC
LIMIT 1000;

-- Expected: SEARCH file_entries USING INDEX idx_file_entries_size (scan_id=?)
```

### Optimization Tips

1. **Batch Inserts**: Use transactions for bulk inserts
   ```rust
   let mut tx = pool.begin().await?;
   for entry in entries {
       sqlx::query("INSERT INTO file_entries ...").execute(&mut *tx).await?;
   }
   tx.commit().await?;
   ```

2. **Limit Results**: Always use `LIMIT` for large result sets
   ```sql
   SELECT * FROM file_entries WHERE scan_id = ? ORDER BY size DESC LIMIT 1000;
   ```

3. **Avoid `SELECT *`**: Specify needed columns
   ```sql
   -- Good
   SELECT id, path, size FROM file_entries WHERE ...;

   -- Bad (in performance-critical code)
   SELECT * FROM file_entries WHERE ...;
   ```

4. **Use Covering Indices**: When possible, make index include all needed columns
   ```sql
   -- Future optimization
   CREATE INDEX idx_entries_size_with_name ON file_entries(scan_id, size DESC, name);
   ```

## Migrations

Migrations are stored in `crates/rootkitty/migrations/` and applied automatically by sqlx.

### Migration Naming

Format: `YYYYMMDDHHMMSS_description.sql`

Example: `20250101000000_initial.sql`

### Current Migrations

1. **20250101000000_initial.sql**: Initial schema with three tables

### Adding a Migration

1. Create new file: `migrations/20250102120000_add_feature.sql`
2. Write SQL (both UP and DOWN in same file)
3. Test with: `cargo sqlx migrate run`
4. Update `.sqlx/` cache: `cargo sqlx prepare`

Example migration:
```sql
-- Add tags to file entries
ALTER TABLE file_entries ADD COLUMN tags TEXT;

-- Index for tag searches
CREATE INDEX idx_file_entries_tags ON file_entries(scan_id, tags);

-- Note: SQLite doesn't support DROP COLUMN, so rollback would require full table recreation
```

### Migration Best Practices

- **Backwards Compatible**: If possible, make changes additive
- **Test Rollback**: Ensure migrations can be reversed
- **Data Migration**: Include data transformation if needed
- **Index Creation**: Create indices after bulk inserts

## Data Types

### INTEGER vs TEXT

SQLite stores integers as variable-length (1, 2, 4, or 8 bytes). For our use:
- File sizes: Always 8 bytes (i64)
- IDs: Usually 4 bytes (i32) but can grow
- Booleans: 1 byte (0 or 1)

Timestamps are stored as TEXT (ISO 8601) rather than INTEGER (unix timestamp) for:
- Readability in database browsers
- Timezone preservation
- Easier debugging

### Path Storage

Paths are stored as TEXT in platform-specific format:
- Unix: `/home/user/file.txt`
- Windows: `C:\Users\User\file.txt`

**Advantages**:
- Natural format, easy to read
- Works with SQLite's text functions

**Disadvantages**:
- Not portable across platforms
- Larger storage than binary format

Future: Could normalize to always use `/` and convert on load.

## Database Maintenance

### Vacuum

SQLite databases can become fragmented over time. To compact:

```bash
sqlite3 ~/.config/rootkitty/rootkitty.db "VACUUM;"
```

This should be done:
- After deleting many scans
- Periodically (monthly) for active users
- Before backups

### Backup

WAL mode creates multiple files:
- `rootkitty.db`: Main database
- `rootkitty.db-wal`: Write-ahead log
- `rootkitty.db-shm`: Shared memory

To backup properly:
```bash
# Option 1: Use SQLite backup command
sqlite3 ~/.config/rootkitty/rootkitty.db ".backup /path/to/backup.db"

# Option 2: Copy all files (when app is not running)
cp ~/.config/rootkitty/rootkitty.db* /path/to/backup/
```

### Integrity Check

```bash
sqlite3 ~/.config/rootkitty/rootkitty.db "PRAGMA integrity_check;"
```

Should return: `ok`

## SQL Style Guide

### Formatting

```sql
-- Good: Clear, readable
SELECT
    id,
    path,
    size,
    modified_at
FROM file_entries
WHERE scan_id = ?
    AND is_dir = 0
ORDER BY size DESC
LIMIT 1000;

-- Bad: Hard to read
SELECT id,path,size,modified_at FROM file_entries WHERE scan_id=? AND is_dir=0 ORDER BY size DESC LIMIT 1000;
```

### Naming Conventions

- **Tables**: Plural, snake_case (`file_entries`, not `FileEntry` or `file_entry`)
- **Columns**: snake_case (`modified_at`, not `modifiedAt`)
- **Indices**: `idx_<table>_<columns>` (`idx_file_entries_scan_id`)
- **Foreign Keys**: Usually `<table>_id` (`scan_id`)

### NULL Handling

Avoid `NULL` where possible:
- Use `DEFAULT 0` for integers
- Use `DEFAULT ''` for text (if empty string is valid)
- Use `NULL` only when "unknown" is different from "empty"

Example:
- `completed_at`: NULL means "not completed yet" ✓
- `total_size`: 0 means "empty" ✓
- `reason`: NULL means "no reason given" (could use empty string instead)

## Query Examples

### Advanced Queries

#### 1. Find largest files by extension

```sql
SELECT
    SUBSTR(name, INSTR(name, '.') + 1) AS extension,
    COUNT(*) AS count,
    SUM(size) AS total_size,
    AVG(size) AS avg_size
FROM file_entries
WHERE scan_id = ?
    AND is_dir = 0
    AND name LIKE '%.%'
GROUP BY extension
ORDER BY total_size DESC
LIMIT 20;
```

#### 2. Find directories with most files

```sql
SELECT
    parent_path,
    COUNT(*) AS file_count,
    SUM(size) AS total_size
FROM file_entries
WHERE scan_id = ?
    AND is_dir = 0
GROUP BY parent_path
ORDER BY file_count DESC
LIMIT 20;
```

#### 3. Compare two scans (files added/removed)

```sql
-- Files in scan2 but not scan1 (added)
SELECT s2.path, s2.size
FROM file_entries s2
LEFT JOIN file_entries s1
    ON s2.path = s1.path AND s1.scan_id = ?
WHERE s2.scan_id = ?
    AND s1.id IS NULL;

-- Files in scan1 but not scan2 (removed)
SELECT s1.path, s1.size
FROM file_entries s1
LEFT JOIN file_entries s2
    ON s1.path = s2.path AND s2.scan_id = ?
WHERE s1.scan_id = ?
    AND s2.id IS NULL;

-- Files in both with changed size
SELECT
    s1.path,
    s1.size AS old_size,
    s2.size AS new_size,
    (s2.size - s1.size) AS delta
FROM file_entries s1
INNER JOIN file_entries s2 ON s1.path = s2.path
WHERE s1.scan_id = ?
    AND s2.scan_id = ?
    AND s1.size != s2.size
ORDER BY ABS(s2.size - s1.size) DESC;
```

#### 4. Find old files (candidates for cleanup)

```sql
SELECT path, size, modified_at
FROM file_entries
WHERE scan_id = ?
    AND is_dir = 0
    AND modified_at < DATE('now', '-1 year')
ORDER BY size DESC
LIMIT 100;
```

#### 5. Duplicate files by size

```sql
SELECT size, COUNT(*) AS count
FROM file_entries
WHERE scan_id = ?
    AND is_dir = 0
GROUP BY size
HAVING COUNT(*) > 1
ORDER BY size * count DESC;
```

## Troubleshooting

### Database Locked

**Symptom**: `SQLITE_BUSY` or "database is locked" errors

**Causes**:
- Another process has the database open
- Long-running transaction
- WAL checkpoint in progress

**Solutions**:
```rust
// Increase timeout
let options = SqliteConnectOptions::from_str(&url)?
    .busy_timeout(Duration::from_secs(30));

// Retry with backoff
for attempt in 0..3 {
    match db.query().await {
        Ok(result) => return Ok(result),
        Err(e) if e.is_database_locked() => {
            tokio::time::sleep(Duration::from_millis(100 * 2u64.pow(attempt))).await;
        }
        Err(e) => return Err(e),
    }
}
```

### Slow Queries

**Symptom**: Queries taking seconds instead of milliseconds

**Diagnosis**:
```sql
EXPLAIN QUERY PLAN <your query>;
```

Look for:
- `SCAN TABLE`: Bad! Should use an index
- `SEARCH ... USING INDEX`: Good!

**Solutions**:
- Add missing index
- Rewrite query to use existing index
- Use `ANALYZE` to update query planner statistics

### Database Corruption

**Symptom**: `SQLITE_CORRUPT` errors

**Recovery**:
```bash
# Try integrity check
sqlite3 rootkitty.db "PRAGMA integrity_check;"

# If corrupted, try to export and reimport
sqlite3 rootkitty.db ".dump" | sqlite3 new_rootkitty.db

# As last resort, delete and rescan
rm rootkitty.db*
rootkitty scan /path/to/scan
```

## Future Schema Changes

Potential additions:

1. **File hashes** for duplicate detection
   ```sql
   ALTER TABLE file_entries ADD COLUMN sha256 TEXT;
   CREATE INDEX idx_file_entries_hash ON file_entries(sha256);
   ```

2. **Tags** for custom categorization
   ```sql
   CREATE TABLE tags (
       id INTEGER PRIMARY KEY,
       name TEXT UNIQUE NOT NULL
   );

   CREATE TABLE file_tags (
       file_entry_id INTEGER,
       tag_id INTEGER,
       FOREIGN KEY (file_entry_id) REFERENCES file_entries(id),
       FOREIGN KEY (tag_id) REFERENCES tags(id),
       PRIMARY KEY (file_entry_id, tag_id)
   );
   ```

3. **Scan metadata** for advanced queries
   ```sql
   ALTER TABLE scans ADD COLUMN machine_hostname TEXT;
   ALTER TABLE scans ADD COLUMN user_name TEXT;
   ALTER TABLE scans ADD COLUMN notes TEXT;
   ```

4. **File type detection**
   ```sql
   ALTER TABLE file_entries ADD COLUMN mime_type TEXT;
   CREATE INDEX idx_file_entries_mime ON file_entries(scan_id, mime_type);
   ```
