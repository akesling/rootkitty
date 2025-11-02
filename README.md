# rootkitty

A blazingly fast disk usage analyzer with a beautiful TUI, built with Rust.

<div align="center">

**Scan • Analyze • Clean**

Fast filesystem scanning | SQLite-backed storage | Interactive TUI | Historical comparisons

</div>

## Features

- **Ultra-fast scanning** - Parallel directory traversal using Rayon
- **SQLite database** - Keep track of multiple scans over time with sqlx
- **Interactive TUI** - Beautiful terminal interface powered by ratatui
- **Historical tracking** - Compare scans to see disk usage changes
- **Smart cleanup** - Mark files for deletion and generate cleanup scripts
- **Zero-copy operations** - Efficient memory usage for large filesystems

## Installation

### Build from source

```bash
cargo build --release
```

The binary will be available at `target/release/rootkitty`.

### Quick start

```bash
# Add to PATH (optional)
cargo install --path crates/rootkitty

# Or run directly
cargo run --release -- --help
```

## Usage

### Scan a directory

```bash
rootkitty scan /path/to/directory
```

This will recursively scan the directory, analyze disk usage, and store the results in a SQLite database.

### Launch the interactive TUI

```bash
rootkitty browse
```

The TUI provides three views:
- **Scans (1)**: View all historical scans
- **Files (2)**: Browse files sorted by size
- **Cleanup (3)**: Review marked files and generate cleanup scripts

#### TUI Navigation

```
General:
  q        Quit
  1        View scans list
  2        View files in selected scan
  3        View cleanup list
  ↑/↓      Navigate up/down
  j/k      Navigate up/down (vim-style)

Scans view:
  Enter    Select scan and view files

Files view:
  Space    Mark/unmark file for cleanup

Cleanup view:
  Space    Remove item from cleanup list
  g        Generate cleanup.sh script
```

### List all scans

```bash
rootkitty list
```

Example output:
```
ID    Path                Files        Size (MB)    Date
----------------------------------------------------------------------------------
3     /Users/you/projects 45231        15234.56     2025-01-02 14:30:00
2     /Users/you/Downloads 1234        8765.43      2025-01-01 09:15:00
1     /home               89012        102345.67    2024-12-31 18:00:00
```

### Show scan details

```bash
rootkitty show 1
```

Displays detailed information about a specific scan including the largest files.

### Compare scans

```bash
rootkitty diff 1 2
```

Compare two scans to see how disk usage changed over time:

```
Comparing scans 1 and 2

Scan 1: /Users/you/projects
  Date: 2025-01-01 10:00:00
  Files: 45231
  Size: 15234.56 MB

Scan 2: /Users/you/projects
  Date: 2025-01-02 14:30:00
  Files: 45892
  Size: 16103.22 MB

Differences:
  Files: +661
  Size: +868.66 MB

  ⚠️  Disk usage increased!
```

## Database

By default, rootkitty stores its database at `~/.config/rootkitty/rootkitty.db`.

You can specify a custom database location:

```bash
rootkitty --db /path/to/database.db scan /some/path
```

The database schema includes:
- **scans**: Metadata for each filesystem scan
- **file_entries**: Detailed information about files and directories
- **cleanup_items**: User-marked files for deletion

## Cleanup Workflow

1. **Scan** your filesystem: `rootkitty scan /path`
2. **Browse** in TUI: `rootkitty browse`
3. **Navigate** to the scan (press `1`, then `Enter`)
4. **View files** (automatically switches to view `2`)
5. **Mark files** for cleanup by pressing `Space`
6. **Switch to cleanup view** (press `3`)
7. **Generate script** by pressing `g`
8. **Review** the generated `cleanup.sh` file
9. **Execute** cleanup (after careful review!):
   ```bash
   bash cleanup.sh
   ```

## Architecture

```
rootkitty/
├── Cargo.toml              # Workspace configuration
└── crates/
    └── rootkitty/
        ├── Cargo.toml      # Package manifest
        ├── migrations/     # SQLite schema migrations
        └── src/
            ├── main.rs     # CLI interface
            ├── scanner.rs  # Filesystem scanning
            ├── db.rs       # Database layer
            └── ui.rs       # TUI implementation
```

### Key Technologies

- **ratatui**: Terminal UI framework
- **sqlx**: Async SQL toolkit with compile-time query checking
- **tokio**: Async runtime
- **rayon**: Data parallelism for fast scanning
- **clap**: Command-line argument parsing
- **crossterm**: Cross-platform terminal manipulation

## Performance

Rootkitty is designed for speed:

- **Parallel scanning**: Leverages multiple CPU cores
- **Efficient I/O**: Minimizes syscalls during traversal
- **Smart batching**: Database operations are batched for performance
- **Indexed queries**: SQLite indices for fast data retrieval

Typical performance on modern hardware:
- **~500k files/second** for cached filesystems
- **~100k files/second** for cold storage

## Safety

- **Read-only scanning**: Scans never modify your filesystem
- **Explicit cleanup**: File deletion requires manual script execution
- **Careful generation**: Cleanup scripts use proper shell escaping

## License

MIT OR Apache-2.0

## Contributing

Contributions welcome! This project follows the Rust community's code of conduct.

## Roadmap

- [ ] Pattern-based file filtering
- [ ] Plugin system for custom analyzers
- [ ] Remote scan support
- [ ] Export to JSON/CSV
- [ ] Duplicate file detection
- [ ] Visual charts in TUI

## Acknowledgments

Inspired by tools like `ncdu`, `dust`, and `dua`, but with a focus on historical tracking and cleanup workflows.

Built with love by godlike Rust developers.
