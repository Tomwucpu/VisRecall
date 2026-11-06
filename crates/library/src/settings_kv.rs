//! `settings` 表的键值读写（design §4.3）。
//!
//! 策略类配置（容量、保留天数）属于「这个库」而不是「这台机器」，所以存在库内而不是
//! `config.json`：换库后策略随库走，不会张冠李戴。
//!
//! 读取刻意宽容：值不是合法 JSON、类型不匹配或数据库读失败都返回 `None` 并记 `warn`，
//! 由调用方回落到默认值——一条坏配置不该卡住整个设置页。

use rusqlite::{params, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::Serialize;
use visrecall_domain::AppError;

use crate::db::{db_error, Library};

impl Library {
    /// 读取并反序列化 `key` 对应的值；缺失、损坏或读失败一律返回 `None`。
    pub fn setting_get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let conn = match self.conn() {
            Ok(conn) => conn,
            Err(err) => {
                tracing::warn!(key, %err, "setting read failed: connection unavailable");
                return None;
            }
        };

        let raw: Option<String> = match conn
            .query_row(
                "SELECT value_json FROM settings WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()
        {
            Ok(raw) => raw,
            Err(err) => {
                tracing::warn!(key, %err, "setting read failed");
                return None;
            }
        };

        let raw = raw?;
        match serde_json::from_str(&raw) {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::warn!(key, raw, %err, "setting value is not valid json");
                None
            }
        }
    }

    /// 写入（覆盖）`key` 的值，序列化为 JSON 存储。
    pub fn setting_set<T: Serialize>(&self, key: &str, value: &T) -> Result<(), AppError> {
        let json = serde_json::to_string(value)
            .map_err(|err| AppError::Internal(format!("serialize setting {key}: {err}")))?;
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value_json) VALUES (?1, ?2)",
            params![key, json],
        )
        .map_err(db_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_library() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path().to_path_buf()).unwrap();
        (dir, lib)
    }

    #[test]
    fn setting_round_trips() {
        let (_dir, lib) = temp_library();
        lib.setting_set("storage.capacity_gb", &50u32).unwrap();
        assert_eq!(lib.setting_get::<u32>("storage.capacity_gb"), Some(50));

        lib.setting_set("storage.capacity_gb", &128u32).unwrap();
        assert_eq!(lib.setting_get::<u32>("storage.capacity_gb"), Some(128));
    }

    #[test]
    fn setting_supports_option_payloads() {
        let (_dir, lib) = temp_library();
        lib.setting_set("storage.retention_days", &None::<u32>)
            .unwrap();
        assert_eq!(
            lib.setting_get::<Option<u32>>("storage.retention_days"),
            Some(None)
        );

        lib.setting_set("storage.retention_days", &Some(90u32))
            .unwrap();
        assert_eq!(
            lib.setting_get::<Option<u32>>("storage.retention_days"),
            Some(Some(90))
        );
    }

    #[test]
    fn missing_key_returns_none() {
        let (_dir, lib) = temp_library();
        assert_eq!(lib.setting_get::<u32>("does.not.exist"), None);
    }

    #[test]
    fn invalid_json_returns_none() {
        let (_dir, lib) = temp_library();
        let conn = lib.conn().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value_json) VALUES ('broken', 'not json')",
            [],
        )
        .unwrap();
        drop(conn);

        assert_eq!(lib.setting_get::<u32>("broken"), None);
    }

    #[test]
    fn type_mismatch_returns_none() {
        let (_dir, lib) = temp_library();
        lib.setting_set("storage.capacity_gb", &"fifty").unwrap();
        assert_eq!(lib.setting_get::<u32>("storage.capacity_gb"), None);
    }
}
