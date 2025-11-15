mod tree;
mod types;

pub use types::{SortMode, View};

use anyhow::Result;
use crossterm::{
    cursor,
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
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
use crate::settings::Settings;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use tree::compute_visible_entries;
use types::{ActiveScan, ResumePreparation, ScanProgress};

/// Result types for async loading operations
enum LoadingResult {
    /// Loaded entries for a scan (scan_id, entries)
    ScanEntries(i64, Vec<StoredFileEntry>),
    /// Loaded children for a directory (parent_path, children)
    DirectoryChildren(String, Vec<StoredFileEntry>),
    /// Loaded all descendants recursively (parent_path, all_descendants)
    RecursiveChildren(String, Vec<StoredFileEntry>),
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
    /// Scan ID pending deletion (for confirmation dialog)
    delete_scan_id: Option<i64>,
    /// Active deletion task
    delete_task: Option<tokio::task::JoinHandle<Result<i64>>>,
    /// Throbber frame for deletion animation
    delete_throbber_frame: usize,
    /// Active resume preparation (loading scanned paths)
    resume_prep: Option<ResumePreparation>,
    /// Active loading task for non-blocking data operations
    loading_task: Option<tokio::task::JoinHandle<Result<LoadingResult>>>,
    /// Path of directory currently being loaded (for showing throbber)
    loading_path: Option<String>,
    /// Throbber frame for loading animation
    loading_throbber_frame: usize,
    /// Settings list state for navigation
    settings_list_state: ListState,
    /// File tree sort mode
    file_tree_sort: SortMode,
    /// Scan list sort mode
    scan_list_sort: SortMode,
    /// Path to settings file for saving
    settings_path: PathBuf,
    /// Path to database file
    db_path: PathBuf,
    /// Input buffer for editing paths
    path_input: String,
    /// Cursor position in path input
    path_input_cursor: usize,
    /// Which path we're editing: 0 = config, 1 = database, None = not editing
    editing_path_index: Option<usize>,
    /// Pending path change awaiting confirmation (index, new_path, old_path)
    pending_path_change: Option<(usize, PathBuf, PathBuf)>,
    /// Whether pending path exists
    pending_path_exists: bool,
    /// Whether to follow symlinks during scanning
    follow_symlinks: bool,
    /// Search query string for filtering file tree
    search_query: String,
}

impl App {
    pub fn new(db: Database, settings: Settings, settings_path: PathBuf, db_path: PathBuf) -> Self {
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
            delete_scan_id: None,
            delete_task: None,
            delete_throbber_frame: 0,
            resume_prep: None,
            loading_task: None,
            loading_path: None,
            loading_throbber_frame: 0,
            settings_list_state: ListState::default(),
            file_tree_sort: settings.ui.file_tree_sort,
            scan_list_sort: settings.ui.scan_list_sort,
            settings_path,
            db_path,
            path_input: String::new(),
            path_input_cursor: 0,
            editing_path_index: None,
            pending_path_change: None,
            pending_path_exists: false,
            follow_symlinks: settings.scan.follow_symlinks,
            search_query: String::new(),
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
                            KeyCode::Char('S') => {
                                self.previous_view = View::ScanList;
                                self.view = View::Settings;
                                if self.settings_list_state.selected().is_none() {
                                    self.settings_list_state.select(Some(0));
                                }
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
                            KeyCode::Char('x') => {
                                // Delete scan (with confirmation)
                                if let Some(selected) = self.scan_list_state.selected() {
                                    if let Some(scan) = self.scans.get(selected) {
                                        self.delete_scan_id = Some(scan.id);
                                        self.previous_view = View::ScanList;
                                        self.view = View::ConfirmDelete;
                                    }
                                }
                                self.g_pressed = false;
                            }
                            KeyCode::Enter | KeyCode::Char('o') => {
                                if let Err(e) = self.select_scan() {
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
                            KeyCode::Char('t') => {
                                self.scan_list_sort = self.scan_list_sort.toggle();
                                self.status_message = format!(
                                    "Scan list sort: {}",
                                    self.scan_list_sort.display_name()
                                );
                                self.g_pressed = false;
                            }
                            _ => {
                                self.g_pressed = false;
                            }
                        },
                        View::FileTree => match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Esc => {
                                // If search is active, clear it; otherwise go back to scan list
                                if !self.search_query.is_empty() {
                                    self.search_query.clear();
                                    self.status_message = "Search cleared".to_string();
                                } else {
                                    self.view = View::ScanList;
                                }
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
                                if let Err(e) = self.toggle_fold_directory(false) {
                                    self.status_message = format!("Error unfolding: {}", e);
                                }
                                self.g_pressed = false;
                            }
                            KeyCode::Char('Z') | KeyCode::Char('O') => {
                                // Unfold all nested folders recursively
                                if let Err(e) = self.toggle_fold_directory(true) {
                                    self.status_message = format!("Error unfolding: {}", e);
                                }
                                self.g_pressed = false;
                            }
                            KeyCode::Char('s') => {
                                // Open shell in selected directory or parent
                                if let Err(e) = self.open_shell(terminal) {
                                    self.status_message = format!("Error opening shell: {}", e);
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
                                if let Err(e) = self.load_cleanup_items().await {
                                    self.status_message = format!("Error: {}", e);
                                } else {
                                    self.view = View::CleanupList;
                                }
                                self.g_pressed = false;
                            }
                            KeyCode::Char('t') => {
                                self.file_tree_sort = self.file_tree_sort.toggle();
                                self.status_message = format!(
                                    "File tree sort: {}",
                                    self.file_tree_sort.display_name()
                                );
                                self.g_pressed = false;
                            }
                            KeyCode::Char('S') => {
                                // Shift+S - open settings
                                self.previous_view = View::FileTree;
                                self.view = View::Settings;
                                self.g_pressed = false;
                            }
                            KeyCode::Char('/') => {
                                // Open search dialog
                                self.previous_view = View::FileTree;
                                self.view = View::SearchDialog;
                                self.search_query.clear();
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
                            KeyCode::Char('S') => {
                                // Shift+S - open settings
                                self.previous_view = View::CleanupList;
                                self.view = View::Settings;
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
                        View::ConfirmDelete => match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                // Confirm delete - spawn background task
                                if let Some(scan_id) = self.delete_scan_id {
                                    let db = self.db.clone();
                                    let delete_task = tokio::spawn(async move {
                                        db.delete_scan(scan_id).await?;
                                        Ok(scan_id)
                                    });
                                    self.delete_task = Some(delete_task);
                                    self.view = View::Deleting;
                                    self.delete_throbber_frame = 0;
                                }
                            }
                            KeyCode::Char('n')
                            | KeyCode::Char('N')
                            | KeyCode::Esc
                            | KeyCode::Char('q') => {
                                // Cancel delete
                                self.delete_scan_id = None;
                                self.view = View::ScanList;
                            }
                            _ => {}
                        },
                        View::Deleting => {
                            // No input allowed during deletion
                        }
                        View::PreparingResume => {
                            // No input allowed during preparation
                        }
                        View::ConfirmPathChange => match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                // Apply the path change
                                if let Some((idx, new_path, _old_path)) =
                                    self.pending_path_change.take()
                                {
                                    if idx == 0 {
                                        // Config path - save current settings to new location
                                        let current_settings = Settings {
                                            ui: crate::settings::UiSettings {
                                                file_tree_sort: self.file_tree_sort,
                                                scan_list_sort: self.scan_list_sort,
                                                auto_fold_depth: 1,
                                            },
                                            scan: crate::settings::ScanSettings::default(),
                                        };

                                        match current_settings.save(&new_path) {
                                            Ok(_) => {
                                                self.settings_path = new_path.clone();

                                                // Reload settings from new path
                                                match Settings::load(&new_path) {
                                                    Ok(loaded_settings) => {
                                                        self.file_tree_sort =
                                                            loaded_settings.ui.file_tree_sort;
                                                        self.scan_list_sort =
                                                            loaded_settings.ui.scan_list_sort;
                                                        self.status_message = format!(
                                                            "Config path updated and loaded: {}",
                                                            new_path.display()
                                                        );
                                                    }
                                                    Err(e) => {
                                                        self.status_message = format!(
                                                            "Path updated but error loading: {}",
                                                            e
                                                        );
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                self.status_message =
                                                    format!("Error saving to new path: {}", e);
                                            }
                                        }
                                    } else if idx == 1 {
                                        // Database path - just update display
                                        self.db_path = new_path.clone();
                                        self.status_message = format!(
                                            "Database path updated: {} (restart required to take effect)",
                                            new_path.display()
                                        );
                                    }
                                }
                                // Return to Settings view
                                self.view = View::Settings;
                            }
                            KeyCode::Char('n')
                            | KeyCode::Char('N')
                            | KeyCode::Esc
                            | KeyCode::Char('q') => {
                                // Cancel - keep old path
                                self.pending_path_change = None;
                                self.status_message = "Path change cancelled".to_string();
                                // Return to Settings view (previous_view still points to the view before Settings)
                                self.view = View::Settings;
                            }
                            _ => {}
                        },
                        View::Settings => {
                            if let Some(editing_idx) = self.editing_path_index {
                                // Handle path editing mode
                                match key.code {
                                    KeyCode::Enter => {
                                        // Prepare the new path
                                        let new_path = PathBuf::from(
                                            shellexpand::tilde(&self.path_input).to_string(),
                                        );

                                        // Get the old path for comparison
                                        let old_path = if editing_idx == 0 {
                                            self.settings_path.clone()
                                        } else {
                                            self.db_path.clone()
                                        };

                                        // Check if path actually changed
                                        if new_path == old_path {
                                            self.editing_path_index = None;
                                            self.path_input.clear();
                                            self.path_input_cursor = 0;
                                            self.status_message = "Path unchanged".to_string();
                                        } else {
                                            // Check if new path exists
                                            let path_exists = new_path.exists();

                                            // Store pending change and show confirmation dialog
                                            self.pending_path_change =
                                                Some((editing_idx, new_path, old_path));
                                            self.pending_path_exists = path_exists;
                                            // Don't change previous_view - we want to return to Settings,
                                            // then Settings can return to wherever it came from
                                            self.view = View::ConfirmPathChange;
                                            self.editing_path_index = None;
                                            self.path_input.clear();
                                            self.path_input_cursor = 0;
                                        }
                                    }
                                    KeyCode::Esc => {
                                        // Cancel editing
                                        self.editing_path_index = None;
                                        self.path_input.clear();
                                        self.path_input_cursor = 0;
                                        self.status_message = "Path edit cancelled".to_string();
                                    }
                                    KeyCode::Left => {
                                        // Move cursor left
                                        if self.path_input_cursor > 0 {
                                            self.path_input_cursor -= 1;
                                        }
                                    }
                                    KeyCode::Right => {
                                        // Move cursor right
                                        if self.path_input_cursor < self.path_input.len() {
                                            self.path_input_cursor += 1;
                                        }
                                    }
                                    KeyCode::Home => {
                                        // Move to start
                                        self.path_input_cursor = 0;
                                    }
                                    KeyCode::End => {
                                        // Move to end
                                        self.path_input_cursor = self.path_input.len();
                                    }
                                    KeyCode::Backspace => {
                                        // Delete character before cursor
                                        if self.path_input_cursor > 0 {
                                            self.path_input.remove(self.path_input_cursor - 1);
                                            self.path_input_cursor -= 1;
                                        }
                                    }
                                    KeyCode::Delete => {
                                        // Delete character at cursor
                                        if self.path_input_cursor < self.path_input.len() {
                                            self.path_input.remove(self.path_input_cursor);
                                        }
                                    }
                                    KeyCode::Char(c) => {
                                        // Insert character at cursor
                                        self.path_input.insert(self.path_input_cursor, c);
                                        self.path_input_cursor += 1;
                                    }
                                    _ => {}
                                }
                            } else {
                                // Normal settings navigation mode
                                match key.code {
                                    KeyCode::Char('q') | KeyCode::Esc => {
                                        self.view = self.previous_view;
                                        self.g_pressed = false;
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        self.settings_list_next();
                                        self.g_pressed = false;
                                    }
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        self.settings_list_previous();
                                        self.g_pressed = false;
                                    }
                                    KeyCode::Char('e') => {
                                        // Start editing path (config or database)
                                        if let Some(selected) = self.settings_list_state.selected()
                                        {
                                            if selected == 0 {
                                                // Config path
                                                self.editing_path_index = Some(0);
                                                self.path_input =
                                                    self.settings_path.display().to_string();
                                                self.path_input_cursor = self.path_input.len();
                                                self.status_message =
                                                    "Editing config path...".to_string();
                                            } else if selected == 1 {
                                                // Database path
                                                self.editing_path_index = Some(1);
                                                self.path_input =
                                                    self.db_path.display().to_string();
                                                self.path_input_cursor = self.path_input.len();
                                                self.status_message =
                                                    "Editing database path (restart required)..."
                                                        .to_string();
                                            }
                                        }
                                        self.g_pressed = false;
                                    }
                                    KeyCode::Char('r') => {
                                        // Restore path to default
                                        if let Some(selected) = self.settings_list_state.selected()
                                        {
                                            if selected == 0 {
                                                // Config path
                                                let default_path = Settings::default_path();

                                                let current_settings = Settings {
                                                    ui: crate::settings::UiSettings {
                                                        file_tree_sort: self.file_tree_sort,
                                                        scan_list_sort: self.scan_list_sort,
                                                        auto_fold_depth: 1,
                                                    },
                                                    scan: crate::settings::ScanSettings::default(),
                                                };

                                                match current_settings.save(&default_path) {
                                                    Ok(_) => {
                                                        self.settings_path = default_path.clone();
                                                        self.status_message = format!(
                                                            "Config path restored to default: {}",
                                                            default_path.display()
                                                        );
                                                    }
                                                    Err(e) => {
                                                        self.status_message = format!(
                                                            "Error restoring to default: {}",
                                                            e
                                                        );
                                                    }
                                                }
                                            } else if selected == 1 {
                                                // Database path - restore to CLI default
                                                self.status_message = "Database path cannot be restored (set via --db flag)".to_string();
                                            }
                                        }
                                        self.g_pressed = false;
                                    }
                                    KeyCode::Char('t') => {
                                        // Toggle the selected setting
                                        if let Some(selected) = self.settings_list_state.selected()
                                        {
                                            match selected {
                                                3 => {
                                                    // File Tree Sort (index 3 - after config path, db path, spacer)
                                                    self.file_tree_sort =
                                                        self.file_tree_sort.toggle();
                                                    self.status_message = format!(
                                                        "File tree sort: {}",
                                                        self.file_tree_sort.display_name()
                                                    );
                                                    // Save settings to disk
                                                    if let Err(e) = self.save_settings() {
                                                        self.status_message =
                                                            format!("Error saving settings: {}", e);
                                                    }
                                                }
                                                4 => {
                                                    // Scan List Sort (index 4)
                                                    self.scan_list_sort =
                                                        self.scan_list_sort.toggle();
                                                    self.status_message = format!(
                                                        "Scan list sort: {}",
                                                        self.scan_list_sort.display_name()
                                                    );
                                                    // Save settings to disk
                                                    if let Err(e) = self.save_settings() {
                                                        self.status_message =
                                                            format!("Error saving settings: {}", e);
                                                    }
                                                }
                                                6 => {
                                                    // Follow Symlinks (index 6)
                                                    self.follow_symlinks = !self.follow_symlinks;
                                                    self.status_message = format!(
                                                        "Follow symlinks: {}",
                                                        if self.follow_symlinks {
                                                            "Yes"
                                                        } else {
                                                            "No"
                                                        }
                                                    );
                                                    // Save settings to disk
                                                    if let Err(e) = self.save_settings() {
                                                        self.status_message =
                                                            format!("Error saving settings: {}", e);
                                                    }
                                                }
                                                _ => {
                                                    // Other settings are not toggleable
                                                }
                                            }
                                        }
                                        self.g_pressed = false;
                                    }
                                    _ => {
                                        self.g_pressed = false;
                                    }
                                }
                            }
                        }
                        View::SearchDialog => match key.code {
                            KeyCode::Esc => {
                                // Cancel search and return to file tree
                                self.view = self.previous_view;
                                self.search_query.clear();
                            }
                            KeyCode::Enter => {
                                // Apply search and return to file tree
                                self.view = self.previous_view;
                                // Keep the search_query so filtering remains active
                                if !self.search_query.is_empty() {
                                    self.status_message =
                                        format!("Searching for: {}", self.search_query);
                                    // Reset file list to top when search is applied
                                    self.file_list_state.select(Some(0));
                                } else {
                                    self.status_message = "Search cleared".to_string();
                                }
                            }
                            KeyCode::Backspace => {
                                self.search_query.pop();
                            }
                            KeyCode::Char(c) => {
                                self.search_query.push(c);
                            }
                            _ => {}
                        },
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

            // Handle active deletion task
            if let Some(delete_task) = &self.delete_task {
                if delete_task.is_finished() {
                    // Take ownership of the task to finalize it
                    if let Some(delete_task) = self.delete_task.take() {
                        match delete_task.await {
                            Ok(Ok(scan_id)) => {
                                self.status_message = format!("Deleted scan {}", scan_id);
                                // Reload scan list
                                let _ = self.load_scans().await;
                            }
                            Ok(Err(e)) => {
                                self.status_message = format!("Delete error: {}", e);
                            }
                            Err(e) => {
                                self.status_message = format!("Delete task error: {}", e);
                            }
                        }
                        self.delete_scan_id = None;
                        self.view = View::ScanList;
                    }
                } else {
                    // Update throbber animation
                    self.delete_throbber_frame = (self.delete_throbber_frame + 1) % 8;
                }
            }

            // Handle active loading task
            if let Some(loading_task) = &self.loading_task {
                if loading_task.is_finished() {
                    // Take ownership of the task to finalize it
                    if let Some(loading_task) = self.loading_task.take() {
                        match loading_task.await {
                            Ok(Ok(result)) => {
                                match result {
                                    LoadingResult::ScanEntries(scan_id, entries) => {
                                        // Set scan as current and populate entries
                                        if let Some(scan) =
                                            self.scans.iter().find(|s| s.id == scan_id).cloned()
                                        {
                                            self.current_scan = Some(scan);
                                            self.file_entries = entries;
                                            self.file_list_state.select(Some(0));
                                            self.folded_dirs.clear();
                                            self.initialize_folded_state();
                                            self.status_message = format!(
                                                "Loaded {} entries",
                                                self.file_entries.len()
                                            );
                                            self.view = View::FileTree;
                                        }
                                    }
                                    LoadingResult::DirectoryChildren(parent_path, children) => {
                                        // Save the count before moving
                                        let child_count = children.len();

                                        // Add children to file_entries if not already there
                                        for child in children {
                                            if !self
                                                .file_entries
                                                .iter()
                                                .any(|e| e.path == child.path)
                                            {
                                                // Fold directories by default
                                                if child.is_dir {
                                                    self.folded_dirs.insert(child.path.clone());
                                                }
                                                self.file_entries.push(child);
                                            }
                                        }
                                        // Unfold the parent directory
                                        self.folded_dirs.remove(&parent_path);
                                        self.status_message =
                                            format!("Loaded {} children", child_count);
                                    }
                                    LoadingResult::RecursiveChildren(
                                        parent_path,
                                        all_descendants,
                                    ) => {
                                        // Save the count before moving
                                        let descendant_count = all_descendants.len();

                                        // Add all descendants to file_entries if not already there
                                        for child in all_descendants {
                                            if !self
                                                .file_entries
                                                .iter()
                                                .any(|e| e.path == child.path)
                                            {
                                                self.file_entries.push(child);
                                            }
                                        }
                                        // Unfold the parent and all descendants
                                        self.folded_dirs.remove(&parent_path);
                                        let prefix = format!("{}/", parent_path);
                                        self.folded_dirs.retain(|path| !path.starts_with(&prefix));
                                        self.status_message = format!(
                                            "Loaded {} descendants recursively",
                                            descendant_count
                                        );
                                    }
                                }
                                // Clear loading state
                                self.loading_path = None;
                            }
                            Ok(Err(e)) => {
                                self.status_message = format!("Loading error: {}", e);
                                self.loading_path = None;
                            }
                            Err(e) => {
                                self.status_message = format!("Loading task error: {}", e);
                                self.loading_path = None;
                            }
                        }
                    }
                } else {
                    // Update throbber animation
                    self.loading_throbber_frame = (self.loading_throbber_frame + 1) % 8;
                }
            }

            // Handle resume preparation (loading scanned paths)
            if let Some(resume_prep) = &self.resume_prep {
                if resume_prep.load_task.is_finished() {
                    // Take ownership to finalize
                    if let Some(resume_prep) = self.resume_prep.take() {
                        match resume_prep.load_task.await {
                            Ok(Ok(scanned_paths)) => {
                                // Paths loaded successfully, now start the actual scan
                                let _ = self
                                    .start_resume_scan_with_paths(
                                        resume_prep.scan_id,
                                        resume_prep.path,
                                        scanned_paths,
                                    )
                                    .await;
                            }
                            Ok(Err(e)) => {
                                self.status_message = format!("Error loading scan paths: {}", e);
                                self.view = View::ScanList;
                            }
                            Err(e) => {
                                self.status_message =
                                    format!("Resume preparation task error: {}", e);
                                self.view = View::ScanList;
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
            View::ConfirmDelete => self.render_confirm_delete(f, main_chunks[0]),
            View::Deleting => self.render_deleting(f, main_chunks[0]),
            View::PreparingResume => self.render_preparing_resume(f, main_chunks[0]),
            View::Settings => self.render_settings(f, main_chunks[0]),
            View::ConfirmPathChange => self.render_confirm_path_change(f, main_chunks[0]),
            View::SearchDialog => self.render_search_dialog(f, main_chunks[0]),
        }

        let status_idx = if use_info_pane { 2 } else { 1 };
        self.render_status_bar(f, main_chunks[status_idx]);
    }

    fn render_scan_list(&mut self, f: &mut Frame, area: Rect) {
        // Sort scans based on current sort mode
        let mut sorted_scans = self.scans.clone();
        match self.scan_list_sort {
            SortMode::BySize => {
                sorted_scans.sort_by(|a, b| b.total_size.cmp(&a.total_size));
            }
            SortMode::ByPath => {
                sorted_scans.sort_by(|a, b| a.root_path.cmp(&b.root_path));
            }
        }

        let items: Vec<ListItem> = sorted_scans
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

        // Calculate database size for display
        let db_size = self.get_database_size();
        let db_size_str = format_size(db_size);

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Scans (1) | Enter: view | r: resume paused | n: new | ↑/↓ or j/k: navigate")
                    .title_top(
                        Line::from(format!(" DB: {} ", db_size_str))
                            .right_aligned()
                    )
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
            let search_info = if !self.search_query.is_empty() {
                format!(" | Search: '{}'", self.search_query)
            } else {
                String::new()
            };
            format!(
                "Files (2) | Scan: {}{} | /: search | z: fold | Z: unfold all | s: shell | Space: mark",
                scan.root_path, search_info
            )
        } else {
            "Files (2)".to_string()
        };

        let visible_entries = self.get_visible_entries();
        let throbber_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
        let throbber = throbber_chars[self.loading_throbber_frame % throbber_chars.len()];

        let items: Vec<ListItem> = visible_entries
            .iter()
            .map(|entry| {
                let size_str = format_size(entry.size as u64);
                let is_folded = entry.is_dir && self.folded_dirs.contains(&entry.path);
                let is_loading = self.loading_path.as_ref() == Some(&entry.path);

                let icon = if entry.is_dir {
                    if is_loading {
                        format!("{} 📁", throbber)
                    } else if is_folded {
                        "▶ 📁".to_string()
                    } else {
                        "▼ 📁".to_string()
                    }
                } else {
                    "  📄".to_string()
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

    fn render_search_dialog(&self, f: &mut Frame, area: Rect) {
        let text = vec![
            Line::from(""),
            Line::from("Search files by name or path:"),
            Line::from(""),
            Line::from(vec![Span::styled(
                &self.search_query,
                Style::default().fg(Color::Cyan),
            )]),
            Line::from(""),
            Line::from("Press Enter to apply search, Esc to cancel"),
            Line::from("(Case-insensitive search)"),
        ];

        let paragraph = Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("Search Files"))
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
            Line::from("  x           Delete scan (Scan list view)"),
            Line::from("  t           Toggle sort mode (size/path)"),
            Line::from("  Space       Mark/unmark file for cleanup (File view)"),
            Line::from("  Space       Remove from cleanup list (Cleanup view)"),
            Line::from("  /           Search files by name or path (File view)"),
            Line::from("  z/o         Fold/unfold directory (File view)"),
            Line::from("  Z/O         Unfold directory and all subdirs (File view)"),
            Line::from("  s           Open shell in directory/parent (File view)"),
            Line::from("  s/g         Generate cleanup script (Cleanup view)"),
            Line::from("  Enter/o     Select/open"),
            Line::from(""),
            Line::from(vec![Span::styled(
                "General:",
                Style::default().fg(Color::Yellow),
            )]),
            Line::from("  ?           Show this help"),
            Line::from("  S           Show settings"),
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

    fn render_confirm_delete(&self, f: &mut Frame, area: Rect) {
        let mut lines = vec![Line::from("")];

        if let Some(scan_id) = self.delete_scan_id {
            // Find the scan in the list to show its path
            let scan_path = self
                .scans
                .iter()
                .find(|s| s.id == scan_id)
                .map(|s| s.root_path.as_str())
                .unwrap_or("unknown");

            lines.push(Line::from(vec![Span::styled(
                "Delete Scan?",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(""));
            lines.push(Line::from(format!("Scan ID: {}", scan_id)));
            lines.push(Line::from(format!("Path: {}", scan_path)));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                "This will permanently delete all scan data.",
                Style::default().fg(Color::Yellow),
            )]));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                "Press Y to confirm deletion, N/Esc to cancel",
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::ITALIC),
            )]));
        }

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Confirm Deletion"),
            )
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
    }

    fn render_deleting(&self, f: &mut Frame, area: Rect) {
        let throbber_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
        let throbber = throbber_chars[self.delete_throbber_frame % throbber_chars.len()];

        let mut lines = vec![
            Line::from(""),
            Line::from(vec![Span::styled(
                format!("{} Deleting scan...", throbber),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
        ];

        if let Some(scan_id) = self.delete_scan_id {
            // Find the scan in the list to show its path
            let scan_path = self
                .scans
                .iter()
                .find(|s| s.id == scan_id)
                .map(|s| s.root_path.as_str())
                .unwrap_or("unknown");

            lines.push(Line::from(format!("Scan ID: {}", scan_id)));
            lines.push(Line::from(format!("Path: {}", scan_path)));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                "Removing scan data from database...",
                Style::default().fg(Color::Gray),
            )]));
        }

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Deleting Scan"),
            )
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
    }

    fn render_confirm_path_change(&self, f: &mut Frame, area: Rect) {
        let mut lines = vec![Line::from("")];

        if let Some((idx, new_path, old_path)) = &self.pending_path_change {
            let path_type = if *idx == 0 { "Config" } else { "Database" };

            lines.push(Line::from(vec![Span::styled(
                format!("Change {} Path?", path_type),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )]));
            lines.push(Line::from(""));
            lines.push(Line::from(format!("Old path: {}", old_path.display())));
            lines.push(Line::from(format!("New path: {}", new_path.display())));
            lines.push(Line::from(""));

            if self.pending_path_exists {
                lines.push(Line::from(vec![Span::styled(
                    "✓ File exists at new path",
                    Style::default().fg(Color::Green),
                )]));
                lines.push(Line::from(""));
                if *idx == 0 {
                    lines.push(Line::from("Settings will be loaded from the new location."));
                } else {
                    lines.push(Line::from(
                        "Note: Changing database path requires restarting the application.",
                    ));
                }
            } else {
                lines.push(Line::from(vec![Span::styled(
                    "⚠ File does not exist at new path",
                    Style::default().fg(Color::Red),
                )]));
                lines.push(Line::from(""));
                if *idx == 0 {
                    lines.push(Line::from(
                        "A new config file will be created with current settings.",
                    ));
                } else {
                    lines.push(Line::from(
                        "Note: Database will need to be created after restart.",
                    ));
                }
            }

            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                "Y: Apply change | N/Esc: Keep old path",
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::ITALIC),
            )]));
        }

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Confirm Path Change"),
            )
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
    }

    fn render_preparing_resume(&self, f: &mut Frame, area: Rect) {
        let throbber_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
        let throbber = throbber_chars[self.delete_throbber_frame % throbber_chars.len()];

        let mut lines = vec![
            Line::from(""),
            Line::from(vec![Span::styled(
                format!("{} Preparing to resume scan...", throbber),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
        ];

        if let Some(resume_prep) = &self.resume_prep {
            lines.push(Line::from(format!("Scan ID: {}", resume_prep.scan_id)));
            lines.push(Line::from(format!("Path: {}", resume_prep.path)));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                "Loading already-scanned paths from database...",
                Style::default().fg(Color::Gray),
            )]));
            lines.push(Line::from(vec![Span::styled(
                "This may take a moment for large scans.",
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::ITALIC),
            )]));
        }

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Preparing Resume"),
            )
            .wrap(Wrap { trim: true });

        f.render_widget(paragraph, area);
    }

    fn render_settings(&mut self, f: &mut Frame, area: Rect) {
        // Settings items - dynamically show current sort modes
        let config_path_display = if self.editing_path_index == Some(0) {
            // Show cursor at current position when editing config path
            let mut display = self.path_input.clone();
            display.insert(self.path_input_cursor, '█');
            display
        } else {
            self.settings_path.display().to_string()
        };

        let db_path_display = if self.editing_path_index == Some(1) {
            // Show cursor at current position when editing database path
            let mut display = self.path_input.clone();
            display.insert(self.path_input_cursor, '█');
            display
        } else {
            self.db_path.display().to_string()
        };

        let settings_items = vec![
            ("Config File Path", config_path_display),
            ("Database Path", db_path_display),
            ("", "".to_string()), // Spacer
            (
                "File Tree Sort",
                self.file_tree_sort.display_name().to_string(),
            ),
            (
                "Scan List Sort",
                self.scan_list_sort.display_name().to_string(),
            ),
            ("Auto-fold Depth", "1 level".to_string()),
            (
                "Follow Symlinks",
                if self.follow_symlinks { "Yes" } else { "No" }.to_string(),
            ),
            ("", "".to_string()), // Spacer
            ("About", "Rootkitty - Disk Usage Analyzer".to_string()),
            ("Version", env!("CARGO_PKG_VERSION").to_string()),
        ];

        let items: Vec<ListItem> = settings_items
            .iter()
            .enumerate()
            .map(|(idx, (key, value))| {
                if key.is_empty() {
                    ListItem::new("")
                } else {
                    // Index 0: config path (editable)
                    // Index 1: database path (editable)
                    // Indices 3, 4, and 6: sort settings and follow symlinks (toggleable)
                    let indicator = if idx == 0 || idx == 1 {
                        if self.editing_path_index == Some(idx) {
                            "[editing...]"
                        } else {
                            "[e/r]"
                        }
                    } else if idx == 3 || idx == 4 || idx == 6 {
                        "[t]"
                    } else {
                        "   "
                    };

                    // Special handling when editing paths - add visual emphasis
                    if (idx == 0 || idx == 1) && self.editing_path_index == Some(idx) {
                        let line = Line::from(vec![
                            Span::raw(format!("{} {}: ", indicator, key)),
                            Span::styled(
                                value,
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]);
                        ListItem::new(line)
                    } else {
                        let content = if value.is_empty() {
                            format!("{} {}", indicator, key)
                        } else {
                            format!("{} {}: {}", indicator, key, value)
                        };
                        ListItem::new(content)
                    }
                }
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Settings (Press Shift+S to open | Esc to close)"),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">> ");

        f.render_stateful_widget(list, area, &mut self.settings_list_state);
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
                "q: quit | n: new | t: toggle sort | S: settings | 1: scans | 2: files | 3: cleanup"
            }
            View::FileTree => {
                "q: quit | t: toggle sort | s: shell | Space: mark | z: fold | ↑↓/jk: navigate"
            }
            View::CleanupList => {
                "q: quit | S: settings | 1: scans | 2: files | 3: cleanup | s: generate | Space: remove"
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
            View::ConfirmDelete => {
                "y: confirm delete | n/Esc: cancel"
            }
            View::Deleting => {
                "Deleting scan, please wait..."
            }
            View::PreparingResume => {
                "Loading scanned paths, please wait..."
            }
            View::Settings => {
                if self.editing_path_index.is_some() {
                    "Enter: save | Esc: cancel | ←→: move cursor | Type path"
                } else {
                    "q/Esc: close | t: toggle | e: edit path | r: restore default | ↑↓/jk: navigate"
                }
            }
            View::ConfirmPathChange => {
                "y: apply change | n/Esc: keep old path"
            }
            View::SearchDialog => {
                "Enter: apply search | Esc: cancel | Type to search files by name or path"
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

        // Fix up any orphaned "running" scans (from Ctrl+C or crashes)
        // These should be marked as "paused" so they can be resumed
        for scan in &self.scans {
            if scan.status == "running" && scan.completed_at.is_none() {
                // Calculate stats from what's already been scanned and saved to the database
                let stats = self.db.calculate_scan_stats(scan.id).await?;
                let _ = self.db.pause_scan(scan.id, &stats).await;
            }
        }

        // Reload after fixing up statuses
        self.scans = self.db.list_scans().await?;

        if !self.scans.is_empty() && self.scan_list_state.selected().is_none() {
            self.scan_list_state.select(Some(0));
        }
        Ok(())
    }

    fn save_settings(&self) -> Result<()> {
        let settings = Settings {
            ui: crate::settings::UiSettings {
                file_tree_sort: self.file_tree_sort,
                scan_list_sort: self.scan_list_sort,
                auto_fold_depth: 1, // Current default, will be configurable later
            },
            scan: crate::settings::ScanSettings {
                follow_symlinks: self.follow_symlinks,
            },
        };
        settings.save(&self.settings_path)?;
        Ok(())
    }

    fn select_scan(&mut self) -> Result<()> {
        if let Some(selected) = self.scan_list_state.selected() {
            if let Some(scan) = self.scans.get(selected).cloned() {
                let scan_id = scan.id;
                let db = self.db.clone();

                // Spawn background task to load entries
                let loading_task = tokio::spawn(async move {
                    // Load the root directory and its immediate children
                    let root = db.get_root_entry(scan_id).await?;

                    let mut entries = Vec::new();
                    if let Some(root) = root {
                        // Load root + its immediate children (depth 1)
                        let children = db.get_entries_by_parent(scan_id, Some(&root.path)).await?;
                        entries.push(root);
                        entries.extend(children);
                    }

                    Ok(LoadingResult::ScanEntries(scan_id, entries))
                });

                self.loading_task = Some(loading_task);
                self.loading_path = Some(scan.root_path.clone());
                self.loading_throbber_frame = 0;
                self.status_message = format!("Loading scan: {}", scan.root_path);
                self.view = View::FileTree;
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

    fn toggle_fold_directory(&mut self, recursive: bool) -> Result<()> {
        if let Some(selected) = self.file_list_state.selected() {
            let visible_entries = self.get_visible_entries();
            if let Some(entry) = visible_entries.get(selected) {
                if !entry.is_dir {
                    self.status_message = "Not a directory".to_string();
                    return Ok(());
                }

                let dir_path = entry.path.clone();
                let dir_name = entry.name.clone();
                let is_folded = self.folded_dirs.contains(&dir_path);

                if is_folded {
                    // Unfold - spawn background task to load children
                    if let Some(scan) = &self.current_scan {
                        let scan_id = scan.id;
                        let db = self.db.clone();
                        let parent_path = dir_path.clone();

                        if !recursive {
                            // Non-recursive: load immediate children only
                            let loading_task = tokio::spawn(async move {
                                let children = db
                                    .get_entries_by_parent(scan_id, Some(&parent_path))
                                    .await?;
                                Ok(LoadingResult::DirectoryChildren(parent_path, children))
                            });

                            self.loading_task = Some(loading_task);
                            self.loading_path = Some(dir_path.clone());
                            self.loading_throbber_frame = 0;
                            self.status_message = format!("Loading: {}", dir_name);
                        } else {
                            // Recursive: load all descendants
                            let loading_task = tokio::spawn(async move {
                                let all_descendants =
                                    db.get_all_descendants(scan_id, &parent_path).await?;
                                Ok(LoadingResult::RecursiveChildren(
                                    parent_path,
                                    all_descendants,
                                ))
                            });

                            self.loading_task = Some(loading_task);
                            self.loading_path = Some(dir_path.clone());
                            self.loading_throbber_frame = 0;
                            self.status_message =
                                format!("Loading all descendants of: {}", dir_name);
                        }
                    }
                } else {
                    // Fold - collapse this directory and all its descendants
                    self.fold_directory(&dir_path);
                    self.status_message = format!("Folded '{}'", dir_name);
                }
            }
        }
        Ok(())
    }

    fn fold_directory(&mut self, dir_path: &str) {
        // Add this directory to folded set
        self.folded_dirs.insert(dir_path.to_string());

        // We don't need to fold children since they'll be hidden anyway
    }

    fn open_shell(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    ) -> Result<()> {
        if let Some(selected) = self.file_list_state.selected() {
            let visible_entries = self.get_visible_entries();
            if let Some(entry) = visible_entries.get(selected) {
                // Determine the directory to open:
                // - If it's a directory, use it directly
                // - If it's a file, use its parent directory
                let target_dir = if entry.is_dir {
                    entry.path.clone()
                } else {
                    // Use parent_path if available, otherwise extract from path
                    entry.parent_path.clone().unwrap_or_else(|| {
                        // Extract parent from path (everything before last /)
                        entry
                            .path
                            .rsplit_once('/')
                            .map(|(parent, _)| parent.to_string())
                            .unwrap_or_else(|| entry.path.clone())
                    })
                };

                // Determine the shell to use (check SHELL env var, fallback to common shells)
                let shell = std::env::var("SHELL").unwrap_or_else(|_| {
                    // Fallback chain: bash -> zsh -> sh
                    if std::path::Path::new("/bin/bash").exists() {
                        "/bin/bash".to_string()
                    } else if std::path::Path::new("/bin/zsh").exists() {
                        "/bin/zsh".to_string()
                    } else {
                        "/bin/sh".to_string()
                    }
                });

                // We need to exit the TUI temporarily to run the shell
                // Restore terminal first
                disable_raw_mode()?;
                execute!(
                    io::stdout(),
                    LeaveAlternateScreen,
                    DisableMouseCapture,
                    cursor::Show
                )?;

                // Print informative message to stderr before launching shell
                eprintln!("\n{}", "=".repeat(60));
                eprintln!(" Rootkitty: Launching shell in:");
                eprintln!(" {}", target_dir);
                eprintln!();
                eprintln!(" Exit the shell (Ctrl+D or 'exit') to return to rootkitty");
                eprintln!("{}\n", "=".repeat(60));

                // Launch shell in the target directory
                let status = std::process::Command::new(&shell)
                    .current_dir(&target_dir)
                    .status();

                // Print return message before re-enabling TUI
                eprintln!("\n{}", "=".repeat(60));
                eprintln!(" Returning to rootkitty...");
                eprintln!("{}\n", "=".repeat(60));

                // Re-enable TUI
                enable_raw_mode()?;
                execute!(
                    io::stdout(),
                    EnterAlternateScreen,
                    EnableMouseCapture,
                    Clear(ClearType::All)
                )?;

                // Force a full redraw of the terminal
                terminal.clear()?;

                match status {
                    Ok(exit_status) => {
                        if exit_status.success() {
                            self.status_message =
                                format!("✓ Returned from shell (was in: {})", target_dir);
                        } else {
                            self.status_message =
                                format!("Shell exited with status: {:?}", exit_status.code());
                        }
                    }
                    Err(e) => {
                        return Err(anyhow::anyhow!("Failed to launch shell: {}", e));
                    }
                }
            } else {
                self.status_message = "No entry selected".to_string();
            }
        } else {
            self.status_message = "No entry selected".to_string();
        }
        Ok(())
    }

    fn get_visible_entries(&self) -> Vec<&StoredFileEntry> {
        let search_query = if self.search_query.is_empty() {
            None
        } else {
            Some(self.search_query.as_str())
        };
        compute_visible_entries(
            &self.file_entries,
            &self.folded_dirs,
            self.file_tree_sort,
            search_query,
        )
    }

    /// Get the total size of the database on disk (including WAL files)
    fn get_database_size(&self) -> u64 {
        let mut total_size = 0u64;

        // Main database file
        if let Ok(metadata) = std::fs::metadata(&self.db_path) {
            total_size += metadata.len();
        }

        // WAL file (Write-Ahead Log)
        let mut wal_path = self.db_path.clone();
        wal_path.set_extension("db-wal");
        if let Ok(metadata) = std::fs::metadata(&wal_path) {
            total_size += metadata.len();
        }

        // SHM file (Shared Memory)
        let mut shm_path = self.db_path.clone();
        shm_path.set_extension("db-shm");
        if let Ok(metadata) = std::fs::metadata(&shm_path) {
            total_size += metadata.len();
        }

        total_size
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
        // Show loading message and switch to preparing view
        self.status_message = "Loading already-scanned paths from database...".to_string();
        self.view = View::PreparingResume;

        // Start loading scanned paths in background (non-blocking)
        let db_clone = self.db.clone();
        let load_task = tokio::spawn(async move { db_clone.get_scanned_paths(scan_id).await });

        // Store the preparation state
        self.resume_prep = Some(ResumePreparation {
            scan_id,
            path,
            load_task,
        });

        Ok(())
    }

    async fn start_resume_scan_with_paths(
        &mut self,
        scan_id: i64,
        path: String,
        scanned_paths: std::collections::HashSet<String>,
    ) -> Result<()> {
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

        self.status_message = format!(
            "Resuming scan, skipping {} already-scanned paths...",
            scanned_paths.len()
        );

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

        // Spawn scanner in blocking thread (with resume support)
        let follow_symlinks = self.follow_symlinks;
        let scan_handle = tokio::task::spawn_blocking(move || {
            let scanner = Scanner::with_sender(
                &path_clone,
                tx_clone,
                Some(progress_tx),
                cancelled_clone,
                follow_symlinks,
            );
            scanner.scan_resuming(scanned_paths)
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
        let follow_symlinks = self.follow_symlinks;

        // Spawn scanner in blocking thread
        let scan_handle = tokio::task::spawn_blocking(move || {
            let scanner = Scanner::with_sender(
                &path_clone,
                tx_clone,
                Some(progress_tx),
                cancelled_clone,
                follow_symlinks,
            );
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
            Some(i) => i.saturating_sub(page_size),
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
            Some(i) => i.saturating_sub(page_size),
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
            Some(i) => i.saturating_sub(page_size),
            None => 0,
        };
        self.cleanup_list_state.select(Some(i));
    }

    fn settings_list_next(&mut self) {
        // Settings has 10 items (0-9)
        // Empty lines are at indices 2 and 7 (should be skipped)
        let num_items = 10;
        let empty_indices = [2, 7];

        let mut i = match self.settings_list_state.selected() {
            Some(i) => {
                if i >= num_items - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };

        // Skip empty lines
        while empty_indices.contains(&i) {
            i = if i >= num_items - 1 { 0 } else { i + 1 };
        }

        self.settings_list_state.select(Some(i));
    }

    fn settings_list_previous(&mut self) {
        // Settings has 10 items (0-9)
        // Empty lines are at indices 2 and 7 (should be skipped)
        let num_items = 10;
        let empty_indices = [2, 7];

        let mut i = match self.settings_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    num_items - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };

        // Skip empty lines
        while empty_indices.contains(&i) {
            i = if i == 0 { num_items - 1 } else { i - 1 };
        }

        self.settings_list_state.select(Some(i));
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
        // Calculate parent_path from the full path
        let parent_path = if depth > 0 {
            path.rsplit_once('/').map(|(parent, _)| parent.to_string())
        } else {
            None
        };

        StoredFileEntry {
            id,
            scan_id: 1,
            path: path.to_string(),
            name: name.to_string(),
            parent_path,
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
        let visible = compute_visible_entries(&entries, &folded_dirs, SortMode::ByPath, None);
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
        let visible = compute_visible_entries(&entries, &folded_dirs, SortMode::ByPath, None);
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
        let visible = compute_visible_entries(&entries, &folded_dirs, SortMode::ByPath, None);

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
        let visible = compute_visible_entries(&entries, &folded_dirs, SortMode::ByPath, None);

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
    fn test_hierarchical_sort_by_size() {
        // Test that children are kept with their parent when sorting by size
        // Structure:
        // /root (depth 0)
        //   /root/big_dir (depth 1, size 1000)
        //     /root/big_dir/file1.txt (depth 2, size 500)
        //     /root/big_dir/file2.txt (depth 2, size 500)
        //   /root/small_dir (depth 1, size 100)
        //     /root/small_dir/tiny.txt (depth 2, size 100)
        //   /root/medium_file.txt (depth 1, size 200)

        let entries = vec![
            create_test_entry(1, "/root", "root", 0, true),
            create_test_entry(2, "/root/big_dir", "big_dir", 1, true),
            create_test_entry(3, "/root/big_dir/file1.txt", "file1.txt", 2, false),
            create_test_entry(4, "/root/big_dir/file2.txt", "file2.txt", 2, false),
            create_test_entry(5, "/root/small_dir", "small_dir", 1, true),
            create_test_entry(6, "/root/small_dir/tiny.txt", "tiny.txt", 2, false),
            create_test_entry(7, "/root/medium_file.txt", "medium_file.txt", 1, false),
        ];

        // Manually set sizes for testing
        let mut entries_with_size = entries.clone();
        entries_with_size[1].size = 1000; // big_dir
        entries_with_size[2].size = 500; // file1.txt
        entries_with_size[3].size = 500; // file2.txt
        entries_with_size[4].size = 100; // small_dir
        entries_with_size[5].size = 100; // tiny.txt
        entries_with_size[6].size = 200; // medium_file.txt

        let folded_dirs = HashSet::new(); // All unfolded
        let visible =
            compute_visible_entries(&entries_with_size, &folded_dirs, SortMode::BySize, None);

        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        // Expected order when sorted by size hierarchically:
        // /root (always first - root)
        // /root/big_dir (1000) - largest sibling
        //   /root/big_dir/file1.txt (500)
        //   /root/big_dir/file2.txt (500)
        // /root/medium_file.txt (200) - second largest sibling
        // /root/small_dir (100) - smallest sibling
        //   /root/small_dir/tiny.txt (100)

        assert_eq!(paths[0], "/root", "Root should be first");
        assert_eq!(
            paths[1], "/root/big_dir",
            "big_dir should be second (largest child of root)"
        );

        // Children of big_dir should come before any siblings of big_dir
        // Find where big_dir's descendants end (where we hit something that's not under /root/big_dir)
        let big_dir_idx = paths.iter().position(|&p| p == "/root/big_dir").unwrap();
        let big_dir_children_end = paths[big_dir_idx + 1..]
            .iter()
            .position(|&p| !p.starts_with("/root/big_dir/"))
            .map(|i| i + big_dir_idx + 1)
            .unwrap_or(paths.len());

        // big_dir is at index 1, its children should be at 2 and 3, so children_end should be 4
        assert!(
            big_dir_children_end == 4,
            "All children of big_dir should end at index 4, but ended at index {}",
            big_dir_children_end
        );

        // medium_file should come after all big_dir descendants
        let medium_idx = paths
            .iter()
            .position(|&p| p == "/root/medium_file.txt")
            .unwrap();
        let file1_idx = paths
            .iter()
            .position(|&p| p == "/root/big_dir/file1.txt")
            .unwrap();
        let file2_idx = paths
            .iter()
            .position(|&p| p == "/root/big_dir/file2.txt")
            .unwrap();
        assert!(
            medium_idx > file1_idx,
            "medium_file should come after big_dir's children"
        );
        assert!(
            medium_idx > file2_idx,
            "medium_file should come after big_dir's children"
        );

        // small_dir should come after medium_file (size ordering among siblings)
        let small_dir_idx = paths.iter().position(|&p| p == "/root/small_dir").unwrap();
        assert!(
            small_dir_idx > medium_idx,
            "small_dir should come after medium_file (smaller size)"
        );

        // tiny.txt should come after small_dir but before end
        let tiny_idx = paths
            .iter()
            .position(|&p| p == "/root/small_dir/tiny.txt")
            .unwrap();
        assert!(
            tiny_idx > small_dir_idx,
            "tiny.txt should come after its parent small_dir"
        );
        assert!(tiny_idx == paths.len() - 1, "tiny.txt should be last");
    }

    // ========================================================================
    // COMPREHENSIVE TESTING OF SORT + FOLD COMBINATIONS
    // ========================================================================

    /// Create a realistic test fixture representing a typical directory structure
    ///
    /// Structure:
    /// /project (5000 bytes total)
    ///   /project/src (3000 bytes)
    ///     /project/src/main.rs (1000 bytes)
    ///     /project/src/lib.rs (500 bytes)
    ///     /project/src/utils (1500 bytes)
    ///       /project/src/utils/helper.rs (800 bytes)
    ///       /project/src/utils/config.rs (700 bytes)
    ///   /project/tests (1500 bytes)
    ///     /project/tests/integration.rs (1500 bytes)
    ///   /project/docs (300 bytes)
    ///     /project/docs/README.md (300 bytes)
    ///   /project/Cargo.toml (200 bytes)
    fn create_test_fixture() -> Vec<StoredFileEntry> {
        vec![
            // Root
            create_test_entry_with_size(1, "/project", "project", 0, true, 5000),
            // src directory (largest)
            create_test_entry_with_size(2, "/project/src", "src", 1, true, 3000),
            create_test_entry_with_size(3, "/project/src/main.rs", "main.rs", 2, false, 1000),
            create_test_entry_with_size(4, "/project/src/lib.rs", "lib.rs", 2, false, 500),
            create_test_entry_with_size(5, "/project/src/utils", "utils", 2, true, 1500),
            create_test_entry_with_size(
                6,
                "/project/src/utils/helper.rs",
                "helper.rs",
                3,
                false,
                800,
            ),
            create_test_entry_with_size(
                7,
                "/project/src/utils/config.rs",
                "config.rs",
                3,
                false,
                700,
            ),
            // tests directory (medium)
            create_test_entry_with_size(8, "/project/tests", "tests", 1, true, 1500),
            create_test_entry_with_size(
                9,
                "/project/tests/integration.rs",
                "integration.rs",
                2,
                false,
                1500,
            ),
            // docs directory (small)
            create_test_entry_with_size(10, "/project/docs", "docs", 1, true, 300),
            create_test_entry_with_size(11, "/project/docs/README.md", "README.md", 2, false, 300),
            // Cargo.toml (file at root, smallest)
            create_test_entry_with_size(12, "/project/Cargo.toml", "Cargo.toml", 1, false, 200),
        ]
    }

    fn create_test_entry_with_size(
        id: i64,
        path: &str,
        name: &str,
        depth: i64,
        is_dir: bool,
        size: i64,
    ) -> StoredFileEntry {
        let parent_path = if depth > 0 {
            path.rsplit_once('/').map(|(parent, _)| parent.to_string())
        } else {
            None
        };

        StoredFileEntry {
            id,
            scan_id: 1,
            path: path.to_string(),
            name: name.to_string(),
            parent_path,
            size,
            is_dir,
            modified_at: Some(Utc::now()),
            depth,
        }
    }

    #[test]
    fn test_sort_by_path_all_unfolded() {
        let entries = create_test_fixture();
        let folded = HashSet::new();

        let visible = compute_visible_entries(&entries, &folded, SortMode::ByPath, None);
        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        // All 12 entries should be visible
        assert_eq!(visible.len(), 12);

        // Should be in alphabetical/path order
        assert_eq!(paths[0], "/project");
        assert_eq!(paths[1], "/project/Cargo.toml");
        assert_eq!(paths[2], "/project/docs");
        assert_eq!(paths[3], "/project/docs/README.md");
        assert_eq!(paths[4], "/project/src");
        assert_eq!(paths[5], "/project/src/lib.rs");
        assert_eq!(paths[6], "/project/src/main.rs");
        assert_eq!(paths[7], "/project/src/utils");
        assert_eq!(paths[8], "/project/src/utils/config.rs");
        assert_eq!(paths[9], "/project/src/utils/helper.rs");
        assert_eq!(paths[10], "/project/tests");
        assert_eq!(paths[11], "/project/tests/integration.rs");
    }

    #[test]
    fn test_sort_by_size_all_unfolded() {
        let entries = create_test_fixture();
        let folded = HashSet::new();

        let visible = compute_visible_entries(&entries, &folded, SortMode::BySize, None);
        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        // All 12 entries should be visible
        assert_eq!(visible.len(), 12);

        // Expected order (largest to smallest within each level):
        // /project (root)
        //   /project/src (3000) - largest child
        //     /project/src/main.rs (1000) - largest in src
        //     /project/src/utils (1500) - next largest in src
        //       /project/src/utils/helper.rs (800)
        //       /project/src/utils/config.rs (700)
        //     /project/src/lib.rs (500) - smallest in src
        //   /project/tests (1500) - next largest child of project
        //     /project/tests/integration.rs (1500)
        //   /project/docs (300) - smaller
        //     /project/docs/README.md (300)
        //   /project/Cargo.toml (200) - smallest

        assert_eq!(paths[0], "/project");
        assert_eq!(
            paths[1], "/project/src",
            "src should be first child (largest)"
        );

        // All of src's descendants should come before tests
        let tests_idx = paths.iter().position(|&p| p == "/project/tests").unwrap();
        let main_idx = paths
            .iter()
            .position(|&p| p == "/project/src/main.rs")
            .unwrap();
        let utils_idx = paths
            .iter()
            .position(|&p| p == "/project/src/utils")
            .unwrap();
        let lib_idx = paths
            .iter()
            .position(|&p| p == "/project/src/lib.rs")
            .unwrap();

        assert!(main_idx < tests_idx, "main.rs should come before tests");
        assert!(utils_idx < tests_idx, "utils should come before tests");
        assert!(lib_idx < tests_idx, "lib.rs should come before tests");

        // Within src's children, order should be by size: utils (1500), main.rs (1000), lib.rs (500)
        assert!(
            utils_idx < main_idx,
            "utils (1500) should come before main.rs (1000)"
        );
        assert!(
            main_idx < lib_idx,
            "main.rs (1000) should come before lib.rs (500)"
        );

        // Within utils, helper.rs (800) should come before config.rs (700)
        let helper_idx = paths
            .iter()
            .position(|&p| p == "/project/src/utils/helper.rs")
            .unwrap();
        let config_idx = paths
            .iter()
            .position(|&p| p == "/project/src/utils/config.rs")
            .unwrap();
        assert!(
            helper_idx < config_idx,
            "helper.rs (800) should come before config.rs (700)"
        );
    }

    #[test]
    fn test_sort_by_size_with_folded_dirs() {
        let entries = create_test_fixture();
        let mut folded = HashSet::new();
        folded.insert("/project/src/utils".to_string());

        let visible = compute_visible_entries(&entries, &folded, SortMode::BySize, None);
        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        // Should see: project, src, main.rs, lib.rs, utils (folded), tests, integration.rs, docs, README.md, Cargo.toml
        // utils children should be hidden
        assert!(
            !paths.contains(&"/project/src/utils/helper.rs"),
            "helper.rs should be hidden"
        );
        assert!(
            !paths.contains(&"/project/src/utils/config.rs"),
            "config.rs should be hidden"
        );
        assert!(
            paths.contains(&"/project/src/utils"),
            "utils itself should be visible"
        );

        // Should be 10 items (12 - 2 hidden files)
        assert_eq!(visible.len(), 10);
    }

    #[test]
    fn test_sort_by_path_with_folded_dirs() {
        let entries = create_test_fixture();
        let mut folded = HashSet::new();
        folded.insert("/project/src".to_string());

        let visible = compute_visible_entries(&entries, &folded, SortMode::ByPath, None);
        let paths: Vec<&str> = visible.iter().map(|e| e.path.as_str()).collect();

        // Should hide everything under /project/src
        assert!(
            paths.contains(&"/project/src"),
            "src should be visible (just folded)"
        );
        assert!(
            !paths.contains(&"/project/src/main.rs"),
            "main.rs should be hidden"
        );
        assert!(
            !paths.contains(&"/project/src/lib.rs"),
            "lib.rs should be hidden"
        );
        assert!(
            !paths.contains(&"/project/src/utils"),
            "utils should be hidden"
        );
        assert!(
            !paths.contains(&"/project/src/utils/helper.rs"),
            "helper.rs should be hidden"
        );

        // Should be 6 items (project, src, tests, integration.rs, docs, README.md, Cargo.toml)
        assert_eq!(visible.len(), 7);
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
        let visible = compute_visible_entries(&entries, &folded_dirs, SortMode::ByPath, None);

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
