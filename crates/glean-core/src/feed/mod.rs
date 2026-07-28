//! HTTP fetch + RSS/Atom/JSON Feed parse (M1).

mod fetch;
pub mod parse;

pub use fetch::{fetch_feed_bytes, FetchResult, HttpClient};
pub use parse::{parse_feed, ParsedEntry, ParsedFeed};
