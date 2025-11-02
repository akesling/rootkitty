use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;

use crate::db::{ActorMessage, Database, DatabaseActor, Scan, StoredFileEntry};
use crate::scanner::{ProgressUpdate, Scanner};
use std::path::PathBuf;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    ScanList,
    FileTree,
    CleanupList,
    ScanDialog,
    Scanning,
}

#[derive(Debug, Clone)]
struct ScanProgress {
    entries_scanned: u64,
    active_dirs: Vec<(String, usize, usize)>,
    active_workers: usize,
}

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
    scan_input: String,
    scan_progress: Option<ScanProgress>,
    previous_view: View,
}

impl App {
    pub fn new(db: Database) -> Self {
        let mut scan_list_state = ListState::default();
        scan_list_state.select(Some(0));

        Self {
            db,
            view: View::ScanList,
            scans: Vec::new(),
            scan_list_state,
            current_scan: None,
            file_entries: Vec::new(),
            file_list_state: ListState::default(),
            cleanup_items: Vec::new(),
            cleanup_list_state: ListState::default(),
            status_message: String::from("Press 'n' to scan | '?' for help"),
            scan_input: String::new(),
            scan_progress: None,
            previous_view: View::ScanList,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        self.load_scans().await?;

        let result = self.run_event_loop(&mut terminal).await;

        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        result
    }

    async fn run_event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    ) -> Result<()> {
        loop {
            terminal.draw(|f| self.render(f))?;

            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    match self.view {
                        View::ScanList => match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Char('n') => {
                                self.previous_view = View::ScanList;
                                self.view = View::ScanDialog;
                                self.scan_input.clear();
                            }
                            KeyCode::Down | KeyCode::Char('j') => self.scan_list_next(),
                            KeyCode::Up | KeyCode::Char('k') => self.scan_list_previous(),
                            KeyCode::Enter => {
                                if let Err(e) = self.select_scan().await {
                                    self.status_message = format!("Error: {}", e);
                                }
                            }
                            KeyCode::Char('1') => self.view = View::ScanList,
                            KeyCode::Char('2') => {
                                if self.current_scan.is_some() {
                                    self.view = View::FileTree;
                                }
                            }
                            KeyCode::Char('3') => {
                                if self.current_scan.is_some() {
                                    if let Err(e) = self.load_cleanup_items().await {
                                        self.status_message = format!("Error: {}", e);
                                    } else {
                                        self.view = View::CleanupList;
                                    }
                                }
                            }
                            _ => {}
                        },
                        View::FileTree => match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Down | KeyCode::Char('j') => self.file_list_next(),
                            KeyCode::Up | KeyCode::Char('k') => self.file_list_previous(),
                            KeyCode::Char(' ') => {
                                if let Err(e) = self.toggle_cleanup_mark().await {
                                    self.status_message = format!("Error: {}", e);
                                }
                            }
                            KeyCode::Char('1') => self.view = View::ScanList,
                            KeyCode::Char('2') => self.view = View::FileTree,
                            KeyCode::Char('3') => {
                                if let Err(e) = self.load_cleanup_items().await {
                                    self.status_message = format!("Error: {}", e);
                                } else {
                                    self.view = View::CleanupList;
                                }
                            }
                            _ => {}
                        },
                        View::CleanupList => match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Down | KeyCode::Char('j') => self.cleanup_list_next(),
                            KeyCode::Up | KeyCode::Char('k') => self.cleanup_list_previous(),
                            KeyCode::Char('g') => {
                                self.generate_cleanup_script();
                            }
                            KeyCode::Char(' ') => {
                                if let Err(e) = self.remove_from_cleanup().await {
                                    self.status_message = format!("Error: {}", e);
                                }
                            }
                            KeyCode::Char('1') => self.view = View::ScanList,
                            KeyCode::Char('2') => self.view = View::FileTree,
                            KeyCode::Char('3') => self.view = View::CleanupList,
                            _ => {}
                        },
                        View::ScanDialog => match key.code {
                            KeyCode::Esc => {
                                self.view = self.previous_view;
                                self.scan_input.clear();
                            }
                            KeyCode::Enter => {
                                if !self.scan_input.is_empty() {
                                    let path = self.scan_input.clone();
                                    if let Err(e) = self.start_scan(path, terminal).await {
                                        self.status_message = format!("Scan error: {}", e);
                                        self.view = self.previous_view;
                                    }
                                }
                            }
                            KeyCode::Backspace => {
                                self.scan_input.pop();
                            }
                            KeyCode::Char(c) => {
                                self.scan_input.push(c);
                            }
                            _ => {}
                        },
                        View::Scanning => {
                            // During scanning, only allow cancel
                            if let KeyCode::Char('q') | KeyCode::Esc = key.code {
                                self.status_message =
                                    "Scan cancelled (not implemented yet)".to_string();
                                // TODO: Actually cancel the scan
                            }
                        }
                    }
                }
            }
        }
    }

    fn render(&mut self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(3)])
            .split(f.area());

        match self.view {
            View::ScanList => self.render_scan_list(f, chunks[0]),
            View::FileTree => self.render_file_tree(f, chunks[0]),
            View::CleanupList => self.render_cleanup_list(f, chunks[0]),
            View::ScanDialog => self.render_scan_dialog(f, chunks[0]),
            View::Scanning => self.render_scanning(f, chunks[0]),
        }

        self.render_status_bar(f, chunks[1]);
    }

    fn render_scan_list(&mut self, f: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .scans
            .iter()
            .map(|scan| {
                let size_mb = scan.total_size as f64 / 1_048_576.0;
                let status = match scan.status.as_str() {
                    "completed" => "✓",
                    "running" => "⟳",
                    _ => "✗",
                };
                let content = format!(
                    "{} {} | {} files | {:.2} MB | {}",
                    status,
                    scan.root_path,
                    scan.total_files,
                    size_mb,
                    scan.started_at.format("%Y-%m-%d %H:%M")
                );
                ListItem::new(content)
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Scans (1) | Press Enter to view | ↑/↓ or j/k to navigate"),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">> ");

        f.render_stateful_widget(list, area, &mut self.scan_list_state);
    }

    fn render_file_tree(&mut self, f: &mut Frame, area: Rect) {
        let title = if let Some(scan) = &self.current_scan {
            format!(
                "Files (2) | Scan: {} | Space to mark for cleanup",
                scan.root_path
            )
        } else {
            "Files (2)".to_string()
        };

        let items: Vec<ListItem> = self
            .file_entries
            .iter()
            .map(|entry| {
                let size_str = format_size(entry.size as u64);
                let icon = if entry.is_dir { "📁" } else { "📄" };
                let indent = "  ".repeat(entry.depth as usize);
                let content = format!("{}{} {} ({})", indent, icon, entry.name, size_str);
                ListItem::new(content)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">> ");

        f.render_stateful_widget(list, area, &mut self.file_list_state);
    }

    fn render_cleanup_list(&mut self, f: &mut Frame, area: Rect) {
        let total_size: i64 = self.cleanup_items.iter().map(|e| e.size).sum();
        let title = format!(
            "Cleanup List (3) | {} items | {} total | 'g' to generate script | Space to remove",
            self.cleanup_items.len(),
            format_size(total_size as u64)
        );

        let items: Vec<ListItem> = self
            .cleanup_items
            .iter()
            .map(|entry| {
                let size_str = format_size(entry.size as u64);
                let icon = if entry.is_dir { "📁" } else { "📄" };
                let content = format!("{} {} ({})", icon, entry.path, size_str);
                ListItem::new(content)
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(title))
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">> ");

        f.render_stateful_widget(list, area, &mut self.cleanup_list_state);
    }

    fn render_scan_dialog(&self, f: &mut Frame, area: Rect) {
        let text = vec![
            Line::from(""),
            Line::from("Enter path to scan:"),
            Line::from(""),
            Line::from(vec![Span::styled(
                &self.scan_input,
                Style::default().fg(Color::Cyan),
            )]),
            Line::from(""),
            Line::from("Press Enter to start scan, Esc to cancel"),
        ];

        let paragraph = Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("New Scan"))
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }

    fn render_scanning(&self, f: &mut Frame, area: Rect) {
        let mut lines = vec![
            Line::from(""),
            Line::from(vec![Span::styled(
                "Scanning in progress...",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
        ];

        if let Some(progress) = &self.scan_progress {
            lines.push(Line::from(format!(
                "Entries scanned: {}",
                progress.entries_scanned
            )));

            if progress.active_workers > 0 {
                lines.push(Line::from(format!(
                    "Parallel workers: {}",
                    progress.active_workers
                )));
            }

            lines.push(Line::from(""));
            lines.push(Line::from("Active directories:"));

            for (path, done, total) in progress.active_dirs.iter().take(5) {
                let percentage = if *total > 0 {
                    (*done as f64 / *total as f64 * 100.0) as usize
                } else {
                    0
                };
                lines.push(Line::from(format!(
                    "  [{}/{}] {:>3}% {}",
                    done,
                    total,
                    percentage,
                    Self::smart_truncate_path(path, 60)
                )));
            }

            if progress.active_dirs.len() > 5 {
                lines.push(Line::from(format!(
                    "  ... and {} more directories",
                    progress.active_dirs.len() - 5
                )));
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from("Press q or Esc to cancel (not implemented)"));

        let paragraph = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Scanning"))
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }

    fn render_status_bar(&self, f: &mut Frame, area: Rect) {
        let help_text = match self.view {
            View::ScanList => {
                "q: quit | n: new scan | 1: scans | 2: files | 3: cleanup | ↑↓/jk: navigate | Enter: select"
            }
            View::FileTree => {
                "q: quit | 1: scans | 2: files | 3: cleanup | Space: mark | ↑↓/jk: navigate"
            }
            View::CleanupList => {
                "q: quit | 1: scans | 2: files | 3: cleanup | g: generate | Space: remove"
            }
            View::ScanDialog => {
                "Enter: start scan | Esc: cancel | Type path to scan"
            }
            View::Scanning => {
                "Scanning in progress... | q/Esc: cancel"
            }
        };

        let text = vec![
            Line::from(vec![Span::styled(
                &self.status_message,
                Style::default().fg(Color::Yellow),
            )]),
            Line::from(vec![Span::raw(help_text)]),
        ];

        let paragraph = Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
    }

    async fn load_scans(&mut self) -> Result<()> {
        self.scans = self.db.list_scans().await?;
        if !self.scans.is_empty() && self.scan_list_state.selected().is_none() {
            self.scan_list_state.select(Some(0));
        }
        Ok(())
    }

    async fn select_scan(&mut self) -> Result<()> {
        if let Some(selected) = self.scan_list_state.selected() {
            if let Some(scan) = self.scans.get(selected) {
                self.current_scan = Some(scan.clone());
                self.file_entries = self.db.get_largest_entries(scan.id, 1000).await?;
                self.file_list_state.select(Some(0));
                self.view = View::FileTree;
                self.status_message = format!("Loaded {} entries", self.file_entries.len());
            }
        }
        Ok(())
    }

    async fn toggle_cleanup_mark(&mut self) -> Result<()> {
        if let Some(scan) = &self.current_scan {
            if let Some(selected) = self.file_list_state.selected() {
                if let Some(entry) = self.file_entries.get(selected) {
                    self.db.mark_for_cleanup(scan.id, entry.id, None).await?;
                    self.status_message = format!("Marked '{}' for cleanup", entry.name);
                }
            }
        }
        Ok(())
    }

    async fn load_cleanup_items(&mut self) -> Result<()> {
        if let Some(scan) = &self.current_scan {
            self.cleanup_items = self.db.get_cleanup_items(scan.id).await?;
            if !self.cleanup_items.is_empty() {
                self.cleanup_list_state.select(Some(0));
            }
            self.status_message = format!("Loaded {} cleanup items", self.cleanup_items.len());
        }
        Ok(())
    }

    async fn remove_from_cleanup(&mut self) -> Result<()> {
        if let Some(scan) = &self.current_scan {
            if let Some(selected) = self.cleanup_list_state.selected() {
                if let Some(entry) = self.cleanup_items.get(selected) {
                    let entry_id = entry.id;
                    let entry_name = entry.name.clone();

                    self.db.remove_cleanup_item(scan.id, entry_id).await?;
                    self.load_cleanup_items().await?;
                    self.status_message = format!("Removed '{}' from cleanup", entry_name);

                    // Adjust selection
                    if !self.cleanup_items.is_empty() {
                        let new_selected = selected.min(self.cleanup_items.len() - 1);
                        self.cleanup_list_state.select(Some(new_selected));
                    }
                }
            }
        }
        Ok(())
    }

    async fn start_scan(
        &mut self,
        path: String,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    ) -> Result<()> {
        let path_buf = PathBuf::from(shellexpand::tilde(&path).to_string());

        // Switch to scanning view
        self.view = View::Scanning;
        self.scan_progress = Some(ScanProgress {
            entries_scanned: 0,
            active_dirs: Vec::new(),
            active_workers: 0,
        });

        // Create scan in database
        let scan_id = self.db.create_scan(&path_buf).await?;

        // Create channels
        let (tx, rx) = mpsc::channel(100);
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<ProgressUpdate>();

        // Spawn database actor
        let actor = DatabaseActor::new(self.db.clone(), scan_id, rx);
        let actor_handle = tokio::spawn(async move { actor.run().await });

        // Clone for the scanning thread
        let tx_clone = tx.clone();
        let path_clone = path_buf.clone();

        // Spawn scanner in blocking thread
        let scan_handle = tokio::task::spawn_blocking(move || {
            let scanner = Scanner::with_sender(&path_clone, tx_clone, Some(progress_tx));
            scanner.scan()
        });

        // Poll for progress updates and render
        loop {
            // Check for progress updates
            match progress_rx.try_recv() {
                Ok(progress) => {
                    self.scan_progress = Some(ScanProgress {
                        entries_scanned: progress.files_scanned,
                        active_dirs: progress.active_dirs.clone(),
                        active_workers: progress.active_workers,
                    });
                    terminal.draw(|f| self.render(f))?;
                }
                Err(mpsc::error::TryRecvError::Empty) => {
                    // No update available, continue
                }
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    // Scanner finished
                    break;
                }
            }

            // Small sleep to avoid busy-waiting
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

            // Check if scan is complete
            if scan_handle.is_finished() {
                break;
            }
        }

        // Get scan result
        let (_, stats) = scan_handle.await??;

        // Signal actor to shutdown
        tx.send(ActorMessage::Shutdown).await?;
        drop(tx);

        // Wait for database writes to complete
        actor_handle.await??;

        // Complete the scan
        self.db.complete_scan(scan_id, &stats).await?;

        // Reload scans and return to scan list
        self.load_scans().await?;
        self.view = View::ScanList;
        self.status_message = format!(
            "Scan complete! {} files, {} dirs, {} bytes",
            stats.total_files, stats.total_dirs, stats.total_size
        );
        self.scan_progress = None;

        Ok(())
    }

    fn smart_truncate_path(path: &str, max_len: usize) -> String {
        if path.len() <= max_len {
            return path.to_string();
        }

        let parts: Vec<&str> = path.split('/').collect();

        if parts.len() <= 3 {
            let end_len = max_len.saturating_sub(3);
            return format!("...{}", &path[path.len().saturating_sub(end_len)..]);
        }

        let base = parts[..2.min(parts.len())].join("/");
        let end = if parts.len() >= 2 {
            parts[parts.len() - 2..].join("/")
        } else {
            parts.last().unwrap_or(&"").to_string()
        };

        let base_and_end = format!("{}/.../{}", base, end);

        if base_and_end.len() <= max_len {
            base_and_end
        } else {
            let start_len = (max_len / 2).saturating_sub(2);
            let end_len = (max_len / 2).saturating_sub(2);
            format!(
                "{}...{}",
                &path[..start_len],
                &path[path.len().saturating_sub(end_len)..]
            )
        }
    }

    fn generate_cleanup_script(&mut self) {
        let script_path = "cleanup.sh";
        let mut script = String::from(
            "#!/bin/bash\n\n# Rootkitty cleanup script\n# Review carefully before running!\n\n",
        );

        for entry in &self.cleanup_items {
            if entry.is_dir {
                script.push_str(&format!("rm -rf '{}'\n", entry.path));
            } else {
                script.push_str(&format!("rm '{}'\n", entry.path));
            }
        }

        if let Err(e) = std::fs::write(script_path, script) {
            self.status_message = format!("Error writing script: {}", e);
        } else {
            self.status_message = format!("Generated {} - Review before running!", script_path);
        }
    }

    fn scan_list_next(&mut self) {
        if self.scans.is_empty() {
            return;
        }
        let i = match self.scan_list_state.selected() {
            Some(i) => {
                if i >= self.scans.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.scan_list_state.select(Some(i));
    }

    fn scan_list_previous(&mut self) {
        if self.scans.is_empty() {
            return;
        }
        let i = match self.scan_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.scans.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.scan_list_state.select(Some(i));
    }

    fn file_list_next(&mut self) {
        if self.file_entries.is_empty() {
            return;
        }
        let i = match self.file_list_state.selected() {
            Some(i) => {
                if i >= self.file_entries.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.file_list_state.select(Some(i));
    }

    fn file_list_previous(&mut self) {
        if self.file_entries.is_empty() {
            return;
        }
        let i = match self.file_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.file_entries.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.file_list_state.select(Some(i));
    }

    fn cleanup_list_next(&mut self) {
        if self.cleanup_items.is_empty() {
            return;
        }
        let i = match self.cleanup_list_state.selected() {
            Some(i) => {
                if i >= self.cleanup_items.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.cleanup_list_state.select(Some(i));
    }

    fn cleanup_list_previous(&mut self) {
        if self.cleanup_items.is_empty() {
            return;
        }
        let i = match self.cleanup_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.cleanup_items.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.cleanup_list_state.select(Some(i));
    }
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
