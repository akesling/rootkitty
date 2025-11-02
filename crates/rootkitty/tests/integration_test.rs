use rootkitty::db::{ActorMessage, Database, DatabaseActor};
use rootkitty::scanner::{ProgressUpdate, Scanner};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::fs;
use std::str::FromStr;
use tempfile::TempDir;
use tokio::sync::mpsc;

fn create_test_filesystem() -> TempDir {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create a nested directory structure for testing
    fs::write(root.join("file1.txt"), b"hello world").unwrap();
    fs::write(root.join("file2.txt"), b"test data").unwrap();

    fs::create_dir(root.join("subdir")).unwrap();
    fs::write(root.join("subdir/file3.txt"), b"nested file").unwrap();
    fs::write(root.join("subdir/large.bin"), vec![0u8; 1024]).unwrap();

    fs::create_dir(root.join("subdir/nested")).unwrap();
    fs::write(root.join("subdir/nested/deep.txt"), b"deep file content").unwrap();

    fs::create_dir(root.join("empty_dir")).unwrap();

    temp_dir
}

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

    // Manually create schema for tests
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
            status TEXT NOT NULL DEFAULT 'running' CHECK(status IN ('running', 'completed', 'failed'))
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
            file_entry_id INTEGER NOT NULL,
            marked_at TEXT NOT NULL,
            reason TEXT,
            FOREIGN KEY (scan_id) REFERENCES scans(id) ON DELETE CASCADE,
            FOREIGN KEY (file_entry_id) REFERENCES file_entries(id) ON DELETE CASCADE,
            UNIQUE(scan_id, file_entry_id)
        );
        CREATE INDEX idx_cleanup_items_scan_id ON cleanup_items(scan_id);
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    Database::from_pool(pool)
}

#[tokio::test]
async fn test_full_scan_workflow() {
    let temp_fs = create_test_filesystem();
    let db = create_test_db().await;

    let scan_id = db.create_scan(temp_fs.path()).await.unwrap();

    // Create channels
    let (tx, rx) = mpsc::channel(100);
    let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressUpdate>();

    // Spawn database actor
    let actor = DatabaseActor::new(db.clone(), scan_id, rx);
    let actor_handle = tokio::spawn(async move { actor.run().await });

    // Spawn progress collector
    let progress_handle = tokio::spawn(async move {
        let mut count = 0;
        while progress_rx.recv().await.is_some() {
            count += 1;
        }
        count
    });

    // Perform scan in blocking thread
    let tx_clone = tx.clone();
    let path_clone = temp_fs.path().to_path_buf();
    let scan_result = tokio::task::spawn_blocking(move || {
        let scanner = Scanner::with_sender(&path_clone, tx_clone, Some(progress_tx));
        scanner.scan()
    })
    .await
    .unwrap();

    let (_entries, stats) = scan_result.unwrap();

    // Signal shutdown
    tx.send(ActorMessage::Shutdown).await.unwrap();
    drop(tx);

    // Wait for actor to finish
    actor_handle.await.unwrap().unwrap();

    // Complete the scan
    db.complete_scan(scan_id, &stats).await.unwrap();

    // Verify scan was saved correctly
    let scan = db.get_scan(scan_id).await.unwrap().unwrap();
    assert_eq!(scan.status, "completed");
    assert!(scan.total_files >= 5); // At least 5 files
    assert!(scan.total_dirs >= 4); // At least 4 directories
    assert!(scan.total_size > 0);

    // Verify entries were stored
    let entries = db.get_largest_entries(scan_id, 100).await.unwrap();
    assert!(!entries.is_empty());
    assert_eq!(
        entries.len() as i64,
        stats.total_files as i64 + stats.total_dirs as i64
    );

    // Verify largest entry is the large.bin file
    let largest = &entries[0];
    assert!(largest.name.contains("large") || largest.is_dir);
    assert!(largest.size >= 1024);

    // Wait for progress collector
    let progress_count = progress_handle.await.unwrap();
    println!("Received {} progress updates", progress_count);
}

#[tokio::test]
async fn test_concurrent_scans() {
    let temp_fs1 = create_test_filesystem();
    let temp_fs2 = create_test_filesystem();
    let db = create_test_db().await;

    // Create two scans concurrently
    let scan1_id = db.create_scan(temp_fs1.path()).await.unwrap();
    let scan2_id = db.create_scan(temp_fs2.path()).await.unwrap();

    // Set up actors for both scans
    let (tx1, rx1) = mpsc::channel(100);
    let (tx2, rx2) = mpsc::channel(100);

    let actor1 = DatabaseActor::new(db.clone(), scan1_id, rx1);
    let actor2 = DatabaseActor::new(db.clone(), scan2_id, rx2);

    let handle1 = tokio::spawn(async move { actor1.run().await });
    let handle2 = tokio::spawn(async move { actor2.run().await });

    // Run scans concurrently
    let path1 = temp_fs1.path().to_path_buf();
    let path2 = temp_fs2.path().to_path_buf();

    let (result1, result2) = tokio::join!(
        tokio::task::spawn_blocking(move || {
            let scanner = Scanner::with_sender(&path1, tx1, None);
            scanner.scan()
        }),
        tokio::task::spawn_blocking(move || {
            let scanner = Scanner::with_sender(&path2, tx2, None);
            scanner.scan()
        })
    );

    let (_, stats1) = result1.unwrap().unwrap();
    let (_, stats2) = result2.unwrap().unwrap();

    // Complete scans
    db.complete_scan(scan1_id, &stats1).await.unwrap();
    db.complete_scan(scan2_id, &stats2).await.unwrap();

    // Wait for actors
    handle1.await.unwrap().unwrap();
    handle2.await.unwrap().unwrap();

    // Verify both scans are in database
    let scans = db.list_scans().await.unwrap();
    assert_eq!(scans.len(), 2);

    // Verify entries are correctly associated with their scans
    let entries1 = db.get_largest_entries(scan1_id, 100).await.unwrap();
    let entries2 = db.get_largest_entries(scan2_id, 100).await.unwrap();

    assert!(!entries1.is_empty());
    assert!(!entries2.is_empty());
    assert_eq!(entries1.len(), entries2.len()); // Same filesystem structure
}

#[tokio::test]
async fn test_cleanup_workflow() {
    let temp_fs = create_test_filesystem();
    let db = create_test_db().await;

    let scan_id = db.create_scan(temp_fs.path()).await.unwrap();

    let (tx, rx) = mpsc::channel(100);
    let actor = DatabaseActor::new(db.clone(), scan_id, rx);
    let actor_handle = tokio::spawn(async move { actor.run().await });

    let path_clone = temp_fs.path().to_path_buf();
    let scan_result = tokio::task::spawn_blocking(move || {
        let scanner = Scanner::with_sender(&path_clone, tx, None);
        scanner.scan()
    })
    .await
    .unwrap();

    let (_, stats) = scan_result.unwrap();

    actor_handle.await.unwrap().unwrap();
    db.complete_scan(scan_id, &stats).await.unwrap();

    // Get some entries to mark for cleanup
    let entries = db.get_largest_entries(scan_id, 3).await.unwrap();
    assert!(entries.len() >= 2);

    // Mark first two entries for cleanup
    db.mark_for_cleanup(scan_id, entries[0].id, Some("Too large"))
        .await
        .unwrap();
    db.mark_for_cleanup(scan_id, entries[1].id, Some("Old file"))
        .await
        .unwrap();

    // Retrieve cleanup items
    let cleanup = db.get_cleanup_items(scan_id).await.unwrap();
    assert_eq!(cleanup.len(), 2);

    // Verify they're sorted by size descending
    assert!(cleanup[0].size >= cleanup[1].size);

    // Remove one from cleanup
    db.remove_cleanup_item(scan_id, entries[0].id)
        .await
        .unwrap();

    let cleanup = db.get_cleanup_items(scan_id).await.unwrap();
    assert_eq!(cleanup.len(), 1);
    assert_eq!(cleanup[0].id, entries[1].id);
}

#[tokio::test]
async fn test_large_batch_processing() {
    let temp_dir = TempDir::new().unwrap();
    let root = temp_dir.path();

    // Create many files to test batching
    for i in 0..2500 {
        fs::write(
            root.join(format!("file{}.txt", i)),
            format!("content {}", i),
        )
        .unwrap();
    }

    let db = create_test_db().await;
    let scan_id = db.create_scan(root).await.unwrap();

    let (tx, rx) = mpsc::channel(100);
    let actor = DatabaseActor::new(db.clone(), scan_id, rx);
    let actor_handle = tokio::spawn(async move { actor.run().await });

    let path_clone = root.to_path_buf();
    let scan_result = tokio::task::spawn_blocking(move || {
        let scanner = Scanner::with_sender(&path_clone, tx, None);
        scanner.scan()
    })
    .await
    .unwrap();

    let (_, stats) = scan_result.unwrap();

    actor_handle.await.unwrap().unwrap();
    db.complete_scan(scan_id, &stats).await.unwrap();

    // Verify all files were stored
    assert_eq!(stats.total_files, 2500);

    // Should be able to query largest entries
    let largest = db.get_largest_entries(scan_id, 100).await.unwrap();
    assert_eq!(largest.len(), 100);
}
