//! 提交回读的拼接逻辑（design §6：逐显示器求交 + 拼合全局选区）。
//!
//! `compose_bgra` / `region_offsets` 是纯函数：不创建 D3D 设备，也不碰 Win32。

use visrecall_domain::geometry::PhysicalRect;
use visrecall_shell::overlay::input::{compose_bgra, region_offsets};

/// 生成 `w × h` 的单色 BGRA 区域（`b,g,r,a` 全部等于 `value`）。
fn solid(w: u32, h: u32, value: u8) -> Vec<u8> {
    vec![value; (w as usize) * (h as usize) * 4]
}

#[test]
fn compose_copies_region_at_offset() {
    let dst_w = 4u32;
    let dst_h = 2u32;
    let src = vec![9u8; 2 * 4];
    let out = compose_bgra(dst_w, dst_h, &[(1, 1, 2, 1, src.clone())]);
    assert_eq!(out.len(), (dst_w * dst_h * 4) as usize);
    // (1,1) 处应写入 9
    let idx = ((dst_w + 1) * 4) as usize;
    assert_eq!(&out[idx..idx + 4], &[9, 9, 9, 9]);
    // 未覆盖的 (0,0) 保持 0。
    assert_eq!(&out[0..4], &[0, 0, 0, 0]);
}

#[test]
fn compose_fills_only_covered_pixels() {
    let out = compose_bgra(2, 2, &[(0, 0, 1, 1, vec![7, 8, 9, 255])]);
    assert_eq!(out.len(), 16);
    assert_eq!(&out[0..4], &[7, 8, 9, 255]);
    // 其余三个像素保持全 0。
    assert!(out[4..].iter().all(|&b| b == 0));
}

#[test]
fn compose_without_regions_is_transparent_zero() {
    let out = compose_bgra(3, 2, &[]);
    assert_eq!(out.len(), 3 * 2 * 4);
    assert!(out.iter().all(|&b| b == 0));
}

#[test]
fn compose_clips_region_at_right_and_bottom() {
    // 2×2 的源画在 (3, 1)：目标 4×2 只容纳 (3,1) 一个像素。
    let src = vec![
        1, 1, 1, 255, 2, 2, 2, 255, // 第一行
        3, 3, 3, 255, 4, 4, 4, 255, // 第二行
    ];
    let out = compose_bgra(4, 2, &[(3, 1, 2, 2, src)]);
    // 目标 (3,1) = 源 (0,0)。
    let idx = ((4 + 3) * 4) as usize;
    assert_eq!(&out[idx..idx + 4], &[1, 1, 1, 255]);
}

#[test]
fn compose_ignores_region_fully_outside() {
    let out = compose_bgra(2, 2, &[(5, 5, 1, 1, vec![9, 9, 9, 9])]);
    assert!(out.iter().all(|&b| b == 0));
}

#[test]
fn compose_ignores_short_buffer_and_zero_size() {
    let out = compose_bgra(2, 2, &[(0, 0, 2, 2, vec![1, 2, 3, 4])]);
    assert!(out.iter().all(|&b| b == 0), "short buffer must be ignored");

    let out = compose_bgra(2, 2, &[(0, 0, 0, 2, Vec::new()), (0, 0, 2, 0, Vec::new())]);
    assert!(out.iter().all(|&b| b == 0));
}

#[test]
fn compose_later_region_overwrites_earlier() {
    let out = compose_bgra(
        1,
        1,
        &[(0, 0, 1, 1, solid(1, 1, 10)), (0, 0, 1, 1, solid(1, 1, 20))],
    );
    assert_eq!(&out[..], &[20, 20, 20, 20]);
}

#[test]
fn compose_stitches_two_displays_side_by_side() {
    // 选区跨两台 2×1 显示器：左屏填 1，右屏填 2，目标为 4×1。
    let out = compose_bgra(
        4,
        1,
        &[(0, 0, 2, 1, solid(2, 1, 1)), (2, 0, 2, 1, solid(2, 1, 2))],
    );
    assert_eq!(out, vec![1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2]);
}

// ---- region_offsets ----

#[test]
fn region_offsets_inside_single_display() {
    let selection = PhysicalRect::new(100, 120, 40, 30);
    let display = PhysicalRect::new(0, 0, 1920, 1080);
    assert_eq!(
        region_offsets(selection, display),
        Some((0, 0, 100, 120, 40, 30))
    );
}

#[test]
fn region_offsets_clips_at_display_edge() {
    // 选区横跨左屏右边界：左屏只贡献前 20 列。
    let selection = PhysicalRect::new(1900, 10, 100, 50);
    let left = PhysicalRect::new(0, 0, 1920, 1080);
    assert_eq!(
        region_offsets(selection, left),
        Some((0, 0, 1900, 10, 20, 50))
    );

    // 右屏（原点 1920，可负坐标）贡献剩余 80 列，目标偏移 20。
    let right = PhysicalRect::new(1920, 0, 2560, 1440);
    assert_eq!(
        region_offsets(selection, right),
        Some((20, 0, 0, 10, 80, 50))
    );
}

#[test]
fn region_offsets_handles_negative_display_origin() {
    // 副屏在主屏左侧：显示器原点为 -2560。
    let display = PhysicalRect::new(-2560, 0, 2560, 1440);
    let selection = PhysicalRect::new(-2600, -10, 100, 40);
    assert_eq!(
        region_offsets(selection, display),
        Some((40, 10, 0, 0, 60, 30))
    );
}

#[test]
fn region_offsets_none_when_disjoint() {
    let selection = PhysicalRect::new(0, 0, 100, 100);
    assert_eq!(
        region_offsets(selection, PhysicalRect::new(200, 0, 100, 100)),
        None
    );
    // 只接触边界（半开区间）同样不算相交。
    assert_eq!(
        region_offsets(selection, PhysicalRect::new(100, 0, 100, 100)),
        None
    );
}
