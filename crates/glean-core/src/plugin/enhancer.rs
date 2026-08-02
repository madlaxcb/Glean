//! §11.5.13 Enhancer 接口骨架。
//!
//! Enhancer 是 entry 入库前的内容增强钩子：翻译、摘要、嵌入富媒体等。
//! M6 仅定义接口 + Mock 实现，真正的 Translation/Summarize Enhancer 排到 M7+。
//!
//! 设计：
//! - `Enhancer` 是 object-safe trait，可注册多个实现按优先级链式调用。
//! - `HostApi` 由 Host（GleanService）实现，向 Enhancer 提供受控的远程能力
//!   （翻译、摘要等）。Enhancer 不直接接触网络/凭证。
//! - `EntryPatch` 是 Enhancer 对 entry 的局部修改提案，Host 决定是否应用。

use crate::error::Result;
use crate::model::EntryDetail;

/// Host 提供给 Enhancer 的受控能力集。M6 仅声明 M7+ 需要的两个原语。
pub trait HostApi {
    /// 翻译 `text` 到 `target_lang`（如 "zh"）。
    fn call_translation(&self, text: &str, target_lang: &str) -> Result<String>;
    /// 生成 `text` 的摘要。
    fn call_summarize(&self, text: &str) -> Result<String>;
}

/// Enhancer 对 entry 的局部修改提案。`None` 字段表示不修改。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryPatch {
    pub title: Option<String>,
    pub summary: Option<String>,
    pub content_html: Option<String>,
}

/// 内容增强器接口。§11.5.13
///
/// - `id`：稳定标识，用于配置开关 / 去重。
/// - `applies_to`：根据 entry 元数据快速判断是否需要增强（如只对外语标题生效）。
/// - `enhance`：实际增强逻辑，返回 `EntryPatch`。Host 在入库前应用 patch。
pub trait Enhancer {
    fn id(&self) -> &str;
    fn applies_to(&self, entry: &EntryDetail) -> bool;
    fn enhance(&self, entry: &EntryDetail, host: &dyn HostApi) -> Result<EntryPatch>;
}

// ---- M6 Mock 实现（用于接口验证 + 后续 UI/服务层接入测试）----

/// 测试用 HostApi：翻译加前缀，摘要固定字符串。
pub struct MockHostApi;

impl HostApi for MockHostApi {
    fn call_translation(&self, text: &str, target_lang: &str) -> Result<String> {
        Ok(format!("[MOCK-{target_lang}] {text}"))
    }
    fn call_summarize(&self, _text: &str) -> Result<String> {
        Ok("[摘要]".into())
    }
}

/// 测试用 Enhancer：对所有 entry 生效，patch.title 加 "[译]" 前缀。
pub struct MockEnhancer;

impl Enhancer for MockEnhancer {
    fn id(&self) -> &str {
        "mock"
    }
    fn applies_to(&self, _entry: &EntryDetail) -> bool {
        true
    }
    fn enhance(&self, entry: &EntryDetail, _host: &dyn HostApi) -> Result<EntryPatch> {
        Ok(EntryPatch {
            title: Some(format!("[译] {}", entry.summary.title)),
            summary: None,
            content_html: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EntryId, EntrySummary, FeedId};

    fn make_detail(title: &str) -> EntryDetail {
        EntryDetail {
            summary: EntrySummary {
                id: EntryId(1),
                feed_id: Some(FeedId(1)),
                title: title.into(),
                url: None,
                published_at: None,
                is_read: false,
                is_starred: false,
                has_content: false,
                thumbnail_url: None,
            },
            author: None,
            content_html: String::new(),
            extracted_html: String::new(),
            enhancements: Vec::new(),
        }
    }

    #[test]
    fn mock_enhancer_applies_to_all() {
        let e = MockEnhancer;
        let d = make_detail("Hello");
        assert!(e.applies_to(&d));
    }

    #[test]
    fn mock_enhancer_patches_title() {
        let e = MockEnhancer;
        let host = MockHostApi;
        let d = make_detail("Hello");
        let patch = e.enhance(&d, &host).expect("enhance");
        assert_eq!(patch.title.as_deref(), Some("[译] Hello"));
        assert!(patch.summary.is_none());
        assert!(patch.content_html.is_none());
    }

    #[test]
    fn mock_host_api_translation_prefixes() {
        let host = MockHostApi;
        let s = host.call_translation("hi", "zh").expect("translate");
        assert_eq!(s, "[MOCK-zh] hi");
        let s = host.call_summarize("any").expect("summarize");
        assert_eq!(s, "[摘要]");
    }
}
