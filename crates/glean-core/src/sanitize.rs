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
    // 保留 class 属性：AI 增强区块（.ai-enhancement/.ai-label/.ai-content）依赖
    // class 应用阅读区样式。class 本身无脚本风险。
    builder.generic_attributes(
        ["class"]
            .into_iter()
            .collect::<std::collections::HashSet<_>>(),
    );
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
            // Keep remote images and the local image-cache custom protocol.
            builder.add_url_schemes(&["data", "glean-img"]);
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

    #[test]
    fn allow_policy_keeps_custom_image_scheme() {
        let input = r#"<img src="glean-img://abc123.jpg">"#;
        let output = sanitize_html_with_policy(input, ImagePolicy::Allow);
        assert!(output.contains(r#"src="glean-img://abc123.jpg""#));
    }

    #[test]
    fn allow_policy_keeps_inlined_image_data() {
        let input = r#"<img src="data:image/jpeg;base64,aGVsbG8=">"#;
        let output = sanitize_html_with_policy(input, ImagePolicy::Allow);
        assert!(output.contains(r#"src="data:image/jpeg;base64,aGVsbG8=""#));
    }

    /// AI 增强区块（摘要/翻译结果）必须能穿透消毒管线：class 保留、文本保留。
    #[test]
    fn enhancement_block_survives_sanitize() {
        let input = r#"<p>原文</p><div class="ai-enhancement"><div class="ai-label">AI 摘要</div><div class="ai-content">第一句。<br>第二句。</div></div>"#;
        let output = sanitize_html_with_policy(input, ImagePolicy::Block);
        assert!(
            output.contains("class=\"ai-enhancement\""),
            "ai-enhancement class 被剥掉: {output}"
        );
        assert!(output.contains("第一句"), "增强文本丢失: {output}");
    }
}
