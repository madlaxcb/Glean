//! HTML sanitization for entry bodies.
//! Defaults: no scripts; remote images stripped (privacy).

/// Clean feed HTML for local storage / WebView (script disabled there too).
pub fn sanitize_html(html: &str) -> String {
    let mut builder = ammonia::Builder::default();
    // Default Block remote images: drop <img> entirely for M1.
    builder.rm_tags([
        "img", "picture", "source", "video", "audio", "iframe", "object", "embed", "form",
    ]);
    builder.clean(html).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_script_and_img() {
        let raw = r#"<p>hi</p><script>alert(1)</script><img src="https://evil/x.png"/><a href="https://ex.com">x</a>"#;
        let out = sanitize_html(raw);
        assert!(!out.to_lowercase().contains("script"));
        assert!(!out.to_lowercase().contains("<img"));
        assert!(out.contains("hi"));
        assert!(out.contains("ex.com") || out.contains("x"));
    }
}
