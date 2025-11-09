//! Types and enums used across the UI

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    ScanList,
    FileTree,
    CleanupList,
    ScanDialog,
    Scanning,
    Help,
    ConfirmDelete,
    Deleting,
    PreparingResume,
    Settings,
}

#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub entries_scanned: u64,
    pub active_dirs: Vec<(String, usize, usize)>,
    pub active_workers: usize,
}

pub struct ActiveScan {
    pub scan_id: i64,
    pub scan_handle: tokio::task::JoinHandle<
        anyhow::Result<(Vec<crate::scanner::FileEntry>, crate::scanner::ScanStats)>,
    >,
    pub actor_handle: tokio::task::JoinHandle<anyhow::Result<()>>,
    pub tx: tokio::sync::mpsc::Sender<crate::db::ActorMessage>,
    pub progress_rx: tokio::sync::mpsc::UnboundedReceiver<crate::scanner::ProgressUpdate>,
    pub cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

pub struct ResumePreparation {
    pub scan_id: i64,
    pub path: String,
    pub load_task: tokio::task::JoinHandle<anyhow::Result<std::collections::HashSet<String>>>,
}
