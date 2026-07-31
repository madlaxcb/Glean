//! HTTP fetch + RSS/Atom/JSON Feed parse (M1).

mod categorize;
mod discover;
mod fetch;
pub mod parse;
pub mod tier0;

pub use categorize::categorize;
pub use discover::discover_feed_urls;
pub use fetch::{fetch_feed_bytes, FetchResult, HttpClient};
pub use parse::{parse_feed, ParsedEntry, ParsedFeed};
pub use tier0::normalize as normalize_url_tier0;

/// One feed to refresh (produced by service, consumed by background thread).
#[derive(Debug, Clone)]
pub struct RefreshTask {
    pub feed_id: crate::model::FeedId,
    pub url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// 是否走代理抓取（use_proxy = true 时使用设置页配置的代理）。
    pub use_proxy: bool,
}

/// Outcome of refreshing one feed (sent back from background thread).
#[derive(Debug)]
pub enum RefreshOutcome {
    NotModified {
        feed_id: crate::model::FeedId,
    },
    Updated {
        feed_id: crate::model::FeedId,
        parsed: ParsedFeed,
        etag: Option<String>,
        last_modified: Option<String>,
    },
    Error {
        feed_id: crate::model::FeedId,
        error: String,
    },
}

/// Execute one refresh task on a background thread (blocking HTTP + parse).
pub fn run_refresh_task(task: RefreshTask) -> RefreshOutcome {
    let client = HttpClient::default();
    match fetch_feed_bytes(
        &client,
        &task.url,
        task.etag.as_deref(),
        task.last_modified.as_deref(),
    ) {
        Ok(FetchResult::NotModified) => RefreshOutcome::NotModified {
            feed_id: task.feed_id,
        },
        Ok(FetchResult::Body {
            bytes,
            etag,
            last_modified,
            ..
        }) => match parse_feed(&bytes) {
            Ok(parsed) => RefreshOutcome::Updated {
                feed_id: task.feed_id,
                parsed,
                etag,
                last_modified,
            },
            Err(e) => RefreshOutcome::Error {
                feed_id: task.feed_id,
                error: e.to_string(),
            },
        },
        Err(e) => RefreshOutcome::Error {
            feed_id: task.feed_id,
            error: e.to_string(),
        },
    }
}
