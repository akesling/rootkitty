// Quick test to verify orphaned scan recovery
use rootkitty::db::Database;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db_path = shellexpand::tilde("~/.config/rootkitty/rootkitty.db");
    let db = Database::new(db_path.as_ref()).await?;

    println!("Before recovery:");
    let scans = db.list_scans().await?;
    for scan in &scans {
        if scan.id == 9 {
            println!("  Scan {}: status={}, files={}, dirs={}",
                scan.id, scan.status, scan.total_files, scan.total_dirs);
        }
    }

    // Simulate what the UI does on load
    for scan in &scans {
        if scan.status == "running" && scan.completed_at.is_none() {
            println!("\nRecovering orphaned scan {}...", scan.id);
            let stats = db.calculate_scan_stats(scan.id).await?;
            println!("  Calculated stats: {} files, {} dirs, {} bytes",
                stats.total_files, stats.total_dirs, stats.total_size);
            db.pause_scan(scan.id, &stats).await?;
        }
    }

    println!("\nAfter recovery:");
    let scans = db.list_scans().await?;
    for scan in &scans {
        if scan.id == 9 {
            println!("  Scan {}: status={}, files={}, dirs={}",
                scan.id, scan.status, scan.total_files, scan.total_dirs);
        }
    }

    Ok(())
}
