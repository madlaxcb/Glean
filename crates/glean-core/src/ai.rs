//! AI 增强客户端：OpenAI 兼容协议（Chat Completions）。
//!
//! 设计与 `extract.rs` 对称：UI 通过 service 准备 `EnhanceTask`，交给 worker 线程
//! 调 `run_enhance_task`，结果经 `EnhanceOutcome` 回传，service 落库 + 发事件。
//!
//! 触发方式：**手动按需**（用户在阅读界面点「摘要」/「翻译」按钮）。
//! 结果存 `entry_enhancements` 表，与原 entry 内容并列展示，不覆盖原文。
//!
//! 凭证：`AiConfig.api_key_cipher` = `plugin::credential::encrypt_secret(api_key)`，
//! 调用时短暂解密，不常驻内存明文。

use crate::error::{CoreError, Result};
use crate::model::{AiConfig, EntryId};
use crate::plugin::credential::decrypt_secret;
use serde::{Deserialize, Serialize};

/// 手动触发的增强动作。需 Serialize/Deserialize 因为 `AppCommand::EnhanceEntry`
/// 内嵌此类型且 `AppCommand` 派生了 serde。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EnhanceAction {
    /// 生成中文摘要（≤3 句）。
    Summarize,
    /// 翻译到目标语言（如 "中文"、"English"）。
    Translate { target_lang: String },
}

impl EnhanceAction {
    /// 落库用的 kind 字符串（`entry_enhancements.kind`）。
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Summarize => "summary",
            Self::Translate { .. } => "translate",
        }
    }
}

/// 后台增强任务：UI prepare → worker 线程 run → service apply。
#[derive(Debug, Clone)]
pub struct EnhanceTask {
    pub entry_id: EntryId,
    pub action: EnhanceAction,
    pub title: String,
    /// 已去标签的纯文本正文（提取自 content_html 或 extracted_html）。
    pub content: String,
}

/// 增强结果。
#[derive(Debug, Clone)]
pub enum EnhanceOutcome {
    Success {
        entry_id: EntryId,
        kind: String,
        result: String,
    },
    Failed {
        entry_id: EntryId,
        kind: String,
        error: String,
    },
}

/// 在 worker 线程运行增强任务。阻塞；UI 应在 `std::thread::spawn` 中调用。
pub fn run_enhance_task(
    client: &reqwest::blocking::Client,
    cfg: &AiConfig,
    task: &EnhanceTask,
) -> EnhanceOutcome {
    let kind = task.action.kind_str().to_string();
    let (system, user) = build_prompt(&task.action, &task.title, &task.content);
    match chat(client, cfg, &system, &user) {
        Ok(text) => EnhanceOutcome::Success {
            entry_id: task.entry_id,
            kind,
            result: text.trim().to_string(),
        },
        Err(e) => EnhanceOutcome::Failed {
            entry_id: task.entry_id,
            kind,
            error: e.to_string(),
        },
    }
}

/// 构造 system/user prompt。摘要与翻译共用模板，区别在 system 指令。
fn build_prompt(action: &EnhanceAction, title: &str, content: &str) -> (String, String) {
    let plain = strip_html(content);
    let user = format!("标题：{title}\n\n正文：\n{plain}");
    let system = match action {
        EnhanceAction::Summarize => {
            "你是一个摘要助手。用简洁的中文总结用户提供的文章，不超过 3 句话，只输出摘要正文，不要前后缀。"
                .to_string()
        }
        EnhanceAction::Translate { target_lang } => {
            format!("你是一个翻译助手。把用户提供的文章翻译成{target_lang}，保留原意，只输出译文，不要前后缀。")
        }
    };
    (system, user)
}

/// 简易去标签：去掉 `<...>`，折叠空白。保留文本节点内容。
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---- OpenAI Chat Completions 协议 ----

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

/// 调用 `{base_url}/chat/completions`，返回第一条回复的 content。
pub fn chat(
    client: &reqwest::blocking::Client,
    cfg: &AiConfig,
    system: &str,
    user: &str,
) -> Result<String> {
    let api_key = decrypt_secret(&cfg.api_key_cipher)?;
    if api_key.is_empty() {
        return Err(CoreError::Message("AI api_key 未配置".into()));
    }
    let base = cfg.base_url.trim_end_matches('/');
    let url = format!("{base}/chat/completions");
    let body = ChatRequest {
        model: &cfg.model,
        messages: vec![
            ChatMessage {
                role: "system",
                content: system,
            },
            ChatMessage {
                role: "user",
                content: user,
            },
        ],
    };
    let body_json = serde_json::to_string(&body)
        .map_err(|e| CoreError::Message(format!("AI request serialize: {e}")))?;
    let resp = client
        .post(&url)
        .bearer_auth(&api_key)
        .header("Content-Type", "application/json")
        .body(body_json)
        .send()
        .map_err(|e| CoreError::Message(format!("AI request: {e}")))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(CoreError::Message(format!("AI HTTP {status}: {text}")));
    }
    let parsed: ChatResponse = serde_json::from_str(&text)
        .map_err(|e| CoreError::Message(format!("AI response parse: {e}: {text}")))?;
    parsed
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .ok_or_else(|| CoreError::Message("AI response has no choices".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_removes_tags_and_collapses_whitespace() {
        let html = "<p>Hello <b>world</b></p>\n<br>\n  <div>第二段</div>";
        let plain = strip_html(html);
        assert_eq!(plain, "Hello world 第二段");
        assert!(!plain.contains('<'));
    }

    #[test]
    fn build_prompt_summarize_uses_chinese_system() {
        let (system, user) = build_prompt(
            &EnhanceAction::Summarize,
            "Test Title",
            "<p>Body text here.</p>",
        );
        assert!(system.contains("摘要"));
        assert!(system.contains("3 句话"));
        assert!(user.contains("标题：Test Title"));
        assert!(user.contains("Body text here."));
        // 去标签后正文不应包含 HTML
        assert!(!user.contains("<p>"));
    }

    #[test]
    fn build_prompt_translate_includes_target_lang() {
        let (system, _user) = build_prompt(
            &EnhanceAction::Translate {
                target_lang: "English".into(),
            },
            "标题",
            "<p>正文</p>",
        );
        assert!(system.contains("English"));
        assert!(system.contains("翻译"));
    }

    #[test]
    fn kind_str_distinct_per_action() {
        assert_eq!(EnhanceAction::Summarize.kind_str(), "summary");
        assert_eq!(
            EnhanceAction::Translate {
                target_lang: "x".into()
            }
            .kind_str(),
            "translate"
        );
    }

    /// chat() 在 api_key 为空时应直接报错，不发网络请求。
    #[test]
    fn chat_rejects_empty_api_key() {
        let cfg = AiConfig {
            base_url: "https://example.invalid/v1".into(),
            model: "m".into(),
            api_key_cipher: String::new(), // 未配置
        };
        let client = reqwest::blocking::Client::new();
        let err = chat(&client, &cfg, "sys", "usr").unwrap_err();
        assert!(err.to_string().contains("未配置"));
    }

    /// run_enhance_task 在 api_key 未配置时返回 Failed，不 panic。
    #[test]
    fn run_enhance_task_fails_cleanly_without_api_key() {
        let cfg = AiConfig {
            base_url: "https://example.invalid/v1".into(),
            model: "m".into(),
            api_key_cipher: String::new(),
        };
        let client = reqwest::blocking::Client::new();
        let task = EnhanceTask {
            entry_id: EntryId(1),
            action: EnhanceAction::Summarize,
            title: "T".into(),
            content: "<p>body</p>".into(),
        };
        match run_enhance_task(&client, &cfg, &task) {
            EnhanceOutcome::Failed { kind, error, .. } => {
                assert_eq!(kind, "summary");
                assert!(error.contains("未配置"));
            }
            _ => panic!("expected Failed"),
        }
    }

    /// 端到端：调真实 OpenAI 兼容端点。手动跑：
    /// `cargo test -p glean-core ai::tests::chat_end_to_end -- --ignored`
    ///
    /// 需先在 AppConfig 配置好 `ai` 字段（base_url + model + api_key_cipher）。
    /// 这里临时用环境变量 `GLEAN_AI_BASE_URL` / `GLEAN_AI_KEY` / `GLEAN_AI_MODEL`。
    #[test]
    #[ignore = "需联网 + 真实 OpenAI 兼容 API key（环境变量）"]
    fn chat_end_to_end() {
        let base = std::env::var("GLEAN_AI_BASE_URL").expect("GLEAN_AI_BASE_URL");
        let key = std::env::var("GLEAN_AI_KEY").expect("GLEAN_AI_KEY");
        let model = std::env::var("GLEAN_AI_MODEL").expect("GLEAN_AI_MODEL");
        let cfg = AiConfig {
            base_url: base,
            model,
            api_key_cipher: crate::plugin::credential::encrypt_secret(&key).unwrap(),
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap();
        let out = chat(&client, &cfg, "你只能说「好」", "测试").expect("chat ok");
        assert!(!out.trim().is_empty());
    }
}
