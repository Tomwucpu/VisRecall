//! GDI 抓帧单测：空帧检测与 BGRA→PNG 调试导出。
//!
//! 不依赖真实屏幕：只构造 [`RawFrame`] 检查纯函数行为；真实抓帧由
//! `cargo run -p visrecall-capture --example grab_once` 人工验证。

use visrecall_capture::grab_gdi::is_blank_frame;
use visrecall_capture::trait_defs::{RawFrame, RawFrameExt};

fn frame4x2(bgra: Vec<u8>) -> RawFrame {
    RawFrame {
        display_id: "DISPLAY1".into(),
        width: 4,
        height: 2,
        stride: 16,
        bgra,
    }
}

#[test]
fn detects_all_black_frame() {
    let f = frame4x2(vec![0; 32]);
    assert!(is_blank_frame(&f));
}

#[test]
fn non_black_frame_is_not_blank() {
    let mut bgra = vec![0u8; 32];
    bgra[0] = 255;
    let f = frame4x2(bgra);
    assert!(!is_blank_frame(&f));
}

#[test]
fn empty_buffer_is_blank() {
    let f = frame4x2(Vec::new());
    assert!(is_blank_frame(&f));
}

#[test]
fn to_png_rejects_short_buffer() {
    // stride * height = 32，但只给了 16 字节。
    let f = frame4x2(vec![0; 16]);
    assert!(f.to_png().is_err());
}

#[test]
fn to_png_encodes_bgra_as_rgba() {
    // 左上角一个纯红像素（BGRA: 00 00 FF FF），其余透明黑。
    let mut bgra = vec![0u8; 32];
    bgra[..4].copy_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);
    let png = frame4x2(bgra).to_png().expect("encoding should succeed");

    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "PNG signature");
    // IHDR 紧跟 8 字节签名 + 4 字节长度 + 4 字节类型，宽高为大端 u32。
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 4);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 2);
}
