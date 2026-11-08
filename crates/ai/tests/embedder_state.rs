//! 不需要模型文件的嵌入 Provider 状态机用例（CI 也能跑）。

use visrecall_ai::embedder::BgeEmbedder;
use visrecall_domain::providers::EmbeddingProvider;
use visrecall_domain::AppError;

#[test]
fn starts_not_ready() {
    let embedder = BgeEmbedder::new("Z:/definitely/missing");
    assert!(!embedder.ready());
}

#[test]
fn missing_model_errors_without_panicking() {
    let embedder = BgeEmbedder::new("Z:/definitely/missing");
    let err = embedder.warm_up().expect_err("模型缺失必须报错");
    assert!(
        matches!(err, AppError::FileMissing(_)),
        "期望 FileMissing，实际 {err:?}"
    );
    assert!(!embedder.ready());
}

#[test]
fn failure_is_sticky_and_reported_again() {
    let embedder = BgeEmbedder::new("Z:/definitely/missing");
    assert!(embedder.warm_up().is_err());
    // 二次调用不应重新尝试加载（Failed 终态），但仍返回错误而不是 panic
    let second = embedder.warm_up();
    assert!(second.is_err());
    assert!(!embedder.ready());
}

#[test]
fn empty_batch_is_a_noop_even_when_not_ready() {
    let embedder = BgeEmbedder::new("Z:/definitely/missing");
    assert!(embedder.embed_sync(&[]).unwrap().is_empty());
}

#[tokio::test]
async fn async_trait_impl_reports_errors() {
    // `#[async_trait]` 包装后行为不变：未加载时返回错误而不是 panic
    let embedder = BgeEmbedder::new("Z:/definitely/missing");
    let err = embedder
        .embed(&["你好".to_string()])
        .await
        .expect_err("应报错");
    assert!(matches!(err, AppError::FileMissing(_)));
    assert!(!embedder.ready());
}
