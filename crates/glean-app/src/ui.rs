use crate::SpikeState;
use eframe::egui::{self, Color32, Frame, Margin, RichText, Sense, Stroke, Ui, Vec2};
use glean_core::ReaderHostMode;

pub struct SpikeApp {
    state: SpikeState,
    /// Once true, first article has been pushed to the reader.
    primed: bool,
}

impl SpikeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_style(&cc.egui_ctx, false);
        Self {
            state: SpikeState::new(),
            primed: false,
        }
    }
}

impl eframe::App for SpikeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Repaint while dragging splitters so the reader host can track bounds.
        ctx.request_repaint();

        if self.state.dark {
            ctx.set_visuals(egui::Visuals::dark());
        } else {
            ctx.set_visuals(egui::Visuals::light());
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
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
                    self.state.toggle_theme();
                }
                if ui.button("Re-open x1").clicked() {
                    self.state.push_current_to_reader();
                }
                if ui.button("Stress x50").clicked() {
                    for _ in 0..50 {
                        self.state.next();
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(&self.state.status).small());
                });
            });
        });

        egui::TopBottomPanel::bottom("hints").show(ctx, |ui| {
            ui.label(
                RichText::new(
                    "Shortcuts: j/k next/prev | t theme | 1/2 host mode | Esc clear search  |  \
                     Fill docs/spike-ui.md on Windows — CI artifact is not a Pass.",
                )
                .small()
                .weak(),
            );
        });

        if ctx.input(|i| i.key_pressed(egui::Key::J)) {
            self.state.next();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::K)) {
            self.state.prev();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::T)) {
            self.state.toggle_theme();
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
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.state.search.clear();
        }

        egui::CentralPanel::default()
            .frame(Frame::NONE)
            .show(ctx, |ui| {
                let full = ui.available_rect_before_wrap();
                let h = full.height();
                let mut x = full.min.x;

                let nav_w = self.state.nav_width.clamp(120.0, 360.0);
                let nav_rect =
                    egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(nav_w, h));
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(nav_rect), |ui| {
                    frame_panel(ui, "导航", |ui| {
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
                x += splitter(ui, ctx, &mut self.state.nav_width, x, full, "split_nav");

                let list_w = self.state.list_width.clamp(180.0, 520.0);
                let list_rect =
                    egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(list_w, h));
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(list_rect), |ui| {
                    frame_panel(ui, "列表", |ui| {
                        ui.horizontal(|ui| {
                            ui.label("搜索");
                            ui.text_edit_singleline(&mut self.state.search);
                        });
                        ui.separator();
                        egui::ScrollArea::vertical().show(ui, |ui| {
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
                x += splitter(ui, ctx, &mut self.state.list_width, x, full, "split_list");

                let reader_w = (full.max.x - x).max(200.0);
                let reader_rect =
                    egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(reader_w, h));
                self.state.reader_rect = reader_rect;

                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(reader_rect), |ui| {
                    Frame::new()
                        .fill(Color32::from_gray(if self.state.dark { 30 } else { 245 }))
                        .stroke(Stroke::new(1.0, Color32::from_gray(80)))
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| {
                            ui.label(RichText::new("阅读区 (WebView)").strong());
                            #[cfg(windows)]
                            ui.label(
                                RichText::new(format!(
                                    "host={} · rect=({:.0},{:.0},{:.0}x{:.0})",
                                    self.state.host_mode.label(),
                                    reader_rect.min.x,
                                    reader_rect.min.y,
                                    reader_rect.width(),
                                    reader_rect.height()
                                ))
                                .small(),
                            );
                            #[cfg(not(windows))]
                            {
                                ui.colored_label(
                                    Color32::from_rgb(200, 80, 40),
                                    "非 Windows：WebView2 未启用。请下载 CI artifact 或在 Win 上运行。",
                                );
                                ui.separator();
                                ui.label(RichText::new(&self.state.current().title).heading());
                            }
                        });
                });
            });

        #[cfg(windows)]
        {
            let ppp = ctx.pixels_per_point();
            if let Err(e) = self.state.reader.ensure_attached(
                self.state.host_mode,
                self.state.reader_rect,
                ppp,
            ) {
                self.state.status = format!("WebView error: {e}");
            } else {
                self.state
                    .reader
                    .sync_bounds(self.state.reader_rect, ppp);
                if !self.primed {
                    self.state.push_current_to_reader();
                    self.primed = true;
                }
            }
        }

        #[cfg(not(windows))]
        if !self.primed {
            self.state.status =
                "Linux/dev shell only — use Actions artifact glean-spike-windows-x64".into();
            self.primed = true;
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

fn frame_panel(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(ui.visuals().panel_fill)
        .stroke(Stroke::new(1.0, ui.visuals().window_stroke.color))
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.label(RichText::new(title).strong());
            ui.separator();
            add(ui);
        });
}

fn splitter(
    ui: &mut Ui,
    ctx: &egui::Context,
    target_width: &mut f32,
    x: f32,
    full: egui::Rect,
    id: &'static str,
) -> f32 {
    let hit = 6.0;
    let rect = egui::Rect::from_min_size(egui::pos2(x, full.min.y), Vec2::new(hit, full.height()));
    let resp = ui.interact(rect, ui.id().with(id), Sense::drag());
    if resp.hovered() || resp.dragged() {
        ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        ui.painter().rect_filled(
            rect,
            0.0,
            Color32::from_rgba_unmultiplied(100, 140, 200, 80),
        );
    }
    if resp.dragged() {
        *target_width = (*target_width + resp.drag_delta().x).clamp(120.0, 520.0);
    }
    hit
}
