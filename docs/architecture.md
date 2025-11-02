# Architecture

This document describes the architecture and design decisions of rootkitty.

## Overview

Rootkitty is structured as a monolithic Rust binary with four main modules:

1. **Scanner** - Filesystem traversal and size calculation
2. **Database** - SQLite persistence layer using sqlx
3. **UI** - Terminal user interface using ratatui
4. **CLI** - Command-line interface using clap

## Design Principles

### 1. Separation of Concerns

Each module has a single, well-defined responsibility:

- Scanner knows nothing about databases or UI
- Database provides a clean CRUD interface, no business logic
- UI handles presentation and user input, delegates to database
- CLI orchestrates the other modules

### 2. Data Flow

```
User Input (CLI)
    │
    ├─→ Scan Command
    │       │
    │       ├─→ Scanner: Walk filesystem → Vec<FileEntry>
    │       │
    │       └─→ Database: Store entries → scan_id
    │
    ├─→ Browse Command
    │       │
    │       └─→ UI: Load from DB → Interactive TUI
    │               │
    │               ├─→ Database: Query scans
    │               ├─→ Database: Query entries
    │               └─→ Database: Mark for cleanup
    │
    ├─→ List/Show/Diff Commands
    │       │
    │       └─→ Database: Query → Format output
    │
    └─→ (Future) Analyze Command
            │
            └─→ Analyzer: Process entries → Statistics
```

### 3. Async vs Sync

**Async (Tokio)**:
- All database operations (sqlx requires async)
- UI event loop (to integrate with database calls)
- Future: Network operations, remote scanning

**Sync (Blocking)**:
- Filesystem scanning (I/O bound, but uses Rayon for parallelism)
- File operations during cleanup
- Terminal rendering (ratatui is synchronous)

**Why this split?**
- Database operations benefit from async (connection pooling, concurrent queries)
- Filesystem scanning is CPU/I/O bound, benefits more from thread parallelism (Rayon)
- Mixing async I/O with sync I/O via `tokio::task::spawn_blocking` when needed

## Module Details

### Scanner Module (`scanner.rs`)

**Purpose**: Recursively walk filesystem and calculate directory sizes.

**Key Types**:
```rust
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub parent_path: Option<PathBuf>,
    pub size: u64,
    pub is_dir: bool,
    pub modified_at: Option<DateTime<Utc>>,
    pub depth: usize,
}

pub struct Scanner {
    root_path: PathBuf,
    entries: Arc<Mutex<Vec<FileEntry>>>,
    stats: Arc<ScanStats>,
}
```

**Algorithm**:
1. Start at root directory
2. For each directory:
   - Read directory entries
   - If >100 children, use Rayon parallel iteration
   - Recursively process subdirectories
   - Accumulate sizes bottom-up
3. Store each file/directory as a FileEntry
4. Return (entries, stats)

**Performance Characteristics**:
- Time: O(n) where n = total files
- Space: O(n) to store all entries
- Parallelism: Rayon thread pool (defaults to CPU count)
- I/O pattern: Depth-first traversal

**Future Optimizations**:
- Incremental scanning (only scan changed files)
- Memory-mapped file handling for very large scans
- Streaming results to database (avoid buffering all entries)

### Database Module (`db.rs`)

**Purpose**: Persist scans and provide query interface.

**Key Types**:
```rust
pub struct Database {
    pool: SqlitePool,
}

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

pub struct StoredFileEntry {
    // Similar to FileEntry but with database IDs
}
```

**Connection Pool**:
- Max 5 connections (SQLite limitation)
- WAL mode for better concurrency
- Shared cache mode

**Transaction Strategy**:
- Batch inserts in single transaction (1000 entries at a time)
- Read operations don't need transactions
- Cleanup operations use transactions for consistency

**Query Patterns**:
1. **By scan_id**: Most common, heavily indexed
2. **By size**: For "largest files" queries, descending index
3. **By parent_path**: For tree navigation (not yet used in UI)
4. **Joins**: Cleanup items join with file_entries

**Migration Strategy**:
- sqlx migrations in `migrations/` directory
- Applied automatically on database open
- Versioned by timestamp prefix
- Reversible migrations not currently supported

### UI Module (`ui.rs`)

**Purpose**: Interactive terminal interface using ratatui.

**Key Types**:
```rust
pub struct App {
    db: Database,
    view: View,
    scans: Vec<Scan>,
    scan_list_state: ListState,
    current_scan: Option<Scan>,
    file_entries: Vec<StoredFileEntry>,
    file_list_state: ListState,
    cleanup_items: Vec<StoredFileEntry>,
    cleanup_list_state: ListState,
    status_message: String,
}

enum View {
    ScanList,
    FileTree,
    CleanupList,
}
```

**Event Loop**:
1. Render current view
2. Poll for keyboard input (100ms timeout)
3. Handle key event (navigate, select, mark, etc.)
4. Update state (possibly async database call)
5. Repeat

**View Responsibilities**:
- **ScanList**: Display all historical scans, allow selection
- **FileTree**: Show files sorted by size, allow marking for cleanup
- **CleanupList**: Show marked files, generate cleanup script

**State Management**:
- App owns all state (no separate state store)
- ListState tracks scroll position and selection
- Database queries on view transitions
- Status message for user feedback

**Performance Considerations**:
- Only load top 1000 files initially (pagination possible in future)
- List rendering is O(visible_lines), not O(total_entries)
- Database queries are async, don't block rendering

### CLI Module (`main.rs`)

**Purpose**: Parse arguments and orchestrate subcommands.

**Structure**:
```rust
#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    #[arg(short, long, default_value = "~/.config/rootkitty/rootkitty.db")]
    db: String,
}

#[derive(Subcommand)]
enum Commands {
    Scan { path: PathBuf },
    Browse,
    List,
    Show { scan_id: i64 },
    Diff { scan_id_1: i64, scan_id_2: i64 },
}
```

**Responsibilities**:
1. Parse command-line arguments with clap
2. Initialize database connection
3. Dispatch to appropriate handler
4. Format output for non-interactive commands
5. Error handling and user-friendly messages

**Future Subcommands**:
- `analyze`: File type statistics, size distributions
- `export`: Export scan data to JSON/CSV
- `dedupe`: Find duplicate files
- `search`: Find files by name/pattern
- `prune`: Interactive cleanup mode (alternative to browse)

## Data Structures

### File Entry Representation

We store two representations of file entries:

1. **In-memory** (`scanner.rs::FileEntry`):
   - Uses `PathBuf` for paths
   - Optimized for construction during scanning
   - No database IDs

2. **Database** (`file_entries` table, `db.rs::StoredFileEntry`):
   - Paths as TEXT (serialized)
   - Has database IDs (primary key, foreign keys)
   - Indexed for fast queries

**Why two representations?**
- Scanner doesn't need database overhead
- Database representation optimized for queries
- Clean separation of concerns

### Size Calculation

Directory sizes are calculated **recursively** during scanning:
```
/home/user/
    documents/
        file1.txt (1KB)
        file2.txt (2KB)
    photos/
        image.jpg (5KB)
```

Results in:
- `file1.txt`: size = 1KB
- `file2.txt`: size = 2KB
- `documents/`: size = 3KB (sum of children)
- `image.jpg`: size = 5KB
- `photos/`: size = 5KB
- `/home/user/`: size = 8KB (sum of all descendants)

This allows sorting directories by total size in TUI.

## Error Handling Strategy

### Error Types

1. **I/O Errors**: File not found, permission denied
   - Strategy: Log and skip, continue scanning
   - User visible: Warning in scan summary

2. **Database Errors**: Connection failed, query error
   - Strategy: Propagate to caller with context
   - User visible: Error message and exit

3. **UI Errors**: Terminal init failed, render error
   - Strategy: Clean up terminal state, show error
   - User visible: Error message in original terminal

### Error Propagation

```rust
// Library functions return Result
pub async fn create_scan(&self, path: &Path) -> Result<i64> {
    self.db.create_scan(path)
        .await
        .context("Failed to create scan")?
}

// Main handles top-level errors
#[tokio::main]
async fn main() -> Result<()> {
    // ... setup
    match cli.command {
        Commands::Scan { path } => {
            scan_directory(&db, &path).await?;
        }
    }
    Ok(())
}
```

### Graceful Degradation

- Inaccessible files: Skip and continue
- Database query timeout: Retry once, then fail
- TUI render error: Exit cleanly, restore terminal

## Concurrency Model

### Thread Safety

- **Scanner**: Uses `Arc<Mutex<Vec<FileEntry>>>` for shared state across Rayon threads
- **Database**: SqlitePool handles connection pooling, all operations via async/await
- **UI**: Single-threaded (terminal output can't be multiplexed)

### Parallelism

1. **Rayon (Scanner)**:
   - Work-stealing thread pool
   - Parallel iteration over directory entries
   - Automatic load balancing

2. **Tokio (Database)**:
   - Cooperative multitasking
   - Connection pool for concurrent queries
   - Doesn't provide parallelism, just concurrency

### Lock Granularity

- Scanner: Fine-grained locks (per entry insertion)
- Database: Connection-level locking (handled by sqlx)
- UI: No locks needed (single-threaded)

## Storage Format

### Database File

- **Format**: SQLite 3
- **Journal Mode**: WAL (Write-Ahead Logging)
- **Location**: `~/.config/rootkitty/rootkitty.db` (configurable)
- **Size**: Approximately 200 bytes per file entry

### Cleanup Script

- **Format**: Bash script
- **Location**: `cleanup.sh` in current directory
- **Content**: `rm` or `rm -rf` commands for marked items
- **Safety**: User must manually review and execute

## Extension Points

### Adding New Scan Types

Currently only filesystem scanning. Future possibilities:
1. **Docker volume scanning**: Scan container filesystems
2. **S3 bucket scanning**: Scan cloud storage
3. **Git repository scanning**: Analyze repo size over time

Interface:
```rust
trait Scanner {
    fn scan(&self) -> Result<(Vec<FileEntry>, ScanStats)>;
}

struct FilesystemScanner { /* ... */ }
struct DockerScanner { /* ... */ }
struct S3Scanner { /* ... */ }
```

### Adding New Output Formats

Currently: CLI text, TUI, bash script. Future:
1. **JSON export**: For programmatic use
2. **HTML report**: For sharing/archiving
3. **CSV export**: For spreadsheet analysis

Interface:
```rust
trait Exporter {
    fn export(&self, scan: &Scan, entries: &[FileEntry]) -> Result<String>;
}
```

### Adding New Analysis

Currently: Basic size statistics. Future:
1. **File type analysis**: Group by extension, mime type
2. **Age analysis**: Find old files
3. **Duplicate detection**: Hash-based or name-based
4. **Growth analysis**: Compare scans automatically

Each would be a separate module with database queries optimized for that analysis.

## Performance Targets

### Scanning Performance

- **Target**: 100k files/second on NVMe SSD
- **Bottleneck**: Usually I/O (stat syscalls), not CPU
- **Optimization**: Batch stat calls, use parallelism

### Database Performance

- **Target**: 10k inserts/second (batched)
- **Bottleneck**: Transaction overhead
- **Optimization**: Large batch sizes (1000+), single transaction

### TUI Performance

- **Target**: 60 FPS (16ms frame time)
- **Bottleneck**: Terminal I/O
- **Optimization**: Only redraw on changes, double buffering (ratatui handles this)

### Memory Usage

- **Target**: <100MB for 1M file scan
- **Current**: ~200 bytes per entry in memory
- **Future**: Stream to database, don't buffer all entries

## Security Considerations

### Path Traversal

- Scanner respects filesystem boundaries
- No symlink following (yet - would need cycle detection)
- Permission errors handled gracefully

### SQL Injection

- All queries use sqlx prepared statements
- No string concatenation for SQL
- Compile-time query checking

### Shell Injection

- Cleanup script uses single quotes around paths
- Paths with single quotes are escaped
- User must review script before execution

### File System Race Conditions

- Scanner captures point-in-time snapshot
- Files may be modified during scan
- Size discrepancies possible but acceptable

## Testing Strategy

### Unit Tests

- Scanner: Test with temporary directories
- Database: Test with `:memory:` SQLite
- UI: Test individual render functions (mock terminal)
- Formatting: Test size formatting, date formatting

### Integration Tests

- Full workflow: scan → store → query → cleanup
- Multiple scans and comparison
- Database migrations
- Error cases (permission denied, disk full, etc.)

### Manual Testing

- Large directory scan (>100k files)
- TUI navigation and responsiveness
- Cleanup script generation and execution
- Database persistence and concurrent access

### Performance Testing

- Benchmark scanning speed vs `du`, `ncdu`, `dust`
- Profile with `cargo flamegraph`
- Memory profiling with `valgrind` or `heaptrack`
- Database query performance with `EXPLAIN QUERY PLAN`

## Future Architecture Changes

### Streaming Database Writes

Currently: Buffer all entries → write in batches
Future: Stream entries → write incrementally

Benefits:
- Lower memory usage
- Progress updates during scan
- Partial results if scan interrupted

### Pluggable Storage Backends

Currently: SQLite only
Future: Abstract `Database` trait with multiple implementations

Options:
- PostgreSQL for multi-user deployments
- DuckDB for analytical queries
- Parquet files for archival

### Distributed Scanning

Currently: Single machine only
Future: Coordinate multiple scanners

Use cases:
- Scan multiple machines simultaneously
- Aggregate results in central database
- Compare disk usage across fleet

This would require:
- Network protocol for coordination
- Distributed database or aggregation layer
- Authentication and authorization
