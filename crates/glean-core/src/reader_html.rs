//! Reader HTML shell (no scripts). Shared by spike and real entries.
//!
//! Theme is driven by a `data-theme` attribute on <html> and CSS custom
//! properties, so the host can switch themes via `evaluate_script` without
//! reloading the document.

use crate::model::ImagePolicy;
use crate::sanitize::sanitize_html_with_policy;

/// Build a full reader document. Safe for IsScriptEnabled=false.
/// Always shows title; body may be summary-only for many feeds.
#[allow(clippy::too_many_arguments)]
pub fn reader_document(
    title: &str,
    url: Option<&str>,
    author: Option<&str>,
    body_html: &str,
    dark: bool,
    has_content: bool,
    image_policy: ImagePolicy,
    font_size_px: u16,
    line_width_rem: u16,
) -> String {
    let theme_attr = if dark { "dark" } else { "light" };

    let cache_label = if has_content {
        "已缓存".to_string()
    } else {
        "未缓存".to_string()
    };
    let mut meta_bits = vec![format!("Glean · 脚本已禁用 · {cache_label}")];
    if let Some(a) = author {
        if !a.is_empty() {
            meta_bits.push(html_escape(a));
        }
    }
    let meta = meta_bits.join(" · ");

    let link_html = match url {
        Some(u) if !u.is_empty() => format!(
            r#"<p class="orig"><a href="{href}">查看原文</a></p>"#,
            href = html_escape(u)
        ),
        _ => String::new(),
    };

    let body = if body_html.trim().is_empty() {
        r#"<p class="empty">此条目没有缓存正文。需要联网刷新后才能阅读，或使用「查看原文」在浏览器中打开。</p>"#
            .to_string()
    } else {
        // Apply image policy at render time (DB stores raw sanitized HTML with img tags).
        sanitize_html_with_policy(body_html, image_policy)
    };

    // CSP: allow remote img only when policy is Allow.
    // LoadOnDemand strips img at render (like Block); the host re-renders with
    // Allow on a per-article "显示图片" click, which swaps CSP too.
    let img_src = match image_policy {
        ImagePolicy::Block | ImagePolicy::LoadOnDemand => "img-src data: glean-img:;",
        ImagePolicy::Allow => "img-src data: glean-img: https: http:;",
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN" data-theme="{theme_attr}">
<head>
<meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; {img_src} base-uri 'none'; form-action 'none';"/>
<title>{title}</title>
<style>
  html[data-theme="light"] {{ --bg: #F7F7F5; --fg: #1C1C1E; --muted: #6C6C70; --link: #0A84FF; }}
  html[data-theme="dark"]  {{ --bg: #1C1C1E; --fg: #F2F2F7; --muted: #8E8E93; --link: #64D2FF; }}
  html, body {{ margin: 0; padding: 0; background: var(--bg); color: var(--fg);
    font-family: "Segoe UI", "Microsoft YaHei UI", sans-serif; line-height: 1.65;
    font-size: {font_size_px}px;
    transition: background-color 0.15s, color 0.15s; }}
  main {{ max-width: {line_width_rem}rem; margin: 0 auto; padding: 1.25rem 1.5rem 2.5rem; }}
  h1 {{ font-size: 1.45rem; font-weight: 650; margin: 0 0 0.5rem; line-height: 1.35; }}
  .meta {{ color: var(--muted); font-size: 0.82rem; margin-bottom: 0.75rem; }}
  .orig {{ margin: 0 0 1.25rem; font-size: 0.92rem; }}
  .orig a {{ color: var(--link); }}
  .empty {{ color: var(--muted); }}
  p, li {{ font-size: inherit; }}
  a {{ color: var(--link); }}
  img {{ max-width: 100%; height: auto; }}
  .pixiv-page {{ color: var(--muted); font-size: 0.82rem; margin: 1rem 0 0.25rem; }}
  .ai-enhancement {{ margin-top: 2rem; padding: 0.85rem 1rem; border-left: 3px solid var(--link);
    background: color-mix(in srgb, var(--link) 6%, var(--bg)); border-radius: 4px; }}
  .ai-label {{ font-size: 0.8rem; font-weight: 600; color: var(--link); margin: 0 0 0.4rem;
    text-transform: uppercase; letter-spacing: 0.04em; }}
  .ai-content {{ font-size: 0.95rem; line-height: 1.6; }}
</style>
</head>
<body>
<main>
  <h1>{title}</h1>
  <div class="meta">{meta}</div>
  {link_html}
  <div class="body">{body}</div>
</main>
</body>
</html>
"#,
        theme_attr = theme_attr,
        title = html_escape(title),
        meta = meta,
        link_html = link_html,
        body = body,
        img_src = img_src,
        font_size_px = font_size_px,
        line_width_rem = line_width_rem,
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
    fn includes_h1_and_link() {
        let doc = reader_document(
            "Hello",
            Some("https://ex.com/a"),
            None,
            "<p>x</p>",
            false,
            true,
            ImagePolicy::Block,
            16,
            42,
        );
        assert!(doc.contains("<h1>Hello</h1>"));
        assert!(doc.contains("https://ex.com/a"));
        assert!(doc.contains("查看原文"));
        assert!(!doc.to_lowercase().contains("<script"));
    }

    #[test]
    fn has_data_theme_dark() {
        let doc = reader_document(
            "t",
            None,
            None,
            "<p>x</p>",
            true,
            true,
            ImagePolicy::Block,
            16,
            42,
        );
        assert!(doc.contains(r#"data-theme="dark""#));
    }

    #[test]
    fn has_data_theme_light() {
        let doc = reader_document(
            "t",
            None,
            None,
            "<p>x</p>",
            false,
            true,
            ImagePolicy::Block,
            16,
            42,
        );
        assert!(doc.contains(r#"data-theme="light""#));
    }

    #[test]
    fn has_css_variables() {
        let doc = reader_document(
            "t",
            None,
            None,
            "<p>x</p>",
            false,
            true,
            ImagePolicy::Block,
            16,
            42,
        );
        assert!(doc.contains("--bg"));
        assert!(doc.contains("--fg"));
        assert!(doc.contains("var(--bg)"));
    }
}
