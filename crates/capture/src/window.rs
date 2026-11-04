//! 窗口枚举、前台窗口与鼠标命中链。
//!
//! 坐标全部为虚拟屏幕物理像素（Per-Monitor V2，design §6）：窗口边界优先取
//! `DWMWA_EXTENDED_FRAME_BOUNDS`（不含不可见阴影边框），失败回退 `GetWindowRect`。
//! 数据形状 [`WindowInfo`]/[`ForegroundInfo`] 定义在 domain，本模块只做 Win32 读取，
//! 不涉及覆盖层、选区或滚轮层级。

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::addr_of_mut;

use tracing::{debug, warn};
use visrecall_domain::capture::ForegroundInfo;
use visrecall_domain::error::AppError;
use visrecall_domain::geometry::{PhysicalPoint, PhysicalRect};
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, ScreenToClient, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, RealChildWindowFromPoint,
    WindowFromPoint, GA_ROOT, GWL_EXSTYLE, WS_EX_TOOLWINDOW,
};

pub use visrecall_domain::capture::WindowInfo;

/// 命中链最多展开的层数，防御 `RealChildWindowFromPoint` 返回环。
const MAX_CHAIN_DEPTH: usize = 32;
/// `GetClassNameW` 缓冲区长度（Win32 类名上限 256）。
const CLASS_NAME_CAPACITY: usize = 256;
/// `GetWindowTextW` 缓冲区长度；超长标题被截断，不影响吸附。
const TITLE_CAPACITY: usize = 512;
/// `QueryFullProcessImageNameW` 缓冲区长度；超长路径（> 1000 字符）进程名置空。
const PROCESS_PATH_CAPACITY: usize = 1024;

/// 窗口探针：前台窗口、可见窗口枚举、鼠标命中链。
pub trait WindowProbe: Send + Sync {
    /// 捕获瞬间的前台窗口；没有前台窗口时返回 [`AppError::CaptureFailed`]。
    fn foreground(&self) -> Result<ForegroundInfo, AppError>;
    /// 当前可见的候选窗口，按 `EnumWindows` 的 Z 序（从上到下）。
    fn enumerate(&self) -> Result<Vec<WindowInfo>, AppError>;
    /// 从根到子的窗口链，`point` 为虚拟屏幕物理像素。
    fn hit_chain(&self, point: PhysicalPoint) -> Result<Vec<WindowInfo>, AppError>;
}

/// 基于 Win32 的 [`WindowProbe`]；无状态，可跨线程共享。
#[derive(Clone, Copy, Debug, Default)]
pub struct Win32WindowProbe;

impl Win32WindowProbe {
    pub fn new() -> Self {
        Self
    }
}

impl WindowProbe for Win32WindowProbe {
    fn foreground(&self) -> Result<ForegroundInfo, AppError> {
        foreground_info()
    }

    fn enumerate(&self) -> Result<Vec<WindowInfo>, AppError> {
        enumerate_windows()
    }

    fn hit_chain(&self, point: PhysicalPoint) -> Result<Vec<WindowInfo>, AppError> {
        hit_chain(point)
    }
}

/// 枚举可见的顶层窗口候选，顺序与 `EnumWindows` 一致（Z 序从上到下）。
///
/// 回调阶段先排除不可见窗口与最小化窗口（`IsIconic`）；后者在 Win32 里报
/// `-32000` 一类的占位坐标，对吸附没有意义，直接跳过而不进入候选。
/// 其余规则（工具窗口、DWM 隐身、空标题）统一由 [`filter_candidates`] 处理。
pub fn enumerate_windows() -> Result<Vec<WindowInfo>, AppError> {
    let mut windows: Vec<WindowInfo> = Vec::new();
    // SAFETY: `windows` 在调用期间位于栈上且不被移动，指针只作为 `LPARAM` 传给同步
    // 执行的 `EnumWindows` 回调；回调不保存该指针。
    let data = LPARAM(addr_of_mut!(windows) as isize);
    unsafe { EnumWindows(Some(enum_window_proc), data) }
        .map_err(|err| AppError::CaptureFailed(format!("EnumWindows failed: {err}")))?;
    Ok(filter_candidates(&windows).into_iter().cloned().collect())
}

/// `EnumWindows` 回调：把通过可见性检查的窗口追加到 `LPARAM` 指向的 `Vec<WindowInfo>`。
///
/// 回调内不得 panic：所有 Win32 读取失败都退化成默认值或跳过该窗口。
unsafe extern "system" fn enum_window_proc(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` 由 `enumerate_windows` 传入，指向枚举期间存活的 `Vec<WindowInfo>`。
    let windows = unsafe { &mut *(data.0 as *mut Vec<WindowInfo>) };

    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return TRUE;
    }
    if unsafe { IsIconic(hwnd) }.as_bool() {
        debug!(hwnd = hwnd_as_isize(hwnd), "minimized window skipped");
        return TRUE;
    }

    windows.push(window_info(hwnd));
    TRUE
}

/// 捕获瞬间的前台窗口信息。
///
/// `display_name` 是进程显示名（进程文件名去扩展名，如 `chrome`）；所在显示器由
/// `bounds` 与显示器 `bounds` 的交集推断，schema 不存显示器列。
pub fn foreground_info() -> Result<ForegroundInfo, AppError> {
    // SAFETY: 无参数；没有前台窗口时返回空句柄。
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return Err(AppError::CaptureFailed("no foreground window".into()));
    }

    let mut pid = 0u32;
    // SAFETY: 输出指针指向栈上有效 `u32`。
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let process_name = process_name_for_pid(pid);

    Ok(ForegroundInfo {
        hwnd: hwnd_as_isize(hwnd),
        pid,
        display_name: display_name(&process_name),
        process_name,
        title: window_title(hwnd),
        bounds: window_bounds(hwnd),
    })
}

/// 鼠标命中链：从根窗口到最内层子窗口，`point` 为虚拟屏幕物理像素。
///
/// `WindowFromPoint` 拿到起点并提升到顶层窗口（[`root_window`]），之后逐层
/// `RealChildWindowFromPoint`（先把屏幕坐标换算成父窗口客户区坐标），最多
/// [`MAX_CHAIN_DEPTH`] 层。命中桌面类（`Progman`/`WorkerW`/`Shell_TrayWnd`）时链上该
/// 节点改写为该点所在显示器的整屏矩形。
pub fn hit_chain(point: PhysicalPoint) -> Result<Vec<WindowInfo>, AppError> {
    let screen_point = POINT {
        x: point.x,
        y: point.y,
    };
    // SAFETY: 纯查询，无指针参数。
    let hit = unsafe { WindowFromPoint(screen_point) };
    if hit.is_invalid() {
        return Err(AppError::CaptureFailed(format!(
            "WindowFromPoint found no window at ({}, {})",
            point.x, point.y
        )));
    }

    let mut chain: Vec<WindowInfo> = Vec::new();
    let mut current = root_window(hit);
    for depth in 0..MAX_CHAIN_DEPTH {
        let info = window_info(current);

        if is_desktop_class(&info.class_name) {
            // 桌面本体/壁纸宿主/任务栏没有可吸附的窗口边界，用所在显示器整屏代替。
            chain.push(desktop_entry(info, point));
            return Ok(chain);
        }

        chain.push(info);

        let child = child_at(current, screen_point);
        if child.is_invalid() || child == current {
            debug!(depth, "hit chain ended");
            return Ok(chain);
        }
        current = child;
    }

    warn!(
        max_depth = MAX_CHAIN_DEPTH,
        "hit chain depth limit reached; chain truncated"
    );
    Ok(chain)
}

/// 纯函数：从枚举结果里挑出可吸附的候选窗口。
///
/// 规则：非工具窗口（`WS_EX_TOOLWINDOW`）、未被 DWM 隐身（`DWMWA_CLOAKED`）、
/// 标题去空白后非空。可见性检查依赖句柄，已在枚举阶段完成，不在此重复。
pub fn filter_candidates(windows: &[WindowInfo]) -> Vec<&WindowInfo> {
    windows.iter().filter(|w| is_candidate(w)).collect()
}

/// 单个窗口是否满足候选规则；[`filter_candidates`] 的逐项谓词。
fn is_candidate(window: &WindowInfo) -> bool {
    !window.tool_window && !window.cloaked && !window.title.trim().is_empty()
}

/// 桌面类窗口：桌面本体 `Progman`、壁纸宿主 `WorkerW`、任务栏 `Shell_TrayWnd`。
///
/// 精确匹配类名（大小写敏感、不做前缀匹配）。
pub fn is_desktop_class(class_name: &str) -> bool {
    matches!(class_name, "Progman" | "WorkerW" | "Shell_TrayWnd")
}

/// 读取单个窗口的全部字段；不做候选过滤，命中链需要未经筛选的原始信息。
fn window_info(hwnd: HWND) -> WindowInfo {
    WindowInfo {
        hwnd: hwnd_as_isize(hwnd),
        title: window_title(hwnd),
        class_name: window_class(hwnd),
        process_name: window_process_name(hwnd),
        bounds: window_bounds(hwnd),
        tool_window: is_tool_window(hwnd),
        cloaked: is_cloaked(hwnd),
    }
}

/// 桌面类节点改写为所在显示器整屏矩形；取不到显示器时保留窗口自身边界。
fn desktop_entry(info: WindowInfo, point: PhysicalPoint) -> WindowInfo {
    let bounds = monitor_bounds(point).unwrap_or(info.bounds);
    WindowInfo { bounds, ..info }
}

/// 把命中到的句柄提升到顶层窗口（`GetAncestor(GA_ROOT)`）。
///
/// `WindowFromPoint` 可能直接返回子窗口（Chrome 的渲染子窗口、任务栏的 `MSTaskSwWClass`
/// 等），而命中链要求从根开始，否则链首会是一个没有顶层边界的内部窗口。
/// 取不到根句柄时退回原句柄。
fn root_window(hit: HWND) -> HWND {
    // SAFETY: 纯查询；`GA_ROOT` 沿父链向上，跨进程同样成立。
    let root = unsafe { GetAncestor(hit, GA_ROOT) };
    if root.is_invalid() {
        hit
    } else {
        root
    }
}

/// `hwnd` 位于该屏幕点的直接子窗口；`RealChildWindowFromPoint` 需要父客户区坐标。
///
/// 没有子窗口（点在非客户区/父窗口外）或坐标换算失败时返回空句柄，调用方据此结束链。
fn child_at(parent: HWND, screen_point: POINT) -> HWND {
    let mut client = screen_point;
    // SAFETY: `client` 指向栈上有效 `POINT`，`parent` 由调用方保证有效。
    if !unsafe { ScreenToClient(parent, &mut client) }.as_bool() {
        debug!(
            parent = parent.0 as isize,
            "ScreenToClient failed; hit chain stopped"
        );
        return HWND::default();
    }
    // SAFETY: 纯查询；坐标已换算为 `parent` 的客户区坐标。
    unsafe { RealChildWindowFromPoint(parent, client) }
}

/// 窗口标题；读取失败或空标题返回空字符串。
fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; TITLE_CAPACITY];
    // SAFETY: 缓冲区在栈上，长度由切片传入；失败时返回 0，不写越界。
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    utf16_string(&buf, len)
}

/// 窗口类名；读取失败返回空字符串。
fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; CLASS_NAME_CAPACITY];
    // SAFETY: 同上，`GetClassNameW` 按切片长度截断。
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    utf16_string(&buf, len)
}

/// 窗口边界：优先 `DWMWA_EXTENDED_FRAME_BOUNDS`，失败回退 `GetWindowRect`。
fn window_bounds(hwnd: HWND) -> PhysicalRect {
    let mut frame = RECT::default();
    // SAFETY: `frame` 指向栈上有效 `RECT`，`cbattribute` 与 `RECT` 大小一致。
    let dwm = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            addr_of_mut!(frame).cast::<c_void>(),
            size_of::<RECT>() as u32,
        )
    };
    if dwm.is_ok() {
        return physical_rect(&frame);
    }
    if let Err(err) = dwm {
        debug!(
            hwnd = hwnd_as_isize(hwnd),
            error = %err,
            "DWMWA_EXTENDED_FRAME_BOUNDS failed; falling back to GetWindowRect"
        );
    }

    let mut fallback = RECT::default();
    // SAFETY: `fallback` 指向栈上有效 `RECT`。
    if unsafe { GetWindowRect(hwnd, &mut fallback) }.is_ok() {
        physical_rect(&fallback)
    } else {
        PhysicalRect::default()
    }
}

/// 是否设置了 `WS_EX_TOOLWINDOW`（工具窗口不进候选，避免把调试面板/输入法候选框当目标）。
fn is_tool_window(hwnd: HWND) -> bool {
    // SAFETY: 纯查询；`GWL_EXSTYLE` 返回 32 位样式，读取失败返回 0。
    let ex_style = u32::try_from(unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) }).unwrap_or(0);
    ex_style & WS_EX_TOOLWINDOW.0 != 0
}

/// 是否被 DWM 隐身（UWP 后台应用、其它虚拟桌面上的窗口）。
///
/// 查询失败（如窗口正在销毁）按「未隐身」处理，避免漏掉真实窗口。
fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked: u32 = 0;
    // SAFETY: `cloaked` 指向栈上有效 `u32`，`cbattribute` 与类型大小一致。
    let result = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            addr_of_mut!(cloaked).cast::<c_void>(),
            size_of::<u32>() as u32,
        )
    };
    match result {
        Ok(()) => cloaked != 0,
        Err(err) => {
            debug!(
                hwnd = hwnd_as_isize(hwnd),
                error = %err,
                "DWMWA_CLOAKED query failed; treating window as not cloaked"
            );
            false
        }
    }
}

/// 窗口所属进程的文件名（保留扩展名，如 `chrome.exe`）；读取失败返回空字符串。
fn window_process_name(hwnd: HWND) -> String {
    let mut pid = 0u32;
    // SAFETY: 输出指针指向栈上有效 `u32`。
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    process_name_for_pid(pid)
}

/// 按 PID 取进程文件名；任何一步失败都返回空字符串，不 panic。
fn process_name_for_pid(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    let Some(process) = ProcessHandle::open(pid) else {
        return String::new();
    };

    let mut buf = [0u16; PROCESS_PATH_CAPACITY];
    let mut len = buf.len() as u32;
    // SAFETY: 缓冲区在栈上，`len` 是它的容量（字符数）；成功时 `len` 被改写为实际长度。
    let result = unsafe {
        QueryFullProcessImageNameW(
            process.handle(),
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    match result {
        Ok(()) => file_name(&utf16_string(&buf, len)),
        Err(err) => {
            debug!(pid, error = %err, "QueryFullProcessImageNameW failed; process name left empty");
            String::new()
        }
    }
}

/// 进程句柄的 RAII 包装，`Drop` 时 `CloseHandle`，失败不影响调用方。
struct ProcessHandle(HANDLE);

impl ProcessHandle {
    /// 以 `PROCESS_QUERY_LIMITED_INFORMATION` 打开进程；系统/更高权限进程会被拒绝，返回 `None`。
    fn open(pid: u32) -> Option<Self> {
        // SAFETY: 只请求查询权限，不继承句柄；失败返回 `Err` 而不是空句柄。
        match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
            Ok(handle) => Some(Self(handle)),
            Err(err) => {
                debug!(pid, error = %err, "OpenProcess failed; process name left empty");
                None
            }
        }
    }

    fn handle(&self) -> HANDLE {
        self.0
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: 句柄由 `open` 创建且只在此处释放一次；失败无副作用。
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// 该屏幕点所在显示器的整屏矩形（`MonitorFromPoint` + `GetMonitorInfoW`）。
fn monitor_bounds(point: PhysicalPoint) -> Option<PhysicalRect> {
    let point = POINT {
        x: point.x,
        y: point.y,
    };
    // SAFETY: 纯查询；离屏点由 `MONITOR_DEFAULTTONEAREST` 兜底。
    let monitor: HMONITOR = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return None;
    }

    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info.cbSize` 已按 `MONITORINFO` 大小填写，指针在调用期间有效。
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        Some(physical_rect(&info.rcMonitor))
    } else {
        debug!(
            error = %windows::core::Error::from_thread(),
            "GetMonitorInfoW failed for point; desktop bounds fall back to window rect"
        );
        None
    }
}

/// 取路径最后一段（保留扩展名）；无分隔符时原样返回。
fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

/// 进程显示名：进程文件名去扩展名（`chrome.exe` → `chrome`）。
///
/// 无扩展名或去扩展名后为空（如 `.gitignore`）时保留原名。
fn display_name(process_name: &str) -> String {
    match process_name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => process_name.to_string(),
    }
}

/// UTF-16 缓冲区按 Win32 报告的字符数转 `String`；负数/越界一律裁剪到有效范围。
fn utf16_string(buf: &[u16], len: impl TryInto<usize>) -> String {
    let len = len.try_into().unwrap_or(0).min(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Win32 `RECT`（右/下为开区间外的边）转 [`PhysicalRect`]；宽高饱和到非负。
fn physical_rect(rc: &RECT) -> PhysicalRect {
    PhysicalRect::new(
        rc.left,
        rc.top,
        u32::try_from(rc.right.saturating_sub(rc.left)).unwrap_or(0),
        u32::try_from(rc.bottom.saturating_sub(rc.top)).unwrap_or(0),
    )
}

/// 句柄转 domain 侧使用的 `isize`（`WindowInfo::hwnd`）。
fn hwnd_as_isize(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_keeps_last_component_of_both_separators() {
        assert_eq!(
            file_name(r"C:\Program Files\Chrome\chrome.exe"),
            "chrome.exe"
        );
        assert_eq!(file_name("C:/tools/code.exe"), "code.exe");
        assert_eq!(file_name("explorer.exe"), "explorer.exe");
    }

    #[test]
    fn display_name_strips_extension_once() {
        assert_eq!(display_name("chrome.exe"), "chrome");
        assert_eq!(display_name("Code - Insiders.exe"), "Code - Insiders");
        assert_eq!(display_name("noext"), "noext");
        assert_eq!(display_name(".gitignore"), ".gitignore");
        assert_eq!(display_name(""), "");
    }

    #[test]
    fn utf16_string_clamps_reported_length() {
        let buf = [b'h' as u16, b'i' as u16, 0];
        assert_eq!(utf16_string(&buf, 2i32), "hi");
        assert_eq!(utf16_string(&buf, -1i32), "");
        assert_eq!(utf16_string(&buf, 99i32), "hi\u{0}");
    }

    #[test]
    fn physical_rect_clamps_inverted_edges() {
        let rc = RECT {
            left: -2560,
            top: 0,
            right: 0,
            bottom: 1440,
        };
        assert_eq!(physical_rect(&rc), PhysicalRect::new(-2560, 0, 2560, 1440));

        let inverted = RECT {
            left: 10,
            top: 10,
            right: 5,
            bottom: 5,
        };
        assert!(physical_rect(&inverted).is_empty());
    }
}
