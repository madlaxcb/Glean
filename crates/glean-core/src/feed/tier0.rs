//! Tier 0 站点适配器：内置 URL 规范化（§11.5.2）
//!
//! 对有官方 RSS/Atom 的网站（GitHub releases、YouTube channel）做 URL 重写，
//! ship 在核心代码里，不算插件。规则：
//!
//! - GitHub `https://github.com/{owner}/{repo}` → `https://github.com/{owner}/{repo}/releases.atom`
//!   - 已带 `.atom` 后缀的不再处理
//!   - 只处理恰好两段 path（owner/repo）的形态；更深路径（如 `/releases/tag/x`）不动
//! - YouTube
//!   - `https://www.youtube.com/channel/UCxxxx` → `https://www.youtube.com/feeds/videos.xml?channel_id=UCxxxx`
//!   - YouTube playlist URL 保留原地址，交由后续解析流程处理
//!   - `https://www.youtube.com/@handle` → 需网络请求解析 channel_id（见 `resolve_youtube_handle`）
//!   - 已是 `feeds/videos.xml` 的不再处理
//! - Medium `https://medium.com/{path}` → `https://medium.com/feed/{path}`
//!   - 支持用户 `@handle`、出版物名称、`tag/{topic}`
//!   - 已带 `/feed/` 前缀的不再处理
//!   - 文章路径（2 段且非 tag）不动
//! - GitLab `https://gitlab.com/{group}/{project}` → `https://gitlab.com/{group}/{project}/-/releases.atom`
//!   - 支持嵌套命名空间（group/subgroup/project）
//!   - 已含 `/-/` 分隔符的路径（issues、merge requests 等）不动
//! - Mastodon / 联邦宇宙 `https://{instance}/@user` → `https://{instance}/@user.rss`
//!   - 作为兜底规则，仅在已知 host 未命中时生效
//!   - 已带 `.rss` 后缀或更深路径（具体嘟文）不动
//! - Substack `https://{pub}.substack.com` → `https://{pub}.substack.com/feed`
//!   - 仅处理根路径（无路径或单段），文章路径（`/p/...`）不动
//! - Reddit `https://www.reddit.com/r/{sub}` → `https://www.reddit.com/r/{sub}/.rss`
//!   - 同理 `/user/{name}` → `/user/{name}/.rss`
//!   - 更深路径（comments 等）和已带 `.rss` 的不动
//! - Steam `https://store.steampowered.com/app/{id}` → `https://store.steampowered.com/feeds/news/app/{id}/`
//!   - 支持 `/app/{id}/{name}` 形式
//!   - 已是 feeds 路径或非 app 页面不动
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
        "gitlab.com" => normalize_gitlab(&mut url, raw),
        "medium.com" => normalize_medium(&mut url, raw),
        "reddit.com" | "old.reddit.com" | "new.reddit.com" => normalize_reddit(&mut url, raw),
        "store.steampowered.com" => normalize_steam(&mut url, raw),
        "youtube.com" | "m.youtube.com" => normalize_youtube(&mut url, raw),
        "pixiv.net" => normalize_pixiv(&mut url, raw),
        _ => {
            if normalized_host.ends_with(".substack.com") {
                normalize_substack(&mut url, raw)
            } else {
                normalize_fediverse(&mut url, raw)
            }
        }
    }
}

pub fn is_youtube_handle_url(raw: &str) -> bool {
    let Ok(url) = Url::parse(raw.trim()) else {
        return false;
    };
    let host = url.host_str().unwrap_or("").trim_start_matches("www.");
    let segments: Vec<&str> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    matches!(host, "youtube.com" | "m.youtube.com")
        && segments.len() == 1
        && segments[0].starts_with('@')
        && segments[0].len() > 1
}

pub fn extract_youtube_channel_id(html: &str) -> Option<String> {
    let markers = ["\"channelId\":\"", "\"externalId\":\"", "\"browseId\":\""];
    markers.iter().find_map(|marker| {
        let start = html.find(marker)? + marker.len();
        let end = html[start..].find('"')?;
        let channel_id = &html[start..start + end];
        if channel_id.starts_with("UC")
            && channel_id.len() > 2
            && channel_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            Some(channel_id.to_string())
        } else {
            None
        }
    })
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

/// Medium 官方 RSS 格式：`medium.com/feed/{path}`。
/// 支持 `@handle`（用户）、出版物名称、`tag/{topic}`。
fn normalize_medium(url: &mut Url, raw: &str) -> String {
    let segments: Vec<&str> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    // 已是 feed URL
    if segments.first() == Some(&"feed") {
        return raw.to_string();
    }

    // 1 段（@user 或出版物名）→ feed
    // 2 段且首段是 tag → feed
    let should_normalize = match segments.len() {
        1 => true,
        2 => segments[0] == "tag",
        _ => false,
    };

    if should_normalize {
        url.set_path(&format!("/feed/{}", segments.join("/")));
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }

    raw.to_string()
}

/// GitLab Releases Atom feed：`{project}/-/releases.atom`。
/// 支持嵌套命名空间（group/subgroup/project）。
fn normalize_gitlab(url: &mut Url, raw: &str) -> String {
    let segments: Vec<&str> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    // 已含 GitLab `/-/` 分隔符（issues、merge requests、releases 等）→ 不动
    if segments.iter().any(|s| *s == "-") {
        return raw.to_string();
    }

    // ≥2 段（group/project 或 group/subgroup/project）→ releases.atom
    if segments.len() >= 2 {
        url.set_path(&format!("{}/-/releases.atom", segments.join("/")));
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }

    raw.to_string()
}

/// Reddit：`/r/{sub}` 和 `/user/{name}` → 追加 `.rss`
/// 更深路径（comments 等）和已带 `.rss` 的不动。
fn normalize_reddit(url: &mut Url, raw: &str) -> String {
    let segments: Vec<&str> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    // `/r/{sub}` 或 `/user/{name}`（恰好 2 段）且未已是 .rss → 追加 .rss
    if segments.len() == 2
        && (segments[0] == "r" || segments[0] == "user")
        && !segments[1].ends_with(".rss")
    {
        url.set_path(&format!("/{}/{}/.rss", segments[0], segments[1]));
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }

    raw.to_string()
}

/// Steam：`/app/{id}` 或 `/app/{id}/{name}` → `/feeds/news/app/{id}/`
/// 已是 feeds 路径或非 app 页面不动。
fn normalize_steam(url: &mut Url, raw: &str) -> String {
    let segments: Vec<&str> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    // 已是 feeds 路径
    if segments.first() == Some(&"feeds") {
        return raw.to_string();
    }

    // `/app/{id}` 或 `/app/{id}/{name}`（≥2 段且首段是 app）
    if segments.len() >= 2 && segments[0] == "app" {
        let app_id = segments[1];
        url.set_path(&format!("/feeds/news/app/{}/", app_id));
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }

    raw.to_string()
}

/// Substack：`https://{pub}.substack.com` → `https://{pub}.substack.com/feed`
/// 仅处理根路径（无路径或单段），文章路径（`/p/...`）不动。
fn normalize_substack(url: &mut Url, raw: &str) -> String {
    let segments: Vec<&str> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    // 根路径（无路径或单段）且未已是 /feed → 追加 /feed
    if segments.len() <= 1 && segments.first() != Some(&"feed") {
        url.set_path("/feed");
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }

    raw.to_string()
}

/// 联邦宇宙（Mastodon 等）：`/@username` → `/@username.rss`
/// 作为兜底规则，仅在已知 host 未命中时生效。
fn normalize_fediverse(url: &mut Url, raw: &str) -> String {
    let segments: Vec<&str> = url
        .path_segments()
        .map(|p| p.filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();

    // 单段 `@username` 且未已是 .rss → 追加 .rss
    if segments.len() == 1 && segments[0].starts_with('@') && !segments[0].ends_with(".rss") {
        url.set_path(&format!("/{}.rss", segments[0]));
        url.set_query(None);
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
    fn medium_profile_normalizes_to_feed() {
        assert_eq!(
            normalize("https://medium.com/@user"),
            "https://medium.com/feed/@user"
        );
    }

    #[test]
    fn medium_publication_normalizes_to_feed() {
        assert_eq!(
            normalize("https://medium.com/publication"),
            "https://medium.com/feed/publication"
        );
    }

    #[test]
    fn medium_topic_normalizes_to_feed() {
        assert_eq!(
            normalize("https://medium.com/tag/rust"),
            "https://medium.com/feed/tag/rust"
        );
    }

    #[test]
    fn medium_feed_and_story_urls_are_untouched() {
        let feed = "https://medium.com/feed/@user";
        let story = "https://medium.com/publication/story-title-abc123";
        assert_eq!(normalize(feed), feed);
        assert_eq!(normalize(story), story);
    }

    #[test]
    fn gitlab_project_normalizes_to_releases_atom() {
        assert_eq!(
            normalize("https://gitlab.com/group/project"),
            "https://gitlab.com/group/project/-/releases.atom"
        );
    }

    #[test]
    fn gitlab_nested_project_normalizes_to_releases_atom() {
        assert_eq!(
            normalize("https://gitlab.com/group/subgroup/project"),
            "https://gitlab.com/group/subgroup/project/-/releases.atom"
        );
    }

    #[test]
    fn gitlab_releases_and_deeper_paths_are_untouched() {
        let feed = "https://gitlab.com/group/project/-/releases.atom";
        let issue = "https://gitlab.com/group/project/-/issues/1";
        assert_eq!(normalize(feed), feed);
        assert_eq!(normalize(issue), issue);
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
    fn recognizes_youtube_handle_url() {
        assert!(is_youtube_handle_url("https://www.youtube.com/@somehandle"));
        assert!(!is_youtube_handle_url(
            "https://www.youtube.com/channel/UCxxx"
        ));
    }

    #[test]
    fn extracts_youtube_channel_id_from_page_data() {
        let html = r#"<script>{"browseId":"UC1234567890abcdefghij"}</script>"#;
        assert_eq!(
            extract_youtube_channel_id(html).as_deref(),
            Some("UC1234567890abcdefghij")
        );
    }

    #[test]
    fn youtube_playlist_is_untouched() {
        let u = "https://www.youtube.com/playlist?list=PLxxxxxxxxxxxxxxxxxxxxxxxx";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn youtube_playlist_with_www_is_untouched() {
        let u = "https://m.youtube.com/playlist?list=PLxxxxxxxxxxxxxxxxxxxxxxxx";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn youtube_playlist_without_list_param_untouched() {
        let u = "https://www.youtube.com/playlist";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn youtube_playlist_invalid_prefix_untouched() {
        let u = "https://www.youtube.com/playlist?list=ABxxxx";
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

    // --- Mastodon / 联邦宇宙 ---

    #[test]
    fn mastodon_social_profile_normalizes_to_rss() {
        assert_eq!(
            normalize("https://mastodon.social/@user"),
            "https://mastodon.social/@user.rss"
        );
    }

    #[test]
    fn mastodon_other_instance_profile_normalizes_to_rss() {
        assert_eq!(
            normalize("https://infosec.exchange/@user"),
            "https://infosec.exchange/@user.rss"
        );
    }

    #[test]
    fn mastodon_already_rss_untouched() {
        let u = "https://mastodon.social/@user.rss";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn mastodon_specific_post_untouched() {
        let u = "https://mastodon.social/@user/123456789";
        assert_eq!(normalize(u), u);
    }

    // --- Substack ---

    #[test]
    fn substack_root_normalizes_to_feed() {
        assert_eq!(
            normalize("https://newsletter.substack.com"),
            "https://newsletter.substack.com/feed"
        );
    }

    #[test]
    fn substack_with_trailing_slash_normalizes_to_feed() {
        assert_eq!(
            normalize("https://newsletter.substack.com/"),
            "https://newsletter.substack.com/feed"
        );
    }

    #[test]
    fn substack_already_feed_untouched() {
        let u = "https://newsletter.substack.com/feed";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn substack_article_untouched() {
        let u = "https://newsletter.substack.com/p/some-article";
        assert_eq!(normalize(u), u);
    }

    // --- Reddit ---

    #[test]
    fn reddit_subreddit_normalizes_to_rss() {
        assert_eq!(
            normalize("https://www.reddit.com/r/rust"),
            "https://www.reddit.com/r/rust/.rss"
        );
    }

    #[test]
    fn reddit_user_normalizes_to_rss() {
        assert_eq!(
            normalize("https://www.reddit.com/user/spez"),
            "https://www.reddit.com/user/spez/.rss"
        );
    }

    #[test]
    fn reddit_already_rss_untouched() {
        let u = "https://www.reddit.com/r/rust/.rss";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn reddit_comment_page_untouched() {
        let u = "https://www.reddit.com/r/rust/comments/abc123";
        assert_eq!(normalize(u), u);
    }

    // --- Steam ---

    #[test]
    fn steam_app_page_normalizes_to_news_feed() {
        assert_eq!(
            normalize("https://store.steampowered.com/app/570"),
            "https://store.steampowered.com/feeds/news/app/570/"
        );
    }

    #[test]
    fn steam_app_with_name_normalizes_to_news_feed() {
        assert_eq!(
            normalize("https://store.steampowered.com/app/570/Dota_2"),
            "https://store.steampowered.com/feeds/news/app/570/"
        );
    }

    #[test]
    fn steam_already_feed_untouched() {
        let u = "https://store.steampowered.com/feeds/news/app/570/";
        assert_eq!(normalize(u), u);
    }

    #[test]
    fn steam_non_app_page_untouched() {
        let u = "https://store.steampowered.com/genre/Action";
        assert_eq!(normalize(u), u);
    }
}
