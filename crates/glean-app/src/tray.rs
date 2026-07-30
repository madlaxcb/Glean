//! System tray icon (Windows only).
//!
//! Provides minimize-to-tray UX: a toolbar button hides the main window;
//! the tray icon's left-click, double-click and "显示" menu item restore it.
//! Right-click menu also offers 刷新 and 退出.
//!
//! Critical: when the main window is hidden, `ctx.request_repaint()` is a
//! no-op because winit's `request_redraw` calls `RedrawWindow(RDW_INTERNALPAINT)`
//! which is **ignored for invisible windows** (per MSDN). So the egui event
//! loop never calls `update()`, and channel-based tray polling can't work.
//!
//! Fix: tray event callbacks directly call Win32 `ShowWindow(SW_RESTORE)` (for
//! Show/Refresh) or `PostMessage(WM_CLOSE)` (for Quit) to wake the event loop.
//! They also hold an `egui::Context` clone to sync viewport visibility state.
//!
//! Linux is stubbed (`Tray::new` returns `None`) to keep `cargo check` free
//! of GTK/AppIndicator deps.

/// Action emitted by the tray (polled on the UI thread each frame).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    Refresh,
    Quit,
}

#[cfg(windows)]
mod imp {
    use super::TrayAction;
    use crate::reader;
    use std::sync::{mpsc::Sender, Arc, Mutex};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, TrayIcon, TrayIconBuilder, TrayIconEvent};

    /// Cell holding the egui context, set after app creation.
    type CtxCell = Arc<Mutex<Option<egui::Context>>>;

    pub struct TrayInner {
        /// Keep the tray icon alive for the process lifetime.
        _tray: TrayIcon,
        rx: std::sync::mpsc::Receiver<TrayAction>,
        ctx_cell: CtxCell,
    }

    impl TrayInner {
        pub fn new() -> Option<Self> {
            let icon = build_icon()?;

            let menu = Menu::new();
            let show = MenuItem::new("显示", true, None);
            let refresh = MenuItem::new("刷新", true, None);
            let quit = MenuItem::new("退出", true, None);
            menu.append(&show).ok()?;
            menu.append(&refresh).ok()?;
            menu.append(&PredefinedMenuItem::separator()).ok()?;
            menu.append(&quit).ok()?;

            let show_id = show.id().clone();
            let refresh_id = refresh.id().clone();
            let quit_id = quit.id().clone();

            let (tx, rx) = std::sync::mpsc::channel::<TrayAction>();
            let shared: Arc<Mutex<Sender<TrayAction>>> = Arc::new(Mutex::new(tx));
            let ctx_cell: CtxCell = Arc::new(Mutex::new(None));

            // Menu clicks → restore window + channel.
            let menu_tx = Arc::clone(&shared);
            let menu_ctx = Arc::clone(&ctx_cell);
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let (action, is_quit) = if event.id == show_id {
                    (TrayAction::Show, false)
                } else if event.id == refresh_id {
                    (TrayAction::Refresh, false)
                } else if event.id == quit_id {
                    (TrayAction::Quit, true)
                } else {
                    return;
                };
                if is_quit {
                    // PostMessage(WM_CLOSE) works for hidden windows (unlike
                    // RedrawWindow which is ignored). Triggers CloseRequested
                    // → eframe on_exit → save config + shutdown.
                    post_close_to_main_window();
                    if let Some(ctx) = menu_ctx.lock().unwrap().as_ref() {
                        ctx.request_repaint();
                    }
                } else {
                    // Show/Refresh: must make window visible to wake the loop.
                    restore_main_window();
                    if let Some(ctx) = menu_ctx.lock().unwrap().as_ref() {
                        if action == TrayAction::Show {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        }
                        ctx.request_repaint();
                    }
                    if let Ok(s) = menu_tx.lock() {
                        let _ = s.send(action);
                    }
                }
            }));

            // Tray click/double-click → Show.
            let tray_tx = Arc::clone(&shared);
            let tray_ctx = Arc::clone(&ctx_cell);
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                let is_show = match &event {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: tray_icon::MouseButtonState::Up,
                        ..
                    } => true,
                    TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => true,
                    _ => false,
                };
                if !is_show {
                    return;
                }
                // Restore window directly via Win32 (bypasses the broken
                // request_repaint path for hidden windows).
                restore_main_window();
                if let Some(ctx) = tray_ctx.lock().unwrap().as_ref() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.request_repaint();
                }
                if let Ok(s) = tray_tx.lock() {
                    let _ = s.send(TrayAction::Show);
                }
            }));

            let tray = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("Glean / 拾光")
                .with_icon(icon)
                .build()
                .ok()?;

            Some(Self {
                _tray: tray,
                rx,
                ctx_cell,
            })
        }

        pub fn poll(&self) -> Option<TrayAction> {
            self.rx.try_recv().ok()
        }

        pub fn set_egui_ctx(&self, ctx: egui::Context) {
            *self.ctx_cell.lock().unwrap() = Some(ctx);
        }
    }

    /// Find the main Glean window and restore it from hidden/minimized state.
    /// This generates Win32 events that wake the winit event loop, allowing
    /// `update()` to run (which `ctx.request_repaint()` cannot do for hidden
    /// windows because `RedrawWindow(RDW_INTERNALPAINT)` is ignored).
    fn restore_main_window() {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetForegroundWindow, ShowWindow, SW_RESTORE,
        };
        if let Some(hwnd) = reader::find_main_hwnd() {
            unsafe {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }

    /// Post WM_CLOSE to the main window. This works for hidden windows (unlike
    /// repaint-based wakeups) and triggers the normal close flow:
    /// winit CloseRequested → eframe on_exit → save + shutdown.
    fn post_close_to_main_window() {
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
        const WM_CLOSE: u32 = 0x0010;
        if let Some(hwnd) = reader::find_main_hwnd() {
            unsafe {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// Build a 32×32 RGBA icon: blue rounded square with a white ring.
    fn build_icon() -> Option<Icon> {
        let size: u32 = 32;
        let mut rgba = vec![0u8; (size * size * 4) as usize];
        let cx = (size / 2) as f32;
        let cy = (size / 2) as f32;
        let r_outer = 14.0_f32;
        let r_inner = 9.5_f32;
        for y in 0..size {
            for x in 0..size {
                let xf = x as f32 + 0.5;
                let yf = y as f32 + 0.5;
                let dx = xf - cx;
                let dy = yf - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let idx = ((y * size + x) * 4) as usize;
                if dist > r_outer {
                    rgba[idx..idx + 4].copy_from_slice(&[0, 0, 0, 0]);
                } else if dist >= r_inner {
                    rgba[idx..idx + 4].copy_from_slice(&[245, 245, 245, 255]);
                } else {
                    rgba[idx..idx + 4].copy_from_slice(&[60, 130, 210, 255]);
                }
            }
        }
        Icon::from_rgba(rgba, size, size).ok()
    }
}

#[cfg(not(windows))]
mod imp {
    use super::TrayAction;

    pub struct TrayInner;

    impl TrayInner {
        pub fn new() -> Option<Self> {
            None
        }

        pub fn poll(&self) -> Option<TrayAction> {
            None
        }

        pub fn set_egui_ctx(&self, _ctx: egui::Context) {}
    }
}

pub struct Tray {
    inner: Option<imp::TrayInner>,
}

impl Tray {
    /// Create the tray icon. Returns `Tray { inner: None }` on platforms
    /// without tray support (or if the tray failed to initialise).
    pub fn new() -> Self {
        Self {
            inner: imp::TrayInner::new(),
        }
    }

    pub fn poll(&self) -> Option<TrayAction> {
        self.inner.as_ref().and_then(|t| t.poll())
    }

    /// Set the egui context so tray callbacks can directly drive viewport
    /// commands and request repaints. Must be called after `SpikeApp::new`
    /// receives the creation context.
    pub fn set_egui_ctx(&self, ctx: egui::Context) {
        if let Some(inner) = self.inner.as_ref() {
            inner.set_egui_ctx(ctx);
        }
    }

    /// Whether the tray is active (used to show/hide the "最小化到托盘" button).
    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }
}
