//! `CF_DIB` / `CF_UNICODETEXT` 字节布局的纯函数测试。
//!
//! 不触碰系统剪贴板：那是一个全局单例，并行测试会互相干扰，真实写入放在
//! `clipboard::tests` 的 `#[ignore]` 用例里手工跑。

use visrecall_shell::clipboard::{dib_bytes, utf16_crlf};

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn i32_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

#[test]
fn dib_starts_with_bitmapinfoheader_without_file_header() {
    let width = 3u32;
    let height = 5u32;
    let dib = dib_bytes(&vec![0u8; (width * height * 4) as usize], width, height);

    // 没有 14 字节 BITMAPFILEHEADER：前 4 字节就是 biSize = 40。
    assert_eq!(u32_at(&dib, 0), 40, "biSize");
    assert_eq!(i32_at(&dib, 4), width as i32, "biWidth");
    assert_eq!(
        i32_at(&dib, 8),
        height as i32,
        "biHeight (positive = bottom-up)"
    );
    assert_eq!(u16_at(&dib, 12), 1, "biPlanes");
    assert_eq!(u16_at(&dib, 14), 32, "biBitCount");
    assert_eq!(u32_at(&dib, 16), 0, "biCompression = BI_RGB");
    assert_eq!(u32_at(&dib, 20), width * height * 4, "biSizeImage");
    assert_eq!(dib.len() as u32, 40 + width * height * 4);
}

#[test]
fn dib_first_pixel_is_bottom_left_in_bgra() {
    // 2×2：行 0 = (红, 绿)，行 1 = (蓝, 白)。
    let rgba = [
        255, 0, 0, 255, // 左上 红
        0, 255, 0, 255, // 右上 绿
        0, 0, 255, 255, // 左下 蓝
        255, 255, 255, 255, // 右下 白
    ];
    let dib = dib_bytes(&rgba, 2, 2);

    assert_eq!(&dib[40..44], &[255, 0, 0, 255], "bottom-left = blue BGRA");
    assert_eq!(&dib[44..48], &[255, 255, 255, 255], "bottom-right = white");
    assert_eq!(&dib[48..52], &[0, 0, 255, 255], "top-left = red BGRA");
    assert_eq!(&dib[52..56], &[0, 255, 0, 255], "top-right = green BGRA");
}

#[test]
fn dib_alpha_channel_is_preserved() {
    let rgba = [10, 20, 30, 40];
    let dib = dib_bytes(&rgba, 1, 1);
    assert_eq!(&dib[40..44], &[30, 20, 10, 40]);
}

#[test]
fn utf16_crlf_adds_carriage_returns_and_trailing_nul() {
    let units = utf16_crlf("a\nb");
    assert_eq!(units.len(), 5, "a CR LF b NUL");
    assert_eq!(units, vec![0x61, 0x0d, 0x0a, 0x62, 0x00]);
}

#[test]
fn utf16_crlf_does_not_double_carriage_returns() {
    assert_eq!(
        utf16_crlf("line1\r\nline2"),
        vec![0x6c, 0x69, 0x6e, 0x65, 0x31, 0x0d, 0x0a, 0x6c, 0x69, 0x6e, 0x65, 0x32, 0x00]
    );
}

#[test]
fn utf16_crlf_handles_empty_and_non_bmp_text() {
    assert_eq!(utf16_crlf(""), vec![0]);
    // emoji 是代理对：两个 u16 + NUL。
    let units = utf16_crlf("😀");
    assert_eq!(units.len(), 3);
    assert_eq!(units[2], 0);
}
