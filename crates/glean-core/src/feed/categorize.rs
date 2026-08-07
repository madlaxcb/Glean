//! 订阅内容类型自动推断（§导航栏分类）。
//!
//! 依据订阅 URL 的主域名匹配已知平台规则表；未命中默认归为「文章」。
//! 已知主流平台的归类：
//! - 视频：YouTube / Bilibili / Vimeo / Twitch / 抖音 / TikTok 等
//! - 社交媒体：X(Twitter) / 微博 / Instagram / Facebook / Reddit / 小红书 等
//! - 图片：Pixiv / DeviantArt / Flickr / 500px / Unsplash / Pinterest 等
//! - 音乐：SoundCloud / Bandcamp / Spotify / 网易云 / QQ 音乐 / 播客 等
//! - 其余：文章（文本）

use crate::model::FeedCategory;

/// 按订阅 URL 的主机名推断内容分类。解析失败（非法 URL）时回退 Article。
pub fn categorize(url: &str) -> FeedCategory {
    let Some(host) = url::Url::parse(url)
        .ok()
        .map(|u| u.host_str().unwrap_or("").to_string())
    else {
        return FeedCategory::Article;
    };
    let host = host.trim_end_matches('.').to_lowercase();
    if matches_host(&host, VIDEO) {
        FeedCategory::Video
    } else if matches_host(&host, SOCIAL) {
        FeedCategory::Social
    } else if matches_host(&host, IMAGE) {
        FeedCategory::Image
    } else if matches_host(&host, MUSIC) {
        FeedCategory::Music
    } else {
        FeedCategory::Article
    }
}

/// host（小写）是否等于 domain 或以 "." + domain 结尾（支持子域）。
fn matches_host(host: &str, domains: &[&str]) -> bool {
    domains
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// 视频平台。
const VIDEO: &[&str] = &[
    "youtube.com",
    "youtu.be",
    "bilibili.com",
    "b23.tv",
    "vimeo.com",
    "twitch.tv",
    "dailymotion.com",
    "niconico.jp",
    "douyin.com",
    "tiktok.com",
    "huya.com",
    "douyu.com",
    "kuaishou.com",
];

/// 社交媒体（图文）。
const SOCIAL: &[&str] = &[
    "twitter.com",
    "x.com",
    "weibo.com",
    "weibo.cn",
    "instagram.com",
    "facebook.com",
    "threads.net",
    "reddit.com",
    "t.me",
    "bsky.app",
    "mastodon.social",
    "mas.to",
    "xiaohongshu.com",
    "douban.com",
];

/// 图片平台。
const IMAGE: &[&str] = &[
    "pixiv.net",
    "fanbox.cc",
    "deviantart.com",
    "flickr.com",
    "500px.com",
    "unsplash.com",
    "pinterest.com",
    "danbooru.donmai.us",
    "gelbooru.com",
    "artstation.com",
    "behance.net",
    "lofter.com",
];

/// 音乐 / 播客平台。
const MUSIC: &[&str] = &[
    "soundcloud.com",
    "bandcamp.com",
    "spotify.com",
    "music.163.com",
    "y.qq.com",
    "kuwo.cn",
    "kugou.com",
    "podcasts.apple.com",
    "anchor.fm",
    "mixcloud.com",
    "podbean.com",
    "rss.com",
    "xiaoyuzhoufm.com",
    "feedburner.google.com",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(url: &str) -> FeedCategory {
        categorize(url)
    }

    #[test]
    fn video_platforms() {
        assert_eq!(cat("https://www.youtube.com/@user"), FeedCategory::Video);
        assert_eq!(
            cat("https://space.bilibili.com/3428150"),
            FeedCategory::Video
        );
        assert_eq!(cat("https://youtu.be/abc"), FeedCategory::Video);
        assert_eq!(cat("https://www.tiktok.com/@user"), FeedCategory::Video);
    }

    #[test]
    fn social_platforms() {
        assert_eq!(cat("https://x.com/user"), FeedCategory::Social);
        assert_eq!(cat("https://twitter.com/user"), FeedCategory::Social);
        assert_eq!(cat("https://m.weibo.cn/u/123"), FeedCategory::Social);
        assert_eq!(cat("https://www.reddit.com/r/rust/"), FeedCategory::Social);
        assert_eq!(
            cat("https://www.xiaohongshu.com/user"),
            FeedCategory::Social
        );
    }

    #[test]
    fn image_platforms() {
        assert_eq!(cat("https://www.pixiv.net/users/123"), FeedCategory::Image);
        assert_eq!(cat("https://www.flickr.com/photos/x"), FeedCategory::Image);
        assert_eq!(cat("https://unsplash.com/@x"), FeedCategory::Image);
        assert_eq!(cat("https://www.fanbox.cc/@mana"), FeedCategory::Image);
        assert_eq!(cat("https://mana.fanbox.cc/"), FeedCategory::Image);
    }

    #[test]
    fn fantia_is_article() {
        assert_eq!(
            cat("https://fantia.jp/fanclubs/509981"),
            FeedCategory::Article
        );
        assert_eq!(
            cat("https://fantia.jp/posts/3986720"),
            FeedCategory::Article
        );
    }

    #[test]
    fn civitai_is_article() {
        assert_eq!(
            cat("https://civitai.com/user/madlaxcb"),
            FeedCategory::Article
        );
        assert_eq!(
            cat("https://civitai.red/user/madlaxcb/videos"),
            FeedCategory::Article
        );
    }

    #[test]
    fn music_platforms() {
        assert_eq!(cat("https://soundcloud.com/artist"), FeedCategory::Music);
        assert_eq!(cat("https://music.163.com/#/artist"), FeedCategory::Music);
        assert_eq!(
            cat("https://podcasts.apple.com/podcast/x"),
            FeedCategory::Music
        );
    }

    #[test]
    fn unknown_falls_back_to_article() {
        assert_eq!(cat("https://example.com/feed.xml"), FeedCategory::Article);
        assert_eq!(cat("https://github.com/user/repo"), FeedCategory::Article);
        assert_eq!(cat("not a url"), FeedCategory::Article);
        assert_eq!(cat(""), FeedCategory::Article);
    }

    #[test]
    fn as_str_roundtrip() {
        for c in crate::model::FEED_CATEGORIES {
            assert_eq!(FeedCategory::from_str(c.as_str()), c);
        }
        assert_eq!(FeedCategory::from_str("unknown"), FeedCategory::Article);
    }
}
