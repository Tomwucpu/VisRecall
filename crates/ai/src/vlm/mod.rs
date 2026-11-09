//! 云端 Provider 与探测（design §6.5、P3-R03/R11）。
//!
//! 一个模型可选四种通信协议：`openai` / `ollama` 走 `chat/completions`，
//! `anthropic` 走 Messages API，`rest` 把「协议标识」作为请求路径、复用
//! chat/completions 报文。`chat_completions()` 是统一入口，报文在内部转译。
//!
//! 错误分类决定重试（同一模型直连，不做主备降级）：
//! - `401/403` → [`AppError::ProviderConfig`]：**终止重试**，UI 明示「密钥或地址配置错误」；
//! - 连接失败/超时/5xx/429 → [`AppError::Provider`]：同模型退避重试，全失败即提示用户；
//! - 返回体不是合法 JSON / 不符合 schema → 修复一次，仍失败 [`AppError::SchemaViolation`]。
//!
//! 密钥只在内存里用于请求头，绝不进日志与错误消息。

pub mod schema;

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use visrecall_domain::providers::{TextInsight, TextProvider, VisionInsight, VisionProvider};
use visrecall_domain::AppError;

pub use schema::{repair_instruction, validate_text, validate_vision};

/// 请求超时（design §6.5）。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// 1×1 透明 PNG（探测视觉能力用，避免真的传一张截图）。
pub const PROBE_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

/// 图片理解的系统提示词（要求纯 JSON，与 design §4.5 的契约一致）。
pub const VISION_PROMPT: &str =
    "你是截图理解助手。只输出一个 JSON 对象，不要 Markdown 代码块，不要任何解释文字。\
字段与类型：\n\
{\"summary\": \"≤120 字中文摘要\", \"tags\": [\"≤8 个中文短标签，无虚词\"], \
\"content_type\": \"screenshot|photo|diagram|document|other\", \
\"primary_color\": \"red|orange|yellow|green|blue|purple|pink|brown|black|white|gray|multicolor\", \
\"chart_type\": \"bar|line|pie|funnel|table|none\", \
\"ui_type\": \"dialog|settings|list|article|chat|code|other\", \
\"entities\": [\"人名/产品/公司等\"], \"latex\": \"公式 LaTeX，无则空串\", \
\"has_formula\": false, \"confidence\": 0.0}";

/// 文本摘要的系统提示词。
pub const TEXT_PROMPT: &str = "你是文本摘要助手。只输出一个 JSON 对象，不要 Markdown 代码块。\
字段与类型：{\"summary\": \"≤120 字中文摘要\", \"tags\": [\"≤8 个中文短标签\"], \"confidence\": 0.0}";

/// 通信协议（与 `config.json` 里的字符串、界面下拉选项同源）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Protocol {
    #[default]
    OpenAi,
    Anthropic,
    Ollama,
    CustomRest,
}

impl Protocol {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "anthropic" => Self::Anthropic,
            "ollama" => Self::Ollama,
            "rest" => Self::CustomRest,
            _ => Self::OpenAi,
        }
    }
}

/// 云端 Provider（绑定到一个具体模型）。
///
/// 手写 `Debug`：`api_key` 绝不能出现在日志或错误链里（P3-R11）。
#[derive(Clone)]
pub struct OpenAiCompatible {
    slot: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
    protocol: Protocol,
    protocol_id: String,
    client: reqwest::Client,
}

impl std::fmt::Debug for OpenAiCompatible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatible")
            .field("slot", &self.slot)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("protocol", &self.protocol)
            .field("api_key", &self.api_key.as_ref().map(|_| "***"))
            .finish()
    }
}

impl OpenAiCompatible {
    /// `slot` 是稳定标识（`vlm-primary` / `vlm-backup`），写入 `insights.model_provider`。
    pub fn new(
        slot: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            slot: slot.into(),
            base_url: base_url.into(),
            model: model.into(),
            api_key,
            protocol: Protocol::OpenAi,
            protocol_id: String::new(),
            client,
        }
    }

    /// 按配置里的协议标识选择报文与端点；未知值回落到 OpenAI 兼容。
    pub fn with_protocol(mut self, protocol: &str, protocol_id: &str) -> Self {
        self.protocol = Protocol::parse(protocol);
        self.protocol_id = protocol_id.trim().trim_matches('/').to_string();
        self
    }

    /// 覆盖请求超时（测试与「探测超时」场景用；生产走 [`REQUEST_TIMEOUT`]）。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        if let Ok(client) = reqwest::Client::builder().timeout(timeout).build() {
            self.client = client;
        }
        self
    }

    pub fn slot(&self) -> &str {
        &self.slot
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    pub fn has_api_key(&self) -> bool {
        self.api_key.as_deref().is_some_and(|k| !k.is_empty())
    }

    fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.protocol {
            Protocol::Anthropic => format!("{base}/messages"),
            Protocol::CustomRest => format!("{base}/{}", self.protocol_id),
            Protocol::OpenAi | Protocol::Ollama => format!("{base}/chat/completions"),
        }
    }

    /// 统一调用入口：内部把 OpenAI 风格消息转译到目标协议。
    pub async fn chat_completions(
        &self,
        messages: Value,
        max_tokens: u32,
    ) -> Result<String, AppError> {
        match self.protocol {
            Protocol::Anthropic => self.anthropic_messages(messages, max_tokens).await,
            _ => self.openai_chat_completions(messages, max_tokens).await,
        }
    }

    /// `chat/completions` 报文（openai / ollama / rest 共用）。
    async fn openai_chat_completions(
        &self,
        messages: Value,
        max_tokens: u32,
    ) -> Result<String, AppError> {
        let body = json!({
            "model": self.model,
            "messages": messages,
            "temperature": 0,
            "max_tokens": max_tokens,
        });
        let mut request = self.client.post(self.endpoint()).json(&body);
        if let Some(key) = self.api_key.as_deref().filter(|k| !k.is_empty()) {
            request = request.bearer_auth(key);
        }

        let response = request.send().await.map_err(map_send_error)?;
        check_status(response.status())?;

        let payload: Value = response
            .json()
            .await
            .map_err(|e| AppError::Provider(format!("invalid response body: {e}")))?;
        let content = payload["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| {
                AppError::Provider("response has no choices[0].message.content".into())
            })?;
        Ok(content.to_string())
    }

    /// Anthropic Messages API：`system` 独立字段，图片走 base64 source。
    async fn anthropic_messages(
        &self,
        messages: Value,
        max_tokens: u32,
    ) -> Result<String, AppError> {
        let body = anthropic_payload(&messages, &self.model, max_tokens);
        let mut request = self
            .client
            .post(self.endpoint())
            .header("anthropic-version", "2023-06-01")
            .json(&body);
        if let Some(key) = self.api_key.as_deref().filter(|k| !k.is_empty()) {
            request = request.header("x-api-key", key);
        }

        let response = request.send().await.map_err(map_send_error)?;
        check_status(response.status())?;

        let payload: Value = response
            .json()
            .await
            .map_err(|e| AppError::Provider(format!("invalid response body: {e}")))?;
        let content = payload["content"]
            .as_array()
            .and_then(|blocks| blocks.iter().find(|block| block["type"] == "text"))
            .and_then(|block| block["text"].as_str())
            .ok_or_else(|| AppError::Provider("response has no content[].text".into()))?;
        Ok(content.to_string())
    }

    /// 图片理解：一次调用 + schema 校验失败后修复一次。
    pub async fn analyze_image(&self, png: &[u8]) -> Result<VisionInsight, AppError> {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(png);
        let data_url = format!("data:image/png;base64,{encoded}");

        let mut messages = json!([
            {"role": "system", "content": VISION_PROMPT},
            {"role": "user", "content": [
                {"type": "text", "text": "请按 schema 输出这张截图的 JSON。"},
                {"type": "image_url", "image_url": {"url": data_url}}
            ]}
        ]);

        let first = self.chat_completions(messages.clone(), 900).await?;
        match validate_vision(&first) {
            Ok(insight) => Ok(insight),
            Err(err) => {
                tracing::warn!(slot = %self.slot, %err, "vlm schema invalid; repairing once");
                messages
                    .as_array_mut()
                    .expect("messages is an array")
                    .push(json!({"role": "assistant", "content": first}));
                messages
                    .as_array_mut()
                    .expect("messages is an array")
                    .push(json!({"role": "user", "content": repair_instruction(&err.to_string())}));
                let repaired = self.chat_completions(messages, 900).await?;
                validate_vision(&repaired)
            }
        }
    }

    /// 文本摘要：一次调用 + 修复一次。
    pub async fn summarize_text(&self, text: &str) -> Result<TextInsight, AppError> {
        let mut messages = json!([
            {"role": "system", "content": TEXT_PROMPT},
            {"role": "user", "content": text}
        ]);
        let first = self.chat_completions(messages.clone(), 600).await?;
        match validate_text(&first) {
            Ok(insight) => Ok(insight),
            Err(err) => {
                tracing::warn!(slot = %self.slot, %err, "text schema invalid; repairing once");
                messages
                    .as_array_mut()
                    .expect("messages is an array")
                    .push(json!({"role": "assistant", "content": first}));
                messages
                    .as_array_mut()
                    .expect("messages is an array")
                    .push(json!({"role": "user", "content": repair_instruction(&err.to_string())}));
                let repaired = self.chat_completions(messages, 600).await?;
                validate_text(&repaired)
            }
        }
    }
}

fn map_send_error(err: reqwest::Error) -> AppError {
    if err.is_timeout() {
        AppError::Provider(format!("timeout after {}s", REQUEST_TIMEOUT.as_secs()))
    } else {
        AppError::Provider(format!("request failed: {err}"))
    }
}

fn check_status(status: reqwest::StatusCode) -> Result<(), AppError> {
    let code = status.as_u16();
    if code == 401 || code == 403 {
        return Err(AppError::ProviderConfig(format!(
            "HTTP {code} (检查 API Key 与 base URL)"
        )));
    }
    if code == 429 {
        return Err(AppError::Provider("HTTP 429 rate limited".into()));
    }
    if !status.is_success() {
        return Err(AppError::Provider(format!("HTTP {code}")));
    }
    Ok(())
}

/// OpenAI 风格 messages → Anthropic `system` + `messages` 内容块。
fn anthropic_payload(messages: &Value, model: &str, max_tokens: u32) -> Value {
    let mut system: Vec<Value> = Vec::new();
    let mut out: Vec<Value> = Vec::new();

    if let Some(items) = messages.as_array() {
        for item in items {
            let role = item["role"].as_str().unwrap_or("user");
            let content = &item["content"];
            if role == "system" {
                if let Some(text) = content.as_str() {
                    system.push(json!({"type": "text", "text": text}));
                }
                continue;
            }
            let blocks: Vec<Value> = match content {
                Value::String(text) => vec![json!({"type": "text", "text": text})],
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|part| match part["type"].as_str() {
                        Some("text") => part["text"]
                            .as_str()
                            .map(|text| json!({"type": "text", "text": text})),
                        Some("image_url") => {
                            let url = part["image_url"]["url"].as_str()?;
                            let (media_type, data) =
                                url.strip_prefix("data:")?.split_once(";base64,")?;
                            Some(json!({
                                "type": "image",
                                "source": {"type": "base64", "media_type": media_type, "data": data}
                            }))
                        }
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            if !blocks.is_empty() {
                out.push(json!({"role": role, "content": blocks}));
            }
        }
    }

    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "temperature": 0,
        "messages": out,
    });
    // 探测 ping 没有系统提示词；空数组会被 Anthropic 判为非法输入，索性不发。
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }
    body
}

#[async_trait]
impl VisionProvider for OpenAiCompatible {
    fn id(&self) -> &str {
        &self.slot
    }

    async fn analyze(&self, png: &[u8]) -> Result<VisionInsight, AppError> {
        self.analyze_image(png).await
    }
}

#[async_trait]
impl TextProvider for OpenAiCompatible {
    fn id(&self) -> &str {
        &self.slot
    }

    async fn summarize(&self, text: &str) -> Result<TextInsight, AppError> {
        self.summarize_text(text).await
    }
}

/// 探测结果（`model_probe` IPC 的载荷）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// 文本 ping 是否成功（连通性）。
    pub ok: bool,
    pub latency_ms: u32,
    /// 视觉能力：1×1 PNG 请求是否成功。
    pub vision: bool,
    pub message: Option<String>,
}

/// 探测（默认 OpenAI 兼容协议）：文本 ping + 视觉请求，返回延迟与能力。
pub async fn probe(base_url: &str, model: &str, api_key: Option<String>) -> ProbeOutcome {
    probe_with(base_url, model, api_key, "openai", "").await
}

/// 按指定协议探测某个已保存模型；失败信息**不含密钥**。
pub async fn probe_with(
    base_url: &str,
    model: &str,
    api_key: Option<String>,
    protocol: &str,
    protocol_id: &str,
) -> ProbeOutcome {
    let client = OpenAiCompatible::new("probe", base_url, model, api_key)
        .with_protocol(protocol, protocol_id);
    let started = std::time::Instant::now();

    let text_messages = json!([
        {"role": "user", "content": "ping"}
    ]);
    let text_result = client.chat_completions(text_messages, 1).await;
    let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

    match text_result {
        Err(err) => ProbeOutcome {
            ok: false,
            latency_ms,
            vision: false,
            message: Some(err.to_string()),
        },
        Ok(_) => {
            // 视觉探测只要求「模型能接受图片输入」，不校验 JSON schema（1×1 图没有内容可摘要）
            let vision = client
                .chat_completions(
                    json!([{
                        "role": "user",
                        "content": [
                            {"type": "text", "text": "这张图是什么颜色？一个词回答。"},
                            {"type": "image_url", "image_url": {"url": probe_data_url()}}
                        ]
                    }]),
                    8,
                )
                .await
                .is_ok();
            ProbeOutcome {
                ok: true,
                latency_ms,
                vision,
                message: if vision {
                    None
                } else {
                    Some("模型可用，但视觉输入失败（可能不支持图片）".to_string())
                },
            }
        }
    }
}

fn probe_data_url() -> String {
    use base64::Engine as _;
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(PROBE_PNG)
    )
}

/// 把 `VisionInsight` 写回 `insights` 需要的 provider/model 标识。
pub fn model_metadata(provider: &OpenAiCompatible) -> (String, String) {
    (provider.slot().to_string(), provider.model().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_handles_trailing_slash() {
        let provider =
            OpenAiCompatible::new("vlm-primary", "https://api.example.com/v1/", "m", None);
        assert_eq!(
            provider.endpoint(),
            "https://api.example.com/v1/chat/completions"
        );
        let provider =
            OpenAiCompatible::new("vlm-primary", "https://api.example.com/v1", "m", None);
        assert_eq!(
            provider.endpoint(),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn protocol_changes_endpoint() {
        let base = "https://api.anthropic.com/v1";
        let anthropic = OpenAiCompatible::new("vlm-primary", base, "m", None)
            .with_protocol("anthropic", "");
        assert_eq!(anthropic.endpoint(), format!("{base}/messages"));

        let rest = OpenAiCompatible::new("vlm-primary", base, "m", None)
            .with_protocol("rest", "/my-proto/v1/");
        assert_eq!(rest.endpoint(), format!("{base}/my-proto/v1"));

        let ollama = OpenAiCompatible::new("vlm-primary", base, "m", None)
            .with_protocol("ollama", "");
        assert_eq!(ollama.protocol(), Protocol::Ollama);
        assert_eq!(ollama.endpoint(), format!("{base}/chat/completions"));
    }

    #[test]
    fn anthropic_payload_splits_system_and_translates_images() {
        let messages = json!([
            {"role": "system", "content": "只输出 JSON"},
            {"role": "user", "content": [
                {"type": "text", "text": "描述这张图"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
            ]}
        ]);
        let body = anthropic_payload(&messages, "claude-3", 16);
        assert_eq!(body["model"], "claude-3");
        assert_eq!(body["system"][0]["text"], "只输出 JSON");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"][0]["text"], "描述这张图");
        assert_eq!(
            body["messages"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );
        assert_eq!(body["messages"][0]["content"][1]["source"]["data"], "AAAA");
    }

    #[test]
    fn anthropic_ping_omits_empty_system() {
        let body = anthropic_payload(&json!([{"role": "user", "content": "ping"}]), "m", 1);
        assert!(body.get("system").is_none());
        assert_eq!(body["messages"][0]["content"][0]["text"], "ping");
    }

    #[test]
    fn api_key_is_never_printed() {
        let provider =
            OpenAiCompatible::new("vlm-primary", "https://x", "m", Some("sk-secret".into()));
        assert!(provider.has_api_key());
        let debug = format!("{provider:?}");
        assert!(
            !debug.contains("sk-secret"),
            "Debug 输出泄露了密钥：{debug}"
        );
        assert!(debug.contains("***"));
    }

    #[test]
    fn probe_png_is_a_valid_png_header() {
        assert_eq!(&PROBE_PNG[1..4], b"PNG");
    }
}
