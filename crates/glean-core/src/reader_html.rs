//! Reader HTML shell (no scripts). Shared by spike and real entries.

/// Build a full reader document. Safe for IsScriptEnabled=false.
/// Always shows title; body may be summary-only for many feeds.
pub fn reader_document(
    title: &str,
    url: Option<&str>,
    author: Option<&str>,
    body_html: &str,
    dark: bool,
) -> String {
    let (bg, fg, muted, link) = if dark {
        ("#1C1C1E", "#F2F2F7", "#8E8E93", "#64D2FF")
    } else {
        ("#F7F7F5", "#1C1C1E", "#6C6C70", "#0A84FF")
    };

    let mut meta_bits = vec!["Glean · 脚本已禁用".to_string()];
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
        body_html.to_string()
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
    font-family: "Segoe UI", "Microsoft YaHei UI", sans-serif; line-height: 1.65; }}
  main {{ max-width: 42rem; margin: 0 auto; padding: 1.25rem 1.5rem 2.5rem; }}
  h1 {{ font-size: 1.45rem; font-weight: 650; margin: 0 0 0.5rem; line-height: 1.35; }}
  .meta {{ color: {muted}; font-size: 0.82rem; margin-bottom: 0.75rem; }}
  .orig {{ margin: 0 0 1.25rem; font-size: 0.92rem; }}
  .orig a {{ color: {link}; }}
  .empty {{ color: {muted}; }}
  p, li {{ font-size: 1rem; }}
  a {{ color: {link}; }}
  img {{ max-width: 100%; height: auto; }}
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
        title = html_escape(title),
        meta = meta,
        link_html = link_html,
        body = body,
        bg = bg,
        fg = fg,
        muted = muted,
        link = link,
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
        let doc = reader_document("Hello", Some("https://ex.com/a"), None, "<p>x</p>", false);
        assert!(doc.contains("<h1>Hello</h1>"));
        assert!(doc.contains("https://ex.com/a"));
        assert!(doc.contains("查看原文"));
        assert!(!doc.to_lowercase().contains("<script"));
    }
}
