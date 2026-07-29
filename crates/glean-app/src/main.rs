//! Glean app — Hybrid shell (egui) + WebView2 reader + core service.
//!
//! See docs/Glean-开发方案.md and docs/spike-ui.md.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod fonts;
mod reader;
mod ui;

use eframe::egui;
use glean_core::{
    default_config_path, default_db_path, run_refresh_task, AppCommand, AppConfig, AppEvent,
    EntryDetail, EntryFilter, EntrySummary, Feed, Folder, FolderId, GleanService, ReaderHostMode,
    RefreshOutcome, RefreshTask,
};
use reader::ReaderHost;
use std::sync::mpsc;
use std::thread;
use ui::SpikeApp;

fn main() -> eframe::Result<()> {
    // Single-instance lock: exit if another instance is already running.
    let _lock = match single_instance_lock() {
        Some(lock) => lock,
        None => {
            eprintln!("Glean 已在运行。");
            return Ok(());
        }
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("Glean / 拾光"),
        ..Default::default()
    };

    eframe::run_native(
        "Glean",
        options,
        Box::new(|cc| Ok(Box::new(SpikeApp::new(cc)))),
    )
}

/// Acquire a single-instance lock. Returns None if another instance holds it.
#[cfg(windows)]
fn single_instance_lock() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "GleanSingleInstance\0".encode_utf16().collect();
    unsafe {
        let handle = CreateMutexW(None, false, PCWSTR(name.as_ptr())).ok()?;
        // ERROR_ALREADY_EXISTS = 183
        if GetLastError().0 == 183 {
            None
        } else {
            Some(handle)
        }
    }
}

#[cfg(not(windows))]
fn single_instance_lock() -> Option<std::fs::File> {
    use fs4::fs_std::FileExt;
    let lock_path = std::env::temp_dir().join("glean-single-instance.lock");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .ok()?;
    if file.try_lock_exclusive().is_err() {
        return None;
    }
    Some(file)
}

/// UI-thread state: projects AppEvent; sends AppCommand to GleanService.
pub struct SpikeState {
    pub service: GleanService,
    pub folders: Vec<Folder>,
    pub feeds: Vec<Feed>,
    pub entries: Vec<EntrySummary>,
    pub unread_total: u64,
    pub selected: Option<usize>,
    pub open_detail: Option<EntryDetail>,
    pub filter: EntryFilter,
    pub dark: bool,
    pub host_mode: ReaderHostMode,
    pub nav_width: f32,
    pub list_width: f32,
    pub status: String,
    pub open_count: u64,
    pub search: String,
    pub reader_rect: egui::Rect,
    pub reader: ReaderHost,
    pub splitting: bool,
    pub feed_url_input: String,
    /// Background refresh state.
    refresh_rx: Option<mpsc::Receiver<RefreshOutcome>>,
    refresh_pending: usize,
    /// Clipboard-captured OPML export text (for copy/paste).
    pub opml_export: Option<String>,
    /// Pasted OPML text for import.
    pub opml_import_input: String,
    /// Feed being renamed (id + current title for editing).
    pub rename_feed: Option<(glean_core::FeedId, String)>,
    /// Recent error messages for notification popup.
    pub errors: Vec<String>,
    /// New folder name input.
    pub new_folder_input: String,
    /// Persistent config.
    pub config: AppConfig,
    /// Config file path.
    config_path: std::path::PathBuf,
    /// Auto-refresh timer: seconds since last check.
    auto_refresh_timer: f32,
    /// Persistent buffer for the refresh-interval TextEdit in settings.
    /// Must outlive the frame so user typing isn't overwritten each frame.
    pub refresh_interval_input: String,
}

impl SpikeState {
    pub fn new() -> Self {
        let db = default_db_path();
        let service = GleanService::open_path(&db).unwrap_or_else(|e| {
            eprintln!("glean: open db {:?}: {e}; falling back to memory", db);
            GleanService::open_in_memory().expect("memory store")
        });

        let config_path = default_config_path();
        let config = load_config(&config_path);

        let mut s = Self {
            service,
            folders: Vec::new(),
            feeds: Vec::new(),
            entries: Vec::new(),
            unread_total: 0,
            selected: None,
            open_detail: None,
            filter: EntryFilter::All,
            dark: config.dark,
            host_mode: ReaderHostMode::ChildEmbed,
            nav_width: config.nav_width,
            list_width: config.list_width,
            status: format!("库: {}", db.display()),
            open_count: 0,
            search: String::new(),
            reader_rect: egui::Rect::NOTHING,
            reader: ReaderHost::new(),
            splitting: false,
            feed_url_input: String::new(),
            refresh_rx: None,
            refresh_pending: 0,
            opml_export: None,
            opml_import_input: String::new(),
            rename_feed: None,
            errors: Vec::new(),
            new_folder_input: String::new(),
            config,
            config_path,
            auto_refresh_timer: 0.0,
            refresh_interval_input: config.refresh_interval_secs.to_string(),
        };
        s.dispatch(AppCommand::Bootstrap { seed_demo: true });
        s
    }

    /// Persist current config to disk.
    pub fn save_config(&self) {
        save_config(&self.config_path, &self.config);
    }

    /// Sync runtime state → config struct (call before save).
    pub fn sync_config(&mut self) {
        self.config.dark = self.dark;
        self.config.nav_width = self.nav_width;
        self.config.list_width = self.list_width;
    }

    pub fn dispatch(&mut self, cmd: AppCommand) {
        let events = self.service.handle(cmd);
        for ev in events {
            self.apply_event(ev);
        }
    }

    /// Poll background refresh results (called every frame from update).
    pub fn poll_refresh(&mut self) {
        let rx = match &self.refresh_rx {
            Some(rx) => rx,
            None => return,
        };
        // Drain all available outcomes first to release the borrow.
        let mut outcomes = Vec::new();
        while let Ok(outcome) = rx.try_recv() {
            outcomes.push(outcome);
        }
        let got = outcomes.len();
        for outcome in outcomes {
            self.refresh_pending = self.refresh_pending.saturating_sub(1);
            let events = self.service.apply_refresh_outcome(outcome);
            for ev in events {
                self.apply_event(ev);
            }
        }
        if got > 0 {
            self.dispatch(AppCommand::RefreshNav);
            self.dispatch(AppCommand::ListEntries {
                filter: self.filter,
            });
        }
        if self.refresh_pending == 0 {
            self.refresh_rx = None;
            self.status = "刷新完成".into();
        } else if got > 0 {
            self.status = format!("刷新中… 剩余 {} 个源", self.refresh_pending);
        }
    }

    /// Called every frame with delta time. Triggers auto-refresh if interval > 0.
    pub fn tick_auto_refresh(&mut self, dt: f32) {
        let interval = self.config.refresh_interval_secs;
        if interval <= 0 || self.refresh_rx.is_some() {
            return;
        }
        self.auto_refresh_timer += dt;
        if self.auto_refresh_timer >= interval as f32 {
            self.auto_refresh_timer = 0.0;
            let tasks = match self
                .service
                .prepare_auto_refresh_tasks(self.config.refresh_interval_secs)
            {
                Ok(t) => t,
                Err(_) => return,
            };
            if tasks.is_empty() {
                return;
            }
            let (tx, rx) = mpsc::channel::<RefreshOutcome>();
            self.refresh_rx = Some(rx);
            self.refresh_pending = tasks.len();
            self.status = format!("自动刷新中… {} 个源", tasks.len());
            thread::spawn(move || {
                for task in tasks {
                    let outcome = run_refresh_task(task);
                    let _ = tx.send(outcome);
                }
            });
        }
    }

    fn apply_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Ready => {}
            AppEvent::NavUpdated {
                folders,
                feeds,
                unread_total,
            } => {
                self.folders = folders;
                self.feeds = feeds;
                self.unread_total = unread_total;
            }
            AppEvent::EntriesUpdated { entries } => {
                self.entries = entries;
                if let Some(i) = self.selected {
                    if i >= self.entries.len() {
                        self.selected = None;
                    }
                }
            }
            AppEvent::EntryOpened { entry } => {
                let same = self
                    .open_detail
                    .as_ref()
                    .map(|d| d.summary.id == entry.summary.id)
                    .unwrap_or(false);
                if !same {
                    self.open_count += 1;
                }
                let html = glean_core::reader_document(
                    &entry.summary.title,
                    entry.summary.url.as_deref(),
                    entry.author.as_deref(),
                    &entry.content_html,
                    self.dark,
                    entry.summary.has_content,
                    self.config.image_policy,
                );
                self.reader.show_html(&html);
                self.refresh_status();
            }
            AppEvent::UnreadChanged { total } => {
                self.unread_total = total;
                self.refresh_status();
            }
            AppEvent::Status { message } => {
                self.status = message;
            }
            AppEvent::Error { message } => {
                self.status = format!("错误: {message}");
                self.errors.push(message);
                // Keep only last 50 errors.
                if self.errors.len() > 50 {
                    self.errors.drain(..self.errors.len() - 50);
                }
            }
            AppEvent::OpmlExported { xml } => {
                self.opml_export = Some(xml);
            }
        }
    }

    pub fn select_index(&mut self, index: usize) {
        self.select_index_with(index, false);
    }

    /// `force`: reload reader even if the same entry is already open.
    pub fn select_index_with(&mut self, index: usize, force: bool) {
        if index >= self.entries.len() {
            return;
        }
        let id = self.entries[index].id;
        let already = self
            .open_detail
            .as_ref()
            .map(|d| d.summary.id == id)
            .unwrap_or(false);
        self.selected = Some(index);
        if already && !force {
            self.refresh_status();
            return;
        }
        self.dispatch(AppCommand::OpenEntry { id });
    }

    fn refresh_status(&mut self) {
        let unread = self.unread_total;
        if let Some(e) = &self.open_detail {
            let read_label = if e.summary.is_read {
                "已读"
            } else {
                "未读"
            };
            self.status = format!(
                "「{}」{} · 未读 {} 篇 · 换文计数 {}",
                e.summary.title, read_label, unread, self.open_count
            );
        } else {
            self.status = format!("未读 {} 篇 · 未打开文章", unread);
        }
    }

    pub fn next(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let i = self
            .selected
            .map(|i| (i + 1) % self.entries.len())
            .unwrap_or(0);
        self.select_index(i);
    }

    pub fn prev(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let n = self.entries.len();
        let i = self.selected.map(|i| (i + n - 1) % n).unwrap_or(0);
        self.select_index(i);
    }

    pub fn toggle_theme(&mut self, ctx: &egui::Context) {
        self.dark = !self.dark;
        ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(if self.dark {
            egui::viewport::SystemTheme::Dark
        } else {
            egui::viewport::SystemTheme::Light
        }));
        self.reader.set_titlebar_dark(self.dark);
        // Update last_html with the new theme so that if the WebView2 is
        // recreated later it picks up the right colours.  Then flip the
        // theme live via JS (no document reload needed).
        if let Some(entry) = self.open_detail.clone() {
            let html = glean_core::reader_document(
                &entry.summary.title,
                entry.summary.url.as_deref(),
                entry.author.as_deref(),
                &entry.content_html,
                self.dark,
                entry.summary.has_content,
                self.config.image_policy,
            );
            self.reader.show_html(&html);
        }
        self.reader.apply_theme(self.dark);
        self.sync_config();
        self.save_config();
    }

    pub fn toggle_host_mode(&mut self) {
        self.host_mode = self.host_mode.toggle();
        self.reader.set_mode(self.host_mode);
        self.status = format!("Host mode -> {}", self.host_mode.label());
    }

    pub fn set_filter(&mut self, filter: EntryFilter) {
        self.filter = filter;
        self.selected = None;
        self.search.clear();
        self.dispatch(AppCommand::ListEntries { filter });
    }

    pub fn add_feed_from_url(&mut self) {
        let url = self.feed_url_input.trim().to_string();
        if url.is_empty() {
            self.status = "请输入 RSS/Atom URL".into();
            return;
        }
        self.status = format!("正在抓取 {url} …");
        self.dispatch(AppCommand::AddFeedFromUrl { feed_url: url });
        if !self.status.starts_with("Error") {
            self.feed_url_input.clear();
        }
    }

    /// Launch background refresh: HTTP on threads, DB writes on UI thread.
    pub fn refresh_all_feeds_async(&mut self) {
        if self.refresh_rx.is_some() {
            self.status = "刷新进行中…".into();
            return;
        }
        let tasks: Vec<RefreshTask> = match self.service.prepare_refresh_tasks(None) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("刷新失败: {e}");
                return;
            }
        };
        if tasks.is_empty() {
            self.status = "没有可刷新的订阅".into();
            return;
        }
        let (tx, rx) = mpsc::channel::<RefreshOutcome>();
        self.refresh_rx = Some(rx);
        self.refresh_pending = tasks.len();
        self.status = format!("刷新中… {} 个源", tasks.len());
        thread::spawn(move || {
            for task in tasks {
                let outcome = run_refresh_task(task);
                let _ = tx.send(outcome);
            }
        });
    }

    pub fn delete_feed(&mut self, id: glean_core::FeedId) {
        self.dispatch(AppCommand::DeleteFeed { id });
    }

    pub fn toggle_star_current(&mut self) {
        if let Some(e) = &self.open_detail {
            let id = e.summary.id;
            self.dispatch(AppCommand::ToggleStar { id });
            self.dispatch(AppCommand::OpenEntry { id });
        }
    }

    pub fn mark_all_read(&mut self, feed_id: Option<glean_core::FeedId>) {
        self.dispatch(AppCommand::MarkAllRead { feed_id });
    }

    pub fn run_search(&mut self) {
        self.dispatch(AppCommand::SearchEntries {
            query: self.search.clone(),
        });
    }

    pub fn export_opml(&mut self) {
        self.dispatch(AppCommand::ExportOpml);
    }

    pub fn import_opml(&mut self) {
        let content = self.opml_import_input.clone();
        if content.trim().is_empty() {
            self.status = "请粘贴 OPML 内容".into();
            return;
        }
        self.dispatch(AppCommand::ImportOpml { content });
        if !self.status.starts_with("错误") {
            self.opml_import_input.clear();
        }
    }

    pub fn rename_feed(&mut self, id: glean_core::FeedId, title: String) {
        self.dispatch(AppCommand::RenameFeed { id, title });
    }

    pub fn move_feed_to_folder(
        &mut self,
        feed_id: glean_core::FeedId,
        folder_id: Option<FolderId>,
    ) {
        self.dispatch(AppCommand::MoveFeedToFolder { feed_id, folder_id });
    }

    pub fn create_folder(&mut self, name: String) {
        self.dispatch(AppCommand::CreateFolder { name });
    }

    pub fn toggle_mute_feed(&mut self, id: glean_core::FeedId) {
        self.dispatch(AppCommand::ToggleMuteFeed { id });
    }

    pub fn toggle_image_policy(&mut self) {
        self.config.image_policy = self.config.image_policy.next();
        self.sync_config();
        self.save_config();
        self.status = format!("图片策略: {}", self.config.image_policy.label());
    }

    pub fn reset_layout(&mut self) {
        self.nav_width = 200.0;
        self.list_width = 320.0;
        self.sync_config();
        self.save_config();
        self.status = "布局已重置".into();
    }

    pub fn set_feed_refresh_interval(&mut self, id: glean_core::FeedId, secs: i64) {
        self.dispatch(AppCommand::SetFeedRefreshInterval { id, secs });
    }

    pub fn set_global_refresh_interval(&mut self, secs: i64) {
        self.config.refresh_interval_secs = secs;
        self.refresh_interval_input = secs.to_string();
        self.auto_refresh_timer = 0.0;
        self.sync_config();
        self.save_config();
        self.status = if secs > 0 {
            format!("自动刷新间隔: {}秒", secs)
        } else {
            "自动刷新已关闭".into()
        };
    }
}

// --- Config load / save helpers ---

fn load_config(path: &std::path::Path) -> AppConfig {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_config(path: &std::path::Path, config: &AppConfig) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = std::fs::write(path, json);
    }
}
