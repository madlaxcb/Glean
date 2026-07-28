//! Glean app — Hybrid shell (egui) + WebView2 reader + core service (M0b).
//!
//! See docs/Glean-开发方案.md and docs/spike-ui.md.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod fonts;
mod reader;
mod ui;

use eframe::egui;
use glean_core::{
    AppCommand, AppEvent, EntryDetail, EntryFilter, EntrySummary, Feed, Folder, GleanService,
    ReaderHostMode,
};
use reader::ReaderHost;
use ui::SpikeApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("Glean / 拾光 — M0b"),
        ..Default::default()
    };

    eframe::run_native(
        "Glean M0b",
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
}

impl SpikeState {
    pub fn new() -> Self {
        let service = GleanService::open_in_memory().expect("open in-memory store");
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
            status: "M0b — local SQLite demo (no network fetch)".into(),
            open_count: 0,
            search: String::new(),
            reader_rect: egui::Rect::NOTHING,
            reader: ReaderHost::new(),
            splitting: false,
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
                self.open_count += 1;
                let html = glean_core::reader_document(
                    &entry.summary.title,
                    &entry.content_html,
                    self.dark,
                );
                self.status = format!(
                    "OpenEntry id={} ({}) opens={} unread={}",
                    entry.summary.id.0, entry.summary.title, self.open_count, self.unread_total
                );
                self.open_detail = Some(entry);
                self.reader.show_html(&html);
            }
            AppEvent::UnreadChanged { total } => {
                self.unread_total = total;
            }
            AppEvent::Status { message } => {
                self.status = message;
            }
            AppEvent::Error { message } => {
                self.status = format!("Error: {message}");
            }
        }
    }

    pub fn select_index(&mut self, index: usize) {
        if index < self.entries.len() {
            self.selected = Some(index);
            let id = self.entries[index].id;
            self.dispatch(AppCommand::OpenEntry { id });
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
            let html =
                glean_core::reader_document(&entry.summary.title, &entry.content_html, self.dark);
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
        self.dispatch(AppCommand::ListEntries { filter });
    }
}
