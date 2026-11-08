//! 模型清单定位与校验（不需要真实模型文件：用临时文件与已知字节构造）。

use std::fs;
use std::path::Path;

use visrecall_ai::models::{
    load_manifest, locate_model_dir, sha256_file, sha256_hex, verify_manifest, MANIFEST_FILE,
};
use visrecall_domain::AppError;

fn write_manifest(dir: &Path, body: &str) {
    fs::write(dir.join(MANIFEST_FILE), body).expect("write manifest");
}

#[test]
fn verify_manifest_accepts_matching_checksums() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bytes = b"fake-onnx-payload";
    fs::write(dir.path().join("det.onnx"), bytes).expect("write model");
    let digest = sha256_hex(bytes);
    write_manifest(
        dir.path(),
        &format!(r#"{{"files":{{"det.onnx":"{digest}"}}}}"#),
    );

    let manifest = verify_manifest(dir.path()).expect("manifest 应校验通过");
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(sha256_file(&dir.path().join("det.onnx")).unwrap(), digest);
}

#[test]
fn verify_manifest_rejects_checksum_mismatch() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("det.onnx"), b"tampered").expect("write model");
    write_manifest(
        dir.path(),
        r#"{"files":{"det.onnx":"0000000000000000000000000000000000000000000000000000000000000000"}}"#,
    );

    let err = verify_manifest(dir.path()).expect_err("校验和不符必须报错");
    assert!(
        matches!(err, AppError::Internal(_)),
        "校验和不符应为 Internal，实际 {err:?}"
    );
}

#[test]
fn verify_manifest_rejects_missing_listed_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_manifest(dir.path(), r#"{"files":{"rec.onnx":"deadbeef"}}"#);

    let err = verify_manifest(dir.path()).expect_err("缺文件必须报错");
    assert!(
        matches!(err, AppError::FileMissing(_)),
        "缺文件应为 FileMissing，实际 {err:?}"
    );
}

#[test]
fn empty_manifest_is_invalid() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_manifest(dir.path(), r#"{"files":{}}"#);

    let err = verify_manifest(dir.path()).expect_err("空清单必须报错");
    assert!(matches!(err, AppError::InvalidInput(_)));
}

#[test]
fn manifest_carries_sources_and_licenses_and_ignores_extra_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("bge.onnx"), b"bge").expect("write model");
    fs::write(dir.path().join("README.txt"), b"unrelated").expect("write extra");
    let digest = sha256_hex(b"bge");
    write_manifest(
        dir.path(),
        &format!(
            r#"{{"files":{{"bge.onnx":"{digest}"}},"sources":{{"bge.onnx":"https://example.invalid/bge"}},"licenses":{{"bge.onnx":"MIT"}}}}"#
        ),
    );

    let manifest = load_manifest(dir.path()).expect("清单可解析");
    assert_eq!(manifest.files.len(), 1);
    assert_eq!(
        manifest.sources.get("bge.onnx").map(String::as_str),
        Some("https://example.invalid/bge")
    );
    assert_eq!(
        manifest.licenses.get("bge.onnx").map(String::as_str),
        Some("MIT")
    );
    verify_manifest(dir.path()).expect("额外文件不影响校验");
}

#[test]
fn locate_model_dir_skips_dirs_without_manifest() {
    let root = tempfile::tempdir().expect("tempdir");
    let empty = root.path().join("empty");
    let with_manifest = root.path().join("models");
    fs::create_dir_all(&empty).expect("mkdir");
    fs::create_dir_all(&with_manifest).expect("mkdir");
    write_manifest(&with_manifest, r#"{"files":{}}"#);

    assert_eq!(
        locate_model_dir(&[empty, with_manifest.clone()]),
        Some(with_manifest)
    );
}
