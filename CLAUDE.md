# CLAUDE.md - AI Assistant Guide for Rootkitty

This document provides guidance for AI assistants (like Claude) working on the rootkitty codebase.

## Project Overview

**Rootkitty** is a high-performance disk usage analyzer with a Terminal User Interface (TUI), built entirely in Rust. It allows users to scan filesystems, store historical scan data in SQLite, interactively browse results, and generate cleanup scripts.

### Core Philosophy

- **Performance first**: Use parallel processing (Rayon), minimize allocations, avoid unnecessary copies
- **Ergonomic APIs**: Follow Rust idioms, use strong types, prefer builder patterns where appropriate
- **Clean separation**: Scanner, database, UI, and CLI are distinct modules with clear boundaries
- **Zero-copy where possible**: Use references and views instead of cloning large data structures
- **Explicit error handling**: Use `Result<T>` and `anyhow` for errors, avoid panics in library code

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                         CLI (main.rs)                   │
│           Command parsing, orchestration                │
└─────────────┬───────────────────────────────────────────┘
              │
              ├──────────────────┬──────────────────┬──────────────────┐
              │                  │                  │                  │
              ▼                  ▼                  ▼                  ▼
      ┌───────────────┐  ┌──────────────┐  ┌─────────────┐  ┌──────────────┐
      │  scanner.rs   │  │    db.rs     │  │   ui.rs     │  │   (future)   │
      │               │  │              │  │             │  │  analyzer.rs  │
      │ Filesystem    │  │ SQLite/sqlx  │  │ Ratatui TUI │  │  Duplicate    │
      │ traversal     │──┤ layer        │──┤             │  │  detection    │
      │ (parallel)    │  │              │  │ 3 views:    │  │  Pattern      │
      │               │  │ Migrations   │  │ - Scans     │  │  matching     │
      └───────────────┘  │ CRUD ops     │  │ - Files     │  └──────────────┘
                         └──────────────┘  │ - Cleanup   │
                                           └─────────────┘
```

### Module Responsibilities

- **`scanner.rs`**: Recursively walks filesystem, calculates sizes, returns `Vec<FileEntry>`
- **`db.rs`**: SQLite persistence layer, migrations, queries for scans/entries/cleanup items
- **`ui.rs`**: TUI implementation with three views, keyboard navigation, screen rendering
- **`main.rs`**: CLI argument parsing, subcommand dispatch, high-level orchestration

## Coding Conventions

### Rust Style

- Follow standard Rust style (rustfmt default configuration)
- Use `cargo clippy` and address all warnings
- Prefer explicit types in public APIs, inference in private code
- Use meaningful variable names; avoid abbreviations except standard ones (e.g., `db`, `tx`)

### Error Handling

```rust
// ✓ Good: Propagate errors with context
pub async fn create_scan(&self, path: &Path) -> Result<i64> {
    let id = self.db.create_scan(path)
        .await
        .context("Failed to create scan in database")?;
    Ok(id)
}

// ✗ Bad: Unwrap or expect in library code
let id = self.db.create_scan(path).await.unwrap();

// ✓ Good: Let caller decide how to handle errors
match scanner.scan() {
    Ok((entries, stats)) => { /* ... */ },
    Err(e) => eprintln!("Scan failed: {}", e),
}
```

### Async/Await

- All database operations are `async` (sqlx requirement)
- Scanner is currently synchronous (CPU-bound, uses Rayon for parallelism)
- UI event loop is `async` to integrate with database calls
- Use `tokio::spawn` for true concurrency, Rayon for data parallelism

### Performance Patterns

```rust
// ✓ Good: Batch database inserts
for chunk in entries.chunks(1000) {
    db.insert_batch(chunk).await?;
}

// ✗ Bad: Individual inserts
for entry in entries {
    db.insert(entry).await?;
}

// ✓ Good: Use references to avoid clones
fn process_entries(entries: &[FileEntry]) { /* ... */ }

// ✗ Bad: Clone unnecessarily
fn process_entries(entries: Vec<FileEntry>) { /* ... */ }
```

## Database Schema

See `docs/database.md` for full schema documentation.

**Key tables**:
- `scans`: Metadata for each filesystem scan
- `file_entries`: Individual files/directories with size, path, depth
- `cleanup_items`: User-marked files for deletion

**Indices**: Optimized for queries by scan_id, size sorting, and path lookups.

## Adding New Features

### Adding a New Subcommand

1. Add variant to `Commands` enum in `main.rs`
2. Implement command handler in `main()`
3. Add any new database methods to `db.rs`
4. Update README with new command documentation

Example:
```rust
#[derive(Subcommand)]
enum Commands {
    // ... existing commands
    /// Analyze file types in a scan
    Analyze {
        scan_id: i64,
    },
}
```

### Adding a New TUI View

1. Add variant to `View` enum in `ui.rs`
2. Implement `render_<view_name>` method
3. Add keyboard shortcuts in `run_event_loop`
4. Update status bar help text
5. Add view state fields to `App` struct

### Adding Database Queries

1. Write SQL in `db.rs` method (use sqlx prepared statements)
2. Test query with `cargo sqlx prepare` for compile-time checking
3. Add appropriate indices if query will be used frequently
4. Document expected performance characteristics

## Testing Strategy

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scanner_basic() {
        let scanner = Scanner::new("/tmp/test");
        // ...
    }

    #[tokio::test]
    async fn test_db_operations() {
        let db = Database::new(":memory:").await.unwrap();
        // ...
    }
}
```

### Integration Tests

- Place in `tests/` directory (not yet implemented)
- Test full workflows: scan → store → query → generate cleanup
- Use temporary directories for filesystem tests
- Use in-memory SQLite for database tests

### Manual Testing Checklist

- [ ] Scan large directory (>100k files)
- [ ] Navigate TUI with keyboard
- [ ] Mark files for cleanup
- [ ] Generate and review cleanup script
- [ ] Compare two scans
- [ ] Verify database persistence across restarts

## Common Pitfalls

### 1. Borrow Checker Issues in UI

**Problem**: Ratatui's `render` methods often require disjoint borrows.

**Solution**: Clone or extract values before async operations:
```rust
// ✗ Bad: Borrow conflict
let entry = self.cleanup_items.get(selected)?;
self.db.remove_cleanup_item(scan.id, entry.id).await?;
self.status_message = format!("Removed '{}'", entry.name); // entry still borrowed!

// ✓ Good: Clone small values
let entry_id = entry.id;
let entry_name = entry.name.clone();
self.db.remove_cleanup_item(scan.id, entry_id).await?;
self.status_message = format!("Removed '{}'", entry_name);
```

### 2. SQLite WAL Mode

The database uses Write-Ahead Logging (WAL) for better concurrency. This creates `.db-shm` and `.db-wal` files alongside the main `.db` file. These are normal and should be in `.gitignore`.

### 3. Parallel Scanning Thresholds

The scanner switches to parallel mode for directories with >100 children. This threshold may need tuning based on benchmarks. Too low = overhead from thread spawning. Too high = missed parallelism opportunities.

### 4. TUI Event Polling

The event loop polls every 100ms for keyboard input. This balances responsiveness with CPU usage. Don't reduce below 50ms without good reason.

## Future Enhancements

See README roadmap, but key areas for expansion:

1. **Pattern filtering**: Filter scans by regex, glob, or file type
2. **Duplicate detection**: Hash-based or size/name-based deduplication
3. **Visual charts**: Use ratatui's chart widgets for size distribution
4. **Export formats**: JSON, CSV, or HTML reports
5. **Incremental scans**: Only scan changed files since last scan
6. **Remote scanning**: Scan over SSH or network filesystems

## Performance Benchmarks

Target performance (not yet formally benchmarked):
- **Scanning**: >100k files/second on NVMe SSD
- **Database inserts**: >10k entries/second (batched)
- **TUI responsiveness**: <16ms frame time (60 FPS)
- **Memory usage**: <100MB for 1M file scan

## Dependencies

Key dependencies and their purpose:
- `tokio`: Async runtime (required by sqlx)
- `sqlx`: Type-safe SQL with compile-time checking
- `ratatui`: Terminal UI framework
- `crossterm`: Cross-platform terminal manipulation
- `rayon`: Data parallelism for scanning
- `clap`: CLI argument parsing with derive macros
- `anyhow`: Ergonomic error handling
- `chrono`: Date/time handling

## SQL Compile-Time Checking

This project uses sqlx's compile-time query verification. To update the `.sqlx/` cache after changing queries:

```bash
# Set DATABASE_URL or it will use the default
export DATABASE_URL="sqlite://~/.config/rootkitty/rootkitty.db"

# Prepare queries (creates .sqlx/ directory)
cargo sqlx prepare

# Or prepare in workspace mode
cargo sqlx prepare --workspace
```

**Important**: The `.sqlx/` directory should be committed to git so CI can build without a database.

## Development Workflow

1. **Make changes** to source files
2. **Check compilation**: `cargo check`
3. **Run clippy**: `cargo clippy`
4. **Format code**: `cargo fmt`
5. **Test manually**: `cargo run -- scan /tmp/test && cargo run -- browse`
6. **Update docs** if public API changed

## Getting Help

- **Rust Book**: https://doc.rust-lang.org/book/
- **Async Book**: https://rust-lang.github.io/async-book/
- **Sqlx Docs**: https://docs.rs/sqlx/latest/sqlx/
- **Ratatui Docs**: https://docs.rs/ratatui/latest/ratatui/
- **Project Docs**: See `docs/` directory

## Questions for Human Developers

When working on this project and uncertain, consider:

1. **Performance**: Will this allocation happen in a hot loop?
2. **Error handling**: Can this operation fail? How should we handle it?
3. **API design**: Is this interface ergonomic and hard to misuse?
4. **Compatibility**: Will this work on Windows, macOS, and Linux?
5. **Documentation**: Would another developer understand this code in 6 months?

## AI Assistant Notes

When extending this codebase:

- **Preserve the architecture**: Keep modules separate, don't mix concerns
- **Match existing style**: Look at surrounding code for patterns
- **Add tests**: Include at least basic test coverage for new features
- **Update docs**: Modify this file and `docs/` if adding major features
- **Consider performance**: This is a performance-focused tool, profile before optimizing
- **Handle errors**: Never use `unwrap()` in library code, always propagate errors

## File Reference Quick Guide

- `src/main.rs:45-70` - Subcommand dispatch logic
- `src/scanner.rs:50-90` - Recursive scan implementation
- `src/db.rs:40-60` - Database connection setup
- `src/ui.rs:80-160` - Event loop and keyboard handling
- `migrations/*.sql` - Database schema definitions
