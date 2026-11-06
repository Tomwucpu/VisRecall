//! 应用配置（`config.json`）：带默认值的 serde 结构与原子持久化。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use visrecall_domain::AppError;

use crate::files;
use crate::paths;

/// 默认全局捕获快捷键。
pub const DEFAULT_CAPTURE_HOTKEY: &str = "Ctrl+Shift+S";
/// 悬浮球默认边长（逻辑像素）。
pub const DEFAULT_BALL_SIZE: u32 = 48;

/// 应用配置，字段见 design §10.3；缺失字段一律回落到 [`AppConfig::default`]。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub library_path: PathBuf,
    pub hotkeys: HotkeyConfig,
    pub ball: BallConfig,
    pub capture: CaptureConfig,
    pub privacy: PrivacyConfig,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub capture: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BallConfig {
    pub enabled: bool,
    pub size: u32,
    pub positions: HashMap<String, (f32, f32)>,
    /// 贴边收起：距工作区边缘 ≤12px 松手后收成长条。
    pub edge_hide: bool,
    /// 全屏/演示/独占全屏时自动隐藏。
    pub hide_in_fullscreen: bool,
    /// 首次运行引导是否已展示过（一次性提示）。
    pub guide_shown: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureConfig {
    pub realtime_fallback: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyConfig {
    pub clipboard_capture: bool,
    pub telemetry: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            // `USERPROFILE` 缺失时退回相对路径；正常 Windows 会话不会走到这里。
            library_path: paths::default_library_root()
                .unwrap_or_else(|_| PathBuf::from("VisRecall")),
            hotkeys: HotkeyConfig::default(),
            ball: BallConfig::default(),
            capture: CaptureConfig::default(),
            privacy: PrivacyConfig::default(),
        }
    }
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        HotkeyConfig {
            capture: DEFAULT_CAPTURE_HOTKEY.to_string(),
        }
    }
}

impl Default for BallConfig {
    fn default() -> Self {
        BallConfig {
            enabled: true,
            size: DEFAULT_BALL_SIZE,
            positions: HashMap::new(),
            edge_hide: true,
            hide_in_fullscreen: true,
            guide_shown: false,
        }
    }
}

impl Default for CaptureConfig {
    fn default() -> Self {
        CaptureConfig {
            realtime_fallback: true,
        }
    }
}

impl AppConfig {
    /// 读取配置；文件不存在或无法解析时返回默认值。
    ///
    /// 解析失败会把坏文件改名为 `config.json.bad-<unix>`（改名失败也只记录日志），
    /// 保证下次保存能覆盖出可用配置。
    pub fn load_or_default(path: &Path) -> AppConfig {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return AppConfig::default(),
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "failed to read config; using defaults");
                return AppConfig::default();
            }
        };

        match serde_json::from_str(&text) {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(path = %path.display(), %err, "failed to parse config; using defaults");
                files::backup_corrupt(path);
                AppConfig::default()
            }
        }
    }

    /// 原子写入配置：`config.json.tmp` → `sync_all` → `rename`。
    pub fn save_atomic(&self, path: &Path) -> Result<(), AppError> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|err| AppError::Internal(format!("serialize config: {err}")))?;
        files::write_atomic(path, &bytes)
    }
}
