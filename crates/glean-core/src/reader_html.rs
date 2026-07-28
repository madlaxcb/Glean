//! Reader HTML shell (no scripts). Shared by spike and real entries.

/// Build a full reader document. Safe for IsScriptEnabled=false.
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
  <div class="meta">Glean reader · script disabled · local HTML</div>
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
