//! 冻结帧尺寸/行距校验（纯数据，不创建 D3D 设备）。

use visrecall_shell::overlay::frame::validate_frame_shape;

#[test]
fn accepts_expected_stride() {
    assert!(validate_frame_shape(4, 2, 16).is_ok());
}

#[test]
fn rejects_short_buffer() {
    assert!(validate_frame_shape(4, 2, 8).is_err());
}

#[test]
fn accepts_padded_stride() {
    // 行对齐填充：stride 大于 width * 4 但仍是 4 的倍数。
    assert!(validate_frame_shape(3, 2, 16).is_ok());
}

#[test]
fn accepts_4k_frame() {
    assert!(validate_frame_shape(3840, 2160, 3840 * 4).is_ok());
}

#[test]
fn rejects_zero_dimensions() {
    assert!(validate_frame_shape(0, 2, 16).is_err());
    assert!(validate_frame_shape(4, 0, 16).is_err());
}

#[test]
fn rejects_unaligned_stride() {
    assert!(validate_frame_shape(4, 2, 17).is_err());
}
