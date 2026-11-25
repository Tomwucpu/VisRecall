//! 与 NSIS 交接首次安装的素材库路径；双方使用当前用户注册表的 64 位视图。

use std::ffi::OsString;
use std::io::{self, ErrorKind};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use visrecall_domain::AppError;
use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_SZ};
use winreg::RegKey;

const INSTALLER_KEY: &str = r"Software\VisRecall\Installer";
const PENDING_PATH: &str = "PendingLibraryPath";

/// 只有缺少键或值才返回空；读失败时保留原值并交给启动流程处理。
pub fn read_pending() -> Result<Option<PathBuf>, AppError> {
    let read = || {
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(INSTALLER_KEY, KEY_QUERY_VALUE | KEY_WOW64_64KEY)?;
        let raw = key.get_raw_value(PENDING_PATH)?;
        if raw.vtype != REG_SZ || raw.bytes.len() % 2 != 0 {
            return Err(io::Error::new(ErrorKind::InvalidData, "素材库路径类型无效"));
        }
        // 保持 Windows 路径的原始 UTF-16 单元，并拒绝内嵌 NUL。
        let mut wide: Vec<_> = raw
            .bytes
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect();
        if wide.pop() != Some(0) || wide.is_empty() || wide.contains(&0) {
            return Err(io::Error::new(ErrorKind::InvalidData, "素材库路径内容无效"));
        }
        Ok(PathBuf::from(OsString::from_wide(&wide)))
    };
    match read() {
        Ok(path) => Ok(Some(path)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(AppError::Io(format!("无法读取安装时选择的素材库：{err}"))),
    }
}

pub fn clear_pending() -> io::Result<()> {
    let result = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(INSTALLER_KEY, KEY_SET_VALUE | KEY_WOW64_64KEY)
        .and_then(|key| key.delete_value(PENDING_PATH));
    match result {
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
