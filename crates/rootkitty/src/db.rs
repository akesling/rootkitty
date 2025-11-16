use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::Row;
use std::path::Path;
use std::str::FromStr;
use tokio::sync::mpsc;

use crate::scanner::{FileEntry, ScanStats};

pub enum ActorMessage {
    InsertBatch(Vec<FileEntry>),
    Shutdown,
}

pub struct DatabaseActor {
    db: Database,
    scan_id: i64,
    receiver: mpsc::Receiver<ActorMessage>,
}

impl DatabaseActor {
    pub fn new(db: Database, scan_id: i64, receiver: mpsc::Receiver<ActorMessage>) -> Self {
        Self {
            db,
            scan_id,
            receiver,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        while let Some(msg) = self.receiver.recv().await {
            match msg {
                ActorMessage::InsertBatch(entries) => {
                    if !entries.is_empty() {
                        self.db.insert_file_entries(self.scan_id, &entries).await?;
                    }
                }
                ActorMessage::Shutdown => {
                    break;
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct Scan {
    pub id: i64,
    pub root_path: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub total_size: i64,
    pub total_files: i64,
    pub total_dirs: i64,
    pub status: String,
    pub entries_table: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StoredFileEntry {
    pub id: i64,
    #[allow(dead_code)]
    pub scan_id: i64,
    pub path: String,
    pub name: String,
    #[allow(dead_code)]
    pub parent_path: Option<String>,
    pub size: i64,
    pub is_dir: bool,
    #[allow(dead_code)]
    pub modified_at: Option<DateTime<Utc>>,
    pub depth: i64,
}

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Create a Database from an existing pool (useful for testing)
    #[doc(hidden)]
    #[allow(dead_code)]
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Get the table name for entries of a given scan
    /// Returns scan-specific table for new scans, or "file_entries" for legacy scans
    async fn get_entries_table(&self, scan_id: i64) -> Result<String> {
        let scan =
            sqlx::query_as::<_, (Option<String>,)>("SELECT entries_table FROM scans WHERE id = ?")
                .bind(scan_id)
                .fetch_one(&self.pool)
                .await?;

        Ok(scan.0.unwrap_or_else(|| "file_entries".to_string()))
    }

    /// Helper to parse a row into StoredFileEntry
    /// Note: scan_id field is populated with the parameter value since per-scan tables don't store it
    fn row_to_entry(row: &sqlx::sqlite::SqliteRow, scan_id: i64) -> StoredFileEntry {
        use sqlx::Row;
        let modified_at_str: Option<String> = row.get("modified_at");

        StoredFileEntry {
            id: row.get("id"),
            scan_id,
            path: row.get("path"),
            name: row.get("name"),
            parent_path: row.get("parent_path"),
            size: row.get("size"),
            is_dir: row.get("is_dir"),
            modified_at: modified_at_str.and_then(|s| {
                DateTime::parse_from_rfc3339(&s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            }),
            depth: row.get("depth"),
        }
    }

    pub async fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let db_path = db_path.as_ref();

        // Create parent directory if it doesn't exist
        if let Some(parent) = db_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path.display()))?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .context("Failed to connect to database")?;

        // Run migrations
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .context("Failed to run migrations")?;

        Ok(Self { pool })
    }

    pub async fn create_scan(&self, root_path: &Path) -> Result<i64> {
        // Canonicalize the path to store absolute paths, resolving ".", "..", "~", etc.
        let canonical_path = root_path
            .canonicalize()
            .unwrap_or_else(|_| root_path.to_path_buf());
        let root_path_str = canonical_path.display().to_string();
        let started_at = Utc::now().to_rfc3339();

        // Insert scan record first to get the ID
        let result = sqlx::query(
            "INSERT INTO scans (root_path, started_at, status) VALUES (?, ?, 'running')",
        )
        .bind(&root_path_str)
        .bind(&started_at)
        .execute(&self.pool)
        .await?;

        let scan_id = result.last_insert_rowid();
        let table_name = format!("scan_entries_{}", scan_id);

        // Create per-scan entries table
        let create_table_sql = format!(
            "CREATE TABLE IF NOT EXISTS {} (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL,
                name TEXT NOT NULL,
                parent_path TEXT,
                size INTEGER NOT NULL,
                is_dir INTEGER NOT NULL,
                modified_at TEXT,
                depth INTEGER NOT NULL
            )",
            table_name
        );
        sqlx::query(&create_table_sql).execute(&self.pool).await?;

        // Create indices for the new table
        let create_indices_sql = vec![
            format!(
                "CREATE INDEX idx_{}_path ON {}(path)",
                table_name, table_name
            ),
            format!(
                "CREATE INDEX idx_{}_parent ON {}(parent_path)",
                table_name, table_name
            ),
            format!(
                "CREATE INDEX idx_{}_size ON {}(size DESC)",
                table_name, table_name
            ),
        ];

        for index_sql in create_indices_sql {
            sqlx::query(&index_sql).execute(&self.pool).await?;
        }

        // Update scan record with table name
        sqlx::query("UPDATE scans SET entries_table = ? WHERE id = ?")
            .bind(&table_name)
            .bind(scan_id)
            .execute(&self.pool)
            .await?;

        Ok(scan_id)
    }

    pub async fn complete_scan(&self, scan_id: i64, stats: &ScanStats) -> Result<()> {
        let completed_at = Utc::now().to_rfc3339();

        sqlx::query(
            "UPDATE scans SET completed_at = ?, total_size = ?, total_files = ?, total_dirs = ?, status = 'completed' WHERE id = ?"
        )
        .bind(&completed_at)
        .bind(stats.total_size as i64)
        .bind(stats.total_files as i64)
        .bind(stats.total_dirs as i64)
        .bind(scan_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn pause_scan(&self, scan_id: i64, stats: &ScanStats) -> Result<()> {
        sqlx::query(
            "UPDATE scans SET total_size = ?, total_files = ?, total_dirs = ?, status = 'paused' WHERE id = ?"
        )
        .bind(stats.total_size as i64)
        .bind(stats.total_files as i64)
        .bind(stats.total_dirs as i64)
        .bind(scan_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Calculate scan statistics from file_entries in the database
    /// Useful for recovering stats from interrupted scans
    pub async fn calculate_scan_stats(&self, scan_id: i64) -> Result<ScanStats> {
        let table_name = self.get_entries_table(scan_id).await?;
        let query_str = format!(
            "SELECT
                COALESCE(SUM(size), 0) as total_size,
                COALESCE(SUM(CASE WHEN is_dir = 0 THEN 1 ELSE 0 END), 0) as total_files,
                COALESCE(SUM(CASE WHEN is_dir = 1 THEN 1 ELSE 0 END), 0) as total_dirs
             FROM {}",
            table_name
        );

        let row = sqlx::query(&query_str).fetch_one(&self.pool).await?;

        Ok(ScanStats {
            total_size: row.get::<i64, _>("total_size") as u64,
            total_files: row.get::<i64, _>("total_files") as u64,
            total_dirs: row.get::<i64, _>("total_dirs") as u64,
        })
    }

    /// Get all scanned paths for a given scan_id
    /// Used to skip already-scanned paths when resuming
    pub async fn get_scanned_paths(
        &self,
        scan_id: i64,
    ) -> Result<std::collections::HashSet<String>> {
        let table_name = self.get_entries_table(scan_id).await?;
        let query_str = format!("SELECT path FROM {}", table_name);

        let rows = sqlx::query(&query_str).fetch_all(&self.pool).await?;

        let paths = rows
            .iter()
            .map(|row| row.get::<String, _>("path"))
            .collect();

        Ok(paths)
    }

    pub async fn delete_scan(&self, scan_id: i64) -> Result<()> {
        // Get the entries table name before deleting scan record
        let table_name = self.get_entries_table(scan_id).await?;

        // Drop the per-scan entries table (simple garbage collection!)
        if table_name != "file_entries" {
            let drop_query = format!("DROP TABLE IF EXISTS {}", table_name);
            sqlx::query(&drop_query).execute(&self.pool).await?;
        } else {
            // Legacy fallback: delete from shared table
            sqlx::query("DELETE FROM file_entries WHERE scan_id = ?")
                .bind(scan_id)
                .execute(&self.pool)
                .await?;
        }

        // Delete cleanup items
        sqlx::query("DELETE FROM cleanup_items WHERE scan_id = ?")
            .bind(scan_id)
            .execute(&self.pool)
            .await?;

        // Delete the scan itself
        sqlx::query("DELETE FROM scans WHERE id = ?")
            .bind(scan_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    #[allow(dead_code)]
    pub async fn is_path_scanned(&self, scan_id: i64, path: &str) -> Result<bool> {
        let table_name = self.get_entries_table(scan_id).await?;
        let query_str = format!("SELECT 1 FROM {} WHERE path = ? LIMIT 1", table_name);

        let result = sqlx::query(&query_str)
            .bind(path)
            .fetch_optional(&self.pool)
            .await?;

        Ok(result.is_some())
    }

    pub async fn insert_file_entries(&self, scan_id: i64, entries: &[FileEntry]) -> Result<()> {
        let table_name = self.get_entries_table(scan_id).await?;
        let mut tx = self.pool.begin().await?;

        for entry in entries {
            let path_str = entry.path.display().to_string();
            let parent_str = entry.parent_path.as_ref().map(|p| p.display().to_string());
            let modified_str = entry.modified_at.map(|dt| dt.to_rfc3339());

            let query_str = format!(
                "INSERT INTO {} (path, name, parent_path, size, is_dir, modified_at, depth)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                table_name
            );

            sqlx::query(&query_str)
                .bind(&path_str)
                .bind(&entry.name)
                .bind(&parent_str)
                .bind(entry.size as i64)
                .bind(entry.is_dir)
                .bind(&modified_str)
                .bind(entry.depth as i64)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn list_scans(&self) -> Result<Vec<Scan>> {
        let rows = sqlx::query(
            "SELECT id, root_path, started_at, completed_at, total_size, total_files, total_dirs, status, entries_table
             FROM scans ORDER BY started_at DESC"
        )
        .fetch_all(&self.pool)
        .await?;

        let scans = rows
            .iter()
            .map(|row| {
                let started_at_str: String = row.get("started_at");
                let completed_at_str: Option<String> = row.get("completed_at");

                Scan {
                    id: row.get("id"),
                    root_path: row.get("root_path"),
                    started_at: DateTime::parse_from_rfc3339(&started_at_str)
                        .unwrap()
                        .with_timezone(&Utc),
                    completed_at: completed_at_str.and_then(|s| {
                        DateTime::parse_from_rfc3339(&s)
                            .ok()
                            .map(|dt| dt.with_timezone(&Utc))
                    }),
                    total_size: row.get("total_size"),
                    total_files: row.get("total_files"),
                    total_dirs: row.get("total_dirs"),
                    status: row.get("status"),
                    entries_table: row.get("entries_table"),
                }
            })
            .collect();

        Ok(scans)
    }

    pub async fn get_scan(&self, scan_id: i64) -> Result<Option<Scan>> {
        let row = sqlx::query(
            "SELECT id, root_path, started_at, completed_at, total_size, total_files, total_dirs, status, entries_table
             FROM scans WHERE id = ?"
        )
        .bind(scan_id)
        .fetch_optional(&self.pool)
        .await?;

        let scan = row.map(|row| {
            let started_at_str: String = row.get("started_at");
            let completed_at_str: Option<String> = row.get("completed_at");

            Scan {
                id: row.get("id"),
                root_path: row.get("root_path"),
                started_at: DateTime::parse_from_rfc3339(&started_at_str)
                    .unwrap()
                    .with_timezone(&Utc),
                completed_at: completed_at_str.and_then(|s| {
                    DateTime::parse_from_rfc3339(&s)
                        .ok()
                        .map(|dt| dt.with_timezone(&Utc))
                }),
                total_size: row.get("total_size"),
                total_files: row.get("total_files"),
                total_dirs: row.get("total_dirs"),
                status: row.get("status"),
                entries_table: row.get("entries_table"),
            }
        });

        Ok(scan)
    }

    pub async fn get_largest_entries(
        &self,
        scan_id: i64,
        limit: i64,
    ) -> Result<Vec<StoredFileEntry>> {
        let table_name = self.get_entries_table(scan_id).await?;
        let query_str = format!(
            "SELECT id, path, name, parent_path, size, is_dir, modified_at, depth
             FROM {} ORDER BY size DESC LIMIT ?",
            table_name
        );

        let rows = sqlx::query(&query_str)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        let entries = rows
            .iter()
            .map(|row| Self::row_to_entry(row, scan_id))
            .collect();

        Ok(entries)
    }

    pub async fn get_root_entry(&self, scan_id: i64) -> Result<Option<StoredFileEntry>> {
        let table_name = self.get_entries_table(scan_id).await?;
        let query_str = format!(
            "SELECT id, path, name, parent_path, size, is_dir, modified_at, depth
             FROM {} WHERE depth = 0 LIMIT 1",
            table_name
        );

        let row = sqlx::query(&query_str).fetch_optional(&self.pool).await?;

        Ok(row.as_ref().map(|r| Self::row_to_entry(r, scan_id)))
    }

    pub async fn get_entries_by_parent(
        &self,
        scan_id: i64,
        parent_path: Option<&str>,
    ) -> Result<Vec<StoredFileEntry>> {
        let table_name = self.get_entries_table(scan_id).await?;

        let rows = if let Some(parent) = parent_path {
            let query_str = format!(
                "SELECT id, path, name, parent_path, size, is_dir, modified_at, depth
                 FROM {} WHERE parent_path = ? ORDER BY size DESC",
                table_name
            );
            sqlx::query(&query_str)
                .bind(parent)
                .fetch_all(&self.pool)
                .await?
        } else {
            let query_str = format!(
                "SELECT id, path, name, parent_path, size, is_dir, modified_at, depth
                 FROM {} WHERE parent_path IS NULL ORDER BY size DESC",
                table_name
            );
            sqlx::query(&query_str).fetch_all(&self.pool).await?
        };

        let entries = rows
            .iter()
            .map(|row| Self::row_to_entry(row, scan_id))
            .collect();

        Ok(entries)
    }

    /// Get all descendants of a directory recursively (all files/dirs under the parent path)
    pub async fn get_all_descendants(
        &self,
        scan_id: i64,
        parent_path: &str,
    ) -> Result<Vec<StoredFileEntry>> {
        let table_name = self.get_entries_table(scan_id).await?;
        // Use path prefix matching to get all descendants
        // We match paths that start with "parent_path/"
        let prefix_pattern = format!("{}/%", parent_path);

        let query_str = format!(
            "SELECT id, path, name, parent_path, size, is_dir, modified_at, depth
             FROM {}
             WHERE path LIKE ? OR parent_path = ?
             ORDER BY path",
            table_name
        );

        let rows = sqlx::query(&query_str)
            .bind(&prefix_pattern)
            .bind(parent_path)
            .fetch_all(&self.pool)
            .await?;

        let entries = rows
            .iter()
            .map(|row| Self::row_to_entry(row, scan_id))
            .collect();

        Ok(entries)
    }

    pub async fn mark_for_cleanup(
        &self,
        scan_id: i64,
        entry_path: &str,
        reason: Option<&str>,
    ) -> Result<()> {
        let marked_at = Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT OR IGNORE INTO cleanup_items (scan_id, entry_path, marked_at, reason) VALUES (?, ?, ?, ?)"
        )
        .bind(scan_id)
        .bind(entry_path)
        .bind(&marked_at)
        .bind(reason)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_cleanup_items(&self, scan_id: i64) -> Result<Vec<StoredFileEntry>> {
        let table_name = self.get_entries_table(scan_id).await?;

        // Get cleanup paths
        let cleanup_paths: Vec<String> =
            sqlx::query_scalar("SELECT entry_path FROM cleanup_items WHERE scan_id = ?")
                .bind(scan_id)
                .fetch_all(&self.pool)
                .await?;

        if cleanup_paths.is_empty() {
            return Ok(vec![]);
        }

        // Build query to get entries for those paths
        let placeholders = cleanup_paths
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let query_str = format!(
            "SELECT id, path, name, parent_path, size, is_dir, modified_at, depth
             FROM {}
             WHERE path IN ({})
             ORDER BY size DESC",
            table_name, placeholders
        );

        let mut query = sqlx::query(&query_str);
        for path in &cleanup_paths {
            query = query.bind(path);
        }

        let rows = query.fetch_all(&self.pool).await?;

        let entries = rows
            .iter()
            .map(|row| Self::row_to_entry(row, scan_id))
            .collect();

        Ok(entries)
    }

    pub async fn remove_cleanup_item(&self, scan_id: i64, entry_path: &str) -> Result<()> {
        sqlx::query("DELETE FROM cleanup_items WHERE scan_id = ? AND entry_path = ?")
            .bind(scan_id)
            .bind(entry_path)
            .execute(&self.pool)
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    async fn create_test_db() -> Database {
        // Use in-memory database for testing
        let options = SqliteConnectOptions::from_str("sqlite::memory:")
            .unwrap()
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();

        // Manually create schema for tests (don't rely on migration files)
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS scans (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                root_path TEXT NOT NULL,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                total_size INTEGER NOT NULL DEFAULT 0,
                total_files INTEGER NOT NULL DEFAULT 0,
                total_dirs INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL DEFAULT 'running' CHECK(status IN ('running', 'completed', 'failed', 'paused')),
                entries_table TEXT
            );
            CREATE INDEX idx_scans_started_at ON scans(started_at DESC);
            CREATE INDEX idx_scans_root_path ON scans(root_path);

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
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        Database { pool }
    }

    fn create_test_entry(name: &str, size: u64, is_dir: bool) -> FileEntry {
        FileEntry {
            path: PathBuf::from(format!("/test/{}", name)),
            name: name.to_string(),
            parent_path: Some(PathBuf::from("/test")),
            size,
            is_dir,
            modified_at: Some(Utc::now()),
            depth: 1,
        }
    }

    #[tokio::test]
    async fn test_create_and_get_scan() {
        let db = create_test_db().await;
        let path = PathBuf::from("/test/path");

        let scan_id = db.create_scan(&path).await.unwrap();
        assert!(scan_id > 0);

        let scan = db.get_scan(scan_id).await.unwrap();
        assert!(scan.is_some());

        let scan = scan.unwrap();
        assert_eq!(scan.id, scan_id);
        assert_eq!(scan.root_path, "/test/path");
        assert_eq!(scan.status, "running");
    }

    #[tokio::test]
    async fn test_complete_scan() {
        let db = create_test_db().await;
        let path = PathBuf::from("/test/path");

        let scan_id = db.create_scan(&path).await.unwrap();

        let stats = ScanStats {
            total_size: 1000,
            total_files: 10,
            total_dirs: 5,
        };

        db.complete_scan(scan_id, &stats).await.unwrap();

        let scan = db.get_scan(scan_id).await.unwrap().unwrap();
        assert_eq!(scan.status, "completed");
        assert_eq!(scan.total_size, 1000);
        assert_eq!(scan.total_files, 10);
        assert_eq!(scan.total_dirs, 5);
        assert!(scan.completed_at.is_some());
    }

    #[tokio::test]
    async fn test_insert_and_retrieve_entries() {
        let db = create_test_db().await;
        let path = PathBuf::from("/test");
        let scan_id = db.create_scan(&path).await.unwrap();

        let entries = vec![
            create_test_entry("file1.txt", 100, false),
            create_test_entry("file2.txt", 200, false),
            create_test_entry("dir1", 300, true),
        ];

        db.insert_file_entries(scan_id, &entries).await.unwrap();

        let largest = db.get_largest_entries(scan_id, 10).await.unwrap();
        assert_eq!(largest.len(), 3);

        // Should be sorted by size descending
        assert_eq!(largest[0].size, 300);
        assert_eq!(largest[1].size, 200);
        assert_eq!(largest[2].size, 100);
    }

    #[tokio::test]
    async fn test_list_scans() {
        let db = create_test_db().await;

        let scan1_id = db.create_scan(&PathBuf::from("/test1")).await.unwrap();
        let scan2_id = db.create_scan(&PathBuf::from("/test2")).await.unwrap();

        let scans = db.list_scans().await.unwrap();
        assert_eq!(scans.len(), 2);

        // Should be ordered by started_at DESC (most recent first)
        assert_eq!(scans[0].id, scan2_id);
        assert_eq!(scans[1].id, scan1_id);
    }

    #[tokio::test]
    async fn test_cleanup_items() {
        let db = create_test_db().await;
        let path = PathBuf::from("/test");
        let scan_id = db.create_scan(&path).await.unwrap();

        let entries = vec![
            create_test_entry("file1.txt", 100, false),
            create_test_entry("file2.txt", 200, false),
        ];

        db.insert_file_entries(scan_id, &entries).await.unwrap();

        // Get entry paths
        let stored_entries = db.get_largest_entries(scan_id, 10).await.unwrap();
        let entry1_path = stored_entries[1].path.clone(); // Smaller file
        let entry2_path = stored_entries[0].path.clone(); // Larger file

        // Mark for cleanup
        db.mark_for_cleanup(scan_id, &entry1_path, Some("test reason"))
            .await
            .unwrap();
        db.mark_for_cleanup(scan_id, &entry2_path, None)
            .await
            .unwrap();

        // Retrieve cleanup items
        let cleanup_items = db.get_cleanup_items(scan_id).await.unwrap();
        assert_eq!(cleanup_items.len(), 2);

        // Remove one cleanup item
        db.remove_cleanup_item(scan_id, &entry1_path).await.unwrap();

        let cleanup_items = db.get_cleanup_items(scan_id).await.unwrap();
        assert_eq!(cleanup_items.len(), 1);
        assert_eq!(cleanup_items[0].path, entry2_path);
    }

    #[tokio::test]
    async fn test_database_actor() {
        let db = create_test_db().await;
        let path = PathBuf::from("/test");
        let scan_id = db.create_scan(&path).await.unwrap();

        let (tx, rx) = tokio::sync::mpsc::channel(100);

        let actor = DatabaseActor::new(db.clone(), scan_id, rx);
        let actor_handle = tokio::spawn(async move { actor.run().await });

        // Send some entries
        let entries = vec![
            create_test_entry("file1.txt", 100, false),
            create_test_entry("file2.txt", 200, false),
        ];
        tx.send(ActorMessage::InsertBatch(entries)).await.unwrap();

        // Send shutdown
        tx.send(ActorMessage::Shutdown).await.unwrap();
        drop(tx);

        // Wait for actor to finish
        actor_handle.await.unwrap().unwrap();

        // Verify entries were inserted
        let stored = db.get_largest_entries(scan_id, 10).await.unwrap();
        assert_eq!(stored.len(), 2);
    }

    #[tokio::test]
    async fn test_batched_inserts() {
        let db = create_test_db().await;
        let path = PathBuf::from("/test");
        let scan_id = db.create_scan(&path).await.unwrap();

        // Create many entries to test batching
        let mut entries = Vec::new();
        for i in 0..1000 {
            entries.push(create_test_entry(
                &format!("file{}.txt", i),
                i as u64,
                false,
            ));
        }

        db.insert_file_entries(scan_id, &entries).await.unwrap();

        let stored = db.get_largest_entries(scan_id, 10).await.unwrap();
        assert_eq!(stored.len(), 10);
        assert_eq!(stored[0].size, 999); // Largest should be first
    }

    #[tokio::test]
    async fn test_calculate_scan_stats() {
        let db = create_test_db().await;
        let path = PathBuf::from("/test");
        let scan_id = db.create_scan(&path).await.unwrap();

        // Insert some test entries
        let entries = vec![
            create_test_entry("file1.txt", 100, false),
            create_test_entry("file2.txt", 200, false),
            create_test_entry("file3.txt", 300, false),
            create_test_entry("dir1", 0, true),
            create_test_entry("dir2", 0, true),
        ];
        db.insert_file_entries(scan_id, &entries).await.unwrap();

        // Calculate stats from database
        let stats = db.calculate_scan_stats(scan_id).await.unwrap();

        assert_eq!(stats.total_files, 3);
        assert_eq!(stats.total_dirs, 2);
        assert_eq!(stats.total_size, 600); // 100 + 200 + 300
    }

    #[tokio::test]
    async fn test_orphaned_scan_recovery() {
        let db = create_test_db().await;
        let path = PathBuf::from("/test");
        let scan_id = db.create_scan(&path).await.unwrap();

        // Verify scan starts as "running"
        let scan = db.get_scan(scan_id).await.unwrap().unwrap();
        assert_eq!(scan.status, "running");
        assert_eq!(scan.total_files, 0);
        assert_eq!(scan.total_dirs, 0);

        // Simulate a scan that was interrupted - it has file entries but status is still "running"
        let entries = vec![
            create_test_entry("file1.txt", 1024, false),
            create_test_entry("file2.txt", 2048, false),
            create_test_entry("dir1", 0, true),
        ];
        db.insert_file_entries(scan_id, &entries).await.unwrap();

        // Simulate what UI does on load: detect orphaned scan and recover it
        let scans = db.list_scans().await.unwrap();
        for scan in &scans {
            if scan.status == "running" && scan.completed_at.is_none() {
                // Calculate stats from what's already been scanned
                let stats = db.calculate_scan_stats(scan.id).await.unwrap();
                db.pause_scan(scan.id, &stats).await.unwrap();
            }
        }

        // Verify the scan is now paused with correct stats
        let scan = db.get_scan(scan_id).await.unwrap().unwrap();
        assert_eq!(scan.status, "paused");
        assert_eq!(scan.total_files, 2);
        assert_eq!(scan.total_dirs, 1);
        assert_eq!(scan.total_size, 3072); // 1024 + 2048
        assert!(scan.completed_at.is_none());
    }
}
