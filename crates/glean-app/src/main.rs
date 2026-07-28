//! Glean M0 UI Spike — egui three-pane shell + WebView2 reader (Windows).
//!
//! Goal: validate Hybrid path A before any RSS business code.
//! See docs/Glean-开发方案.md §9.0 and docs/spike-ui.md.

mod fonts;
mod reader;
mod ui;

use eframe::egui;
use glean_core::{ReaderHostMode, SampleEntry};
use reader::ReaderHost;
use ui::SpikeApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("Glean / 拾光 — M0 UI Spike"),
        ..Default::default()
    };

    eframe::run_native(
        "Glean M0 Spike",
        options,
        Box::new(|cc| Ok(Box::new(SpikeApp::new(cc)))),
    )
}

/// Shared spike state living on the UI thread.
pub struct SpikeState {
    pub samples: Vec<SampleEntry>,
    pub index: usize,
    pub dark: bool,
    pub host_mode: ReaderHostMode,
    pub nav_width: f32,
    pub list_width: f32,
    pub status: String,
    pub open_count: u64,
    pub search: String,
    /// Latest reader panel rect in egui points (window client space).
    pub reader_rect: egui::Rect,
    pub reader: ReaderHost,
}

impl SpikeState {
    pub fn new() -> Self {
        Self {
            samples: SampleEntry::catalog(),
            index: 0,
            dark: false,
            host_mode: ReaderHostMode::ChildEmbed,
            nav_width: 200.0,
            list_width: 320.0,
            status: "M0 Spike — no business feeds yet".into(),
            open_count: 0,
            search: String::new(),
            reader_rect: egui::Rect::NOTHING,
            reader: ReaderHost::new(),
        }
    }

    pub fn current(&self) -> &SampleEntry {
        &self.samples[self.index]
    }

    pub fn select(&mut self, index: usize) {
        if index < self.samples.len() {
            self.index = index;
            self.push_current_to_reader();
        }
    }

    pub fn next(&mut self) {
        let n = self.samples.len();
        self.index = (self.index + 1) % n;
        self.push_current_to_reader();
    }

    pub fn prev(&mut self) {
        let n = self.samples.len();
        self.index = (self.index + n - 1) % n;
        self.push_current_to_reader();
    }

    pub fn toggle_theme(&mut self) {
        self.dark = !self.dark;
        self.reader.set_titlebar_dark(self.dark);
        self.push_current_to_reader();
        self.status = format!("Theme: {}", if self.dark { "dark" } else { "light" });
    }

    pub fn toggle_host_mode(&mut self) {
        self.host_mode = self.host_mode.toggle();
        self.reader.set_mode(self.host_mode);
        self.status = format!("Host mode -> {}", self.host_mode.label());
        self.push_current_to_reader();
    }

    pub fn push_current_to_reader(&mut self) {
        let entry = self.current().clone();
        let html = glean_core::reader_document(&entry.title, &entry.html_body, self.dark);
        self.open_count += 1;
        self.status = format!(
            "OpenEntry #{} id={} ({}) opens={}",
            self.index + 1,
            entry.id,
            entry.title,
            self.open_count
        );
        self.reader.show_html(&html);
    }
}
