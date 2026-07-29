use crate::SpikeState;
use eframe::egui::{self, Color32, Frame, Margin, RichText, Sense, Stroke, Ui, Vec2};
use glean_core::{EntryFilter, ReaderHostMode};

const SPLIT_HIT: f32 = 6.0;
const NAV_MIN: f32 = 120.0;
const NAV_MAX: f32 = 360.0;
const LIST_MIN: f32 = 180.0;
const LIST_MAX: f32 = 520.0;
const READER_MIN: f32 = 240.0;

pub struct SpikeApp {
    state: SpikeState,
    primed: bool,
    /// Show OPML import text area.
    show_opml_import: bool,
}

impl SpikeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::fonts::install(&cc.egui_ctx);
        apply_style(&cc.egui_ctx, false);
        Self {
            state: SpikeState::new(),
            primed: false,
            show_opml_import: false,
        }
    }
}

impl eframe::App for SpikeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll background refresh every frame.
        self.state.poll_refresh();

        if self.state.dark {
            ctx.set_visuals(egui::Visuals::dark());
        } else {
            ctx.set_visuals(egui::Visuals::light());
        }

        let panel_fill = ctx.style().visuals.panel_fill;
        let extreme = ctx.style().visuals.extreme_bg_color;
        let stroke_color = ctx.style().visuals.window_stroke.color;

        // --- Top toolbar ---
        egui::TopBottomPanel::top("toolbar")
            .frame(
                Frame::new()
                    .fill(panel_fill)
                    .inner_margin(Margin::symmetric(8, 6))
                    .stroke(Stroke::new(1.0_f32, stroke_color)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Glean");
                    if ui.button("刷新全部").clicked() {
                        self.state.refresh_all_feeds_async();
                    }
                    if ui.button("全部已读").clicked() {
                        self.state.mark_all_read(None);
                    }
                    ui.separator();
                    if ui.button("星标").clicked() {
                        self.state.toggle_star_current();
                    }
                    ui.separator();
                    if ui.button("导入OPML").clicked() {
                        self.show_opml_import = !self.show_opml_import;
                    }
                    if ui.button("导出OPML").clicked() {
                        self.state.export_opml();
                    }
                    ui.separator();
                    if ui
                        .selectable_label(self.state.host_mode == ReaderHostMode::ChildEmbed, "H1")
                        .clicked()
                        && self.state.host_mode != ReaderHostMode::ChildEmbed
                    {
                        self.state.toggle_host_mode();
                    }
                    if ui
                        .selectable_label(
                            self.state.host_mode == ReaderHostMode::FollowOverlay,
                            "H2",
                        )
                        .clicked()
                        && self.state.host_mode != ReaderHostMode::FollowOverlay
                    {
                        self.state.toggle_host_mode();
                    }
                    ui.separator();
                    if ui.button("Theme").clicked() {
                        self.state.toggle_theme(ctx);
                    }
                    ui.separator();
                    ui.label("搜索");
                    let search_id = egui::Id::new("spike_search");
                    let prev_search = self.state.search.clone();
                    let te = egui::TextEdit::singleline(&mut self.state.search)
                        .id(search_id)
                        .desired_width(160.0)
                        .hint_text("标题/正文…");
                    let search_resp = ui.add(te);
                    if search_resp.clicked() || search_resp.gained_focus() {
                        self.state.reader.reclaim_shell_focus();
                        search_resp.request_focus();
                    }
                    // Run search on text change or Enter.
                    if self.state.search != prev_search
                        || (search_resp.has_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                    {
                        self.state.run_search();
                    }
                    if ui.button("✕").clicked() {
                        self.state.search.clear();
                        self.state.set_filter(self.state.filter);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(&self.state.status).small());
                    });
                });
            });

        // --- Add-feed bar ---
        egui::TopBottomPanel::top("add_feed")
            .frame(
                Frame::new()
                    .fill(panel_fill)
                    .inner_margin(Margin::symmetric(8, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("订阅 URL");
                    let feed_id = egui::Id::new("feed_url_input");
                    let te = egui::TextEdit::singleline(&mut self.state.feed_url_input)
                        .id(feed_id)
                        .desired_width(400.0)
                        .hint_text("https://…/rss.xml");
                    let resp = ui.add(te);
                    if resp.clicked() || resp.gained_focus() {
                        self.state.reader.reclaim_shell_focus();
                        resp.request_focus();
                    }
                    let enter = resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("添加").clicked() || enter {
                        self.state.add_feed_from_url();
                    }
                    ui.label(
                        RichText::new("示例: https://www.reddit.com/r/rust/.rss")
                            .small()
                            .weak(),
                    );
                });
            });

        // --- OPML export popup ---
        let opml_xml = self.state.opml_export.clone();
        if let Some(xml) = &opml_xml {
            let mut close = false;
            let mut copied = false;
            let mut saved = false;
            egui::Window::new("OPML 导出")
                .resizable(true)
                .default_width(520.0)
                .default_height(340.0)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("复制到剪贴板").clicked() {
                            copied = true;
                        }
                        if ui.button("另存为…").clicked() {
                            saved = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("关闭").clicked() {
                                close = true;
                            }
                        });
                    });
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(ui.available_height())
                        .show(ui, |ui| {
                            let mut txt = xml.clone();
                            ui.add(
                                egui::TextEdit::multiline(&mut txt)
                                    .desired_width(f32::INFINITY)
                                    .code_editor(),
                            );
                        });
                });
            if copied {
                ctx.copy_text(xml.clone());
                self.state.status = "OPML 已复制到剪贴板".into();
            }
            if saved {
                let xml_clone = xml.clone();
                let status = self.state.status.clone();
                let path = rfd::FileDialog::new()
                    .set_file_name("glean-subscriptions.opml")
                    .add_filter("OPML", &["opml", "xml"])
                    .save_file();
                if let Some(path) = path {
                    match std::fs::write(&path, &xml_clone) {
                        Ok(()) => self.state.status = format!("已保存到 {}", path.display()),
                        Err(e) => self.state.status = format!("保存失败: {e}"),
                    }
                } else {
                    self.state.status = status;
                }
            }
            if close {
                self.state.opml_export = None;
            }
        }

        // --- OPML import panel ---
        if self.show_opml_import {
            egui::TopBottomPanel::top("opml_import")
                .frame(
                    Frame::new()
                        .fill(panel_fill)
                        .inner_margin(Margin::symmetric(8, 4)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("粘贴 OPML XML：");
                        let te = egui::TextEdit::singleline(&mut self.state.opml_import_input)
                            .desired_width(300.0)
                            .hint_text("<opml>…");
                        let resp = ui.add(te);
                        if resp.clicked() || resp.gained_focus() {
                            self.state.reader.reclaim_shell_focus();
                            resp.request_focus();
                        }
                        if ui.button("导入").clicked() {
                            self.state.import_opml();
                        }
                        if ui.button("取消").clicked() {
                            self.show_opml_import = false;
                        }
                    });
                });
        }

        // --- Bottom hints ---
        egui::TopBottomPanel::bottom("hints")
            .frame(
                Frame::new()
                    .fill(panel_fill)
                    .inner_margin(Margin::symmetric(8, 4)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("j/k 换文 · 搜索实时 · 刷新后台异步 · 删除右键 · OPML 导入导出")
                        .small()
                        .weak(),
                );
            });

        // --- Keyboard shortcuts ---
        let search_focused = ctx.memory(|m| m.has_focus(egui::Id::new("spike_search")));
        let feed_focused = ctx.memory(|m| m.has_focus(egui::Id::new("feed_url_input")));
        if !search_focused && !feed_focused {
            if ctx.input(|i| i.key_pressed(egui::Key::J)) {
                self.state.next();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::K)) {
                self.state.prev();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::T)) {
                self.state.toggle_theme(ctx);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::S)) {
                self.state.toggle_star_current();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::R)) {
                self.state.refresh_all_feeds_async();
            }
        }

        self.state.splitting = false;

        if ctx.input(|i| i.pointer.any_click()) {
            if let Some(pos) = ctx.pointer_latest_pos() {
                let in_reader =
                    self.state.reader_rect.is_positive() && self.state.reader_rect.contains(pos);
                if !in_reader {
                    self.state.reader.reclaim_shell_focus();
                }
            }
        }

        // --- Three columns ---
        egui::CentralPanel::default()
            .frame(Frame::new().fill(extreme).inner_margin(Margin::ZERO))
            .show(ctx, |ui| {
                let full = ui.available_rect_before_wrap();
                let h = full.height();
                let total_w = full.width();

                let max_nav =
                    (total_w - LIST_MIN - READER_MIN - 2.0 * SPLIT_HIT).clamp(NAV_MIN, NAV_MAX);
                let nav_w = self.state.nav_width.clamp(NAV_MIN, max_nav);

                let after_nav = (total_w - nav_w - SPLIT_HIT).max(0.0);
                let max_list = (after_nav - READER_MIN - SPLIT_HIT).clamp(LIST_MIN, LIST_MAX);
                let list_w = self.state.list_width.clamp(LIST_MIN, max_list);

                let mut x = full.min.x;

                // --- Nav column ---
                let nav_rect =
                    egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(nav_w, h));
                paint_column_bg(ui, nav_rect, panel_fill, stroke_color);
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(nav_rect), |ui| {
                    column_contents(ui, "导航", |ui| {
                        ui.label(
                            RichText::new(format!("未读：{}", self.state.unread_total)).strong(),
                        );
                        if ui
                            .selectable_label(
                                matches!(self.state.filter, EntryFilter::All),
                                "全部文章",
                            )
                            .clicked()
                        {
                            self.state.set_filter(EntryFilter::All);
                        }
                        if ui
                            .selectable_label(
                                matches!(self.state.filter, EntryFilter::Unread),
                                "仅未读",
                            )
                            .clicked()
                        {
                            self.state.set_filter(EntryFilter::Unread);
                        }
                        if ui
                            .selectable_label(
                                matches!(self.state.filter, EntryFilter::Starred),
                                "星标",
                            )
                            .clicked()
                        {
                            self.state.set_filter(EntryFilter::Starred);
                        }
                        ui.separator();
                        ui.label(RichText::new("订阅").weak());
                        let feed_items: Vec<(glean_core::FeedId, String, bool)> = self
                            .state
                            .feeds
                            .iter()
                            .map(|f| {
                                let sel = matches!(
                                    self.state.filter,
                                    EntryFilter::Feed(id) if id == f.id
                                );
                                let label = format!(
                                    "{} {}",
                                    f.title,
                                    if f.last_error.is_some() { "⚠" } else { "" }
                                );
                                (f.id, label, sel)
                            })
                            .collect();
                        let mut feed_click = None;
                        let mut feed_delete = None;
                        let mut feed_mark_read = None;
                        for (fid, label, selected) in &feed_items {
                            let resp = ui.selectable_label(*selected, label);
                            if resp.clicked() {
                                feed_click = Some(*fid);
                            }
                            resp.context_menu(|ui| {
                                if ui.button("删除订阅").clicked() {
                                    feed_delete = Some(*fid);
                                    ui.close_menu();
                                }
                                if ui.button("标记全部已读").clicked() {
                                    feed_mark_read = Some(*fid);
                                    ui.close_menu();
                                }
                            });
                        }
                        if let Some(id) = feed_click {
                            self.state.set_filter(EntryFilter::Feed(id));
                        }
                        if let Some(id) = feed_delete {
                            self.state.delete_feed(id);
                        }
                        if let Some(id) = feed_mark_read {
                            self.state.mark_all_read(Some(id));
                        }
                    });
                });
                x += nav_w;

                let (hit, dragged) = splitter_drag(ui, ctx, x, full, "split_nav", |pos_x| {
                    self.state.nav_width = (pos_x - full.min.x).clamp(NAV_MIN, max_nav);
                });
                if dragged {
                    self.state.splitting = true;
                }
                x += hit;

                // --- List column ---
                let list_rect =
                    egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(list_w, h));
                paint_column_bg(ui, list_rect, panel_fill, stroke_color);
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(list_rect), |ui| {
                    column_contents(ui, "列表", |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(ui.available_height())
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let current = self.state.selected;
                                let mut clicked = None;
                                for (i, entry) in self.state.entries.iter().enumerate() {
                                    let state = if entry.is_read { "已读" } else { "未读" };
                                    let star = if entry.is_starred { "★" } else { "" };
                                    let label = format!("[{state}]{star} {}", entry.title);
                                    let rich = if entry.is_read {
                                        RichText::new(label).weak()
                                    } else {
                                        RichText::new(label).strong()
                                    };
                                    if ui.selectable_label(Some(i) == current, rich).clicked() {
                                        clicked = Some(i);
                                    }
                                }
                                if let Some(i) = clicked {
                                    self.state.select_index(i);
                                }
                            });
                    });
                });
                x += list_w;

                let list_left_x = full.min.x + nav_w + SPLIT_HIT;
                let (hit, dragged) = splitter_drag(ui, ctx, x, full, "split_list", |pos_x| {
                    self.state.list_width = (pos_x - list_left_x).clamp(LIST_MIN, max_list);
                });
                if dragged {
                    self.state.splitting = true;
                }
                x += hit;

                // --- Reader column ---
                let reader_w = (full.max.x - x).max(READER_MIN.min(full.width() * 0.2));
                let reader_rect =
                    egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(reader_w, h));
                self.state.reader_rect = reader_rect;

                paint_column_bg(
                    ui,
                    reader_rect,
                    Color32::from_gray(if self.state.dark { 30 } else { 245 }),
                    stroke_color,
                );
                #[cfg(not(windows))]
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(reader_rect), |ui| {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new("阅读区 (WebView)").strong());
                            ui.colored_label(
                                Color32::from_rgb(200, 80, 40),
                                "非 Windows：WebView2 未启用。请下载 CI artifact。",
                            );
                        });
                    });
                });
            });

        // --- WebView2 attach (Windows only) ---
        #[cfg(windows)]
        {
            let ppp = ctx.pixels_per_point();
            match self.state.reader.ensure_attached(
                self.state.host_mode,
                self.state.reader_rect,
                ppp,
            ) {
                Ok(()) => {
                    self.state.reader.sync_bounds(self.state.reader_rect, ppp);
                    if !self.primed {
                        if !self.state.entries.is_empty() {
                            self.state.select_index(0);
                        }
                        self.primed = true;
                    }
                }
                Err(e) => {
                    let transient =
                        e.contains("not ready") || e.contains("retry") || e.contains("not found");
                    if !transient {
                        self.state.status = format!("WebView error: {e}");
                    }
                }
            }
        }

        #[cfg(not(windows))]
        if !self.primed {
            if !self.state.entries.is_empty() {
                self.state.select_index(0);
            }
            self.primed = true;
        }

        // Repaint: faster during refresh/split for responsiveness.
        if self.state.splitting || self.state.refresh_rx.is_some() {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.state.reader.shutdown();
    }
}

fn apply_style(ctx: &egui::Context, dark: bool) {
    if dark {
        ctx.set_visuals(egui::Visuals::dark());
    } else {
        ctx.set_visuals(egui::Visuals::light());
    }
}

fn paint_column_bg(ui: &Ui, rect: egui::Rect, fill: Color32, stroke: Color32) {
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, fill);
    painter.line_segment(
        [rect.right_top(), rect.right_bottom()],
        Stroke::new(1.0_f32, stroke),
    );
}

fn column_contents(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.spacing_mut().item_spacing.y = 2.0;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new(title).strong());
    });
    ui.separator();
    ui.add_space(2.0);
    // Give the body the full remaining height.
    add(ui);
}

fn splitter_drag(
    ui: &mut Ui,
    ctx: &egui::Context,
    x: f32,
    full: egui::Rect,
    id: &'static str,
    mut on_drag: impl FnMut(f32),
) -> (f32, bool) {
    let hit = SPLIT_HIT;
    let rect = egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(hit, full.height()));
    let resp = ui.interact(rect, ui.id().with(id), Sense::drag());
    if resp.hovered() || resp.dragged() {
        ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        ui.painter().rect_filled(
            rect,
            0.0,
            Color32::from_rgba_unmultiplied(100, 140, 200, 120),
        );
    } else {
        ui.painter().rect_filled(
            rect,
            0.0,
            Color32::from_rgba_unmultiplied(120, 120, 120, 40),
        );
    }
    let dragging = resp.dragged();
    if dragging {
        if let Some(pos) = ctx.pointer_latest_pos() {
            on_drag(pos.x);
        }
    }
    (hit, dragging)
}
