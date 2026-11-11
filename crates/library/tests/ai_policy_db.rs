//! 「文本上传云端解析」开关的库内策略测试。
//!
//! 该开关存库内 `settings` 表（与容量/保留同源）：属于「这个库」，
//! `config.json` 跟安装走，换库后会张冠李戴。

use visrecall_library::Library;

fn library() -> (tempfile::TempDir, Library) {
    let dir = tempfile::tempdir().unwrap();
    let lib = Library::open(dir.path().to_path_buf()).unwrap();
    (dir, lib)
}

/// 默认关闭：未写入过任何值时必须保守（不发云端请求）。
#[test]
fn cloud_text_ai_defaults_to_disabled() {
    let (_dir, lib) = library();
    assert!(!lib.cloud_text_ai_enabled());
}

/// 写入后立即读回，并且跨「重开库」保持（值真的落盘而非只存内存）。
#[test]
fn cloud_text_ai_round_trips_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    {
        let lib = Library::open(root.clone()).unwrap();
        lib.set_cloud_text_ai(true).unwrap();
        assert!(lib.cloud_text_ai_enabled());
    }

    let reopened = Library::open(root).unwrap();
    assert!(reopened.cloud_text_ai_enabled());

    // 关闭也要真的写回去（否则用户关不掉）。
    reopened.set_cloud_text_ai(false).unwrap();
    assert!(!reopened.cloud_text_ai_enabled());
}

/// 坏值回落默认 false，而不是把整个设置页卡住。
#[test]
fn cloud_text_ai_corrupted_value_falls_back_to_disabled() {
    let (_dir, lib) = library();
    lib.setting_set(
        visrecall_library::ai_policy::KEY_CLOUD_TEXT_AI,
        &"not-a-bool",
    )
    .unwrap();
    assert!(!lib.cloud_text_ai_enabled());
}
