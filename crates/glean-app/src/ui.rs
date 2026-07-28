use crate::SpikeState;
use eframe::egui::{self, Color32, Frame, Margin, RichText, Sense, Stroke, Ui, Vec2};
use glean_core::ReaderHostMode;

const SPLIT_HIT: f32 = 6.0;
const NAV_MIN: f32 = 120.0;
const NAV_MAX: f32 = 360.0;
const LIST_MIN: f32 = 180.0;
const LIST_MAX: f32 = 520.0;
const READER_MIN: f32 = 240.0;

pub struct SpikeApp {
    state: SpikeState,
    /// Once true, first article has been pushed to the reader.
    primed: bool,
}

impl SpikeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::fonts::install(&cc.egui_ctx);
        apply_style(&cc.egui_ctx, false);
        Self {
            state: SpikeState::new(),
            primed: false,
        }
    }
}

impl eframe::App for SpikeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.state.dark {
            ctx.set_visuals(egui::Visuals::dark());
        } else {
            ctx.set_visuals(egui::Visuals::light());
        }

        let panel_fill = ctx.style().visuals.panel_fill;
        let extreme = ctx.style().visuals.extreme_bg_color;
        let stroke_color = ctx.style().visuals.window_stroke.color;

        // Search lives in the top bar so it is never under the WebView child HWND
        // and is easier to focus for IME tests.
        egui::TopBottomPanel::top("toolbar")
            .frame(
                Frame::new()
                    .fill(panel_fill)
                    .inner_margin(Margin::symmetric(8, 6))
                    .stroke(Stroke::new(1.0_f32, stroke_color)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Glean M0 Spike");
                    ui.separator();
                    if ui
                        .selectable_label(
                            self.state.host_mode == ReaderHostMode::ChildEmbed,
                            "H1 Embed",
                        )
                        .clicked()
                        && self.state.host_mode != ReaderHostMode::ChildEmbed
                    {
                        self.state.toggle_host_mode();
                    }
                    if ui
                        .selectable_label(
                            self.state.host_mode == ReaderHostMode::FollowOverlay,
                            "H2 Overlay",
                        )
                        .clicked()
                        && self.state.host_mode != ReaderHostMode::FollowOverlay
                    {
                        self.state.toggle_host_mode();
                    }
                    ui.separator();
                    if ui.button("Prev (k)").clicked() {
                        self.state.prev();
                    }
                    if ui.button("Next (j)").clicked() {
                        self.state.next();
                    }
                    if ui.button("Theme").clicked() {
                        self.state.toggle_theme(ctx);
                    }
                    if ui.button("Re-open x1").clicked() {
                        self.state.push_current_to_reader();
                    }
                    if ui.button("Stress x50").clicked() {
                        for _ in 0..50 {
                            self.state.next();
                        }
                    }
                    ui.separator();
                    ui.label("搜索");
                    let search_id = egui::Id::new("spike_search");
                    let te = egui::TextEdit::singleline(&mut self.state.search)
                        .id(search_id)
                        .desired_width(180.0)
                        .hint_text("中文 IME…");
                    let search_resp = ui.add(te);
                    if search_resp.clicked() || search_resp.gained_focus() {
                        // WebView keeps Win32 focus; reclaim before TextEdit/IME.
                        self.state.reader.reclaim_shell_focus();
                        search_resp.request_focus();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(&self.state.status).small());
                    });
                });
            });

        egui::TopBottomPanel::bottom("hints")
            .frame(
                Frame::new()
                    .fill(panel_fill)
                    .inner_margin(Margin::symmetric(8, 4)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "j/k 换文 | t 主题 | Esc 清搜索 | 顶栏搜索测 IME | CI 产物 ≠ M0 Pass",
                    )
                    .small()
                    .weak(),
                );
            });

        let search_focused = ctx.memory(|m| m.has_focus(egui::Id::new("spike_search")));
        if !search_focused {
            if ctx.input(|i| i.key_pressed(egui::Key::J)) {
                self.state.next();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::K)) {
                self.state.prev();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::T)) {
                self.state.toggle_theme(ctx);
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Num1))
                && self.state.host_mode != ReaderHostMode::ChildEmbed
            {
                self.state.toggle_host_mode();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Num2))
                && self.state.host_mode != ReaderHostMode::FollowOverlay
            {
                self.state.toggle_host_mode();
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.state.search.clear();
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("spike_search")));
        }

        self.state.splitting = false;

        // Click outside reader → pull keyboard focus off WebView2 back to main HWND.
        if ctx.input(|i| i.pointer.any_click()) {
            if let Some(pos) = ctx.pointer_latest_pos() {
                let in_reader =
                    self.state.reader_rect.is_positive() && self.state.reader_rect.contains(pos);
                if !in_reader {
                    self.state.reader.reclaim_shell_focus();
                }
            }
        }

        egui::CentralPanel::default()
            .frame(Frame::new().fill(extreme).inner_margin(Margin::ZERO))
            .show(ctx, |ui| {
                let full = ui.available_rect_before_wrap();
                let h = full.height();
                let total_w = full.width();

                // Layout uses desired widths clamped for *this frame only*.
                // Never write the clamped value back into state — that permanently
                // shrinks list_width when nav grows and makes the right splitter
                // appear to "follow" the left one.
                let max_nav =
                    (total_w - LIST_MIN - READER_MIN - 2.0 * SPLIT_HIT).clamp(NAV_MIN, NAV_MAX);
                let nav_w = self.state.nav_width.clamp(NAV_MIN, max_nav);

                let after_nav = (total_w - nav_w - SPLIT_HIT).max(0.0);
                let max_list = (after_nav - READER_MIN - SPLIT_HIT).clamp(LIST_MIN, LIST_MAX);
                let list_w = self.state.list_width.clamp(LIST_MIN, max_list);

                let mut x = full.min.x;

                let nav_rect =
                    egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(nav_w, h));
                paint_column_bg(ui, nav_rect, panel_fill, stroke_color);
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(nav_rect), |ui| {
                    column_contents(ui, "导航", |ui| {
                        ui.label("全部未读");
                        ui.label("星标");
                        ui.separator();
                        ui.label(RichText::new("文件夹").weak());
                        ui.label("· 示例");
                        ui.separator();
                        ui.label(RichText::new("订阅（业务未做）").weak());
                    });
                });
                x += nav_w;

                let (hit, dragged) = splitter_drag(ui, ctx, x, full, "split_nav", |pos_x| {
                    // Absolute width from panel left.
                    self.state.nav_width = (pos_x - full.min.x).clamp(NAV_MIN, max_nav);
                });
                if dragged {
                    self.state.splitting = true;
                }
                x += hit;

                let list_rect =
                    egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(list_w, h));
                paint_column_bg(ui, list_rect, panel_fill, stroke_color);
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(list_rect), |ui| {
                    column_contents(ui, "列表", |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let current = self.state.index;
                                let mut clicked = None;
                                for (i, sample) in self.state.samples.iter().enumerate() {
                                    if ui
                                        .selectable_label(
                                            i == current,
                                            format!("{}  {}", sample.id, sample.title),
                                        )
                                        .clicked()
                                    {
                                        clicked = Some(i);
                                    }
                                }
                                if let Some(i) = clicked {
                                    self.state.select(i);
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
                            ui.separator();
                            ui.label(RichText::new(&self.state.current().title).heading());
                        });
                    });
                });
            });

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
                        self.state.push_current_to_reader();
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
            self.state.status =
                "Linux/dev shell only — use Actions artifact glean-spike-windows-x64".into();
            self.primed = true;
        }

        // Only continuous-repaint while dragging splitters so WebView tracks bounds.
        // Full-time request_repaint steals focus and breaks TextEdit / IME.
        if self.state.splitting {
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
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.label(RichText::new(title).strong());
            ui.separator();
            add(ui);
        });
    });
}

/// Returns (hit_width, is_dragging). `on_drag` receives pointer x in points.
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
