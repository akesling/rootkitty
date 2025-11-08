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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    ScanList,
    FileTree,
    CleanupList,
    ScanDialog,
    Scanning,
    Help,
}

#[derive(Debug, Clone)]
struct ScanProgress {
    entries_scanned: u64,
    active_dirs: Vec<(String, usize, usize)>,
    active_workers: usize,
}

struct ActiveScan {
    scan_id: i64,
    scan_handle: tokio::task::JoinHandle<
        Result<(Vec<crate::scanner::FileEntry>, crate::scanner::ScanStats)>,
    >,
    actor_handle: tokio::task::JoinHandle<Result<()>>,
    tx: mpsc::Sender<ActorMessage>,
    progress_rx: mpsc::UnboundedReceiver<ProgressUpdate>,
    cancelled: Arc<AtomicBool>,
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
    /// Active scan state (if a scan is running)
    active_scan: Option<ActiveScan>,
    /// Track if 'g' was pressed for 'gg' sequence
    g_pressed: bool,
    /// Set of folded directory paths (absolute paths)
    folded_dirs: std::collections::HashSet<String>,
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
            active_scan: None,
            g_pressed: false,
            folded_dirs: std::collections::HashSet::new(),
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
                            KeyCode::Char('?') => {
                                self.previous_view = View::ScanList;
                                self.view = View::Help;
                                self.g_pressed = false;
                            }
                            KeyCode::Char('n') => {
                                self.previous_view = View::ScanList;
                                self.view = View::ScanDialog;
                                self.scan_input.clear();
                                self.g_pressed = false;
                            }
                            KeyCode::Char('r') => {
                                // Resume paused scan
                                if let Some(selected) = self.scan_list_state.selected() {
                                    if let Some(scan) = self.scans.get(selected) {
                                        if scan.status == "paused" {
                                            let path = scan.root_path.clone();
                                            let scan_id = scan.id;
                                            if let Err(e) = self.resume_scan(scan_id, path).await {
                                                self.status_message =
                                                    format!("Resume error: {}", e);
                                            }
                                        } else {
                                            self.status_message =
                                                "Selected scan is not paused".to_string();
                                        }
                                    }
                                }
                                self.g_pressed = false;
                            }
                            KeyCode::Char('g') => {
                                if self.g_pressed {
                                    // gg - jump to top
                                    self.scan_list_top();
                                    self.g_pressed = false;
                                } else {
                                    self.g_pressed = true;
                                }
                            }
                            KeyCode::Char('G') => {
                                // Shift+G - jump to bottom
                                self.scan_list_bottom();
                                self.g_pressed = false;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                self.scan_list_next();
                                self.g_pressed = false;
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                self.scan_list_previous();
                                self.g_pressed = false;
                            }
                            KeyCode::Char('d') => {
                                // Page down (Ctrl+d in vim, but using plain 'd' here)
                                self.scan_list_page_down();
                                self.g_pressed = false;
                            }
                            KeyCode::Char('u') => {
                                // Page up (Ctrl+u in vim, but using plain 'u' here)
                                self.scan_list_page_up();
                                self.g_pressed = false;
                            }
                            KeyCode::Enter | KeyCode::Char('o') => {
                                if let Err(e) = self.select_scan().await {
                                    self.status_message = format!("Error: {}", e);
                                }
                                self.g_pressed = false;
                            }
                            KeyCode::Char('1') => {
                                self.view = View::ScanList;
                                self.g_pressed = false;
                            }
                            KeyCode::Char('2') => {
                                if self.current_scan.is_some() {
                                    self.view = View::FileTree;
                                }
                                self.g_pressed = false;
                            }
                            KeyCode::Char('3') => {
                                if self.current_scan.is_some() {
                                    if let Err(e) = self.load_cleanup_items().await {
                                        self.status_message = format!("Error: {}", e);
                                    } else {
                                        self.view = View::CleanupList;
                                    }
                                }
                                self.g_pressed = false;
                            }
                            _ => {
                                self.g_pressed = false;
                            }
                        },
                        View::FileTree => match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Esc => {
                                // Go back to scan list
                                self.view = View::ScanList;
                                self.g_pressed = false;
                            }
                            KeyCode::Char('?') => {
                                self.previous_view = View::FileTree;
                                self.view = View::Help;
                                self.g_pressed = false;
                            }
                            KeyCode::Char('g') => {
                                if self.g_pressed {
                                    // gg - jump to top
                                    self.file_list_top();
                                    self.g_pressed = false;
                                } else {
                                    self.g_pressed = true;
                                }
                            }
                            KeyCode::Char('G') => {
                                // Shift+G - jump to bottom
                                self.file_list_bottom();
                                self.g_pressed = false;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                self.file_list_next();
                                self.g_pressed = false;
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                self.file_list_previous();
                                self.g_pressed = false;
                            }
                            KeyCode::Char('J') => {
                                // Shift+J - jump to next sibling (same depth)
                                self.file_list_next_sibling();
                                self.g_pressed = false;
                            }
                            KeyCode::Char('K') => {
                                // Shift+K - jump to previous sibling (same depth)
                                self.file_list_previous_sibling();
                                self.g_pressed = false;
                            }
                            KeyCode::Char('d') => {
                                // Page down
                                self.file_list_page_down();
                                self.g_pressed = false;
                            }
                            KeyCode::Char('u') => {
                                // Page up
                                self.file_list_page_up();
                                self.g_pressed = false;
                            }
                            KeyCode::Char(' ') => {
                                if let Err(e) = self.toggle_cleanup_mark().await {
                                    self.status_message = format!("Error: {}", e);
                                }
                                self.g_pressed = false;
                            }
                            KeyCode::Char('z') | KeyCode::Char('o') => {
                                // Toggle fold/unfold for selected directory (one level)
                                self.toggle_fold_directory(false);
                                self.g_pressed = false;
                            }
                            KeyCode::Char('Z') | KeyCode::Char('O') => {
                                // Unfold all nested folders recursively
                                self.toggle_fold_directory(true);
                                self.g_pressed = false;
                            }
                            KeyCode::Char('1') => {
                                self.view = View::ScanList;
                                self.g_pressed = false;
                            }
                            KeyCode::Char('2') => {
                                self.view = View::FileTree;
                                self.g_pressed = false;
                            }
                            KeyCode::Char('3') => {
                                if let Err(e) = self.load_cleanup_items().await {
                                    self.status_message = format!("Error: {}", e);
                                } else {
                                    self.view = View::CleanupList;
                                }
                                self.g_pressed = false;
                            }
                            _ => {
                                self.g_pressed = false;
                            }
                        },
                        View::CleanupList => match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Esc => {
                                // Go back to scan list
                                self.view = View::ScanList;
                                self.g_pressed = false;
                            }
                            KeyCode::Char('?') => {
                                self.previous_view = View::CleanupList;
                                self.view = View::Help;
                                self.g_pressed = false;
                            }
                            KeyCode::Char('g') => {
                                if self.g_pressed {
                                    // gg - jump to top
                                    self.cleanup_list_top();
                                    self.g_pressed = false;
                                } else {
                                    self.g_pressed = true;
                                }
                            }
                            KeyCode::Char('G') => {
                                // Shift+G - jump to bottom (or generate script if not using vim nav)
                                if self.g_pressed {
                                    // Was 'g' then 'G' - unclear intent, treat as generate
                                    self.generate_cleanup_script();
                                    self.g_pressed = false;
                                } else {
                                    self.cleanup_list_bottom();
                                }
                            }
                            KeyCode::Char('s') => {
                                // Alternative: 's' for save/script generation
                                self.generate_cleanup_script();
                                self.g_pressed = false;
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                self.cleanup_list_next();
                                self.g_pressed = false;
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                self.cleanup_list_previous();
                                self.g_pressed = false;
                            }
                            KeyCode::Char('d') => {
                                // Page down
                                self.cleanup_list_page_down();
                                self.g_pressed = false;
                            }
                            KeyCode::Char('u') => {
                                // Page up
                                self.cleanup_list_page_up();
                                self.g_pressed = false;
                            }
                            KeyCode::Char(' ') => {
                                if let Err(e) = self.remove_from_cleanup().await {
                                    self.status_message = format!("Error: {}", e);
                                }
                                self.g_pressed = false;
                            }
                            KeyCode::Char('1') => {
                                self.view = View::ScanList;
                                self.g_pressed = false;
                            }
                            KeyCode::Char('2') => {
                                self.view = View::FileTree;
                                self.g_pressed = false;
                            }
                            KeyCode::Char('3') => {
                                self.view = View::CleanupList;
                                self.g_pressed = false;
                            }
                            _ => {
                                self.g_pressed = false;
                            }
                        },
                        View::ScanDialog => match key.code {
                            KeyCode::Esc => {
                                self.view = self.previous_view;
                                self.scan_input.clear();
                            }
                            KeyCode::Enter => {
                                if !self.scan_input.is_empty() {
                                    let path = self.scan_input.clone();
                                    if let Err(e) = self.start_scan(path).await {
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
                                if let Some(active_scan) = &self.active_scan {
                                    active_scan.cancelled.store(true, Ordering::Relaxed);
                                    self.status_message = "Cancelling scan...".to_string();
                                }
                            }
                        }
                        View::Help => {
                            // Any key closes help
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('?') => {
                                    self.view = self.previous_view;
                                }
                                _ => {
                                    self.view = self.previous_view;
                                }
                            }
                        }
                    }
                }
            }

            // Handle active scan updates
            if let Some(active_scan) = &mut self.active_scan {
                // Check for progress updates
                match active_scan.progress_rx.try_recv() {
                    Ok(progress) => {
                        self.scan_progress = Some(ScanProgress {
                            entries_scanned: progress.files_scanned,
                            active_dirs: progress.active_dirs.clone(),
                            active_workers: progress.active_workers,
                        });
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {
                        // No update available
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        // Scanner finished, will be handled below
                    }
                }

                // Check if scan is complete
                if active_scan.scan_handle.is_finished() {
                    // Take ownership of active_scan to finalize it
                    if let Some(active_scan) = self.active_scan.take() {
                        // Get scan result
                        match active_scan.scan_handle.await {
                            Ok(Ok((_, stats))) => {
                                // Signal actor to shutdown
                                let _ = active_scan.tx.send(ActorMessage::Shutdown).await;
                                drop(active_scan.tx);

                                // Wait for database writes to complete
                                if let Ok(Ok(())) = active_scan.actor_handle.await {
                                    // Check if scan was cancelled
                                    let was_cancelled =
                                        active_scan.cancelled.load(Ordering::Relaxed);

                                    // Save scan with appropriate status
                                    if was_cancelled {
                                        let _ =
                                            self.db.pause_scan(active_scan.scan_id, &stats).await;
                                        self.status_message = format!(
                                            "Scan paused. Progress saved: {} files, {} dirs. Press 'r' to resume.",
                                            stats.total_files, stats.total_dirs
                                        );
                                    } else {
                                        let _ = self
                                            .db
                                            .complete_scan(active_scan.scan_id, &stats)
                                            .await;
                                        self.status_message = format!(
                                            "Scan complete! {} files, {} dirs, {} bytes",
                                            stats.total_files, stats.total_dirs, stats.total_size
                                        );
                                    }

                                    // Reload scans and return to scan list
                                    let _ = self.load_scans().await;
                                    self.view = View::ScanList;
                                    self.scan_progress = None;
                                }
                            }
                            Ok(Err(e)) => {
                                self.status_message = format!("Scan error: {}", e);
                                self.view = View::ScanList;
                                self.scan_progress = None;
                            }
                            Err(e) => {
                                self.status_message = format!("Scan join error: {}", e);
                                self.view = View::ScanList;
                                self.scan_progress = None;
                            }
                        }
                    }
                }
            }
        }
    }

    fn render(&mut self, f: &mut Frame) {
        // For FileTree view, use 3-part layout with info pane
        let (main_chunks, use_info_pane) = if self.view == View::FileTree {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),    // Main content
                    Constraint::Length(6), // Info pane
                    Constraint::Length(3), // Status bar
                ])
                .split(f.area());
            (chunks, true)
        } else {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(3)])
                .split(f.area());
            (chunks, false)
        };

        match self.view {
            View::ScanList => self.render_scan_list(f, main_chunks[0]),
            View::FileTree => {
                self.render_file_tree(f, main_chunks[0]);
                if use_info_pane {
                    self.render_file_info(f, main_chunks[1]);
                }
            }
            View::CleanupList => self.render_cleanup_list(f, main_chunks[0]),
            View::ScanDialog => self.render_scan_dialog(f, main_chunks[0]),
            View::Scanning => self.render_scanning(f, main_chunks[0]),
            View::Help => self.render_help(f, main_chunks[0]),
        }

        let status_idx = if use_info_pane { 2 } else { 1 };
        self.render_status_bar(f, main_chunks[status_idx]);
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
                    "paused" => "⏸",
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
            .block(Block::default().borders(Borders::ALL).title(
                "Scans (1) | Enter: view | r: resume paused | n: new | ↑/↓ or j/k: navigate",
            ))
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
                "Files (2) | Scan: {} | z: fold/unfold | Z: unfold all | Space: mark",
                scan.root_path
            )
        } else {
            "Files (2)".to_string()
        };

        let visible_entries = self.get_visible_entries();
        let items: Vec<ListItem> = visible_entries
            .iter()
            .map(|entry| {
                let size_str = format_size(entry.size as u64);
                let is_folded = entry.is_dir && self.folded_dirs.contains(&entry.path);
                let icon = if entry.is_dir {
                    if is_folded {
                        "▶ 📁"
                    } else {
                        "▼ 📁"
                    }
                } else {
                    "  📄"
                };
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

    fn render_file_info(&self, f: &mut Frame, area: Rect) {
        let info_text = if let Some(selected) = self.file_list_state.selected() {
            let visible_entries = self.get_visible_entries();
            if let Some(entry) = visible_entries.get(selected) {
                let file_type = if entry.is_dir { "Directory" } else { "File" };
                let size_str = format_size(entry.size as u64);

                vec![
                    Line::from(vec![
                        Span::styled("Type: ", Style::default().fg(Color::Yellow)),
                        Span::raw(file_type),
                    ]),
                    Line::from(vec![
                        Span::styled("Name: ", Style::default().fg(Color::Yellow)),
                        Span::raw(&entry.name),
                    ]),
                    Line::from(vec![
                        Span::styled("Path: ", Style::default().fg(Color::Yellow)),
                        Span::raw(&entry.path),
                    ]),
                    Line::from(vec![
                        Span::styled("Size: ", Style::default().fg(Color::Yellow)),
                        Span::raw(size_str),
                    ]),
                ]
            } else {
                vec![Line::from("No file selected")]
            }
        } else {
            vec![Line::from("No file selected")]
        };

        let paragraph = Paragraph::new(info_text)
            .block(Block::default().borders(Borders::ALL).title("File Info"))
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
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

    fn render_help(&self, f: &mut Frame, area: Rect) {
        let help_text = vec![
            Line::from(vec![Span::styled(
                "Rootkitty - Keyboard Shortcuts",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
            Line::from(vec![Span::styled(
                "Navigation:",
                Style::default().fg(Color::Yellow),
            )]),
            Line::from("  j/↓         Move down one item"),
            Line::from("  k/↑         Move up one item"),
            Line::from("  J           Jump to next sibling (same depth)"),
            Line::from("  K           Jump to previous sibling (same depth)"),
            Line::from("  d           Page down (10 items)"),
            Line::from("  u           Page up (10 items)"),
            Line::from("  gg          Jump to top"),
            Line::from("  G           Jump to bottom"),
            Line::from(""),
            Line::from(vec![Span::styled(
                "Views:",
                Style::default().fg(Color::Yellow),
            )]),
            Line::from("  1           Scan list view"),
            Line::from("  2           File tree view"),
            Line::from("  3           Cleanup list view"),
            Line::from(""),
            Line::from(vec![Span::styled(
                "Actions:",
                Style::default().fg(Color::Yellow),
            )]),
            Line::from("  n           New scan"),
            Line::from("  r           Resume paused scan"),
            Line::from("  Space       Mark/unmark file for cleanup (File view)"),
            Line::from("  Space       Remove from cleanup list (Cleanup view)"),
            Line::from("  z/o         Fold/unfold directory (File view)"),
            Line::from("  Z/O         Unfold directory and all subdirs (File view)"),
            Line::from("  s/g         Generate cleanup script (Cleanup view)"),
            Line::from("  Enter/o     Select/open"),
            Line::from(""),
            Line::from(vec![Span::styled(
                "General:",
                Style::default().fg(Color::Yellow),
            )]),
            Line::from("  ?           Show this help"),
            Line::from("  q           Quit / Cancel"),
            Line::from("  Esc         Cancel / Go back"),
            Line::from(""),
            Line::from(vec![Span::styled(
                "Press any key to close",
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::ITALIC),
            )]),
        ];

        let paragraph = Paragraph::new(help_text)
            .block(Block::default().borders(Borders::ALL).title("Help (?)"))
            .wrap(Wrap { trim: true });

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
        lines.push(Line::from("Press q or Esc to cancel scan"));

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
                "q/Esc: cancel scan"
            }
            View::Help => {
                "Press any key to close help"
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

                // Initialize folded state: fold all directories except the root
                self.folded_dirs.clear();
                self.initialize_folded_state();

                self.status_message = format!("Loaded {} entries", self.file_entries.len());
            }
        }
        Ok(())
    }

    fn initialize_folded_state(&mut self) {
        // Fold all directories except the root (depth 0)
        for entry in &self.file_entries {
            if entry.is_dir && entry.depth > 0 {
                self.folded_dirs.insert(entry.path.clone());
            }
        }
    }

    fn toggle_fold_directory(&mut self, recursive: bool) {
        if let Some(selected) = self.file_list_state.selected() {
            let visible_entries = self.get_visible_entries();
            if let Some(entry) = visible_entries.get(selected) {
                if !entry.is_dir {
                    self.status_message = "Not a directory".to_string();
                    return;
                }

                let dir_path = entry.path.clone();
                let dir_name = entry.name.clone();
                let dir_depth = entry.depth;
                let is_folded = self.folded_dirs.contains(&dir_path);

                if is_folded {
                    // Unfold
                    if recursive {
                        // Shift+Z: Unfold all nested folders recursively
                        self.unfold_recursive(&dir_path);
                        self.status_message =
                            format!("Unfolded '{}' and all subdirectories", dir_name);
                    } else {
                        // z: Unfold one level only
                        self.unfold_one_level(&dir_path, dir_depth);
                        self.status_message = format!("Unfolded '{}'", dir_name);
                    }
                } else {
                    // Fold - collapse this directory and all its descendants
                    self.fold_directory(&dir_path);
                    self.status_message = format!("Folded '{}'", dir_name);
                }
            }
        }
    }

    fn unfold_one_level(&mut self, parent_path: &str, parent_depth: i64) {
        // Remove the parent from folded set
        self.folded_dirs.remove(parent_path);

        // Ensure all immediate subdirectories are folded
        let target_depth = parent_depth + 1;
        for entry in &self.file_entries {
            if entry.is_dir && entry.depth == target_depth {
                // Check if this is a direct child of parent_path
                if entry.path.starts_with(parent_path) && entry.path != parent_path {
                    // Count slashes to verify it's a direct child
                    let relative_path = &entry.path[parent_path.len()..];
                    let slash_count = relative_path.chars().filter(|c| *c == '/').count();
                    // Direct child should have exactly 1 slash (the leading one)
                    if slash_count == 1 {
                        self.folded_dirs.insert(entry.path.clone());
                    }
                }
            }
        }
    }

    fn fold_directory(&mut self, dir_path: &str) {
        // Add this directory to folded set
        self.folded_dirs.insert(dir_path.to_string());

        // We don't need to fold children since they'll be hidden anyway
    }

    fn unfold_recursive(&mut self, parent_path: &str) {
        // Remove the parent and all children from folded set
        self.folded_dirs.remove(parent_path);

        // Remove all paths that start with parent_path/
        let prefix = format!("{}/", parent_path);
        self.folded_dirs.retain(|path| !path.starts_with(&prefix));
    }

    fn get_visible_entries(&self) -> Vec<&StoredFileEntry> {
        compute_visible_entries(&self.file_entries, &self.folded_dirs)
    }

    async fn toggle_cleanup_mark(&mut self) -> Result<()> {
        if let Some(scan) = &self.current_scan {
            if let Some(selected) = self.file_list_state.selected() {
                let visible_entries = self.get_visible_entries();
                if let Some(entry) = visible_entries.get(selected) {
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

    async fn resume_scan(&mut self, scan_id: i64, path: String) -> Result<()> {
        let path_buf = PathBuf::from(shellexpand::tilde(&path).to_string());

        // Create cancellation flag
        let cancelled = Arc::new(AtomicBool::new(false));

        // Switch to scanning view
        self.view = View::Scanning;
        self.scan_progress = Some(ScanProgress {
            entries_scanned: 0,
            active_dirs: Vec::new(),
            active_workers: 0,
        });

        // Create channels
        let (tx, rx) = mpsc::channel(100);
        let (progress_tx, progress_rx) = mpsc::unbounded_channel::<ProgressUpdate>();

        // Spawn database actor
        let actor = DatabaseActor::new(self.db.clone(), scan_id, rx);
        let actor_handle = tokio::spawn(async move { actor.run().await });

        // Clone for the scanning thread
        let tx_clone = tx.clone();
        let path_clone = path_buf.clone();
        let cancelled_clone = cancelled.clone();
        let db_clone = self.db.clone();

        // Spawn scanner in blocking thread (with resume support)
        let scan_handle = tokio::task::spawn_blocking(move || {
            let scanner =
                Scanner::with_sender(&path_clone, tx_clone, Some(progress_tx), cancelled_clone);
            scanner.scan_resuming(scan_id, db_clone)
        });

        // Store active scan state
        self.active_scan = Some(ActiveScan {
            scan_id,
            scan_handle,
            actor_handle,
            tx,
            progress_rx,
            cancelled,
        });

        Ok(())
    }

    async fn start_scan(&mut self, path: String) -> Result<()> {
        let path_buf = PathBuf::from(shellexpand::tilde(&path).to_string());

        // Create cancellation flag
        let cancelled = Arc::new(AtomicBool::new(false));

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
        let (progress_tx, progress_rx) = mpsc::unbounded_channel::<ProgressUpdate>();

        // Spawn database actor
        let actor = DatabaseActor::new(self.db.clone(), scan_id, rx);
        let actor_handle = tokio::spawn(async move { actor.run().await });

        // Clone for the scanning thread
        let tx_clone = tx.clone();
        let path_clone = path_buf.clone();
        let cancelled_clone = cancelled.clone();

        // Spawn scanner in blocking thread
        let scan_handle = tokio::task::spawn_blocking(move || {
            let scanner =
                Scanner::with_sender(&path_clone, tx_clone, Some(progress_tx), cancelled_clone);
            scanner.scan()
        });

        // Store active scan state
        self.active_scan = Some(ActiveScan {
            scan_id,
            scan_handle,
            actor_handle,
            tx,
            progress_rx,
            cancelled,
        });

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

    fn scan_list_top(&mut self) {
        if !self.scans.is_empty() {
            self.scan_list_state.select(Some(0));
        }
    }

    fn scan_list_bottom(&mut self) {
        if !self.scans.is_empty() {
            self.scan_list_state.select(Some(self.scans.len() - 1));
        }
    }

    fn scan_list_page_down(&mut self) {
        if self.scans.is_empty() {
            return;
        }
        let page_size = 10; // Move 10 items at a time
        let i = match self.scan_list_state.selected() {
            Some(i) => {
                let new_pos = i + page_size;
                if new_pos >= self.scans.len() {
                    self.scans.len() - 1
                } else {
                    new_pos
                }
            }
            None => 0,
        };
        self.scan_list_state.select(Some(i));
    }

    fn scan_list_page_up(&mut self) {
        if self.scans.is_empty() {
            return;
        }
        let page_size = 10; // Move 10 items at a time
        let i = match self.scan_list_state.selected() {
            Some(i) => {
                if i < page_size {
                    0
                } else {
                    i - page_size
                }
            }
            None => 0,
        };
        self.scan_list_state.select(Some(i));
    }

    fn file_list_next(&mut self) {
        let visible_count = self.get_visible_entries().len();
        if visible_count == 0 {
            return;
        }
        let i = match self.file_list_state.selected() {
            Some(i) => {
                if i >= visible_count - 1 {
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
        let visible_count = self.get_visible_entries().len();
        if visible_count == 0 {
            return;
        }
        let i = match self.file_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    visible_count - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.file_list_state.select(Some(i));
    }

    fn file_list_top(&mut self) {
        if !self.get_visible_entries().is_empty() {
            self.file_list_state.select(Some(0));
        }
    }

    fn file_list_bottom(&mut self) {
        let visible_count = self.get_visible_entries().len();
        if visible_count > 0 {
            self.file_list_state.select(Some(visible_count - 1));
        }
    }

    fn file_list_page_down(&mut self) {
        let visible_count = self.get_visible_entries().len();
        if visible_count == 0 {
            return;
        }
        let page_size = 10;
        let i = match self.file_list_state.selected() {
            Some(i) => {
                let new_pos = i + page_size;
                if new_pos >= visible_count {
                    visible_count - 1
                } else {
                    new_pos
                }
            }
            None => 0,
        };
        self.file_list_state.select(Some(i));
    }

    fn file_list_page_up(&mut self) {
        let visible_count = self.get_visible_entries().len();
        if visible_count == 0 {
            return;
        }
        let page_size = 10;
        let i = match self.file_list_state.selected() {
            Some(i) => {
                if i < page_size {
                    0
                } else {
                    i - page_size
                }
            }
            None => 0,
        };
        self.file_list_state.select(Some(i));
    }

    fn file_list_next_sibling(&mut self) {
        let visible_entries = self.get_visible_entries();
        if visible_entries.is_empty() {
            return;
        }

        let current_idx = match self.file_list_state.selected() {
            Some(i) => i,
            None => {
                self.file_list_state.select(Some(0));
                return;
            }
        };

        if let Some(current_entry) = visible_entries.get(current_idx) {
            let current_depth = current_entry.depth;

            // Find the next entry at the same depth
            for (idx, entry) in visible_entries.iter().enumerate().skip(current_idx + 1) {
                if entry.depth == current_depth {
                    let entry_name = entry.name.clone();
                    self.file_list_state.select(Some(idx));
                    self.status_message = format!("Next sibling: {}", entry_name);
                    return;
                } else if entry.depth < current_depth {
                    // We've gone back up to a parent level, no more siblings
                    self.status_message = "No next sibling at this level".to_string();
                    return;
                }
            }

            self.status_message = "No next sibling at this level".to_string();
        }
    }

    fn file_list_previous_sibling(&mut self) {
        let visible_entries = self.get_visible_entries();
        if visible_entries.is_empty() {
            return;
        }

        let current_idx = match self.file_list_state.selected() {
            Some(i) => i,
            None => {
                self.file_list_state.select(Some(0));
                return;
            }
        };

        if let Some(current_entry) = visible_entries.get(current_idx) {
            let current_depth = current_entry.depth;

            // Find the previous entry at the same depth (search backwards)
            for idx in (0..current_idx).rev() {
                if let Some(entry) = visible_entries.get(idx) {
                    if entry.depth == current_depth {
                        let entry_name = entry.name.clone();
                        self.file_list_state.select(Some(idx));
                        self.status_message = format!("Previous sibling: {}", entry_name);
                        return;
                    } else if entry.depth < current_depth {
                        // We've gone back up to a parent level, no more siblings
                        self.status_message = "No previous sibling at this level".to_string();
                        return;
                    }
                }
            }

            self.status_message = "No previous sibling at this level".to_string();
        }
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

    fn cleanup_list_top(&mut self) {
        if !self.cleanup_items.is_empty() {
            self.cleanup_list_state.select(Some(0));
        }
    }

    fn cleanup_list_bottom(&mut self) {
        if !self.cleanup_items.is_empty() {
            self.cleanup_list_state
                .select(Some(self.cleanup_items.len() - 1));
        }
    }

    fn cleanup_list_page_down(&mut self) {
        if self.cleanup_items.is_empty() {
            return;
        }
        let page_size = 10;
        let i = match self.cleanup_list_state.selected() {
            Some(i) => {
                let new_pos = i + page_size;
                if new_pos >= self.cleanup_items.len() {
                    self.cleanup_items.len() - 1
                } else {
                    new_pos
                }
            }
            None => 0,
        };
        self.cleanup_list_state.select(Some(i));
    }

    fn cleanup_list_page_up(&mut self) {
        if self.cleanup_items.is_empty() {
            return;
        }
        let page_size = 10;
        let i = match self.cleanup_list_state.selected() {
            Some(i) => {
                if i < page_size {
                    0
                } else {
                    i - page_size
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

/// Pure function to compute visible entries based on folded state
/// This is separated from App methods to enable unit testing
fn compute_visible_entries<'a>(
    all_entries: &'a [StoredFileEntry],
    folded_dirs: &std::collections::HashSet<String>,
) -> Vec<&'a StoredFileEntry> {
    let mut visible = Vec::new();

    for entry in all_entries {
        // Check if any parent directory is folded
        let mut is_hidden = false;

        // Check each potential parent path
        let path_parts: Vec<&str> = entry.path.split('/').collect();
        let mut current_path = String::new();

        for (i, part) in path_parts.iter().enumerate() {
            if i == path_parts.len() - 1 {
                // This is the entry itself, not a parent
                break;
            }

            if i == 0 {
                current_path = part.to_string();
            } else {
                current_path = format!("{}/{}", current_path, part);
            }

            if folded_dirs.contains(&current_path) {
                is_hidden = true;
                break;
            }
        }

        if !is_hidden {
            visible.push(entry);
        }
    }

    // Sort by path to ensure hierarchical order (parents before children)
    // This is critical because entries from the database are sorted by size, not path
    visible.sort_by(|a, b| a.path.cmp(&b.path));

    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashSet;

    fn create_test_entry(
        id: i64,
        path: &str,
        name: &str,
        depth: i64,
        is_dir: bool,
    ) -> StoredFileEntry {
        StoredFileEntry {
            id,
            scan_id: 1,
            path: path.to_string(),
            name: name.to_string(),
            parent_path: None,
            size: 1000,
            is_dir,
            modified_at: Some(Utc::now()),
            depth,
        }
    }

    #[test]
    fn test_visible_entries_ordering() {
        // Create a directory structure that might expose the ordering bug
        // /root
        //   /root/parent
        //     /root/parent/child1
        //       /root/parent/child1/file.txt
        //     /root/parent/child2

        let entries = vec![
            create_test_entry(1, "/root", "root", 0, true),
            create_test_entry(2, "/root/parent", "parent", 1, true),
            create_test_entry(3, "/root/parent/child1", "child1", 2, true),
            create_test_entry(4, "/root/parent/child1/file.txt", "file.txt", 3, false),
            create_test_entry(5, "/root/parent/child2", "child2", 2, true),
        ];

        // Start with all folders except root folded
        let mut folded_dirs = HashSet::new();
        folded_dirs.insert("/root/parent".to_string());
        folded_dirs.insert("/root/parent/child1".to_string());
        folded_dirs.insert("/root/parent/child2".to_string());

        // Initially, root and parent should be visible (parent is folded but shows up with ▶)
        // but child1, child2, and file.txt should be hidden
        let visible = compute_visible_entries(&entries, &folded_dirs);
        assert_eq!(
            visible.len(),
            2,
            "Expected 2 visible entries (root and parent), got {} entries: {:?}",
            visible.len(),
            visible.iter().map(|e| &e.path).collect::<Vec<_>>()
        );
        assert_eq!(visible[0].path, "/root");
        assert_eq!(visible[1].path, "/root/parent");

        // Unfold parent - should show root, parent, child1 (folded), child2 (folded)
        // but NOT file.txt (child1 is still folded)
        folded_dirs.remove("/root/parent");
        let visible = compute_visible_entries(&entries, &folded_dirs);
        assert_eq!(
            visible.len(),
            4,
            "Expected 4 visible entries (root, parent, child1, child2), got {} entries: {:?}",
            visible.len(),
            visible.iter().map(|e| &e.path).collect::<Vec<_>>()
        ); // root, parent, child1, child2

        // Check ordering: parent should come before its children
        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        // Find indices
        let parent_idx = paths.iter().position(|&p| p == "/root/parent");
        let child1_idx = paths.iter().position(|&p| p == "/root/parent/child1");
        let child2_idx = paths.iter().position(|&p| p == "/root/parent/child2");

        assert!(parent_idx.is_some(), "Parent should be visible");
        assert!(child1_idx.is_some(), "Child1 should be visible");
        assert!(child2_idx.is_some(), "Child2 should be visible");

        // THE BUG: Parent should appear BEFORE its children in the visible list
        assert!(
            parent_idx.unwrap() < child1_idx.unwrap(),
            "Parent (idx {}) should appear before child1 (idx {}), but order is: {:?}",
            parent_idx.unwrap(),
            child1_idx.unwrap(),
            paths
        );
        assert!(
            parent_idx.unwrap() < child2_idx.unwrap(),
            "Parent (idx {}) should appear before child2 (idx {}), but order is: {:?}",
            parent_idx.unwrap(),
            child2_idx.unwrap(),
            paths
        );
    }

    #[test]
    fn test_visible_entries_ordering_with_size_sorted_input() {
        // This test reproduces the bug where entries come from database sorted by size
        // In this case, a large child file appears before its smaller parent directory

        let entries = vec![
            create_test_entry(1, "/root", "root", 0, true),
            // Child1 has larger size (8000) so comes first when sorted by size DESC
            create_test_entry(3, "/root/parent/child1", "child1", 2, true),
            // Parent has smaller size (2000) so comes second
            create_test_entry(2, "/root/parent", "parent", 1, true),
            // Child2 has smallest size
            create_test_entry(4, "/root/parent/child2", "child2", 2, true),
        ];

        // Fold everything except root
        let mut folded_dirs = HashSet::new();
        folded_dirs.insert("/root/parent".to_string());
        folded_dirs.insert("/root/parent/child1".to_string());
        folded_dirs.insert("/root/parent/child2".to_string());

        // Unfold parent
        folded_dirs.remove("/root/parent");
        let visible = compute_visible_entries(&entries, &folded_dirs);

        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        // Should be in hierarchical order: root, parent, child1, child2
        // NOT in size order: root, child1, parent, child2

        let parent_idx = paths.iter().position(|&p| p == "/root/parent").unwrap();
        let child1_idx = paths
            .iter()
            .position(|&p| p == "/root/parent/child1")
            .unwrap();
        let child2_idx = paths
            .iter()
            .position(|&p| p == "/root/parent/child2")
            .unwrap();

        assert!(
            parent_idx < child1_idx,
            "Parent (idx {}) should appear before child1 (idx {}) in hierarchical view, but order is: {:?}",
            parent_idx,
            child1_idx,
            paths
        );
        assert!(
            parent_idx < child2_idx,
            "Parent (idx {}) should appear before child2 (idx {}), but order is: {:?}",
            parent_idx,
            child2_idx,
            paths
        );
    }

    #[test]
    fn test_sibling_navigation() {
        // Test tree structure:
        // /root (depth 0)
        //   /root/a (depth 1)
        //     /root/a/file1.txt (depth 2)
        //   /root/b (depth 1) <- sibling of 'a'
        //   /root/c (depth 1) <- sibling of 'a' and 'b'
        //     /root/c/d (depth 2)
        //       /root/c/d/file2.txt (depth 3)

        let entries = vec![
            create_test_entry(1, "/root", "root", 0, true),
            create_test_entry(2, "/root/a", "a", 1, true),
            create_test_entry(3, "/root/a/file1.txt", "file1.txt", 2, false),
            create_test_entry(4, "/root/b", "b", 1, true),
            create_test_entry(5, "/root/c", "c", 1, true),
            create_test_entry(6, "/root/c/d", "d", 2, true),
            create_test_entry(7, "/root/c/d/file2.txt", "file2.txt", 3, false),
        ];

        let folded_dirs = HashSet::new(); // All unfolded for this test
        let visible = compute_visible_entries(&entries, &folded_dirs);

        // All entries should be visible (sorted by path)
        assert_eq!(visible.len(), 7);

        // Test depth-based sibling relationships
        // At depth 1, we have: a, b, c (indices 1, 3, 4 in visible)
        let a_idx = visible.iter().position(|e| e.name == "a").unwrap();
        let b_idx = visible.iter().position(|e| e.name == "b").unwrap();
        let c_idx = visible.iter().position(|e| e.name == "c").unwrap();

        // Verify a, b, c are at depth 1
        assert_eq!(visible[a_idx].depth, 1);
        assert_eq!(visible[b_idx].depth, 1);
        assert_eq!(visible[c_idx].depth, 1);

        // Verify ordering: a should come before b, b before c
        assert!(a_idx < b_idx, "a should come before b");
        assert!(b_idx < c_idx, "b should come before c");

        // Verify file1.txt is between a and b (depth 2, child of a)
        let file1_idx = visible.iter().position(|e| e.name == "file1.txt").unwrap();
        assert!(a_idx < file1_idx && file1_idx < b_idx);
    }

    #[test]
    fn test_visible_entries_with_complex_tree() {
        // More complex tree to test ordering
        let entries = vec![
            create_test_entry(1, "/root", "root", 0, true),
            create_test_entry(2, "/root/a", "a", 1, true),
            create_test_entry(3, "/root/a/b", "b", 2, true),
            create_test_entry(4, "/root/a/b/c", "c", 3, true),
            create_test_entry(5, "/root/a/file1.txt", "file1.txt", 2, false),
            create_test_entry(6, "/root/z", "z", 1, true),
            create_test_entry(7, "/root/z/file2.txt", "file2.txt", 2, false),
        ];

        // Fold everything except root
        let mut folded_dirs = HashSet::new();
        folded_dirs.insert("/root/a".to_string());
        folded_dirs.insert("/root/a/b".to_string());
        folded_dirs.insert("/root/a/b/c".to_string());
        folded_dirs.insert("/root/z".to_string());

        // Unfold /root/a - should show a, b (folded), and file1.txt
        folded_dirs.remove("/root/a");
        let visible = compute_visible_entries(&entries, &folded_dirs);

        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        // Should contain: root, a, b (folded), file1.txt, z (folded)
        assert!(paths.contains(&"/root"));
        assert!(paths.contains(&"/root/a"));
        assert!(paths.contains(&"/root/a/b"));
        assert!(paths.contains(&"/root/a/file1.txt"));
        assert!(paths.contains(&"/root/z"));

        // Should NOT contain anything under folded directories
        assert!(!paths.contains(&"/root/a/b/c"));
        assert!(!paths.contains(&"/root/z/file2.txt"));

        // Check ordering: each directory should appear before its children
        let a_idx = paths.iter().position(|&p| p == "/root/a").unwrap();
        let b_idx = paths.iter().position(|&p| p == "/root/a/b").unwrap();
        let file1_idx = paths
            .iter()
            .position(|&p| p == "/root/a/file1.txt")
            .unwrap();

        assert!(
            a_idx < b_idx,
            "Directory /root/a should appear before /root/a/b"
        );
        assert!(
            a_idx < file1_idx,
            "Directory /root/a should appear before /root/a/file1.txt"
        );
    }
}
