# Development Guide

This guide covers setting up a development environment and contributing to rootkitty.

## Prerequisites

### Required

- **Rust** 1.75+ (latest stable recommended)
- **Cargo** (comes with Rust)
- **SQLite** 3.35+ (usually system-provided)

### Optional

- **cargo-watch** - Auto-rebuild on file changes
- **cargo-flamegraph** - Performance profiling
- **sqlx-cli** - Database migrations and query preparation

### Installation

```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install optional tools
cargo install cargo-watch
cargo install cargo-flamegraph
cargo install sqlx-cli --no-default-features --features sqlite
```

## Getting Started

### Clone and Build

```bash
# Clone the repository
git clone https://github.com/yourusername/rootkitty.git
cd rootkitty

# Build debug version
cargo build

# Build release version
cargo build --release

# Run tests
cargo test

# Run with cargo
cargo run -- --help
```

### Development Workflow

#### 1. Fast Iteration with cargo-watch

```bash
# Auto-rebuild on changes
cargo watch -x check

# Auto-rebuild and run
cargo watch -x "run -- scan /tmp"

# Auto-rebuild, test, and run clippy
cargo watch -x test -x clippy
```

#### 2. Manual Testing

```bash
# Create test directory
mkdir -p /tmp/rootkitty-test
dd if=/dev/zero of=/tmp/rootkitty-test/large.bin bs=1M count=100
mkdir -p /tmp/rootkitty-test/subdir
echo "test" > /tmp/rootkitty-test/subdir/small.txt

# Scan it
cargo run -- scan /tmp/rootkitty-test

# Browse results
cargo run -- browse

# Clean up
rm -rf /tmp/rootkitty-test
```

#### 3. Database Development

```bash
# Set database location (optional)
export DATABASE_URL="sqlite://~/.config/rootkitty/rootkitty.db"

# Run migrations manually
sqlx migrate run

# Create new migration
sqlx migrate add add_feature_name

# Prepare queries for compile-time checking
cargo sqlx prepare

# Prepare in workspace mode
cargo sqlx prepare --workspace
```

## Project Structure

```
rootkitty/
├── Cargo.toml                   # Workspace manifest
├── README.md                    # User-facing documentation
├── CLAUDE.md                    # AI assistant guide
├── docs/                        # Additional documentation
│   ├── architecture.md
│   ├── database.md
│   ├── development.md (this file)
│   └── user-guide.md
└── crates/
    └── rootkitty/
        ├── Cargo.toml           # Package manifest
        ├── migrations/          # SQLite migrations
        │   └── *.sql
        └── src/
            ├── main.rs          # CLI entry point
            ├── scanner.rs       # Filesystem scanning
            ├── db.rs            # Database layer
            └── ui.rs            # TUI implementation
```

## Code Style

### Formatting

```bash
# Format all code
cargo fmt

# Check formatting without modifying
cargo fmt -- --check
```

**Configuration**: Uses default rustfmt settings.

### Linting

```bash
# Run clippy
cargo clippy

# Run clippy with all features
cargo clippy --all-features

# Deny warnings (CI mode)
cargo clippy -- -D warnings
```

**Key clippy settings**:
- Deny: `clippy::unwrap_used` in library code
- Allow: `clippy::too_many_arguments` for builders
- Warn: `clippy::missing_docs_in_private_items` (future)

### Naming Conventions

```rust
// Modules: snake_case
mod file_scanner;

// Types: PascalCase
struct FileEntry { }
enum ScanStatus { }

// Functions and variables: snake_case
fn calculate_size() -> u64 { }
let total_bytes = 0;

// Constants: SCREAMING_SNAKE_CASE
const MAX_DEPTH: usize = 1000;

// Type parameters: Single letter or PascalCase
fn process<T>(item: T) { }
fn process<TItem>(item: TItem) { }
```

## Testing

### Running Tests

```bash
# Run all tests
cargo test

# Run tests for specific module
cargo test scanner

# Run tests with output
cargo test -- --nocapture

# Run tests in release mode (faster)
cargo test --release

# Run ignored tests (integration tests that are slow)
cargo test -- --ignored
```

### Writing Tests

#### Unit Tests

Place in same file as code:

```rust
// src/scanner.rs
pub fn calculate_size(path: &Path) -> Result<u64> {
    // implementation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_size() {
        // Test implementation
    }
}
```

#### Integration Tests

Place in `tests/` directory:

```rust
// tests/integration_test.rs
use rootkitty::*;

#[tokio::test]
async fn test_full_workflow() {
    // Scan → Store → Query
}
```

#### Testing Database Code

```rust
#[tokio::test]
async fn test_database() {
    // Use in-memory database
    let db = Database::new(":memory:").await.unwrap();

    // Test operations
    let scan_id = db.create_scan(Path::new("/tmp")).await.unwrap();
    assert!(scan_id > 0);
}
```

#### Testing UI Code

UI testing is challenging. Focus on:
- Testing render logic separately from event handling
- Testing state transitions
- Manual testing for visual verification

### Test Coverage

```bash
# Install tarpaulin
cargo install cargo-tarpaulin

# Generate coverage report
cargo tarpaulin --out Html

# Open report
open tarpaulin-report.html
```

## Debugging

### Logging

Add logging for debugging:

```rust
// Add to Cargo.toml
[dependencies]
log = "0.4"
env_logger = "0.11"

// In main.rs
env_logger::init();

// In code
log::debug!("Scanning directory: {:?}", path);
log::info!("Scan completed: {} files", count);
log::warn!("Permission denied: {:?}", path);
log::error!("Database error: {}", e);
```

Run with logging:
```bash
RUST_LOG=debug cargo run -- scan /tmp
RUST_LOG=rootkitty=trace cargo run -- scan /tmp
```

### Debugger

#### VS Code

`.vscode/launch.json`:
```json
{
    "version": "0.2.0",
    "configurations": [
        {
            "type": "lldb",
            "request": "launch",
            "name": "Debug rootkitty",
            "cargo": {
                "args": [
                    "build",
                    "--bin=rootkitty",
                    "--package=rootkitty"
                ],
                "filter": {
                    "name": "rootkitty",
                    "kind": "bin"
                }
            },
            "args": ["scan", "/tmp"],
            "cwd": "${workspaceFolder}"
        }
    ]
}
```

#### Command Line (lldb)

```bash
# Build with debug info
cargo build

# Run in debugger
rust-lldb target/debug/rootkitty -- scan /tmp

# In lldb
(lldb) b scanner.rs:50
(lldb) run
(lldb) print entries
(lldb) continue
```

### Performance Profiling

#### CPU Profiling (flamegraph)

```bash
# On macOS, you may need to grant dtrace permissions
sudo cargo flamegraph -- scan /large/directory

# Open flamegraph.svg in browser
open flamegraph.svg
```

#### Memory Profiling (heaptrack)

```bash
# Linux only
heaptrack target/release/rootkitty scan /large/directory
heaptrack_gui heaptrack.rootkitty.*.gz
```

#### Benchmarking

```rust
// benches/scan_bench.rs
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn benchmark_scan(c: &mut Criterion) {
    c.bench_function("scan /tmp", |b| {
        b.iter(|| {
            // Benchmark code
        });
    });
}

criterion_group!(benches, benchmark_scan);
criterion_main!(benches);
```

Run benchmarks:
```bash
cargo bench
```

## Common Development Tasks

### Adding a New CLI Subcommand

1. **Add to Commands enum** in `src/main.rs`:
```rust
#[derive(Subcommand)]
enum Commands {
    // ... existing commands
    /// New command description
    NewCommand {
        /// Argument description
        arg: String,
    },
}
```

2. **Implement handler** in `main()`:
```rust
match cli.command {
    // ... existing handlers
    Commands::NewCommand { arg } => {
        handle_new_command(&db, &arg).await?;
    }
}
```

3. **Add implementation**:
```rust
async fn handle_new_command(db: &Database, arg: &str) -> Result<()> {
    // Implementation
    Ok(())
}
```

4. **Update documentation** in README.md

5. **Test**:
```bash
cargo run -- new-command test
cargo run -- help
```

### Adding a Database Query

1. **Add method to Database impl** in `src/db.rs`:
```rust
impl Database {
    pub async fn new_query(&self, param: i64) -> Result<Vec<SomeType>> {
        let rows = sqlx::query("SELECT * FROM table WHERE id = ?")
            .bind(param)
            .fetch_all(&self.pool)
            .await?;

        // Map rows to Rust types
        Ok(rows.iter().map(|row| { /* ... */ }).collect())
    }
}
```

2. **Test in unit test**:
```rust
#[tokio::test]
async fn test_new_query() {
    let db = Database::new(":memory:").await.unwrap();
    let result = db.new_query(1).await.unwrap();
    assert!(!result.is_empty());
}
```

3. **Update sqlx cache**:
```bash
cargo sqlx prepare
```

4. **Commit `.sqlx/` changes** with your code

### Adding a TUI View

1. **Add View variant** in `src/ui.rs`:
```rust
enum View {
    ScanList,
    FileTree,
    CleanupList,
    NewView,  // Add this
}
```

2. **Add state to App**:
```rust
pub struct App {
    // ... existing fields
    new_view_data: Vec<SomeData>,
    new_view_state: ListState,
}
```

3. **Implement render method**:
```rust
impl App {
    fn render_new_view(&mut self, f: &mut Frame, area: Rect) {
        // Rendering logic
    }
}
```

4. **Add to main render**:
```rust
fn render(&mut self, f: &mut Frame) {
    // ...
    match self.view {
        // ... existing views
        View::NewView => self.render_new_view(f, chunks[0]),
    }
}
```

5. **Add keyboard shortcut** in event loop:
```rust
match key.code {
    // ... existing keys
    KeyCode::Char('4') => self.view = View::NewView,
}
```

### Optimizing Performance

1. **Profile first**:
```bash
cargo flamegraph -- scan /large/directory
```

2. **Identify bottleneck** in flamegraph

3. **Common optimizations**:
   - Reduce allocations (use references)
   - Batch operations (database inserts)
   - Parallelize (Rayon for CPU, Tokio for I/O)
   - Cache results (memoization)

4. **Measure improvement**:
```bash
hyperfine 'cargo run --release -- scan /test/dir'
```

## Troubleshooting

### Build Failures

#### "error: linking with `cc` failed"

**Solution**: Install C compiler
```bash
# macOS
xcode-select --install

# Linux (Debian/Ubuntu)
sudo apt-get install build-essential

# Linux (Fedora)
sudo dnf install gcc
```

#### "Could not find `sqlite3`"

**Solution**: Install SQLite development libraries
```bash
# macOS
brew install sqlite3

# Linux (Debian/Ubuntu)
sudo apt-get install libsqlite3-dev

# Linux (Fedora)
sudo dnf install sqlite-devel
```

#### "error: could not compile `sqlx-macros`"

**Solution**: Set DATABASE_URL or prepare offline mode
```bash
# Option 1: Use existing database
export DATABASE_URL="sqlite://~/.config/rootkitty/rootkitty.db"

# Option 2: Use prepared queries (offline mode)
cargo sqlx prepare
# Commit .sqlx/ directory
```

### Runtime Errors

#### "database is locked"

**Cause**: Another process has database open

**Solution**:
- Close other rootkitty instances
- Wait for WAL checkpoint to complete
- Increase busy timeout in code

#### "permission denied" during scan

**Cause**: Insufficient permissions to read directory

**Solution**:
- Run with appropriate permissions
- Scanner silently skips inaccessible files (by design)

#### TUI rendering issues

**Cause**: Terminal size too small or incompatible

**Solution**:
- Resize terminal (minimum 80x24)
- Try different terminal emulator
- Check TERM environment variable

## Contributing

### Before Submitting a PR

1. **Run full test suite**:
```bash
cargo test --all
cargo clippy -- -D warnings
cargo fmt -- --check
```

2. **Update documentation**:
- Update README.md if user-facing changes
- Update CLAUDE.md if architecture changes
- Add docstrings to public APIs

3. **Manual testing**:
- Test on your platform
- Verify TUI works correctly
- Check database migrations apply cleanly

4. **Commit message**:
```
Short summary (50 chars or less)

More detailed explanation if needed. Wrap at 72 characters.

- Bullet points for multiple changes
- Reference issues: Fixes #123
```

### Code Review Checklist

- [ ] Code compiles without warnings
- [ ] Tests pass
- [ ] Clippy passes
- [ ] Formatted with rustfmt
- [ ] Documentation updated
- [ ] No unwrap() in library code
- [ ] Error handling is appropriate
- [ ] Performance impact considered
- [ ] Backwards compatibility maintained

## Release Process

(For maintainers)

1. **Update version** in `Cargo.toml`:
```toml
[workspace.package]
version = "0.2.0"
```

2. **Update CHANGELOG.md**:
```markdown
## [0.2.0] - 2025-01-15

### Added
- Feature X
- Feature Y

### Changed
- Improved Z

### Fixed
- Bug fix A
```

3. **Tag release**:
```bash
git tag -a v0.2.0 -m "Release version 0.2.0"
git push origin v0.2.0
```

4. **Publish to crates.io**:
```bash
cargo publish --dry-run
cargo publish
```

5. **Create GitHub release** with changelog

## Resources

### Documentation

- [Rust Book](https://doc.rust-lang.org/book/)
- [Async Book](https://rust-lang.github.io/async-book/)
- [SQLx Documentation](https://docs.rs/sqlx/)
- [Ratatui Documentation](https://docs.rs/ratatui/)
- [Clap Documentation](https://docs.rs/clap/)

### Tools

- [rust-analyzer](https://rust-analyzer.github.io/) - LSP for IDEs
- [cargo-expand](https://github.com/dtolnay/cargo-expand) - Expand macros
- [cargo-tree](https://doc.rust-lang.org/cargo/commands/cargo-tree.html) - View dependency tree
- [cargo-audit](https://github.com/RustSec/rustsec/tree/main/cargo-audit) - Security audits

### Community

- [Rust Users Forum](https://users.rust-lang.org/)
- [Rust Discord](https://discord.gg/rust-lang)
- [r/rust](https://reddit.com/r/rust)

## Getting Help

- **Bug reports**: GitHub Issues
- **Feature requests**: GitHub Issues with "enhancement" label
- **Questions**: GitHub Discussions
- **Security issues**: Email maintainers directly

## License

This project is licensed under MIT OR Apache-2.0. See LICENSE files for details.
