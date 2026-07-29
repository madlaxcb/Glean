//! HTML sanitization for entry bodies.
//! Defaults: no scripts; remote images policy configurable.

use crate::model::ImagePolicy;

/// Clean feed HTML for local storage / WebView (script disabled there too).
/// Uses the default Block image policy.
pub fn sanitize_html(html: &str) -> String {
    sanitize_html_with_policy(html, ImagePolicy::Block)
}

/// Clean feed HTML with a specific image policy.
pub fn sanitize_html_with_policy(html: &str, policy: ImagePolicy) -> String {
    let mut builder = ammonia::Builder::default();
    match policy {
        ImagePolicy::Block | ImagePolicy::LoadOnDemand => {
            // LoadOnDemand strips at sanitize time; the reader re-renders with
            // Allow on a per-article "显示图片" click (see reader_html).
            builder.rm_tags([
                "img", "picture", "source", "video", "audio", "iframe", "object", "embed", "form",
            ]);
        }
        ImagePolicy::Allow => {
            builder.rm_tags(["video", "audio", "iframe", "object", "embed", "form"]);
            // Keep img, picture, source for image display.
        }
    }
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

    #[test]
    fn allow_policy_keeps_img() {
        let raw = r#"<p>hi</p><img src="https://example.com/a.png"/>"#;
        let out = sanitize_html_with_policy(raw, ImagePolicy::Allow);
        assert!(out.contains("<img"));
        assert!(out.contains("example.com"));
    }

    #[test]
    fn block_policy_strips_img() {
        let raw = r#"<p>hi</p><img src="https://example.com/a.png"/>"#;
        let out = sanitize_html_with_policy(raw, ImagePolicy::Block);
        assert!(!out.contains("<img"));
    }
}
