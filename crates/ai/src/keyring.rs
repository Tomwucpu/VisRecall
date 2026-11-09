//! API Key 存取：**只**进 Windows 凭据管理器（design D3-07、P3-R11）。
//!
//! - service 固定 `VisRecall`，account 按模型 id 生成 `vlm-<id>`；
//! - 旧版主备模型的 id 就是 `primary` / `backup`，account 仍是
//!   `vlm-primary` / `vlm-backup`，迁移后旧密钥无需搬移；
//! - 密钥绝不入库文件、`config.json`、日志与任何 IPC 响应；
//! - 本模块的错误消息只描述「哪个 account / 什么类型错误」，不回显密钥内容。

use visrecall_domain::AppError;

/// 凭据管理器的 service 名。
pub const SERVICE: &str = "VisRecall";

/// 一个模型对应的凭据 account（由模型 id 派生，稳定且可读）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySlot(String);

impl KeySlot {
    /// `vlm-<id>`；id 里非 `[A-Za-z0-9_-]` 的字符统一替换为 `-`，避免奇怪的 account 名。
    pub fn for_model(id: &str) -> Self {
        let safe: String = id
            .trim()
            .chars()
            .filter(|c| c.is_ascii())
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .take(64)
            .collect();
        let safe = if safe.is_empty() { "model".to_string() } else { safe };
        Self(format!("vlm-{safe}"))
    }

    pub fn account(&self) -> &str {
        &self.0
    }
}

fn entry(slot: &KeySlot) -> Result<keyring::Entry, AppError> {
    keyring::Entry::new(SERVICE, slot.account())
        .map_err(|e| AppError::Internal(format!("keyring: entry {}: {e}", slot.account())))
}

/// 写入（覆盖）密钥；空串等同于删除。
pub fn save_key(slot: &KeySlot, key: &str) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return delete_key(slot);
    }
    entry(slot)?
        .set_password(key)
        .map_err(|e| AppError::Internal(format!("keyring: save {}: {e}", slot.account())))
}

/// 读取密钥；不存在返回 `Ok(None)`（不是错误）。
pub fn load_key(slot: &KeySlot) -> Result<Option<String>, AppError> {
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
pub fn delete_key(slot: &KeySlot) -> Result<(), AppError> {
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
    fn account_names_are_stable_and_legacy_compatible() {
        assert_eq!(KeySlot::for_model("primary").account(), "vlm-primary");
        assert_eq!(KeySlot::for_model("backup").account(), "vlm-backup");
        assert_eq!(KeySlot::for_model("cm-123_abc").account(), "vlm-cm-123_abc");
        assert_eq!(KeySlot::for_model("a b/c").account(), "vlm-a-b-c");
        assert_eq!(KeySlot::for_model("").account(), "vlm-model");
    }

    /// 真实凭据管理器往返（本机跑；CI 无凭据服务时跳过）。
    #[test]
    #[ignore = "touches the real Windows credential store"]
    fn roundtrip_against_real_credential_manager() {
        let slot = KeySlot::for_model("test-model");
        delete_key(&slot).expect("清理旧值");
        assert_eq!(load_key(&slot).unwrap(), None);

        save_key(&slot, "sk-test-not-a-real-key").expect("写入凭据");
        assert_eq!(
            load_key(&slot).unwrap().as_deref(),
            Some("sk-test-not-a-real-key")
        );

        save_key(&slot, "sk-rotated").expect("覆盖写");
        assert_eq!(load_key(&slot).unwrap().as_deref(), Some("sk-rotated"));

        delete_key(&slot).expect("删除");
        assert_eq!(load_key(&slot).unwrap(), None);
        // 幂等
        delete_key(&slot).expect("重复删除应成功");
    }

    #[test]
    #[ignore = "touches the real Windows credential store"]
    fn empty_key_removes_the_entry() {
        let slot = KeySlot::for_model("test-empty-key");
        save_key(&slot, "sk-temp").expect("写入");
        save_key(&slot, "   ").expect("空串应等价于删除");
        assert_eq!(load_key(&slot).unwrap(), None);
        delete_key(&slot).ok();
    }
}
