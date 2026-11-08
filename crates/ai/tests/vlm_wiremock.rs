//! 云端 Provider 的失败路径与 schema 修复（wiremock，无真实网络）。

use std::time::Duration;

use serde_json::json;
use visrecall_ai::vlm::{probe, OpenAiCompatible, PROBE_PNG};
use visrecall_domain::providers::{TextProvider, VisionProvider};
use visrecall_domain::AppError;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GOOD_JSON: &str = r#"{
  "summary": "季度营收漏斗图，显示转化率提升",
  "tags": ["营收", "漏斗图"],
  "content_type": "screenshot",
  "primary_color": "orange",
  "chart_type": "funnel",
  "ui_type": "list",
  "entities": ["VisRecall"],
  "latex": "",
  "has_formula": false,
  "confidence": 0.85
}"#;

fn chat_response(content: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{"message": {"role": "assistant", "content": content}}]
    }))
}

fn provider(server: &MockServer) -> OpenAiCompatible {
    OpenAiCompatible::new(
        "vlm-primary",
        format!("{}/v1", server.uri()),
        "test-model",
        Some("sk-test".into()),
    )
}

#[tokio::test]
async fn valid_json_maps_all_fields() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(chat_response(GOOD_JSON))
        .expect(1)
        .mount(&server)
        .await;

    let insight = provider(&server)
        .analyze(PROBE_PNG)
        .await
        .expect("合法返回应成功");
    assert_eq!(insight.primary_color.as_deref(), Some("orange"));
    assert_eq!(insight.chart_type.as_deref(), Some("funnel"));
    assert_eq!(insight.tags.len(), 2);
    assert_eq!(insight.confidence.unwrap(), 0.85);
}

#[tokio::test]
async fn markdown_fenced_json_is_accepted_without_repair() {
    let server = MockServer::start().await;
    let fenced = format!("```json\n{GOOD_JSON}\n```");
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(chat_response(&fenced))
        .expect(1)
        .mount(&server)
        .await;

    let insight = provider(&server).analyze(PROBE_PNG).await.unwrap();
    assert_eq!(insight.chart_type.as_deref(), Some("funnel"));
}

#[tokio::test]
async fn malformed_json_triggers_exactly_one_repair() {
    let server = MockServer::start().await;
    // 第一次返回截断 JSON，第二次合法 —— 用 body 是否含「修复」提示区分
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("上一次的输出"))
        .respond_with(chat_response(GOOD_JSON))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(chat_response(r#"{"summary": "截断"#))
        .expect(1)
        .mount(&server)
        .await;

    let insight = provider(&server).analyze(PROBE_PNG).await.unwrap();
    assert_eq!(insight.summary, "季度营收漏斗图，显示转化率提升");
    server.verify().await;
}

#[tokio::test]
async fn repair_failure_surfaces_schema_violation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(chat_response("还是不是 JSON"))
        .expect(2) // 一次原文 + 一次修复
        .mount(&server)
        .await;

    let err = provider(&server)
        .analyze(PROBE_PNG)
        .await
        .expect_err("两次都不合法必须失败");
    assert!(matches!(err, AppError::SchemaViolation(_)), "err={err:?}");
}

#[tokio::test]
async fn unauthorized_is_a_config_error_and_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
        .expect(1)
        .mount(&server)
        .await;

    let err = provider(&server)
        .analyze(PROBE_PNG)
        .await
        .expect_err("401 必须报错");
    assert!(matches!(err, AppError::ProviderConfig(_)), "err={err:?}");
    assert!(err.is_config_error());
    assert!(!err.is_retryable());
    assert!(!err.to_string().contains("sk-test"), "错误信息不得含密钥");
    server.verify().await;
}

#[tokio::test]
async fn server_error_is_retryable() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1)
        .mount(&server)
        .await;

    let err = provider(&server)
        .analyze(PROBE_PNG)
        .await
        .expect_err("500 必须报错");
    assert!(matches!(err, AppError::Provider(_)), "err={err:?}");
    assert!(err.is_retryable());
}

#[tokio::test]
async fn timeout_is_retryable() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(chat_response(GOOD_JSON).set_delay(Duration::from_millis(600)))
        .mount(&server)
        .await;

    let slow = provider(&server).with_timeout(Duration::from_millis(120));
    let err = slow.analyze(PROBE_PNG).await.expect_err("超时必须报错");
    assert!(matches!(err, AppError::Provider(_)), "err={err:?}");
    assert!(err.to_string().contains("timeout"), "{err}");
}

#[tokio::test]
async fn missing_content_in_response_is_a_provider_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"choices": []})))
        .mount(&server)
        .await;

    let err = provider(&server)
        .analyze(PROBE_PNG)
        .await
        .expect_err("缺少 content 必须报错");
    assert!(matches!(err, AppError::Provider(_)), "err={err:?}");
}

#[tokio::test]
async fn text_summary_uses_text_provider_trait() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("会议"))
        .respond_with(chat_response(
            r#"{"summary": "会议纪要", "tags": ["会议"]}"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let insight = provider(&server)
        .summarize("这是一份会议纪要的原文")
        .await
        .unwrap();
    assert_eq!(insight.summary, "会议纪要");
    assert_eq!(insight.tags, vec!["会议"]);
}

#[tokio::test]
async fn probe_reports_connectivity_and_vision() {
    let server = MockServer::start().await;
    // 视觉请求体含 image_url，文本 ping 不含
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("image_url"))
        .respond_with(chat_response("白色"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(chat_response("pong"))
        .mount(&server)
        .await;

    let outcome = probe(&format!("{}/v1", server.uri()), "test-model", None).await;
    assert!(outcome.ok, "{outcome:?}");
    assert!(outcome.vision, "视觉请求成功时 vision 必须为 true");
    assert!(outcome.latency_ms < 30_000);
    assert!(outcome.message.is_none());
}

#[tokio::test]
async fn probe_marks_vision_false_when_image_request_fails() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_string_contains("image_url"))
        .respond_with(ResponseTemplate::new(400))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(chat_response("pong"))
        .mount(&server)
        .await;

    let outcome = probe(&format!("{}/v1", server.uri()), "test-model", None).await;
    assert!(outcome.ok);
    assert!(!outcome.vision);
    assert!(outcome.message.is_some());
}

#[tokio::test]
async fn probe_reports_failure_when_endpoint_is_unreachable() {
    // 一个必然连不上的地址（未监听的端口）
    let outcome = probe("http://127.0.0.1:1/v1", "m", Some("sk-x".into())).await;
    assert!(!outcome.ok);
    assert!(!outcome.vision);
    assert!(outcome.message.is_some());
    assert!(!outcome.message.unwrap().contains("sk-x"));
}
