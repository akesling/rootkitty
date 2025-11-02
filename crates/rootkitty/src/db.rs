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
}

#[derive(Debug, Clone)]
pub struct StoredFileEntry {
    pub id: i64,
    pub scan_id: i64,
    pub path: String,
    pub name: String,
    pub parent_path: Option<String>,
    pub size: i64,
    pub is_dir: bool,
    pub modified_at: Option<DateTime<Utc>>,
    pub depth: i64,
}

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
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
        let root_path_str = root_path.display().to_string();
        let started_at = Utc::now().to_rfc3339();

        let result = sqlx::query(
            "INSERT INTO scans (root_path, started_at, status) VALUES (?, ?, 'running')",
        )
        .bind(&root_path_str)
        .bind(&started_at)
        .execute(&self.pool)
        .await?;

        Ok(result.last_insert_rowid())
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

    pub async fn insert_file_entries(&self, scan_id: i64, entries: &[FileEntry]) -> Result<()> {
        let mut tx = self.pool.begin().await?;

        for entry in entries {
            let path_str = entry.path.display().to_string();
            let parent_str = entry.parent_path.as_ref().map(|p| p.display().to_string());
            let modified_str = entry.modified_at.map(|dt| dt.to_rfc3339());

            sqlx::query(
                "INSERT INTO file_entries (scan_id, path, name, parent_path, size, is_dir, modified_at, depth)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(scan_id)
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
            "SELECT id, root_path, started_at, completed_at, total_size, total_files, total_dirs, status
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
                }
            })
            .collect();

        Ok(scans)
    }

    pub async fn get_scan(&self, scan_id: i64) -> Result<Option<Scan>> {
        let row = sqlx::query(
            "SELECT id, root_path, started_at, completed_at, total_size, total_files, total_dirs, status
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
            }
        });

        Ok(scan)
    }

    pub async fn get_largest_entries(
        &self,
        scan_id: i64,
        limit: i64,
    ) -> Result<Vec<StoredFileEntry>> {
        let rows = sqlx::query(
            "SELECT id, scan_id, path, name, parent_path, size, is_dir, modified_at, depth
             FROM file_entries WHERE scan_id = ? ORDER BY size DESC LIMIT ?",
        )
        .bind(scan_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let entries = rows
            .iter()
            .map(|row| {
                let modified_at_str: Option<String> = row.get("modified_at");

                StoredFileEntry {
                    id: row.get("id"),
                    scan_id: row.get("scan_id"),
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
            })
            .collect();

        Ok(entries)
    }

    pub async fn get_entries_by_parent(
        &self,
        scan_id: i64,
        parent_path: Option<&str>,
    ) -> Result<Vec<StoredFileEntry>> {
        let rows = if let Some(parent) = parent_path {
            sqlx::query(
                "SELECT id, scan_id, path, name, parent_path, size, is_dir, modified_at, depth
                 FROM file_entries WHERE scan_id = ? AND parent_path = ? ORDER BY size DESC",
            )
            .bind(scan_id)
            .bind(parent)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, scan_id, path, name, parent_path, size, is_dir, modified_at, depth
                 FROM file_entries WHERE scan_id = ? AND parent_path IS NULL ORDER BY size DESC",
            )
            .bind(scan_id)
            .fetch_all(&self.pool)
            .await?
        };

        let entries = rows
            .iter()
            .map(|row| {
                let modified_at_str: Option<String> = row.get("modified_at");

                StoredFileEntry {
                    id: row.get("id"),
                    scan_id: row.get("scan_id"),
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
            })
            .collect();

        Ok(entries)
    }

    pub async fn mark_for_cleanup(
        &self,
        scan_id: i64,
        file_entry_id: i64,
        reason: Option<&str>,
    ) -> Result<()> {
        let marked_at = Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT OR IGNORE INTO cleanup_items (scan_id, file_entry_id, marked_at, reason) VALUES (?, ?, ?, ?)"
        )
        .bind(scan_id)
        .bind(file_entry_id)
        .bind(&marked_at)
        .bind(reason)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    pub async fn get_cleanup_items(&self, scan_id: i64) -> Result<Vec<StoredFileEntry>> {
        let rows = sqlx::query(
            "SELECT fe.id, fe.scan_id, fe.path, fe.name, fe.parent_path, fe.size, fe.is_dir, fe.modified_at, fe.depth
             FROM file_entries fe
             INNER JOIN cleanup_items ci ON fe.id = ci.file_entry_id
             WHERE ci.scan_id = ?
             ORDER BY fe.size DESC"
        )
        .bind(scan_id)
        .fetch_all(&self.pool)
        .await?;

        let entries = rows
            .iter()
            .map(|row| {
                let modified_at_str: Option<String> = row.get("modified_at");

                StoredFileEntry {
                    id: row.get("id"),
                    scan_id: row.get("scan_id"),
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
            })
            .collect();

        Ok(entries)
    }

    pub async fn remove_cleanup_item(&self, scan_id: i64, file_entry_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM cleanup_items WHERE scan_id = ? AND file_entry_id = ?")
            .bind(scan_id)
            .bind(file_entry_id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }
}
