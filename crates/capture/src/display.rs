//! 显示器枚举与虚拟屏幕几何。
//!
//! 全部坐标为虚拟屏幕**物理像素**（Per-Monitor V2 DPI 感知，design §6），原点在主显示器
//! 左上角，副屏可为负坐标。`bounds`/`work_area` 来自 `GetMonitorInfoW`，DPI 来自
//! `GetDpiForMonitor(MDT_EFFECTIVE_DPI)`；设备名（如 `\\.\DISPLAY1`）作为 [`DisplayInfo::id`]。

use std::mem::size_of;
use std::ptr::addr_of_mut;

use tracing::{debug, warn};
use visrecall_domain::capture::DisplayInfo;
use visrecall_domain::error::AppError;
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};
use windows::core::Error as WinError;
use windows::Win32::Foundation::{LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

/// 无法取到显示器 DPI 时的回退值（100% 缩放）。
const DEFAULT_DPI: u32 = 96;

/// 声明进程为 Per-Monitor V2 DPI 感知；必须在任何窗口创建前调用（`main` 第一条语句）。
///
/// 已由应用 manifest 声明时 Win32 返回 `ERROR_ACCESS_DENIED`，属于预期情况：
/// 这里只记日志，不改变启动流程。
pub fn set_per_monitor_v2() {
    // SAFETY: 只设置进程级 DPI 感知上下文，无指针参数、无生命周期要求。
    match unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } {
        Ok(()) => debug!("process DPI awareness set to per-monitor v2"),
        // 常见原因：manifest/宿主进程已设置过，忽略即可。
        Err(err) => debug!(error = %err, "SetProcessDpiAwarenessContext ignored"),
    }
}

/// 枚举所有显示器，按虚拟屏幕坐标（左→右、上→下）排序。
///
/// `EnumDisplayMonitors` 本身失败或一台显示器都没有时返回 [`AppError::CaptureFailed`]。
pub fn enumerate_displays() -> Result<Vec<DisplayInfo>, AppError> {
    let mut displays: Vec<DisplayInfo> = Vec::new();

    let ok = {
        // SAFETY: `displays` 在调用期间位于栈上且不被移动，指针只作为 `LPARAM`
        // 传给同步执行的回调；回调不保存该指针。
        let data = LPARAM(addr_of_mut!(displays) as isize);
        unsafe { EnumDisplayMonitors(None, None, Some(monitor_enum_proc), data) }
    };

    if !ok.as_bool() {
        return Err(AppError::CaptureFailed(format!(
            "EnumDisplayMonitors failed: {}",
            WinError::from_thread()
        )));
    }

    if displays.is_empty() {
        return Err(AppError::CaptureFailed(
            "display enumeration returned no monitors".into(),
        ));
    }

    displays.sort_by_key(|d| (d.bounds.x, d.bounds.y, d.id.clone()));
    Ok(displays)
}

/// `EnumDisplayMonitors` 回调：把一台显示器追加到 `LPARAM` 指向的 `Vec<DisplayInfo>`。
///
/// 回调内不得 panic，也不做可能失败后 `unwrap` 的操作；单台显示器信息读取失败只跳过该台。
unsafe extern "system" fn monitor_enum_proc(
    monitor: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    // SAFETY: `data` 由 `enumerate_displays` 传入，指向枚举期间存活的 `Vec<DisplayInfo>`。
    let displays = unsafe { &mut *(data.0 as *mut Vec<DisplayInfo>) };

    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;

    // SAFETY: `MONITORINFOEXW` 的 `monitorInfo` 是首个字段（`repr(C)`），
    // 因此该指针对 `GetMonitorInfoW` 仍是合法的 `MONITORINFO*`。
    let monitor_ptr = addr_of_mut!(info).cast::<MONITORINFO>();
    let filled = unsafe { GetMonitorInfoW(monitor, monitor_ptr) };
    if !filled.as_bool() {
        warn!(
            error = %WinError::from_thread(),
            "GetMonitorInfoW failed; monitor skipped"
        );
        // 继续枚举其他显示器。
        return TRUE;
    }

    displays.push(DisplayInfo {
        id: device_name(&info.szDevice),
        bounds: rect(&info.monitorInfo.rcMonitor),
        work_area: rect(&info.monitorInfo.rcWork),
        dpi: effective_dpi(monitor),
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    });

    TRUE
}

/// 显示器生效 DPI；调用失败（旧系统或无显示器上下文）回退 [`DEFAULT_DPI`]。
fn effective_dpi(monitor: HMONITOR) -> u32 {
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    // SAFETY: 两个输出指针都指向栈上有效 `u32`；`monitor` 来自枚举回调。
    match unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) } {
        Ok(()) if dpi_x != 0 => dpi_x,
        Ok(()) => {
            warn!("GetDpiForMonitor returned 0; assuming {DEFAULT_DPI}");
            DEFAULT_DPI
        }
        Err(err) => {
            debug!(error = %err, "GetDpiForMonitor failed; assuming {DEFAULT_DPI}");
            DEFAULT_DPI
        }
    }
}

/// `szDevice`（UTF-16，以 NUL 结尾，定长 32）转成设备名。
fn device_name(raw: &[u16; 32]) -> String {
    let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..len])
}

/// Win32 `RECT`（右/下为开区间外的边）转 [`PhysicalRect`]；宽高饱和到非负。
fn rect(rc: &RECT) -> PhysicalRect {
    PhysicalRect::new(
        rc.left,
        rc.top,
        u32::try_from(rc.right.saturating_sub(rc.left)).unwrap_or(0),
        u32::try_from(rc.bottom.saturating_sub(rc.top)).unwrap_or(0),
    )
}

/// 命中 `point` 的显示器；使用半开区间，边界恰好落在两屏交界时归属后一个（右侧/下方）。
pub fn display_for_point(displays: &[DisplayInfo], p: PhysicalPoint) -> Option<&DisplayInfo> {
    displays.iter().find(|d| d.bounds.contains(p))
}

/// 所有显示器 `bounds` 的并集，即虚拟屏幕矩形；空列表返回 [`PhysicalRect::default`]。
pub fn virtual_bounds(displays: &[DisplayInfo]) -> PhysicalRect {
    displays
        .iter()
        .fold(PhysicalRect::default(), |acc, d| acc.union(&d.bounds))
}

/// 把全局物理坐标点换算成显示器内的归一化坐标 `(0.0..=1.0)`，用于悬浮球位置记忆。
///
/// 宽或高为 0 的显示器返回 `(0.0, 0.0)`，不产生 `NaN`。不做上下界裁剪：拖动到屏外时
/// 调用方自行决定是否吸附回可视区。
pub fn normalize_position(display: &DisplayInfo, point: PhysicalPoint) -> (f32, f32) {
    let bounds = display.bounds;
    let x = normalize_axis(point.x, bounds.x, bounds.width);
    let y = normalize_axis(point.y, bounds.y, bounds.height);
    (x, y)
}

fn normalize_axis(value: i32, origin: i32, len: u32) -> f32 {
    if len == 0 {
        return 0.0;
    }
    // 先落 `f64` 再转 `f32`：`i32::MIN` 与大幅负坐标相减不会溢出。
    (((value as f64) - (origin as f64)) / (len as f64)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_name_stops_at_first_nul() {
        let mut raw = [0u16; 32];
        for (slot, code) in raw.iter_mut().zip("\\\\.\\DISPLAY1".encode_utf16()) {
            *slot = code;
        }
        assert_eq!(device_name(&raw), "\\\\.\\DISPLAY1");
    }

    #[test]
    fn device_name_handles_full_buffer_without_nul() {
        let raw = [b'A' as u16; 32];
        assert_eq!(device_name(&raw).len(), 32);
    }

    #[test]
    fn rect_converts_win32_edges() {
        let rc = RECT {
            left: -2560,
            top: 0,
            right: 0,
            bottom: 1440,
        };
        assert_eq!(rect(&rc), PhysicalRect::new(-2560, 0, 2560, 1440));
    }

    #[test]
    fn rect_tolerates_inverted_edges() {
        let rc = RECT {
            left: 10,
            top: 10,
            right: 5,
            bottom: 5,
        };
        assert!(rect(&rc).is_empty());
    }
}
