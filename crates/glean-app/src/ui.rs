use crate::tray::TrayAction;
use crate::update;
use crate::SpikeState;
use eframe::egui::{self, Color32, Frame, Margin, RichText, Sense, Stroke, Ui, Vec2};
use glean_core::{EntryFilter, FolderId, ImagePolicy, ReaderHostMode};

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
    /// Show error log popup.
    show_errors: bool,
    /// Show settings popup.
    show_settings: bool,
}

impl SpikeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        crate::fonts::install(&cc.egui_ctx);
        apply_style(&cc.egui_ctx, false);
        Self {
            state: SpikeState::new(),
            primed: false,
            show_opml_import: false,
            show_errors: false,
            show_settings: false,
        }
    }
}

impl eframe::App for SpikeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Auto-refresh timer.
        let dt = ctx.input(|i| i.stable_dt);
        self.state.tick_auto_refresh(dt);

        // Poll background refresh every frame.
        self.state.poll_refresh();

        // Poll update-check thread.
        self.state.poll_update_check();

        // Poll tray actions (Windows only).
        while let Some(action) = self.state.tray.poll() {
            match action {
                TrayAction::Show => self.state.show_from_tray(ctx),
                TrayAction::Refresh => self.state.refresh_all_feeds_async(),
                TrayAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }

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
                    // Per-article "显示图片" only in LoadOnDemand mode and not yet shown.
                    if self.state.config.image_policy == ImagePolicy::LoadOnDemand
                        && !self.state.reader_show_images
                        && self.state.open_detail.is_some()
                    {
                        if ui.button("显示图片").clicked() {
                            self.state.show_reader_images();
                        }
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
                    if ui.button("设置").clicked() {
                        self.show_settings = !self.show_settings;
                    }
                    if self.state.tray.is_active() && ui.button("最小化到托盘").clicked() {
                        self.state.hide_to_tray(ctx);
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
                    // Error badge
                    let err_count = self.state.errors.len();
                    if err_count > 0 {
                        let badge = format!("⚠{}", err_count);
                        if ui.button(&badge).clicked() {
                            self.show_errors = !self.show_errors;
                        }
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

        // --- Bottom hints ---
        egui::TopBottomPanel::bottom("hints")
            .frame(
                Frame::new()
                    .fill(panel_fill)
                    .inner_margin(Margin::symmetric(8, 4)),
            )
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("j/k 换文 · r 刷新 · s 星标 · t 主题 · , 设置 · Esc 关闭弹窗")
                        .small()
                        .weak(),
                );
            });

        // --- Keyboard shortcuts ---
        let search_focused = ctx.memory(|m| m.has_focus(egui::Id::new("spike_search")));
        let feed_focused = ctx.memory(|m| m.has_focus(egui::Id::new("feed_url_input")));
        let rename_focused = self.state.rename_feed.is_some();
        if !search_focused && !feed_focused && !rename_focused {
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
            if ctx.input(|i| i.key_pressed(egui::Key::Comma)) {
                self.show_settings = !self.show_settings;
            }
        }
        // Esc closes topmost popup.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.state.rename_feed.is_some() {
                self.state.rename_feed = None;
            } else if self.show_errors {
                self.show_errors = false;
            } else if self.show_opml_import {
                self.show_opml_import = false;
            } else if self.show_settings {
                self.show_settings = false;
            } else if self.state.opml_export.is_some() {
                self.state.opml_export = None;
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
                        self.draw_nav_contents(ui);
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
                        self.draw_list_contents(ui);
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

        // Hide WebView2 when any popup is open so it doesn't occlude.
        let has_popup = self.state.opml_export.is_some()
            || self.show_opml_import
            || self.state.rename_feed.is_some()
            || self.show_errors
            || self.show_settings
            || self.state.update_available.is_some();
        self.state.reader.set_hidden(has_popup);

        // --- Popups (after CentralPanel so they render on top) ---

        // OPML export popup
        if let Some(xml) = self.state.opml_export.clone() {
            let mut close = false;
            let mut copied = false;
            let mut saved = false;
            let mut txt = xml.clone();
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
                let path = rfd::FileDialog::new()
                    .set_file_name("glean-subscriptions.opml")
                    .add_filter("OPML", &["opml", "xml"])
                    .save_file();
                if let Some(path) = path {
                    match std::fs::write(&path, &xml_clone) {
                        Ok(()) => self.state.status = format!("已保存到 {}", path.display()),
                        Err(e) => self.state.status = format!("保存失败: {e}"),
                    }
                }
            }
            if close {
                self.state.opml_export = None;
            }
        }

        // OPML import popup
        if self.show_opml_import {
            let mut do_import_text = false;
            let mut do_import_file = false;
            let mut close = false;
            egui::Window::new("OPML 导入")
                .resizable(true)
                .default_width(520.0)
                .default_height(200.0)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("选择文件…").clicked() {
                            do_import_file = true;
                        }
                        if ui.button("导入文本").clicked() {
                            do_import_text = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("关闭").clicked() {
                                close = true;
                            }
                        });
                    });
                    ui.separator();
                    ui.label("或粘贴 OPML XML：");
                    let te = egui::TextEdit::multiline(&mut self.state.opml_import_input)
                        .desired_width(f32::INFINITY)
                        .desired_rows(4)
                        .code_editor();
                    let resp = ui.add(te);
                    if resp.clicked() || resp.gained_focus() {
                        self.state.reader.reclaim_shell_focus();
                        resp.request_focus();
                    }
                });
            if do_import_file {
                let path = rfd::FileDialog::new()
                    .add_filter("OPML", &["opml", "xml"])
                    .pick_file();
                if let Some(path) = path {
                    match std::fs::read_to_string(&path) {
                        Ok(content) => {
                            self.state.opml_import_input = content;
                            self.state.import_opml();
                        }
                        Err(e) => self.state.status = format!("读取失败: {e}"),
                    }
                }
            }
            if do_import_text {
                self.state.import_opml();
            }
            if close {
                self.show_opml_import = false;
                self.state.opml_import_input.clear();
            }
        }

        // Rename feed popup
        if let Some((feed_id, ref mut title)) = self.state.rename_feed {
            let mut do_rename = false;
            let mut close = false;
            egui::Window::new("重命名订阅")
                .resizable(false)
                .default_width(360.0)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("新名称");
                        let te = egui::TextEdit::singleline(title)
                            .id(egui::Id::new("rename_feed_input"))
                            .desired_width(f32::INFINITY);
                        let resp = ui.add(te);
                        if resp.clicked() || resp.gained_focus() {
                            self.state.reader.reclaim_shell_focus();
                            resp.request_focus();
                        }
                        if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            do_rename = true;
                        }
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("确定").clicked() {
                            do_rename = true;
                        }
                        if ui.button("取消").clicked() {
                            close = true;
                        }
                    });
                });
            if do_rename {
                let t = title.clone();
                self.state.rename_feed(feed_id, t);
                self.state.rename_feed = None;
            }
            if close {
                self.state.rename_feed = None;
            }
        }

        // Error log popup
        if self.show_errors {
            let mut close = false;
            let mut clear = false;
            egui::Window::new("错误日志")
                .resizable(true)
                .default_width(480.0)
                .default_height(320.0)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("清空").clicked() {
                            clear = true;
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
                            for (i, err) in self.state.errors.iter().enumerate().rev() {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!("#{}", i + 1)).small().weak());
                                    ui.label(RichText::new(err).small());
                                });
                                ui.separator();
                            }
                        });
                });
            if clear {
                self.state.errors.clear();
                self.state.status = "错误日志已清空".into();
            }
            if close {
                self.show_errors = false;
            }
        }

        // Settings popup
        if self.show_settings {
            let mut close = false;
            egui::Window::new("设置")
                .resizable(false)
                .default_width(380.0)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.heading("外观");
                    ui.horizontal(|ui| {
                        ui.label("主题");
                        if ui.selectable_label(!self.state.dark, "浅色").clicked()
                            && self.state.dark
                        {
                            self.state.toggle_theme(ctx);
                        }
                        if ui.selectable_label(self.state.dark, "深色").clicked()
                            && !self.state.dark
                        {
                            self.state.toggle_theme(ctx);
                        }
                    });

                    ui.add_space(8.0);
                    ui.heading("阅读");
                    ui.horizontal(|ui| {
                        ui.label("远程图片");
                        let policy = self.state.config.image_policy;
                        if ui
                            .selectable_label(policy == ImagePolicy::Block, "拦截")
                            .clicked()
                            && policy != ImagePolicy::Block
                        {
                            self.state.config.image_policy = ImagePolicy::Block;
                            self.state.sync_config();
                            self.state.save_config();
                        }
                        if ui
                            .selectable_label(policy == ImagePolicy::LoadOnDemand, "按需")
                            .clicked()
                            && policy != ImagePolicy::LoadOnDemand
                        {
                            self.state.config.image_policy = ImagePolicy::LoadOnDemand;
                            self.state.sync_config();
                            self.state.save_config();
                        }
                        if ui
                            .selectable_label(policy == ImagePolicy::Allow, "允许")
                            .clicked()
                            && policy != ImagePolicy::Allow
                        {
                            self.state.config.image_policy = ImagePolicy::Allow;
                            self.state.sync_config();
                            self.state.save_config();
                        }
                    });
                    ui.label(
                        RichText::new("拦截=去图；按需=每篇可点「显示图片」；允许=始终加载")
                            .small()
                            .weak(),
                    );

                    ui.add_space(8.0);
                    ui.heading("布局");
                    ui.horizontal(|ui| {
                        ui.label(format!(
                            "导航 {} · 列表 {}",
                            self.state.nav_width as i32, self.state.list_width as i32
                        ));
                        if ui.button("重置布局").clicked() {
                            self.state.reset_layout();
                        }
                    });

                    ui.add_space(8.0);
                    ui.heading("刷新");
                    ui.horizontal(|ui| {
                        ui.label("全局自动刷新间隔（秒，0=关闭）");
                        let te = egui::TextEdit::singleline(&mut self.state.refresh_interval_input)
                            .id(egui::Id::new("refresh_interval_input"))
                            .desired_width(80.0);
                        let resp = ui.add(te);
                        if resp.clicked() || resp.gained_focus() {
                            self.state.reader.reclaim_shell_focus();
                            resp.request_focus();
                        }
                        if resp.lost_focus() {
                            if let Ok(v) = self.state.refresh_interval_input.parse::<i64>() {
                                self.state.set_global_refresh_interval(v.max(0));
                            } else {
                                // Reset to current config on invalid input.
                                self.state.refresh_interval_input =
                                    self.state.config.refresh_interval_secs.to_string();
                            }
                        }
                    });
                    ui.label(
                        RichText::new("每源可在右键菜单单独设置刷新间隔")
                            .small()
                            .weak(),
                    );

                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("关闭").clicked() {
                                close = true;
                            }
                        });
                    });
                });
            if close {
                self.show_settings = false;
            }
        }

        // Update available popup (V1: prompt only, no auto-install).
        if self.state.update_available.is_some() {
            let mut close = false;
            let mut open_browser = false;
            egui::Window::new("发现新版本")
                .resizable(false)
                .default_width(420.0)
                .collapsible(false)
                .show(ctx, |ui| {
                    let (current, version, url, changelog) = match &self.state.update_available {
                        Some(update::UpdateCheckResult::Available { current, cast }) => (
                            current.clone(),
                            cast.version.clone(),
                            cast.url.clone(),
                            cast.changelog.clone(),
                        ),
                        _ => unreachable!(),
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("✨").strong());
                        ui.label(format!("新版本 {} 已发布（当前 {}）", version, current));
                    });
                    ui.add_space(6.0);
                    if let Some(log) = &changelog {
                        ui.label(RichText::new("更新日志：").weak());
                        egui::ScrollArea::vertical()
                            .max_height(160.0)
                            .show(ui, |ui| {
                                ui.label(log);
                            });
                    } else {
                        ui.label(RichText::new("无更新日志").weak().small());
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("前往下载").clicked() {
                            open_browser = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("稍后").clicked() {
                                close = true;
                            }
                        });
                    });
                    let _ = (current, version, url);
                });
            if open_browser {
                if let Some(update::UpdateCheckResult::Available { cast, .. }) =
                    &self.state.update_available
                {
                    update::open_url(&cast.url);
                }
            }
            if close {
                self.state.update_available = None;
            }
        }

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
        self.state.sync_config();
        self.state.save_config();
        self.state.reader.shutdown();
    }
}

// --- Nav column contents (folder-grouped feed list) ---

impl SpikeApp {
    fn draw_nav_contents(&mut self, ui: &mut Ui) {
        ui.label(RichText::new(format!("未读：{}", self.state.unread_total)).strong());
        if ui
            .selectable_label(matches!(self.state.filter, EntryFilter::All), "全部文章")
            .clicked()
        {
            self.state.set_filter(EntryFilter::All);
        }
        if ui
            .selectable_label(matches!(self.state.filter, EntryFilter::Unread), "仅未读")
            .clicked()
        {
            self.state.set_filter(EntryFilter::Unread);
        }
        if ui
            .selectable_label(matches!(self.state.filter, EntryFilter::Starred), "星标")
            .clicked()
        {
            self.state.set_filter(EntryFilter::Starred);
        }
        if ui
            .selectable_label(matches!(self.state.filter, EntryFilter::Today), "今日")
            .clicked()
        {
            self.state.set_filter(EntryFilter::Today);
        }
        ui.separator();

        // Collect action requests from the closure.
        let mut feed_click = None;
        let mut feed_delete = None;
        let mut feed_mark_read = None;
        let mut feed_rename = None;
        let mut feed_move_folder = None;
        let mut feed_toggle_mute = None;
        let mut do_create_folder = false;

        // Context menu on "订阅" header for creating folders.
        ui.label(RichText::new("订阅").weak());
        ui.menu_button("＋ 新建文件夹", |ui| {
            let te = egui::TextEdit::singleline(&mut self.state.new_folder_input)
                .id(egui::Id::new("new_folder_input"))
                .desired_width(120.0)
                .hint_text("文件夹名");
            let resp = ui.add(te);
            if resp.clicked() || resp.gained_focus() {
                self.state.reader.reclaim_shell_focus();
                resp.request_focus();
            }
            if ui.button("创建").clicked()
                || (resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
            {
                do_create_folder = true;
            }
        });

        // Group feeds by folder.
        let folders = self.state.folders.clone();
        let feeds = self.state.feeds.clone();

        // Feeds without folder first.
        let orphans: Vec<_> = feeds.iter().filter(|f| f.folder_id.is_none()).collect();

        for feed in &orphans {
            let sel = matches!(
                self.state.filter,
                EntryFilter::Feed(id) if id == feed.id
            );
            let error_mark = if feed.last_error.is_some() {
                " ⚠"
            } else {
                ""
            };
            let mute_mark = if feed.muted { "🔇" } else { "" };
            let label = format!("{mute_mark}{}{error_mark}", feed.title);
            let resp = ui.selectable_label(sel, &label);
            if resp.clicked() {
                feed_click = Some(feed.id);
            }
            self.draw_feed_context_menu(
                ui,
                &resp,
                feed,
                &folders,
                &mut feed_delete,
                &mut feed_rename,
                &mut feed_mark_read,
                &mut feed_move_folder,
                &mut feed_toggle_mute,
            );
        }

        // Feeds grouped by folder.
        for folder in &folders {
            let folder_feeds: Vec<_> = feeds
                .iter()
                .filter(|f| f.folder_id == Some(folder.id))
                .collect();
            let _is_empty = folder_feeds.is_empty();
            ui.label(
                RichText::new(format!("📁 {}", folder.name))
                    .small()
                    .strong(),
            );
            for feed in &folder_feeds {
                let sel = matches!(
                    self.state.filter,
                    EntryFilter::Feed(id) if id == feed.id
                );
                let error_mark = if feed.last_error.is_some() {
                    " ⚠"
                } else {
                    ""
                };
                let mute_mark = if feed.muted { "🔇" } else { "" };
                let label = format!("  {mute_mark}{}{error_mark}", feed.title);
                let resp = ui.selectable_label(sel, &label);
                if resp.clicked() {
                    feed_click = Some(feed.id);
                }
                self.draw_feed_context_menu(
                    ui,
                    &resp,
                    feed,
                    &folders,
                    &mut feed_delete,
                    &mut feed_rename,
                    &mut feed_mark_read,
                    &mut feed_move_folder,
                    &mut feed_toggle_mute,
                );
            }
        }

        // Apply actions after the closure borrows are released.
        if let Some(id) = feed_click {
            self.state.set_filter(EntryFilter::Feed(id));
        }
        if let Some(id) = feed_delete {
            self.state.delete_feed(id);
        }
        if let Some(id) = feed_rename {
            if let Some(f) = self.state.feeds.iter().find(|f| f.id == id) {
                self.state.rename_feed = Some((id, f.title.clone()));
            }
        }
        if let Some((feed_id, folder_id)) = feed_move_folder {
            self.state.move_feed_to_folder(feed_id, folder_id);
        }
        if let Some(id) = feed_toggle_mute {
            self.state.toggle_mute_feed(id);
        }
        if let Some(id) = feed_mark_read {
            self.state.mark_all_read(Some(id));
        }
        if do_create_folder {
            let name = self.state.new_folder_input.trim().to_string();
            if !name.is_empty() {
                self.state.create_folder(name);
                self.state.new_folder_input.clear();
            }
        }
    }

    fn draw_feed_context_menu(
        &mut self,
        _ui: &mut Ui,
        resp: &egui::Response,
        feed: &glean_core::Feed,
        folders: &[glean_core::Folder],
        feed_delete: &mut Option<glean_core::FeedId>,
        feed_rename: &mut Option<glean_core::FeedId>,
        feed_mark_read: &mut Option<glean_core::FeedId>,
        feed_move_folder: &mut Option<(glean_core::FeedId, Option<FolderId>)>,
        feed_toggle_mute: &mut Option<glean_core::FeedId>,
    ) {
        resp.context_menu(|ui| {
            if ui.button("重命名").clicked() {
                *feed_rename = Some(feed.id);
                ui.close_menu();
            }
            let mute_label = if feed.muted { "取消静音" } else { "静音" };
            if ui.button(mute_label).clicked() {
                *feed_toggle_mute = Some(feed.id);
                ui.close_menu();
            }
            if ui.button("标记全部已读").clicked() {
                *feed_mark_read = Some(feed.id);
                ui.close_menu();
            }
            if ui.button("删除订阅").clicked() {
                *feed_delete = Some(feed.id);
                ui.close_menu();
            }
            ui.separator();
            // Move to folder sub-menu.
            ui.menu_button("移动到文件夹", |ui| {
                // Option to remove from folder.
                if feed.folder_id.is_some() {
                    if ui.button("（无文件夹）").clicked() {
                        *feed_move_folder = Some((feed.id, None));
                        ui.close_menu();
                    }
                }
                for folder in folders {
                    let already = feed.folder_id == Some(folder.id);
                    if ui
                        .add_enabled(!already, egui::Button::new(&folder.name))
                        .clicked()
                    {
                        *feed_move_folder = Some((feed.id, Some(folder.id)));
                        ui.close_menu();
                    }
                }
                ui.separator();
                let te = egui::TextEdit::singleline(&mut self.state.new_folder_input)
                    .id(egui::Id::new("ctx_new_folder"))
                    .desired_width(100.0)
                    .hint_text("新文件夹");
                let r = ui.add(te);
                if r.clicked() || r.gained_focus() {
                    self.state.reader.reclaim_shell_focus();
                    r.request_focus();
                }
                if ui.button("创建并移入").clicked() {
                    // Will be handled via do_create_folder below; just signal move.
                    *feed_move_folder = Some((feed.id, None)); // placeholder, will create first
                    ui.close_menu();
                }
            });
            // Refresh interval sub-menu.
            ui.menu_button("刷新间隔", |ui| {
                let current = feed.refresh_interval_secs;
                for &secs in &[0, 300, 900, 1800, 3600, 21600] {
                    let label = if secs == 0 {
                        "默认".into()
                    } else {
                        format!("{}分钟", secs / 60)
                    };
                    if ui.selectable_label(current == secs, label).clicked() {
                        self.state.set_feed_refresh_interval(feed.id, secs);
                        ui.close_menu();
                    }
                }
            });
            // Show feed error details.
            if let Some(err) = &feed.last_error {
                ui.separator();
                ui.colored_label(
                    Color32::from_rgb(220, 80, 60),
                    RichText::new(format!("错误: {err}")).small(),
                );
            }
        });
    }

    fn draw_list_contents(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical()
            .max_height(ui.available_height())
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let current = self.state.selected;
                let mut clicked = None;
                for (i, entry) in self.state.entries.iter().enumerate() {
                    let read_mark = if entry.is_read { "已读" } else { "未读" };
                    let star = if entry.is_starred { "★" } else { "" };
                    let cache = if entry.has_content { "" } else { " ⇊" };
                    let label = format!("[{read_mark}]{star}{cache} {}", entry.title);
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
