//! System tray icon (Windows only).
//!
//! Provides minimize-to-tray UX: a toolbar button hides the main window;
//! the tray icon's left-click and "显示" menu item restore it. Right-click
//! menu also offers 刷新 and 退出.
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
    use std::sync::mpsc::Sender;
    use std::sync::{Arc, Mutex};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{
        Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    };

    pub struct TrayInner {
        /// Keep the tray icon alive for the process lifetime.
        _tray: TrayIcon,
        rx: std::sync::mpsc::Receiver<TrayAction>,
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

            // Menu clicks → channel.
            let menu_tx = Arc::clone(&shared);
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                let action = if event.id == show_id {
                    TrayAction::Show
                } else if event.id == refresh_id {
                    TrayAction::Refresh
                } else if event.id == quit_id {
                    TrayAction::Quit
                } else {
                    return;
                };
                if let Ok(s) = menu_tx.lock() {
                    let _ = s.send(action);
                }
            }));

            // Tray left-click → Show.
            let tray_tx = Arc::clone(&shared);
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    if let Ok(s) = tray_tx.lock() {
                        let _ = s.send(TrayAction::Show);
                    }
                }
            }));

            let tray = TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip("Glean / 拾光")
                .with_icon(icon)
                .build()
                .ok()?;

            Some(Self { _tray: tray, rx })
        }

        pub fn poll(&self) -> Option<TrayAction> {
            self.rx.try_recv().ok()
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
                    // transparent
                    rgba[idx..idx + 4].copy_from_slice(&[0, 0, 0, 0]);
                } else if dist >= r_inner {
                    // white ring
                    rgba[idx..idx + 4].copy_from_slice(&[245, 245, 245, 255]);
                } else {
                    // blue center
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

    /// Whether the tray is active (used to show/hide the "最小化到托盘" button).
    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }
}
