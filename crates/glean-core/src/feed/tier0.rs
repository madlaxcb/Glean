//! Tier 0 站点适配器：内置 URL 规范化（§11.5.2）
//!
//! 对有官方 RSS/Atom 的网站（GitHub releases、YouTube channel）做 URL 重写，
//! ship 在核心代码里，不算插件。规则：
//!
//! - GitHub `https://github.com/{owner}/{repo}` → `https://github.com/{owner}/{repo}/releases.atom`
//!   - 已带 `.atom` 后缀的不再处理
//!   - 只处理恰好两段 path（owner/repo）的形态；更深路径（如 `/releases/tag/x`）不动
//! - YouTube `https://www.youtube.com/channel/UCxxxx` → `https://www.youtube.com/feeds/videos.xml?channel_id=UCxxxx`
//!   - 已是 `feeds/videos.xml` 的不再处理
//!
//! 输入未通过 scheme/host 校验时原样返回（不报错），让上层流程继续走通用发现逻辑。

use url::Url;

/// 应用 Tier 0 URL 规范化。返回规范化后的 URL 字符串。
///
/// 输入无法解析为 URL 时，原样返回输入。
pub fn normalize(raw: &str) -> String {
    // 清理用户粘贴的 markdown 反引号链接（`` `https://…` ``）与首尾空白，
    // 否则 Url::parse 失败、插件路由/请求全部 miss（曾导致「刷新该贴」走到 RSS）。
    let trimmed = raw.trim_matches(|c: char| c == '`' || c.is_whitespace());
    let Ok(mut url) = Url::parse(trimmed) else {
        return raw.to_string();
    };
    // 仅处理 http/https
    if !matches!(url.scheme(), "http" | "https") {
        return raw.to_string();
    }
    // 路由匹配时忽略 www. 前缀，但不修改原 host（保留用户输入形态）。
    let host = url.host_str().unwrap_or("");
    let normalized_host = host.trim_start_matches("www.");

    match normalized_host {
        "github.com" => normalize_github(&mut url, raw),
        "youtube.com" | "m.youtube.com" => normalize_youtube(&mut url, raw),
        "pixiv.net" => normalize_pixiv(&mut url, raw),
        _ => raw.to_string(),
    }
}

fn normalize_github(url: &mut Url, raw: &str) -> String {
    let segments: Vec<&str> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    // 只规范化 `github.com/{owner}/{repo}`（恰好两段），且未已是 .atom
    if segments.len() == 2 && !segments[1].ends_with(".atom") {
        url.set_path(&format!("/{}/{}/releases.atom", segments[0], segments[1]));
        url.set_fragment(None);
        return url.to_string();
    }
    raw.to_string()
}

fn normalize_youtube(url: &mut Url, raw: &str) -> String {
    let segments: Vec<String> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).map(|s| s.to_string()).collect())
        .unwrap_or_default();
    // `youtube.com/channel/UCxxxx` → `feeds/videos.xml?channel_id=UCxxxx`
    if segments.len() == 2 && segments[0] == "channel" {
        let channel_id = &segments[1];
        if channel_id.starts_with("UC") {
            url.set_path("/feeds/videos.xml");
            url.set_query(Some(&format!("channel_id={channel_id}")));
            url.set_fragment(None);
            return url.to_string();
        }
    }
    raw.to_string()
}

/// pixiv 用户主页：OPML 常导出单数 `pixiv.net/user/{id}`，而插件匹配
/// 复数 `pixiv.net/users/*`。归一化为复数形态，保证导入订阅刷新时
/// 能命中 Pixiv 插件（否则 RSS 直抓会 404）。
fn normalize_pixiv(url: &mut Url, raw: &str) -> String {
    let segments: Vec<String> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).map(|s| s.to_string()).collect())
        .unwrap_or_default();
    if segments.len() == 2 && segments[0] == "user" {
        url.set_path(&format!("/users/{}", segments[1]));
        url.set_fragment(None);
        return url.to_string();
    }
    raw.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_owner_repo_gets_releases_atom() {
        assert_eq!(
            normalize("https://github.com/owner/repo"),
            "https://github.com/owner/repo/releases.atom"
        );
    }

    #[test]
    fn github_with_www_normalizes() {
        // 保留用户输入的 www. 前缀，只追加 /releases.atom
        assert_eq!(
            normalize("https://www.github.com/owner/repo"),
            "https://www.github.com/owner/repo/releases.atom"
        );
    }

    #[test]
    fn github_already_atom_untouched() {
        let u = "https://github.com/owner/repo/releases.atom";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn github_trailing_slash_normalizes() {
        assert_eq!(
            normalize("https://github.com/owner/repo/"),
            "https://github.com/owner/repo/releases.atom"
        );
    }

    #[test]
    fn github_deeper_path_untouched() {
        let u = "https://github.com/owner/repo/issues/1";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn github_three_segments_untouched() {
        let u = "https://github.com/owner/repo/releases";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn youtube_channel_normalizes() {
        assert_eq!(
            normalize("https://www.youtube.com/channel/UCxxxxxxxxxxxxxxxxxxxxxxxx"),
            "https://www.youtube.com/feeds/videos.xml?channel_id=UCxxxxxxxxxxxxxxxxxxxxxxxx"
        );
    }

    #[test]
    fn youtube_already_feed_untouched() {
        let u = "https://www.youtube.com/feeds/videos.xml?channel_id=UCxxx";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn pixiv_user_singular_normalizes_to_plural() {
        assert_eq!(
            normalize("https://www.pixiv.net/user/8252709"),
            "https://www.pixiv.net/users/8252709"
        );
    }

    #[test]
    fn pixiv_url_wrapped_in_markdown_backticks_is_cleaned() {
        // 用户粘贴 `` `https://…` `` 形式的 markdown 链接：反引号必须被
        // 剥离，否则 Url::parse 失败、插件路由 miss（曾导致「刷新该贴」
        // 走到 RSS 抓到登录页）。
        assert_eq!(
            normalize("`https://www.pixiv.net/user/8252709`"),
            "https://www.pixiv.net/users/8252709"
        );
    }

    #[test]
    fn youtube_handle_untouched() {
        // @handle 不在 Tier 0 规则内（无法静态解析为 channel_id）
        let u = "https://www.youtube.com/@somehandle";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn pixiv_singular_user_normalized_to_plural() {
        assert_eq!(
            normalize("https://www.pixiv.net/user/112404013"),
            "https://www.pixiv.net/users/112404013"
        );
    }

    #[test]
    fn pixiv_plural_users_untouched() {
        let u = "https://www.pixiv.net/users/112404013";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn pixiv_deeper_path_untouched() {
        let u = "https://www.pixiv.net/user/112404013/illustrations";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn unknown_host_untouched() {
        let u = "https://example.com/feed.xml";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn non_http_untouched() {
        let u = "file:///tmp/feed.xml";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn unparseable_returned_as_is() {
        let u = "not a url at all";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn preserves_http_when_input_is_http() {
        assert_eq!(
            normalize("http://github.com/owner/repo"),
            "http://github.com/owner/repo/releases.atom"
        );
    }
}
