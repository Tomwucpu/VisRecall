//! 应用配置（`config.json`）：带默认值的 serde 结构与原子持久化。
//!
//! 模型配置是「任意数量的自定义模型 + active 指针」（原型 §模型 pane），
//! 不含密钥：密钥按模型 id 单独存 Windows 凭据管理器。
//! 旧版 `primary` / `backup` 双槽位配置由 [`ModelConfig::normalize`] 就地迁移。

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
pub const DEFAULT_BALL_SIZE: u32 = 56;
/// 悬浮球尺寸范围；与壳层的 `BALL_MIN_SIZE` / `BALL_MAX_SIZE` 保持一致。
pub const BALL_MIN_SIZE: u32 = 50;
pub const BALL_MAX_SIZE: u32 = 150;
/// 支持的通信协议标识（与原型 `PROTOS` 及 `Protocol::parse` 同口径）。
pub const MODEL_PROTOCOLS: [&str; 4] = ["openai", "anthropic", "ollama", "rest"];

/// 应用冷启动时是否自动打开主窗口；不影响后台运行或手动唤醒。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupMode {
    #[default]
    Silent,
    ShowWindow,
}

/// 仅磁盘配置容忍未知值，避免一个启动偏好使其它配置整体回退。
fn deserialize_startup_mode<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<StartupMode, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    match StartupMode::deserialize(value) {
        Ok(mode) => Ok(mode),
        Err(_) => {
            tracing::warn!("invalid startup_mode in config; using silent startup");
            Ok(StartupMode::default())
        }
    }
}

/// 应用配置，字段见 design §10.3；缺失字段一律回落到 [`AppConfig::default`]。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub library_path: PathBuf,
    #[serde(default, deserialize_with = "deserialize_startup_mode")]
    pub startup_mode: StartupMode,
    pub hotkeys: HotkeyConfig,
    pub ball: BallConfig,
    pub capture: CaptureConfig,
    pub privacy: PrivacyConfig,
    /// 自定义模型列表与当前启用项（**不含密钥**）。
    pub models: ModelConfig,
}

/// 云端模型配置（机器级，跟安装走；design D3-07）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    /// 用户定义的模型，顺序即界面列表顺序。
    pub entries: Vec<ModelEntry>,
    /// 当前启用的模型 id；`None` = 未配置。
    pub active_id: Option<String>,
    /// 旧版主槽位：**只读**，读入后由 `normalize()` 转成 `entries`，写出时忽略。
    #[serde(skip_serializing)]
    pub primary: Option<ProviderEntry>,
    /// 旧版备槽位：同上。
    #[serde(skip_serializing)]
    pub backup: Option<ProviderEntry>,
}

impl ModelConfig {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() && self.primary.is_none() && self.backup.is_none()
    }

    /// 旧配置迁移：`primary` / `backup` → id 为 `primary` / `backup` 的两个模型。
    ///
    /// 这两个 id 生成的凭据 account 恰好仍是 `vlm-primary` / `vlm-backup`，
    /// 所以旧密钥无需搬移即可继续使用。
    pub fn normalize(&mut self) {
        if self.primary.is_none() && self.backup.is_none() {
            return;
        }
        if self.entries.is_empty() {
            if let Some(entry) = self.primary.take() {
                self.entries
                    .push(ModelEntry::from_legacy("primary", "主模型", entry));
            }
            if let Some(entry) = self.backup.take() {
                self.entries
                    .push(ModelEntry::from_legacy("backup", "备用模型", entry));
            }
            if let Some(first) = self.entries.first() {
                self.active_id = Some(first.id.clone());
            }
        } else {
            self.primary = None;
            self.backup = None;
        }
    }

    /// 当前启用的模型；`active_id` 失效时回落到列表第一项。
    pub fn active_entry(&self) -> Option<&ModelEntry> {
        match self.active_id.as_deref() {
            Some(id) => self
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .or_else(|| self.entries.first()),
            None => self.entries.first(),
        }
    }

    pub fn find(&self, id: &str) -> Option<&ModelEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }
}

/// 一个自定义模型（显示名称 / 模型 ID / 地址 / 协议；**不含密钥**）。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelEntry {
    /// 稳定 id，同时决定凭据 account（`vlm-<id>`）。
    pub id: String,
    /// 用户给定的显示名称。
    pub name: String,
    /// 模型 ID（发给接口的 `model` 字段）。
    pub model: String,
    /// 接口基地址。
    pub base_url: String,
    /// `MODEL_PROTOCOLS` 之一。
    pub protocol: String,
    /// `rest` 协议的请求路径标识；其它协议为空。
    pub protocol_id: String,
}

impl ModelEntry {
    /// 旧版双槽位 → 新列表项；沿用 OpenAI 兼容协议。
    fn from_legacy(id: &str, name: &str, entry: ProviderEntry) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            model: entry.model,
            base_url: entry.base_url,
            protocol: "openai".to_string(),
            protocol_id: String::new(),
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.id.trim().is_empty() {
            return Err(AppError::InvalidInput("model id is empty".to_string()));
        }
        if self.name.trim().is_empty() {
            return Err(AppError::InvalidInput("display name is empty".to_string()));
        }
        if self.model.trim().is_empty() {
            return Err(AppError::InvalidInput("model name is empty".to_string()));
        }
        let url = self.base_url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(AppError::InvalidInput(
                "base URL must start with http:// or https://".to_string(),
            ));
        }
        if !MODEL_PROTOCOLS.contains(&self.protocol.as_str()) {
            return Err(AppError::InvalidInput(format!(
                "unknown protocol: {}",
                self.protocol
            )));
        }
        if self.protocol == "rest" && self.protocol_id.trim().is_empty() {
            return Err(AppError::InvalidInput(
                "custom REST requires a protocol id".to_string(),
            ));
        }
        Ok(())
    }
}

/// 旧版槽位结构（只有地址与模型名），仅用于反序列化 `config.json`。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderEntry {
    pub base_url: String,
    pub model: String,
}

impl ProviderEntry {
    /// 最小可用性校验（旧版语义，保留给迁移路径使用）。
    pub fn validate(&self) -> Result<(), AppError> {
        let url = self.base_url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err(AppError::InvalidInput(
                "base URL must start with http:// or https://".to_string(),
            ));
        }
        if self.model.trim().is_empty() {
            return Err(AppError::InvalidInput("model name is empty".to_string()));
        }
        Ok(())
    }
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
            startup_mode: StartupMode::default(),
            hotkeys: HotkeyConfig::default(),
            ball: BallConfig::default(),
            capture: CaptureConfig::default(),
            privacy: PrivacyConfig::default(),
            models: ModelConfig::default(),
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

        match serde_json::from_str::<AppConfig>(&text) {
            Ok(mut config) => {
                config.models.normalize();
                // 旧配置可能保存了 40/48：加载时夹到新范围，不额外写回磁盘。
                config.ball.size = config.ball.size.clamp(BALL_MIN_SIZE, BALL_MAX_SIZE);
                config
            }
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
