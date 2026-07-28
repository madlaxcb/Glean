//! Domain types and message bus contracts for Glean.
//! This crate must never depend on egui, wry, or tauri.

use serde::{Deserialize, Serialize};

/// UI → core commands (spike subset).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppCommand {
    /// Open a sample / cached entry in the reader host.
    OpenEntry { id: u64 },
    /// Cycle to next sample article (spike helper).
    NextSample,
    /// Cycle to previous sample article.
    PrevSample,
    /// Toggle light/dark reader chrome.
    ToggleTheme,
}

/// Core → UI events (spike subset).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AppEvent {
    EntryOpened { id: u64, title: String },
    ThemeChanged { dark: bool },
    Status { message: String },
}

/// Host mode for the WebView reader during M0 spike.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ReaderHostMode {
    /// Child HWND embedded in the main window client area.
    #[default]
    ChildEmbed,
    /// Borderless top-level window that follows the reader rect.
    FollowOverlay,
}

impl ReaderHostMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::ChildEmbed => "H1 Child embed",
            Self::FollowOverlay => "H2 Follow overlay",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::ChildEmbed => Self::FollowOverlay,
            Self::FollowOverlay => Self::ChildEmbed,
        }
    }
}

/// One sample article used only for spike HTML switching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleEntry {
    pub id: u64,
    pub title: String,
    pub html_body: String,
}

impl SampleEntry {
    pub fn catalog() -> Vec<Self> {
        vec![
            Self {
                id: 1,
                title: "Spike #1 — Hello Glean".into(),
                html_body: r#"
                    <h1>Spike Article 1</h1>
                    <p>This is static sanitized-style HTML. Script should be disabled in WebView.</p>
                    <p><a href="https://example.com">External link (should open system browser)</a></p>
                    <p>中文 IME 与焦点请在壳侧搜索框验证，不在此页输入。</p>
                "#
                .into(),
            },
            Self {
                id: 2,
                title: "Spike #2 — Resize / DPI".into(),
                html_body: r#"
                    <h1>Spike Article 2</h1>
                    <p>Drag the splitter and resize the window. Reader rect should stay aligned.</p>
                    <ul>
                      <li>Maximize / restore</li>
                      <li>Minimise then restore</li>
                      <li>Move across DPI monitors</li>
                    </ul>
                "#
                .into(),
            },
            Self {
                id: 3,
                title: "Spike #3 — Memory reuse".into(),
                html_body: r#"
                    <h1>Spike Article 3</h1>
                    <p>Switch entries 50 times. Private bytes must not climb linearly (single WebView instance).</p>
                    <p>Remote images are omitted on purpose (default Block policy later).</p>
                "#
                .into(),
            },
        ]
    }
}

/// Build a full reader document. No inline scripts; safe for IsScriptEnabled=false.
pub fn reader_document(title: &str, body_html: &str, dark: bool) -> String {
    let (bg, fg, muted, link) = if dark {
        ("#1C1C1E", "#F2F2F7", "#8E8E93", "#64D2FF")
    } else {
        ("#F7F7F5", "#1C1C1E", "#6C6C70", "#0A84FF")
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none';"/>
<title>{title}</title>
<style>
  html, body {{ margin: 0; padding: 0; background: {bg}; color: {fg};
    font-family: "Segoe UI", "Microsoft YaHei UI", sans-serif; line-height: 1.6; }}
  main {{ max-width: 42rem; margin: 0 auto; padding: 1.25rem 1.5rem 2rem; }}
  h1 {{ font-size: 1.5rem; font-weight: 600; margin: 0 0 0.75rem; }}
  p, li {{ font-size: 1rem; }}
  a {{ color: {link}; }}
  .meta {{ color: {muted}; font-size: 0.85rem; margin-bottom: 1rem; }}
</style>
</head>
<body>
<main>
  <div class="meta">Glean M0 reader · script disabled · static HTML</div>
  {body}
</main>
</body>
</html>
"#,
        title = html_escape(title),
        bg = bg,
        fg = fg,
        muted = muted,
        link = link,
        body = body_html,
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_catalog_nonempty() {
        assert_eq!(SampleEntry::catalog().len(), 3);
    }

    #[test]
    fn document_has_no_script_tags() {
        let doc = reader_document("t", "<p>hi</p>", false);
        assert!(!doc.to_lowercase().contains("<script"));
    }

    #[test]
    fn host_mode_toggle() {
        assert_eq!(
            ReaderHostMode::ChildEmbed.toggle(),
            ReaderHostMode::FollowOverlay
        );
    }
}
