# User Guide

A comprehensive guide to using rootkitty for disk usage analysis and cleanup.

## Table of Contents

- [Introduction](#introduction)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Commands](#commands)
- [TUI Guide](#tui-guide)
- [Workflows](#workflows)
- [Tips and Tricks](#tips-and-tricks)
- [Troubleshooting](#troubleshooting)
- [FAQ](#faq)

## Introduction

Rootkitty is a fast, efficient disk usage analyzer that helps you:
- Identify what's taking up space on your disk
- Track disk usage changes over time
- Plan and execute cleanup operations
- Compare disk usage between different time periods

### Key Features

- **Fast scanning**: Parallel filesystem traversal
- **Historical tracking**: Keep multiple scans and compare them
- **Interactive TUI**: Browse files with keyboard navigation
- **Safe cleanup**: Generate review-able scripts instead of deleting directly
- **Persistent storage**: SQLite database stores all scan data

## Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/yourusername/rootkitty.git
cd rootkitty

# Build and install
cargo install --path crates/rootkitty

# Or build and use directly
cargo build --release
# Binary will be at: target/release/rootkitty
```

### Verify Installation

```bash
rootkitty --version
rootkitty --help
```

## Quick Start

### 1. Your First Scan

```bash
# Scan your home directory
rootkitty scan ~

# Scan a specific directory
rootkitty scan /path/to/directory

# Example output:
# Scanning: /Users/you
# Scan complete!
#   Files: 45231
#   Directories: 3421
#   Total size: 15987654321 bytes
# Storing results...
# Scan 1 saved to database
```

### 2. Browse Results

```bash
rootkitty browse
```

This opens an interactive TUI where you can:
- View all scans
- Browse files sorted by size
- Mark files for cleanup
- Generate cleanup scripts

### 3. View Results from CLI

```bash
# List all scans
rootkitty list

# Show details of a specific scan
rootkitty show 1

# Compare two scans
rootkitty diff 1 2
```

## Commands

### `rootkitty scan <PATH>`

Scan a directory and store results in the database.

**Arguments**:
- `<PATH>`: Directory to scan (absolute or relative path)

**Options**:
- `--db <PATH>`: Custom database location (default: `~/.config/rootkitty/rootkitty.db`)

**Examples**:
```bash
# Scan current directory
rootkitty scan .

# Scan home directory
rootkitty scan ~

# Scan with custom database
rootkitty --db /tmp/my-scans.db scan /data

# Scan multiple directories (run multiple commands)
rootkitty scan ~/Documents
rootkitty scan ~/Downloads
rootkitty scan ~/Desktop
```

**What happens during a scan**:
1. Recursively walks directory tree
2. Calculates size of each file and directory
3. Records metadata (modification time, depth, etc.)
4. Stores everything in SQLite database
5. Returns summary statistics

**Performance**:
- Typical speed: 50k-100k files per second (depends on hardware)
- Memory usage: ~200 bytes per file
- Disk usage: ~200 bytes per file in database

### `rootkitty browse`

Launch the interactive TUI for browsing scans and managing cleanup.

**Options**:
- `--db <PATH>`: Custom database location

**Example**:
```bash
rootkitty browse
```

See [TUI Guide](#tui-guide) for detailed usage.

### `rootkitty list`

List all scans in the database.

**Options**:
- `--db <PATH>`: Custom database location

**Example**:
```bash
rootkitty list

# Output:
# ID    Path                         Files    Size (MB)   Date
# -------------------------------------------------------------------
# 3     /Users/you/Documents         5231     1234.56     2025-01-03 14:30:00
# 2     /Users/you/Downloads         892      5678.90     2025-01-02 10:15:00
# 1     /Users/you                   45231    15234.56    2025-01-01 09:00:00
```

### `rootkitty show <SCAN_ID>`

Show detailed information about a specific scan.

**Arguments**:
- `<SCAN_ID>`: ID of the scan to display

**Example**:
```bash
rootkitty show 1

# Output:
# Scan ID: 1
# Root path: /Users/you
# Started: 2025-01-01 09:00:00
# Completed: 2025-01-01 09:05:23
# Status: completed
# Files: 45231
# Directories: 3421
# Total size: 14567.89 MB
#
# Largest files:
#   📁 /Users/you/Movies (8234.56 MB)
#   📁 /Users/you/Photos (3456.78 MB)
#   📄 /Users/you/large-file.zip (1234.56 MB)
#   ...
```

### `rootkitty diff <SCAN_ID_1> <SCAN_ID_2>`

Compare two scans to see changes in disk usage.

**Arguments**:
- `<SCAN_ID_1>`: First scan ID (older)
- `<SCAN_ID_2>`: Second scan ID (newer)

**Example**:
```bash
rootkitty diff 1 2

# Output:
# Comparing scans 1 and 2
#
# Scan 1: /Users/you
#   Date: 2025-01-01 09:00:00
#   Files: 45231
#   Size: 14567.89 MB
#
# Scan 2: /Users/you
#   Date: 2025-01-02 15:30:00
#   Files: 45892
#   Size: 15103.22 MB
#
# Differences:
#   Files: +661
#   Size: +535.33 MB
#
#   ⚠️  Disk usage increased!
```

## TUI Guide

The TUI (Terminal User Interface) provides an interactive way to explore scans and manage cleanup.

### Starting the TUI

```bash
rootkitty browse
```

### Views

The TUI has three main views:

#### 1. Scans View (Press `1`)

Lists all historical scans.

**Display**:
```
┌ Scans (1) | Press Enter to view | ↑/↓ or j/k to navigate ─┐
│>> ✓ /Users/you | 45231 files | 14567.89 MB | 2025-01-01  │
│   ✓ /Users/you/Downloads | 892 files | 5678.90 MB | ...  │
│   ✓ /Users/you/Documents | 5231 files | 1234.56 MB | ... │
└──────────────────────────────────────────────────────────┘
```

**Actions**:
- `↑/↓` or `j/k`: Navigate scans
- `Enter`: Select scan and view files
- `q`: Quit

**Scan status icons**:
- `✓`: Completed successfully
- `⟳`: Currently running
- `✗`: Failed

#### 2. Files View (Press `2`)

Shows files in the selected scan, sorted by size.

**Display**:
```
┌ Files (2) | Scan: /Users/you | Space to mark for cleanup ─┐
│>> 📁 Movies (8234.56 MB)                                   │
│   📁 Photos (3456.78 MB)                                   │
│   📄 large-file.zip (1234.56 MB)                           │
│   📁 node_modules (987.65 MB)                              │
│     📄 video.mp4 (543.21 MB)                               │
└──────────────────────────────────────────────────────────┘
```

**Actions**:
- `↑/↓` or `j/k`: Navigate files
- `Space`: Mark/unmark file for cleanup
- `1/2/3`: Switch views
- `q`: Quit

**File icons**:
- `📁`: Directory (size is cumulative)
- `📄`: File

#### 3. Cleanup View (Press `3`)

Shows files marked for cleanup and allows script generation.

**Display**:
```
┌ Cleanup (3) | 5 items | 12.5 GB total | 'g' to generate │
│>> 📁 /Users/you/Movies/old-movies (8234.56 MB)           │
│   📁 /Users/you/node_modules (987.65 MB)                 │
│   📄 /Users/you/large-file.zip (1234.56 MB)              │
│   📄 /Users/you/temp.bin (543.21 MB)                     │
│   📄 /Users/you/cache.data (234.56 MB)                   │
└────────────────────────────────────────────────────────┘
```

**Actions**:
- `↑/↓` or `j/k`: Navigate items
- `Space`: Remove item from cleanup list
- `g`: Generate cleanup script
- `1/2/3`: Switch views
- `q`: Quit

### Keyboard Shortcuts

Global shortcuts (work in all views):
- `q`: Quit application
- `1`: Switch to Scans view
- `2`: Switch to Files view
- `3`: Switch to Cleanup view
- `↑/↓`: Navigate up/down
- `j/k`: Navigate up/down (vim-style)

View-specific shortcuts:
- **Scans view**: `Enter` to select scan
- **Files view**: `Space` to mark for cleanup
- **Cleanup view**: `Space` to remove, `g` to generate script

### Status Bar

The bottom of the screen shows:
- Current status messages
- Available keyboard shortcuts
- Context-sensitive help

## Workflows

### Workflow 1: Find and Clean Large Files

**Goal**: Free up space by identifying and removing large files.

**Steps**:

1. **Scan your directory**:
   ```bash
   rootkitty scan ~
   ```

2. **Launch TUI**:
   ```bash
   rootkitty browse
   ```

3. **View the scan** (should already be selected):
   - Press `Enter` or `2` to view files

4. **Browse large files**:
   - Files are sorted by size (largest first)
   - Use `↑/↓` or `j/k` to navigate
   - Look for unexpectedly large files or directories

5. **Mark files for cleanup**:
   - Press `Space` on each file you want to delete
   - Status bar confirms: "Marked 'filename' for cleanup"

6. **Review cleanup list**:
   - Press `3` to switch to Cleanup view
   - Review all marked items
   - Use `Space` to unmark any you want to keep

7. **Generate cleanup script**:
   - Press `g` to generate `cleanup.sh`
   - Status bar confirms: "Generated cleanup.sh - Review before running!"

8. **Exit TUI**:
   - Press `q` to quit

9. **Review and execute cleanup**:
   ```bash
   # IMPORTANT: Review the script first!
   cat cleanup.sh

   # If everything looks good, make it executable and run
   chmod +x cleanup.sh
   ./cleanup.sh

   # Or run directly with bash
   bash cleanup.sh
   ```

10. **Verify results**:
    ```bash
    # Scan again to see the difference
    rootkitty scan ~

    # Compare with previous scan
    rootkitty diff 1 2
    ```

### Workflow 2: Track Disk Usage Over Time

**Goal**: Monitor how disk usage changes and identify growth patterns.

**Steps**:

1. **Create baseline scan**:
   ```bash
   rootkitty scan /data
   # Note the scan ID, e.g., "Scan 1 saved to database"
   ```

2. **Work normally** for a period (days, weeks, months)

3. **Create new scan**:
   ```bash
   rootkitty scan /data
   # Note the new scan ID, e.g., "Scan 2 saved to database"
   ```

4. **Compare scans**:
   ```bash
   rootkitty diff 1 2
   ```

5. **Analyze differences**:
   - Positive file count: Files were added
   - Negative file count: Files were removed
   - Positive size: Disk usage increased
   - Negative size: Disk usage decreased

6. **Investigate large changes**:
   ```bash
   # Browse the new scan to see what changed
   rootkitty browse
   # Select scan 2, look for new large files
   ```

### Workflow 3: Compare Different Directories

**Goal**: Compare disk usage across different directories.

**Steps**:

1. **Scan multiple directories**:
   ```bash
   rootkitty scan ~/Documents
   rootkitty scan ~/Downloads
   rootkitty scan ~/Desktop
   rootkitty scan ~/Movies
   ```

2. **List all scans**:
   ```bash
   rootkitty list
   ```

3. **Compare sizes** from the list output

4. **Drill into largest directory**:
   ```bash
   rootkitty show <ID>
   ```

5. **Browse interactively**:
   ```bash
   rootkitty browse
   # Navigate to the largest scan
   # Explore subdirectories
   ```

### Workflow 4: Automated Scheduled Scans

**Goal**: Automatically track disk usage daily/weekly.

**Steps**:

1. **Create scan script** (`~/bin/rootkitty-scan.sh`):
   ```bash
   #!/bin/bash
   /path/to/rootkitty scan /data

   # Optional: Send notification
   # notify-send "Rootkitty" "Scan complete"
   ```

2. **Make executable**:
   ```bash
   chmod +x ~/bin/rootkitty-scan.sh
   ```

3. **Add to cron** (Linux/macOS):
   ```bash
   crontab -e

   # Add line (runs daily at 2 AM):
   0 2 * * * /home/you/bin/rootkitty-scan.sh
   ```

4. **View historical scans**:
   ```bash
   rootkitty list

   # Compare any two scans
   rootkitty diff 5 10
   ```

## Tips and Tricks

### Performance Tips

1. **Scan frequently-changing directories separately**:
   ```bash
   # Instead of scanning entire home
   rootkitty scan ~/Documents
   rootkitty scan ~/Downloads  # Changes frequently
   rootkitty scan ~/Pictures   # Changes less
   ```

2. **Use release build** for large scans:
   ```bash
   cargo build --release
   target/release/rootkitty scan /large/directory
   ```

3. **Exclude unwanted directories** (future feature):
   ```bash
   # Not yet implemented, but planned:
   # rootkitty scan ~ --exclude node_modules --exclude .git
   ```

### Safety Tips

1. **Always review cleanup scripts**:
   ```bash
   cat cleanup.sh  # Review BEFORE executing
   ```

2. **Test with dry run** (create your own):
   ```bash
   # Edit cleanup.sh to add 'echo' before commands
   sed 's/^rm /echo rm /' cleanup.sh > cleanup-dry.sh
   bash cleanup-dry.sh
   ```

3. **Backup before cleanup**:
   ```bash
   # For important data
   tar czf backup-$(date +%Y%m%d).tar.gz /path/to/files
   bash cleanup.sh
   ```

4. **Start small**:
   - Mark a few files first
   - Generate and review script
   - Execute and verify
   - Then mark more files if comfortable

### TUI Tips

1. **Navigate faster**:
   - `j`/`k` are faster than arrow keys (vim-style)
   - Numbers `1`/`2`/`3` quickly switch views

2. **Mark multiple files**:
   - `Space`, `↓`, `Space`, `↓`, ... (mark several)
   - Switch to cleanup view to review all at once

3. **Status messages**:
   - Watch status bar for confirmation
   - Error messages appear there too

### Database Tips

1. **Specify custom database location**:
   ```bash
   # Use project-specific database
   cd ~/project
   rootkitty --db .rootkitty.db scan .
   ```

2. **Backup database**:
   ```bash
   cp ~/.config/rootkitty/rootkitty.db{,.backup}
   ```

3. **View database directly** (advanced):
   ```bash
   sqlite3 ~/.config/rootkitty/rootkitty.db
   # Run SQL queries directly
   ```

## Troubleshooting

### Problem: "Permission denied" during scan

**Symptoms**:
- Some directories are skipped
- File count seems lower than expected

**Cause**: Insufficient permissions to read certain directories

**Solution**:
- This is normal and expected behavior
- Scanner silently skips inaccessible files
- Run with elevated permissions if needed:
  ```bash
  sudo rootkitty scan /
  ```

**Note**: Scan results will still be useful even with some skipped files.

### Problem: "Database is locked"

**Symptoms**:
- Error message: "database is locked"
- Commands fail or hang

**Cause**: Another rootkitty process has the database open

**Solution**:
1. Close other rootkitty instances (especially TUI)
2. Wait a few seconds for locks to release
3. Try again

**Prevention**: Only run one TUI instance at a time.

### Problem: TUI doesn't display correctly

**Symptoms**:
- Garbled text
- Incorrect colors
- Layout issues

**Causes**:
- Terminal too small
- Incompatible terminal emulator
- TERM variable incorrect

**Solutions**:

1. **Resize terminal**: Minimum 80x24 characters
   ```bash
   # Check current size
   echo $COLUMNS x $LINES
   ```

2. **Try different terminal**:
   - macOS: iTerm2, Terminal.app
   - Linux: GNOME Terminal, Konsole, Alacritty
   - Windows: Windows Terminal, Alacritty

3. **Check TERM variable**:
   ```bash
   echo $TERM
   # Should be: xterm-256color or similar

   # Try setting it:
   export TERM=xterm-256color
   rootkitty browse
   ```

### Problem: Scan is very slow

**Symptoms**:
- Scanning takes much longer than expected
- Progress seems stuck

**Causes**:
- Network filesystem (NFS, SMB)
- Spinning disk (not SSD)
- Very large directory tree
- System under heavy load

**Solutions**:

1. **Check if filesystem is local**:
   ```bash
   df -h /path/to/scan
   ```

2. **Monitor scan progress** (in another terminal):
   ```bash
   # Watch file count grow in database
   watch -n 1 'sqlite3 ~/.config/rootkitty/rootkitty.db "SELECT COUNT(*) FROM file_entries"'
   ```

3. **Reduce system load**: Close other applications

4. **Be patient**: Large scans (>1M files) can take several minutes

### Problem: Cleanup script won't run

**Symptoms**:
- `bash cleanup.sh` fails
- Permission errors

**Causes**:
- Files already deleted
- Permissions changed
- Paths with special characters

**Solutions**:

1. **Check if files still exist**:
   ```bash
   # Test first file manually
   ls -l /path/from/script
   ```

2. **Run with appropriate permissions**:
   ```bash
   # May need sudo for system files
   sudo bash cleanup.sh
   ```

3. **Fix path escaping** (if needed):
   - Edit `cleanup.sh`
   - Ensure paths with spaces are quoted correctly

## FAQ

### General Questions

**Q: Is rootkitty safe to use?**

A: Yes. Rootkitty only reads your filesystem during scans. It never deletes files automatically. You must manually review and execute the generated cleanup scripts.

**Q: How much disk space does the database use?**

A: Approximately 200 bytes per file. For example:
- 10k files: ~2 MB
- 100k files: ~20 MB
- 1M files: ~200 MB

**Q: Can I use rootkitty on Windows?**

A: Currently, rootkitty is tested primarily on macOS and Linux. Windows support is planned but not yet fully tested. Contributions welcome!

**Q: Does rootkitty follow symbolic links?**

A: Currently, no. Symlinks are treated as files with their own size (usually small). Following symlinks could cause infinite loops or count files multiple times. This may be added as an option in the future.

### Scanning Questions

**Q: Why doesn't my scan show all files?**

A: Possible reasons:
- Permission denied (files skipped automatically)
- Symlinks not followed (by design)
- Hidden files are included (rootkitty scans everything it can access)

**Q: Can I scan multiple directories at once?**

A: Not in a single command. Run multiple scan commands:
```bash
rootkitty scan /dir1
rootkitty scan /dir2
rootkitty scan /dir3
```

**Q: How do I exclude certain directories?**

A: Not yet implemented. Planned for future release. Track: GitHub issue #xxx

**Q: Why are directory sizes so large?**

A: Directory sizes are **cumulative** (include all contents). This is by design so you can identify which top-level directories consume the most space.

### Database Questions

**Q: Where is the database stored?**

A: Default location: `~/.config/rootkitty/rootkitty.db`

You can specify a different location with `--db`:
```bash
rootkitty --db /path/to/database.db scan ~
```

**Q: Can I delete old scans?**

A: Not yet implemented via CLI. You can delete directly in SQLite:
```bash
sqlite3 ~/.config/rootkitty/rootkitty.db
DELETE FROM scans WHERE id = 1;  -- Cascades to file_entries and cleanup_items
.quit
```

**Q: Can I export scan data?**

A: Not yet implemented. Planned: JSON and CSV export. You can query the SQLite database directly for now.

**Q: Is the database format stable?**

A: Yes, migrations are used to preserve data across schema changes.

### Cleanup Questions

**Q: Can rootkitty delete files directly?**

A: No. This is a safety feature. Rootkitty generates a script that **you** must review and execute manually.

**Q: What if I accidentally mark the wrong file?**

A: No problem! In the Cleanup view (press `3`), select the file and press `Space` to unmark it.

**Q: Can I edit the cleanup script?**

A: Yes! It's just a bash script. Edit with any text editor before running.

**Q: What happens if I mark a directory for cleanup?**

A: The cleanup script will use `rm -rf` for directories, removing the entire directory tree. **Review carefully!**

### Performance Questions

**Q: How fast is rootkitty compared to `du`?**

A: Rootkitty uses parallel scanning and can be 2-5x faster than `du -sh` for large directory trees, especially on multi-core systems.

**Q: Does rootkitty use a lot of memory?**

A: Approximately 200 bytes per file in memory during scanning. After scanning, memory is released (data is in database).

**Q: Can I scan while the TUI is open?**

A: Not recommended. Close the TUI first, run the scan, then reopen the TUI to see new data.

## Advanced Usage

### Using with Scripts

```bash
#!/bin/bash
# Automated cleanup workflow

# Scan
rootkitty scan ~/Downloads

# Get scan ID (last scan)
SCAN_ID=$(sqlite3 ~/.config/rootkitty/rootkitty.db \
    "SELECT id FROM scans ORDER BY started_at DESC LIMIT 1")

# Get size
SIZE=$(sqlite3 ~/.config/rootkitty/rootkitty.db \
    "SELECT total_size FROM scans WHERE id = $SCAN_ID")

echo "Scan $SCAN_ID: $SIZE bytes"

# Alert if over threshold
if [ $SIZE -gt 10000000000 ]; then  # 10 GB
    echo "Downloads folder exceeds 10 GB!"
    # notify-send "Clean up Downloads!"
fi
```

### Custom Database Queries

```bash
# Find largest files across all scans
sqlite3 ~/.config/rootkitty/rootkitty.db \
    "SELECT scans.root_path, file_entries.path, file_entries.size
     FROM file_entries
     JOIN scans ON file_entries.scan_id = scans.id
     WHERE file_entries.is_dir = 0
     ORDER BY file_entries.size DESC
     LIMIT 10"

# Count files by extension
sqlite3 ~/.config/rootkitty/rootkitty.db \
    "SELECT
        SUBSTR(name, INSTR(name, '.') + 1) AS ext,
        COUNT(*) AS count,
        SUM(size) AS total_size
     FROM file_entries
     WHERE is_dir = 0 AND name LIKE '%.%'
     GROUP BY ext
     ORDER BY total_size DESC
     LIMIT 20"
```

## Getting Help

- **Documentation**: See `docs/` directory
- **Issues**: https://github.com/yourusername/rootkitty/issues
- **Discussions**: https://github.com/yourusername/rootkitty/discussions

## Next Steps

- Read [Architecture](architecture.md) to understand how rootkitty works
- Read [Development Guide](development.md) if you want to contribute
- Check out the [Database Schema](database.md) for advanced queries
