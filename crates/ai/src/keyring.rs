//! API Key 存取：**只**进 Windows 凭据管理器（design D3-07、P3-R11）。
//!
//! - service 固定 `VisRecall`，account 分 `vlm-primary` / `vlm-backup`；
//! - 密钥绝不入库文件、`config.json`、日志与任何 IPC 响应；
//! - 本模块的错误消息只描述「哪个槽位 / 什么类型错误」，不回显密钥内容。

use visrecall_domain::AppError;

/// 凭据管理器的 service 名。
pub const SERVICE: &str = "VisRecall";

/// 两个配置槽位。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySlot {
    Primary,
    Backup,
}

impl KeySlot {
    /// 凭据管理器里的 account 名。
    pub fn account(self) -> &'static str {
        match self {
            Self::Primary => "vlm-primary",
            Self::Backup => "vlm-backup",
        }
    }

    /// 从 IPC 字符串解析（`"primary"` / `"backup"`，也接受 `vlm-primary` / `vlm-backup`）。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "primary" | "vlm-primary" => Some(Self::Primary),
            "backup" | "vlm-backup" => Some(Self::Backup),
            _ => None,
        }
    }
}

fn entry(slot: KeySlot) -> Result<keyring::Entry, AppError> {
    keyring::Entry::new(SERVICE, slot.account())
        .map_err(|e| AppError::Internal(format!("keyring: entry {}: {e}", slot.account())))
}

/// 写入（覆盖）密钥；空串等同于删除。
pub fn save_key(slot: KeySlot, key: &str) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return delete_key(slot);
    }
    entry(slot)?
        .set_password(key)
        .map_err(|e| AppError::Internal(format!("keyring: save {}: {e}", slot.account())))
}

/// 读取密钥；不存在返回 `Ok(None)`（不是错误）。
pub fn load_key(slot: KeySlot) -> Result<Option<String>, AppError> {
    match entry(slot)?.get_password() {
        Ok(password) if password.is_empty() => Ok(None),
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(AppError::Internal(format!(
            "keyring: load {}: {err}",
            slot.account()
        ))),
    }
}

/// 删除密钥（幂等：不存在也算成功）。
pub fn delete_key(slot: KeySlot) -> Result<(), AppError> {
    match entry(slot)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(AppError::Internal(format!(
            "keyring: delete {}: {err}",
            slot.account()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_account_names_are_stable() {
        assert_eq!(KeySlot::Primary.account(), "vlm-primary");
        assert_eq!(KeySlot::Backup.account(), "vlm-backup");
        assert_eq!(KeySlot::parse("primary"), Some(KeySlot::Primary));
        assert_eq!(KeySlot::parse("vlm-backup"), Some(KeySlot::Backup));
        assert_eq!(KeySlot::parse("nope"), None);
    }

    /// 真实凭据管理器往返（本机跑；CI 无凭据服务时跳过）。
    #[test]
    #[ignore = "touches the real Windows credential store"]
    fn roundtrip_against_real_credential_manager() {
        let slot = KeySlot::Backup;
        delete_key(slot).expect("清理旧值");
        assert_eq!(load_key(slot).unwrap(), None);

        save_key(slot, "sk-test-not-a-real-key").expect("写入凭据");
        assert_eq!(
            load_key(slot).unwrap().as_deref(),
            Some("sk-test-not-a-real-key")
        );

        save_key(slot, "sk-rotated").expect("覆盖写");
        assert_eq!(load_key(slot).unwrap().as_deref(), Some("sk-rotated"));

        delete_key(slot).expect("删除");
        assert_eq!(load_key(slot).unwrap(), None);
        // 幂等
        delete_key(slot).expect("重复删除应成功");
    }

    #[test]
    #[ignore = "touches the real Windows credential store"]
    fn empty_key_removes_the_entry() {
        let slot = KeySlot::Primary;
        save_key(slot, "sk-temp").expect("写入");
        save_key(slot, "   ").expect("空串应等价于删除");
        assert_eq!(load_key(slot).unwrap(), None);
        delete_key(slot).ok();
    }
}
