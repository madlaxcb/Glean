//! Glean app — Hybrid shell (egui) + WebView2 reader + core service.
//!
//! See docs/Glean-开发方案.md and docs/spike-ui.md.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod fonts;
mod reader;
mod ui;

use eframe::egui;
use glean_core::{
    default_db_path, run_refresh_task, AppCommand, AppEvent, EntryDetail, EntryFilter,
    EntrySummary, Feed, Folder, GleanService, ReaderHostMode, RefreshOutcome, RefreshTask,
};
use reader::ReaderHost;
use std::sync::mpsc;
use std::thread;
use ui::SpikeApp;

fn main() -> eframe::Result<()> {
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
}

impl SpikeState {
    pub fn new() -> Self {
        let db = default_db_path();
        let service = GleanService::open_path(&db).unwrap_or_else(|e| {
            eprintln!("glean: open db {:?}: {e}; falling back to memory", db);
            GleanService::open_in_memory().expect("memory store")
        });
        let mut s = Self {
            service,
            folders: Vec::new(),
            feeds: Vec::new(),
            entries: Vec::new(),
            unread_total: 0,
            selected: None,
            open_detail: None,
            filter: EntryFilter::All,
            dark: false,
            host_mode: ReaderHostMode::ChildEmbed,
            nav_width: 200.0,
            list_width: 320.0,
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
        };
        s.dispatch(AppCommand::Bootstrap { seed_demo: true });
        s
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
                );
                self.open_detail = Some(entry);
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
                self.status = format!("Error: {message}");
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
        if let Some(entry) = self.open_detail.clone() {
            let html = glean_core::reader_document(
                &entry.summary.title,
                entry.summary.url.as_deref(),
                entry.author.as_deref(),
                &entry.content_html,
                self.dark,
            );
            self.reader.show_html(&html);
        }
        self.status = format!("Theme: {}", if self.dark { "dark" } else { "light" });
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
        if !self.status.starts_with("Error") {
            self.opml_import_input.clear();
        }
    }
}
