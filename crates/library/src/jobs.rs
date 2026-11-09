//! `jobs` 表的状态机原语（design §6.2、D3-02）。
//!
//! 阶段划分：`ocr`（仅图片）→ `index`（嵌入+FTS+insights 同事务写）→ `vlm`；另有
//! `rebuild`（蓝绿/修复）。claim 用单条 `UPDATE ... RETURNING` 原子取任务，两个 worker
//! 并发下不会重复执行同一条。
//!
//! 状态语义：
//! - `queued` 待执行；`running` 执行中（崩溃后启动时回退为 `queued`，attempts 不清零）；
//! - `succeeded` / `failed`（终态，attempts 用尽或配置错误）/ `cancelled`（revision 过期）。

use rusqlite::{params, OptionalExtension, Row};
use visrecall_domain::{now_millis, AppError};

use crate::db::{db_error, Library};

/// 任务状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// 一条任务（`jobs` 表一行）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub item_id: String,
    pub stage: String,
    pub revision: i64,
    pub state: JobState,
    pub priority: i64,
    /// 已尝试次数（claim 时 +1，所以第一次执行时是 1）。
    pub attempts: i64,
    pub next_run_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

const JOB_COLUMNS: &str = "id, item_id, stage, revision, state, priority, attempts, next_run_at, \
                           last_error, created_at, updated_at";

impl Library {
    /// 原子 claim 一条到期的任务（`priority` 小的优先，其次 `next_run_at`）。
    pub fn claim_next_job(&self, now_ms: i64) -> Result<Option<Job>, AppError> {
        let conn = self.conn()?;
        let job = conn
            .query_row(
                "UPDATE jobs
                    SET state = 'running', attempts = attempts + 1, updated_at = ?1
                  WHERE id = (
                        SELECT id FROM jobs
                         WHERE state = 'queued'
                           AND (next_run_at IS NULL OR next_run_at <= ?1)
                         ORDER BY priority ASC, COALESCE(next_run_at, 0) ASC, rowid ASC
                         LIMIT 1
                  )
              RETURNING id, item_id, stage, revision, state, priority, attempts, next_run_at,
                        last_error, created_at, updated_at",
                [now_ms],
                row_to_job,
            )
            .optional()
            .map_err(db_error)?;
        Ok(job)
    }

    /// `(queued, running)` 水位。
    pub fn job_counts(&self) -> Result<(u64, u64), AppError> {
        let conn = self.conn()?;
        let queued: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE state = 'queued'",
                [],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        let running: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE state = 'running'",
                [],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        Ok((queued as u64, running as u64))
    }

    /// 入队（revision 取 `items.revision` 当前值）。
    ///
    /// 幂等：同一 `(item_id, stage, revision)` 已存在时，只有处于终态（`failed`/`cancelled`/
    /// `succeeded`）才会被重置为 `queued` 且 `attempts` 归零——这正是「pending 素材重试一轮」
    /// 需要的语义；已在排队/执行中的任务不动，避免把执行中的任务打回队列导致重复执行。
    pub fn enqueue_stage(
        &self,
        item_id: &str,
        stage: &str,
        priority: i64,
    ) -> Result<i64, AppError> {
        let revision = self.item_revision(item_id)?.ok_or(AppError::NotFound)?;
        self.enqueue_stage_at(item_id, stage, revision, priority)?;
        Ok(revision)
    }

    /// 指定 revision 入队（调用方已读好 revision，避免多读一次）。
    pub fn enqueue_stage_at(
        &self,
        item_id: &str,
        stage: &str,
        revision: i64,
        priority: i64,
    ) -> Result<(), AppError> {
        let conn = self.conn()?;
        enqueue_tx(&conn, item_id, stage, revision, priority, now_millis())
    }

    /// `bump_revision` + 入队**同一事务**（design §7.6 的顺序不变量）。
    ///
    /// 拆成两次调用的话，进程在中间崩溃会留下「revision 已增但任务没入队」的半状态——
    /// 新 revision 的索引永远不会被计算，而旧任务又因 revision 不匹配被取消。
    pub fn bump_revision_and_enqueue(
        &self,
        item_id: &str,
        stage: &str,
        priority: i64,
    ) -> Result<i64, AppError> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        let revision = Library::bump_revision_tx(&tx, item_id)?;
        enqueue_tx(&tx, item_id, stage, revision, priority, now_millis())?;
        tx.commit().map_err(db_error)?;
        Ok(revision)
    }

    /// 素材当前 revision；素材不存在返回 `Ok(None)`。
    pub fn item_revision(&self, item_id: &str) -> Result<Option<i64>, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT revision FROM items WHERE id = ?1",
            [item_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)
    }

    /// 任务成功（终态）。
    pub fn finish_job_succeeded(&self, id: &str) -> Result<(), AppError> {
        self.set_job_state(id, JobState::Succeeded, None, None)
    }

    /// 任务失败（终态），记录最后一次错误。
    pub fn finish_job_failed(&self, id: &str, error: &str) -> Result<(), AppError> {
        self.set_job_state(id, JobState::Failed, Some(error), None)
    }

    /// 退避重试：回到 `queued` 并设置 `next_run_at`。
    pub fn retry_job(&self, id: &str, error: &str, next_run_at: i64) -> Result<(), AppError> {
        self.set_job_state(id, JobState::Queued, Some(error), Some(next_run_at))
    }

    /// revision 过期 → 取消。
    pub fn cancel_job(&self, id: &str, reason: &str) -> Result<(), AppError> {
        self.set_job_state(id, JobState::Cancelled, Some(reason), None)
    }

    fn set_job_state(
        &self,
        id: &str,
        state: JobState,
        error: Option<&str>,
        next_run_at: Option<i64>,
    ) -> Result<(), AppError> {
        let conn = self.conn()?;
        let updated = conn
            .execute(
                "UPDATE jobs SET state = ?2, last_error = COALESCE(?3, last_error),
                        next_run_at = ?4, updated_at = ?5
                 WHERE id = ?1",
                params![id, state.as_str(), error, next_run_at, now_millis()],
            )
            .map_err(db_error)?;
        if updated == 0 {
            return Err(AppError::NotFound);
        }
        Ok(())
    }

    /// 读取单条任务（测试与诊断用）。
    pub fn get_job(&self, id: &str) -> Result<Option<Job>, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1"),
            [id],
            row_to_job,
        )
        .optional()
        .map_err(db_error)
    }

    /// 某素材某阶段的所有任务（按 revision 降序；测试用）。
    pub fn jobs_for_stage(&self, item_id: &str, stage: &str) -> Result<Vec<Job>, AppError> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {JOB_COLUMNS} FROM jobs WHERE item_id = ?1 AND stage = ?2
                 ORDER BY revision DESC, rowid DESC"
            ))
            .map_err(db_error)?;
        let rows = stmt
            .query_map(params![item_id, stage], row_to_job)
            .map_err(db_error)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(db_error)?);
        }
        Ok(out)
    }

    /// 该 `content_hash` 是否已有可复用的解析结果（Task 2 的 `insight_by_content_hash`
    /// 的姊妹函数：这里返回素材 id，供 `ocr` handler 复用整段 insight）。
    pub fn item_id_by_content_hash(&self, content_hash: &str) -> Result<Option<String>, AppError> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT id FROM items WHERE content_hash = ?1 ORDER BY rowid ASC LIMIT 1",
            [content_hash],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)
    }
}

/// 入队 SQL（`Connection` 与 `Transaction` 都实现 `Deref<Target = Connection>`）。
fn enqueue_tx(
    conn: &rusqlite::Connection,
    item_id: &str,
    stage: &str,
    revision: i64,
    priority: i64,
    now: i64,
) -> Result<(), AppError> {
    conn.execute(
        "INSERT INTO jobs
           (id, item_id, stage, revision, state, priority, attempts, next_run_at, last_error,
            created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'queued', ?5, 0, NULL, NULL, ?6, ?6)
         ON CONFLICT(item_id, stage, revision) DO UPDATE SET
           state = 'queued',
           priority = excluded.priority,
           attempts = 0,
           next_run_at = NULL,
           last_error = NULL,
           updated_at = excluded.updated_at
         WHERE jobs.state IN ('failed', 'cancelled', 'succeeded')",
        params![
            uuid::Uuid::new_v4().to_string(),
            item_id,
            stage,
            revision,
            priority,
            now
        ],
    )
    .map_err(db_error)?;
    Ok(())
}

fn row_to_job(row: &Row<'_>) -> rusqlite::Result<Job> {
    let state: String = row.get(4)?;
    Ok(Job {
        id: row.get(0)?,
        item_id: row.get(1)?,
        stage: row.get(2)?,
        revision: row.get(3)?,
        state: JobState::parse(&state).unwrap_or(JobState::Failed),
        priority: row.get(5)?,
        attempts: row.get(6)?,
        next_run_at: row.get(7)?,
        last_error: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use visrecall_domain::{ItemKind, NewItem};

    fn temp_library() -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::open(dir.path().to_path_buf()).unwrap();
        (dir, lib)
    }

    fn insert_item(lib: &Library, hash: &str) -> String {
        lib.insert_item(NewItem {
            kind: ItemKind::Image,
            captured_at: 1,
            source_app: None,
            source_title: None,
            source_url: None,
            width: None,
            height: None,
            char_count: None,
            original_path: "originals/images/a.png".into(),
            content_hash: hash.into(),
        })
        .unwrap()
        .id
    }

    #[test]
    fn enqueue_uses_current_revision_and_is_idempotent() {
        let (_dir, lib) = temp_library();
        let id = insert_item(&lib, "h");

        assert_eq!(lib.enqueue_stage(&id, "ocr", 0).unwrap(), 0);
        assert_eq!(lib.enqueue_stage(&id, "ocr", 0).unwrap(), 0);
        assert_eq!(lib.jobs_for_stage(&id, "ocr").unwrap().len(), 1);

        lib.bump_revision(&id).unwrap();
        assert_eq!(lib.enqueue_stage(&id, "ocr", 0).unwrap(), 1);
        assert_eq!(lib.jobs_for_stage(&id, "ocr").unwrap().len(), 2);
    }

    #[test]
    fn claim_then_finish_transitions() {
        let (_dir, lib) = temp_library();
        let id = insert_item(&lib, "h");
        lib.enqueue_stage(&id, "index", 0).unwrap();

        let job = lib
            .claim_next_job(now_millis())
            .unwrap()
            .expect("应能 claim");
        assert_eq!(job.state, JobState::Running);
        assert_eq!(job.attempts, 1);
        assert_eq!(lib.job_counts().unwrap(), (0, 1));

        lib.finish_job_succeeded(&job.id).unwrap();
        let stored = lib.get_job(&job.id).unwrap().unwrap();
        assert_eq!(stored.state, JobState::Succeeded);
        assert!(lib.claim_next_job(now_millis()).unwrap().is_none());
    }

    #[test]
    fn retry_respects_next_run_at() {
        let (_dir, lib) = temp_library();
        let id = insert_item(&lib, "h");
        lib.enqueue_stage(&id, "index", 0).unwrap();
        let job = lib.claim_next_job(now_millis()).unwrap().unwrap();

        let later = now_millis() + 60_000;
        lib.retry_job(&job.id, "boom", later).unwrap();
        assert!(
            lib.claim_next_job(now_millis()).unwrap().is_none(),
            "未到期不该被 claim"
        );
        let again = lib.claim_next_job(later).unwrap().expect("到期后可 claim");
        assert_eq!(again.attempts, 2);
    }

    #[test]
    fn priority_orders_claims() {
        let (_dir, lib) = temp_library();
        let low = insert_item(&lib, "low");
        let high = insert_item(&lib, "high");
        lib.enqueue_stage(&low, "rebuild", 2).unwrap();
        lib.enqueue_stage(&high, "index", 0).unwrap();

        let first = lib.claim_next_job(now_millis()).unwrap().unwrap();
        assert_eq!(first.item_id, high, "priority 0 必须先执行");
        let second = lib.claim_next_job(now_millis()).unwrap().unwrap();
        assert_eq!(second.item_id, low);
    }

    #[test]
    fn reenqueue_resets_only_terminal_jobs() {
        let (_dir, lib) = temp_library();
        let id = insert_item(&lib, "h");
        lib.enqueue_stage(&id, "vlm", 0).unwrap();
        let job = lib.claim_next_job(now_millis()).unwrap().unwrap();
        assert_eq!(job.state, JobState::Running);

        // 执行中的任务被重复入队：不改变其状态（避免重复执行）
        lib.enqueue_stage(&id, "vlm", 0).unwrap();
        assert_eq!(
            lib.get_job(&job.id).unwrap().unwrap().state,
            JobState::Running
        );

        // 终态任务可以被重置（pending 重试轮）
        lib.finish_job_failed(&job.id, "3 attempts").unwrap();
        lib.enqueue_stage(&id, "vlm", 0).unwrap();
        let reset = lib.get_job(&job.id).unwrap().unwrap();
        assert_eq!(reset.state, JobState::Queued);
        assert_eq!(reset.attempts, 0);
        assert!(reset.last_error.is_none());
    }

    #[test]
    fn enqueue_for_missing_item_is_not_found() {
        let (_dir, lib) = temp_library();
        assert!(matches!(
            lib.enqueue_stage("nope", "index", 0),
            Err(AppError::NotFound)
        ));
    }

    #[test]
    fn cancel_and_fail_record_reason() {
        let (_dir, lib) = temp_library();
        let id = insert_item(&lib, "h");
        lib.enqueue_stage(&id, "index", 0).unwrap();
        let job = lib.claim_next_job(now_millis()).unwrap().unwrap();

        lib.cancel_job(&job.id, "stale revision").unwrap();
        let stored = lib.get_job(&job.id).unwrap().unwrap();
        assert_eq!(stored.state, JobState::Cancelled);
        assert_eq!(stored.last_error.as_deref(), Some("stale revision"));

        lib.finish_job_failed(&job.id, "config error").unwrap();
        let stored = lib.get_job(&job.id).unwrap().unwrap();
        assert_eq!(stored.state, JobState::Failed);
        assert_eq!(stored.last_error.as_deref(), Some("config error"));
    }

    #[test]
    fn recover_running_jobs_keeps_attempts() {
        let (_dir, lib) = temp_library();
        let id = insert_item(&lib, "h");
        lib.enqueue_stage(&id, "index", 0).unwrap();
        let job = lib.claim_next_job(now_millis()).unwrap().unwrap();
        assert_eq!(job.attempts, 1);

        lib.recover_jobs().unwrap();
        let stored = lib.get_job(&job.id).unwrap().unwrap();
        assert_eq!(stored.state, JobState::Queued);
        assert_eq!(stored.attempts, 1, "崩溃恢复不清零 attempts");
    }
}
