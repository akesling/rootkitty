# Rootkitty Documentation

Welcome to the rootkitty documentation! This directory contains comprehensive guides for users, developers, and AI assistants.

## Documentation Overview

### For Users

If you're using rootkitty to analyze disk usage:

📖 **[User Guide](user-guide.md)** - Start here!
- Installation instructions
- Command reference
- TUI guide with screenshots
- Common workflows
- Tips and tricks
- Troubleshooting
- FAQ

### For Developers

If you're contributing to rootkitty:

🔧 **[Development Guide](development.md)**
- Setting up dev environment
- Building and testing
- Code style and conventions
- Debugging techniques
- Contributing guidelines

🏗️ **[Architecture](architecture.md)**
- System design and principles
- Module descriptions
- Data flow diagrams
- Performance considerations
- Extension points

🗄️ **[Database Schema](database.md)**
- SQLite schema documentation
- Query examples
- Migration guide
- Optimization tips
- Troubleshooting

### For AI Assistants

If you're an AI (like Claude) working on this codebase:

🤖 **[CLAUDE.md](../CLAUDE.md)** (in root directory)
- Project overview and philosophy
- Coding conventions
- Common patterns and pitfalls
- Module responsibilities
- Quick reference guide

## Quick Links

### Getting Started
- [Installation](user-guide.md#installation)
- [Quick Start](user-guide.md#quick-start)
- [First Scan](user-guide.md#1-your-first-scan)

### Common Tasks
- [Finding Large Files](user-guide.md#workflow-1-find-and-clean-large-files)
- [Tracking Disk Usage Over Time](user-guide.md#workflow-2-track-disk-usage-over-time)
- [Generating Cleanup Scripts](user-guide.md#workflow-1-find-and-clean-large-files)

### Development
- [Building from Source](development.md#clone-and-build)
- [Running Tests](development.md#running-tests)
- [Adding Features](development.md#common-development-tasks)

### Technical Details
- [Scanner Algorithm](architecture.md#scanner-module-scannerrs)
- [Database Design](database.md#schema)
- [TUI Implementation](architecture.md#ui-module-uirs)

## Documentation Structure

```
docs/
├── README.md           # This file - documentation index
├── user-guide.md       # Complete user manual
├── development.md      # Developer setup and workflow
├── architecture.md     # System design and internals
└── database.md         # Database schema and queries

../
├── CLAUDE.md          # AI assistant guide (in project root)
└── README.md          # Project README (in project root)
```

## Finding What You Need

### I want to...

**...use rootkitty**
→ Start with [User Guide](user-guide.md)

**...scan a directory**
→ See [rootkitty scan](user-guide.md#rootkitty-scan-path)

**...use the TUI**
→ Read [TUI Guide](user-guide.md#tui-guide)

**...clean up files**
→ Follow [Cleanup Workflow](user-guide.md#workflow-1-find-and-clean-large-files)

**...contribute code**
→ Read [Development Guide](development.md)

**...understand the architecture**
→ See [Architecture](architecture.md)

**...write custom queries**
→ Check [Database Schema](database.md#query-examples)

**...add a new feature**
→ See [Adding Features](development.md#common-development-tasks)

**...optimize performance**
→ Read [Performance Section](architecture.md#performance-targets)

## Documentation Conventions

### Code Examples

Shell commands are shown with `bash` syntax:
```bash
rootkitty scan ~/Documents
```

Rust code examples are shown with syntax highlighting:
```rust
pub fn scan(&self) -> Result<Vec<FileEntry>> {
    // implementation
}
```

SQL queries are formatted for readability:
```sql
SELECT path, size
FROM file_entries
WHERE scan_id = ?
ORDER BY size DESC;
```

### Placeholders

Documentation uses these placeholders:
- `<SCAN_ID>`: Replace with actual scan ID (e.g., `1`, `2`)
- `<PATH>`: Replace with actual path (e.g., `/home/user`)
- `<argument>`: Replace with actual value

Example:
```bash
# Documentation shows:
rootkitty show <SCAN_ID>

# You would run:
rootkitty show 1
```

### Admonitions

Important information is highlighted:

**Note**: Additional information

**Warning**: Something to be careful about

**Tip**: Helpful suggestion

## Contributing to Documentation

Documentation improvements are always welcome!

### What to Document

- **New features**: Update user-guide.md
- **Code changes**: Update architecture.md or development.md
- **Database changes**: Update database.md
- **Bug fixes**: Update troubleshooting sections
- **Examples**: Add to relevant guides

### Style Guide

- Use clear, simple language
- Include code examples for commands
- Add expected output when helpful
- Link to related sections
- Keep sections focused and scannable

### Submitting Changes

1. Edit the relevant `.md` file
2. Test that links work
3. Check formatting in a Markdown viewer
4. Submit a pull request with description

## Getting Help

### For Users

- **Bugs**: Report at GitHub Issues
- **Questions**: Ask in GitHub Discussions
- **Feature Requests**: Open an issue with "enhancement" label

### For Developers

- **Technical Questions**: GitHub Discussions
- **Code Review**: Pull requests
- **Design Decisions**: GitHub Issues with "design" label

## Version Information

Documentation for rootkitty v0.1.0

Last updated: 2025-01-02

---

## Additional Resources

### Related Tools

- [ncdu](https://dev.yorhel.nl/ncdu) - NCurses Disk Usage
- [dust](https://github.com/bootandy/dust) - du + rust
- [dua](https://github.com/Byron/dua-cli) - Disk Usage Analyzer

### Learning Resources

- [Rust Book](https://doc.rust-lang.org/book/)
- [SQLite Documentation](https://www.sqlite.org/docs.html)
- [Ratatui Book](https://ratatui.rs/)

### Community

- [Project Repository](https://github.com/yourusername/rootkitty)
- [Issue Tracker](https://github.com/yourusername/rootkitty/issues)
- [Discussions](https://github.com/yourusername/rootkitty/discussions)

---

**Happy exploring!** 🐱
