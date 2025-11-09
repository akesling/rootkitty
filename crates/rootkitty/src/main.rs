mod db;
mod scanner;
mod settings;
mod ui;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::db::{ActorMessage, Database, DatabaseActor};
use crate::scanner::{ProgressUpdate, Scanner};
use crate::settings::Settings;
use crate::ui::App;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(name = "rootkitty")]
#[command(about = "A blazingly fast disk usage analyzer TUI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to database file
    #[arg(short, long, default_value = "~/.config/rootkitty/rootkitty.db")]
    db: String,

    /// Path to settings file
    #[arg(short = 'c', long)]
    config: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan a directory and store results
    Scan {
        /// Path to scan
        path: PathBuf,
    },
    /// Run a demo scan (simulated, no real filesystem access)
    DemoScan,
    /// Launch the interactive TUI
    Browse,
    /// List all scans
    List,
    /// Show details of a specific scan
    Show {
        /// Scan ID
        scan_id: i64,
    },
    /// Compare two scans
    Diff {
        /// First scan ID
        scan_id_1: i64,
        /// Second scan ID
        scan_id_2: i64,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let db_path = shellexpand::tilde(&cli.db).to_string();
    let db = Database::new(&db_path)
        .await
        .context("Failed to open database")?;

    match cli.command {
        None => {
            // Default to TUI if no command provided
            // Load settings
            let settings_path = if let Some(config) = &cli.config {
                PathBuf::from(shellexpand::tilde(config).to_string())
            } else {
                Settings::default_path()
            };

            let settings = Settings::load(&settings_path).context("Failed to load settings")?;

            let mut app = App::new(db, settings, settings_path, PathBuf::from(&db_path));
            app.run().await?;
        }
        Some(Commands::DemoScan) => {
            println!("Running demo scan (in-memory database)...");
            // Use in-memory database for demo
            let demo_db = Database::new(":memory:").await?;
            let demo_path = PathBuf::from("/demo");
            let scan_id = demo_db.create_scan(&demo_path).await?;

            // Create channel for streaming entries to database actor
            let (tx, rx) = mpsc::channel(100);

            // Create channel for progress updates
            let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressUpdate>();

            // Spawn database actor to handle inserts
            let actor = DatabaseActor::new(demo_db.clone(), scan_id, rx);
            let actor_handle = tokio::spawn(async move { actor.run().await });

            // Spawn progress monitor task
            let progress_handle = tokio::spawn(async move {
                while let Some(progress) = progress_rx.recv().await {
                    // Clear previous output (up to 5 lines)
                    print!("\r\x1B[J"); // Clear from cursor to end of screen

                    // Show summary line
                    print!("Progress: {} entries scanned", progress.files_scanned);

                    if progress.active_workers > 0 {
                        print!(" | {} parallel workers", progress.active_workers);
                    }
                    println!();

                    // Show active directories (limit to top 4 for readability)
                    let max_display = 4;
                    for (dir_path, done, total) in progress.active_dirs.iter().take(max_display) {
                        let percentage = if *total > 0 {
                            (*done as f64 / *total as f64 * 100.0) as usize
                        } else {
                            0
                        };

                        // Show intelligently truncated path
                        let display_path = smart_truncate_path(dir_path, 70);

                        println!("  [{}/{}] {:>3}% {}", done, total, percentage, display_path);
                    }

                    if progress.active_dirs.len() > max_display {
                        println!(
                            "  ... and {} more directories",
                            progress.active_dirs.len() - max_display
                        );
                    }

                    std::io::Write::flush(&mut std::io::stdout()).ok();
                }
            });

            // Run demo scan (runs in blocking thread)
            let tx_clone = tx.clone();
            let cancelled = Arc::new(AtomicBool::new(false));
            let cancelled_clone = cancelled.clone();
            let scan_result = tokio::task::spawn_blocking(move || {
                let scanner = Scanner::with_sender_demo(
                    &demo_path,
                    tx_clone,
                    Some(progress_tx),
                    cancelled_clone,
                );
                scanner.scan()
            })
            .await?;

            let (_, stats) = scan_result?;

            // Wait for progress task to finish
            drop(progress_handle);

            // Clear progress output and show completion
            print!("\r\x1B[J"); // Clear from cursor to end of screen
            println!("Demo scan complete!");
            println!("  Files: {}", stats.total_files);
            println!("  Directories: {}", stats.total_dirs);
            println!("  Total size: {} bytes", stats.total_size);

            // Signal actor to shutdown and wait for it to finish
            tx.send(ActorMessage::Shutdown).await?;
            drop(tx);

            println!("Waiting for database writes to complete...");
            actor_handle.await??;

            demo_db.complete_scan(scan_id, &stats).await?;
            println!("Demo scan {} saved to in-memory database", scan_id);
        }
        Some(Commands::Scan { path }) => {
            println!("Scanning: {}", path.display());
            let scan_id = db.create_scan(&path).await?;

            // Create channel for streaming entries to database actor
            let (tx, rx) = mpsc::channel(100);

            // Create channel for progress updates
            let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressUpdate>();

            // Spawn database actor to handle inserts
            let actor = DatabaseActor::new(db.clone(), scan_id, rx);
            let actor_handle = tokio::spawn(async move { actor.run().await });

            // Spawn progress monitor task
            let progress_handle = tokio::spawn(async move {
                while let Some(progress) = progress_rx.recv().await {
                    // Clear previous output (up to 5 lines)
                    print!("\r\x1B[J"); // Clear from cursor to end of screen

                    // Show summary line
                    print!("Progress: {} entries scanned", progress.files_scanned);

                    if progress.active_workers > 0 {
                        print!(" | {} parallel workers", progress.active_workers);
                    }
                    println!();

                    // Show active directories (limit to top 4 for readability)
                    let max_display = 4;
                    for (dir_path, done, total) in progress.active_dirs.iter().take(max_display) {
                        let percentage = if *total > 0 {
                            (*done as f64 / *total as f64 * 100.0) as usize
                        } else {
                            0
                        };

                        // Show intelligently truncated path
                        let display_path = smart_truncate_path(dir_path, 70);

                        println!("  [{}/{}] {:>3}% {}", done, total, percentage, display_path);
                    }

                    if progress.active_dirs.len() > max_display {
                        println!(
                            "  ... and {} more directories",
                            progress.active_dirs.len() - max_display
                        );
                    }

                    std::io::Write::flush(&mut std::io::stdout()).ok();
                }
            });

            // Scan with streaming (runs in blocking thread to not block tokio runtime)
            let tx_clone = tx.clone();
            let path_clone = path.clone();
            let cancelled = Arc::new(AtomicBool::new(false));
            let cancelled_clone = cancelled.clone();
            let scan_result = tokio::task::spawn_blocking(move || {
                let scanner =
                    Scanner::with_sender(&path_clone, tx_clone, Some(progress_tx), cancelled_clone);
                scanner.scan()
            })
            .await?;

            let (_, stats) = scan_result?;

            // Wait for progress task to finish
            drop(progress_handle);

            // Clear progress output and show completion
            print!("\r\x1B[J"); // Clear from cursor to end of screen
            println!("Scan complete!");
            println!("  Files: {}", stats.total_files);
            println!("  Directories: {}", stats.total_dirs);
            println!("  Total size: {} bytes", stats.total_size);

            // Signal actor to shutdown and wait for it to finish
            tx.send(ActorMessage::Shutdown).await?;
            drop(tx);

            println!("Waiting for database writes to complete...");
            actor_handle.await??;

            db.complete_scan(scan_id, &stats).await?;
            println!("Scan {} saved to database", scan_id);
        }
        Some(Commands::Browse) => {
            // Load settings
            let settings_path = if let Some(config) = &cli.config {
                PathBuf::from(shellexpand::tilde(config).to_string())
            } else {
                Settings::default_path()
            };

            let settings = Settings::load(&settings_path).context("Failed to load settings")?;

            let mut app = App::new(db, settings, settings_path, PathBuf::from(&db_path));
            app.run().await?;
        }
        Some(Commands::List) => {
            let scans = db.list_scans().await?;
            if scans.is_empty() {
                println!("No scans found. Run 'rootkitty scan <path>' to create one.");
            } else {
                println!(
                    "{:<5} {:<40} {:<12} {:<12} {:<20}",
                    "ID", "Path", "Files", "Size (MB)", "Date"
                );
                println!("{}", "-".repeat(90));
                for scan in scans {
                    let size_mb = scan.total_size as f64 / 1_048_576.0;
                    println!(
                        "{:<5} {:<40} {:<12} {:<12.2} {:<20}",
                        scan.id,
                        scan.root_path,
                        scan.total_files,
                        size_mb,
                        scan.started_at.format("%Y-%m-%d %H:%M:%S")
                    );
                }
            }
        }
        Some(Commands::Show { scan_id }) => {
            let scan = db.get_scan(scan_id).await?;
            if let Some(scan) = scan {
                println!("Scan ID: {}", scan.id);
                println!("Root path: {}", scan.root_path);
                println!("Started: {}", scan.started_at.format("%Y-%m-%d %H:%M:%S"));
                if let Some(completed) = scan.completed_at {
                    println!("Completed: {}", completed.format("%Y-%m-%d %H:%M:%S"));
                }
                println!("Status: {}", scan.status);
                println!("Files: {}", scan.total_files);
                println!("Directories: {}", scan.total_dirs);
                println!("Total size: {:.2} MB", scan.total_size as f64 / 1_048_576.0);

                println!("\nLargest files:");
                let entries = db.get_largest_entries(scan_id, 20).await?;
                for entry in entries {
                    let size_str = format_size(entry.size as u64);
                    let type_icon = if entry.is_dir { "📁" } else { "📄" };
                    println!("  {} {} ({})", type_icon, entry.path, size_str);
                }
            } else {
                println!("Scan {} not found", scan_id);
            }
        }
        Some(Commands::Diff {
            scan_id_1,
            scan_id_2,
        }) => {
            let scan1 = db.get_scan(scan_id_1).await?;
            let scan2 = db.get_scan(scan_id_2).await?;

            match (scan1, scan2) {
                (Some(s1), Some(s2)) => {
                    println!("Comparing scans {} and {}", scan_id_1, scan_id_2);
                    println!("\nScan 1: {}", s1.root_path);
                    println!("  Date: {}", s1.started_at.format("%Y-%m-%d %H:%M:%S"));
                    println!("  Files: {}", s1.total_files);
                    println!("  Size: {:.2} MB", s1.total_size as f64 / 1_048_576.0);

                    println!("\nScan 2: {}", s2.root_path);
                    println!("  Date: {}", s2.started_at.format("%Y-%m-%d %H:%M:%S"));
                    println!("  Files: {}", s2.total_files);
                    println!("  Size: {:.2} MB", s2.total_size as f64 / 1_048_576.0);

                    println!("\nDifferences:");
                    let file_diff = s2.total_files - s1.total_files;
                    let size_diff = s2.total_size - s1.total_size;

                    println!("  Files: {:+}", file_diff);
                    println!("  Size: {:+.2} MB", size_diff as f64 / 1_048_576.0);

                    if size_diff > 0 {
                        println!("\n  ⚠️  Disk usage increased!");
                    } else if size_diff < 0 {
                        println!("\n  ✓ Disk usage decreased!");
                    } else {
                        println!("\n  No change in disk usage");
                    }
                }
                _ => {
                    println!("One or both scans not found");
                }
            }
        }
    }

    Ok(())
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Intelligently truncate a path by preserving the base and end, compressing the middle
fn smart_truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        return path.to_string();
    }

    // Try to split the path into components
    let parts: Vec<&str> = path.split('/').collect();

    if parts.len() <= 3 {
        // If it's a short path, use simple truncation from the start
        let end_len = max_len.saturating_sub(3);
        return format!("...{}", &path[path.len().saturating_sub(end_len)..]);
    }

    // Show first 2 components (like ~/user or /home/user)
    let base = parts[..2.min(parts.len())].join("/");

    // Show last 2 components
    let end = if parts.len() >= 2 {
        parts[parts.len() - 2..].join("/")
    } else {
        parts.last().unwrap_or(&"").to_string()
    };

    // Calculate what we have so far
    let base_and_end = format!("{}/.../{}", base, end);

    if base_and_end.len() <= max_len {
        base_and_end
    } else {
        // If still too long, just truncate with ellipsis in middle
        let start_len = (max_len / 2).saturating_sub(2);
        let end_len = (max_len / 2).saturating_sub(2);
        format!(
            "{}...{}",
            &path[..start_len],
            &path[path.len().saturating_sub(end_len)..]
        )
    }
}
