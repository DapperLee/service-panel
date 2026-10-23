use crate::types::{Desired, ServiceSpec, ServiceView, State};
use rusqlite::{params, Connection};
use std::path::Path;

/// 写库前对整条 spec_json 加密（含 env 密钥）。
fn pack_spec(spec: &ServiceSpec) -> String {
    let json = serde_json::to_string(spec).unwrap_or_else(|_| "{}".to_string());
    crate::secrets::encrypt(&json)
}

/// spec 解密/解析失败时的统一告警文案。
/// 既用于导出中止的提示，也作为 last_error 落到卡片上，让用户看得见。
pub const CONFIG_BROKEN_MSG: &str =
    "配置解析失败（DPAPI 不可用或数据损坏），请重新编辑保存；启动与导出已临时禁止";

/// 读库后解密 spec_json；兼容旧的明文记录。返回 (spec, 是否解析成功)。
fn unpack_spec(stored: &str) -> (ServiceSpec, bool) {
    let json = crate::secrets::decrypt(stored);
    match serde_json::from_str::<ServiceSpec>(&json) {
        Ok(s) => (s, true),
        Err(_) => (ServiceSpec::default(), false),
    }
}

/// 打开（或创建）SQLite 并跑迁移。
pub fn open(db_dir: &Path) -> rusqlite::Result<Connection> {
    std::fs::create_dir_all(db_dir).ok();
    let conn = Connection::open(db_dir.join("panel.db"))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

    if version < 1 {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS groups (
                id   TEXT PRIMARY KEY,
                name TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS services (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                group_id   TEXT,
                sort_order INTEGER NOT NULL DEFAULT 0,
                enabled    INTEGER NOT NULL DEFAULT 0,
                spec_json  TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS service_events (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                service_id TEXT NOT NULL,
                ts         INTEGER NOT NULL,
                from_state TEXT,
                to_state   TEXT NOT NULL,
                reason     TEXT,
                exit_code  INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_events_service ON service_events(service_id, ts);
            "#,
        )?;
    }

    if version < 2 {
        // 旧库的 groups.start_order 是只写不读的死列，存在则删除（拓扑顺序由 spec.after 派生）
        let has: bool = conn
            .prepare("PRAGMA table_info(groups)")?
            .query_map([], |r| r.get::<_, String>(1))?
            .flatten()
            .any(|col| col == "start_order");
        if has {
            conn.execute_batch("ALTER TABLE groups DROP COLUMN start_order;")?;
        }
    }

    if version < 3 {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS tasks (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL,
                grp         TEXT,
                config_json TEXT NOT NULL,
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS task_runs (
                id          TEXT PRIMARY KEY,
                task_id     TEXT NOT NULL,
                task_name   TEXT NOT NULL,
                variant     TEXT,
                extra_args  TEXT NOT NULL DEFAULT '',
                status      TEXT NOT NULL,
                exit_code   INTEGER,
                started_at  INTEGER NOT NULL,
                finished_at INTEGER,
                duration_ms INTEGER,
                log_path    TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_runs_task ON task_runs(task_id, started_at);

            CREATE TABLE IF NOT EXISTS bookmarks (
                id   TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                path TEXT NOT NULL
            );
            "#,
        )?;
    }

    conn.pragma_update(None, "user_version", 3)?;
    Ok(())
}

/// 清理过期事件：删除 keep_days 之前的记录，并限制总量上限。
pub fn prune_events(conn: &Connection, keep_days: i64, max_rows: i64) {
    let cutoff = now_ts() - keep_days * 86400;
    conn.execute("DELETE FROM service_events WHERE ts < ?1", params![cutoff])
        .ok();
    // 兜底：即便时间没到保留期，也只留最近 max_rows 条，防 flapping 撑爆
    conn.execute(
        "DELETE FROM service_events WHERE id NOT IN (
            SELECT id FROM service_events ORDER BY ts DESC LIMIT ?1
        )",
        params![max_rows],
    )
    .ok();
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

fn row_to_view(
    id: String,
    name: String,
    group_id: Option<String>,
    sort_order: i64,
    enabled: bool,
    spec_json: String,
) -> ServiceView {
    let (spec, config_ok) = unpack_spec(&spec_json);
    ServiceView {
        id,
        name,
        group_id,
        sort_order,
        enabled,
        spec,
        state: State::Stopped,
        desired: Desired::Stopped,
        pid: None,
        cpu: None,
        mem_mb: None,
        last_error: if config_ok {
            None
        } else {
            Some(CONFIG_BROKEN_MSG.to_string())
        },
        config_ok,
    }
}

pub fn list_services(conn: &Connection) -> rusqlite::Result<Vec<ServiceView>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, group_id, sort_order, enabled, spec_json
         FROM services ORDER BY sort_order ASC, name ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, bool>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, group_id, sort_order, enabled, spec_json) = row?;
        out.push(row_to_view(
            id, name, group_id, sort_order, enabled, spec_json,
        ));
    }
    Ok(out)
}

/// services 表的轻量行：(id, name, group_id, sort_order, enabled)。
/// 不解密 spec，供轮询与拓扑排序这类只要静态列的场景用。
pub type StaticRow = (String, String, Option<String>, i64, bool);

/// 轻量查询：只取静态列，不解密 spec（避免每轮轮询重复 DPAPI）。
/// 动态状态与 spec 由内存 Runtime 提供。
pub fn list_static(conn: &Connection) -> rusqlite::Result<Vec<StaticRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, group_id, sort_order, enabled
         FROM services ORDER BY sort_order ASC, name ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, i64>(3)?,
            r.get::<_, bool>(4)?,
        ))
    })?;
    rows.collect()
}

pub fn insert_service(
    conn: &Connection,
    id: &str,
    name: &str,
    group_id: Option<&str>,
    sort_order: i64,
    enabled: bool,
    spec: &ServiceSpec,
) -> rusqlite::Result<()> {
    let spec_json = pack_spec(spec);
    let ts = now_ts();
    conn.execute(
        "INSERT INTO services (id, name, group_id, sort_order, enabled, spec_json, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![id, name, group_id, sort_order, enabled, spec_json, ts, ts],
    )?;
    Ok(())
}

pub fn update_service(
    conn: &Connection,
    id: &str,
    name: &str,
    group_id: Option<&str>,
    sort_order: i64,
    enabled: bool,
    spec: &ServiceSpec,
) -> rusqlite::Result<()> {
    let spec_json = pack_spec(spec);
    let ts = now_ts();
    conn.execute(
        "UPDATE services SET name=?2, group_id=?3, sort_order=?4, enabled=?5, spec_json=?6, updated_at=?7
         WHERE id=?1",
        params![id, name, group_id, sort_order, enabled, spec_json, ts],
    )?;
    Ok(())
}

pub fn delete_service(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM services WHERE id=?1", params![id])?;
    conn.execute(
        "DELETE FROM service_events WHERE service_id=?1",
        params![id],
    )?;
    Ok(())
}

pub fn record_event(
    conn: &Connection,
    service_id: &str,
    from_state: Option<&str>,
    to_state: &str,
    reason: Option<&str>,
    exit_code: Option<i32>,
) {
    let ts = now_ts();
    conn.execute(
        "INSERT INTO service_events (service_id, ts, from_state, to_state, reason, exit_code)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![service_id, ts, from_state, to_state, reason, exit_code],
    )
    .ok();
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GroupRow {
    pub id: String,
    pub name: String,
}

pub fn list_groups(conn: &Connection) -> rusqlite::Result<Vec<GroupRow>> {
    let mut stmt = conn.prepare("SELECT id, name FROM groups ORDER BY name ASC")?;
    let rows = stmt.query_map([], |r| {
        Ok(GroupRow {
            id: r.get(0)?,
            name: r.get(1)?,
        })
    })?;
    rows.collect()
}

pub fn create_group(conn: &Connection, id: &str, name: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO groups (id, name) VALUES (?1,?2)",
        params![id, name],
    )?;
    Ok(())
}

pub fn rename_group(conn: &Connection, id: &str, name: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE groups SET name=?2 WHERE id=?1", params![id, name])?;
    Ok(())
}

/// 删除分组，并把组内服务的 group_id 置空（不删服务）
pub fn delete_group(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE services SET group_id=NULL WHERE group_id=?1",
        params![id],
    )?;
    conn.execute("DELETE FROM groups WHERE id=?1", params![id])?;
    Ok(())
}

/// 事件表行：(service_id, ts, from_state, to_state, reason, exit_code)
pub type EventRow = (
    String,
    i64,
    Option<String>,
    String,
    Option<String>,
    Option<i32>,
);

pub fn recent_events(conn: &Connection, limit: i64) -> rusqlite::Result<Vec<EventRow>> {
    let mut stmt = conn.prepare(
        "SELECT service_id, ts, from_state, to_state, reason, exit_code
         FROM service_events ORDER BY ts DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<i32>>(5)?,
        ))
    })?;
    rows.collect()
}

// ============================ 任务 / 运行 / 书签 ============================

fn pack_config(cfg: &crate::types::TaskConfig) -> String {
    let json = serde_json::to_string(cfg).unwrap_or_else(|_| "{}".to_string());
    crate::secrets::encrypt(&json)
}

fn unpack_config(stored: &str) -> (crate::types::TaskConfig, bool) {
    let json = crate::secrets::decrypt(stored);
    match serde_json::from_str::<crate::types::TaskConfig>(&json) {
        Ok(c) => (c, true),
        Err(_) => (crate::types::TaskConfig::default(), false),
    }
}

pub fn list_tasks(conn: &Connection) -> rusqlite::Result<Vec<crate::types::TaskView>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, grp, config_json FROM tasks ORDER BY name ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, grp, cfg_json) = row?;
        let (config, config_ok) = unpack_config(&cfg_json);
        out.push(crate::types::TaskView {
            id,
            name,
            group: grp,
            config,
            config_ok,
        });
    }
    Ok(out)
}

pub fn insert_task(
    conn: &Connection,
    id: &str,
    name: &str,
    group: Option<&str>,
    config: &crate::types::TaskConfig,
) -> rusqlite::Result<()> {
    let ts = now_ts();
    conn.execute(
        "INSERT INTO tasks (id, name, grp, config_json, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6)",
        params![id, name, group, pack_config(config), ts, ts],
    )?;
    Ok(())
}

pub fn update_task(
    conn: &Connection,
    id: &str,
    name: &str,
    group: Option<&str>,
    config: &crate::types::TaskConfig,
) -> rusqlite::Result<()> {
    let ts = now_ts();
    conn.execute(
        "UPDATE tasks SET name=?2, grp=?3, config_json=?4, updated_at=?5 WHERE id=?1",
        params![id, name, group, pack_config(config), ts],
    )?;
    Ok(())
}

pub fn delete_task(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM tasks WHERE id=?1", params![id])?;
    conn.execute("DELETE FROM task_runs WHERE task_id=?1", params![id])?;
    Ok(())
}

/// 插入一条 queued 运行记录
pub fn insert_task_run(
    conn: &Connection,
    run: &crate::types::TaskRun,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO task_runs (id, task_id, task_name, variant, extra_args, status, exit_code, started_at, finished_at, duration_ms, log_path)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            run.id,
            run.task_id,
            run.task_name,
            run.variant,
            run.extra_args,
            status_str(run.status),
            run.exit_code,
            run.started_at,
            run.finished_at,
            run.duration_ms,
            run.log_path
        ],
    )?;
    Ok(())
}

fn status_str(s: crate::types::TaskRunStatus) -> &'static str {
    use crate::types::TaskRunStatus::*;
    match s {
        Queued => "queued",
        Running => "running",
        Success => "success",
        Failed => "failed",
        Cancelled => "cancelled",
    }
}

fn status_from(s: &str) -> crate::types::TaskRunStatus {
    use crate::types::TaskRunStatus::*;
    match s {
        "running" => Running,
        "success" => Success,
        "failed" => Failed,
        "cancelled" => Cancelled,
        _ => Queued,
    }
}

pub fn set_task_run_running(conn: &Connection, id: &str, started_at: i64, log_path: &str) {
    conn.execute(
        "UPDATE task_runs SET status='running', started_at=?2, log_path=?3 WHERE id=?1",
        params![id, started_at, log_path],
    )
    .ok();
}

pub fn finish_task_run(
    conn: &Connection,
    id: &str,
    status: crate::types::TaskRunStatus,
    exit_code: Option<i32>,
    finished_at: i64,
    duration_ms: i64,
) {
    conn.execute(
        "UPDATE task_runs SET status=?2, exit_code=?3, finished_at=?4, duration_ms=?5 WHERE id=?1",
        params![id, status_str(status), exit_code, finished_at, duration_ms],
    )
    .ok();
}

pub fn list_task_runs(
    conn: &Connection,
    limit: i64,
) -> rusqlite::Result<Vec<crate::types::TaskRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, task_id, task_name, variant, extra_args, status, exit_code, started_at, finished_at, duration_ms, COALESCE(log_path,'')
         FROM task_runs ORDER BY started_at DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |r| {
        let status: String = r.get(5)?;
        Ok(crate::types::TaskRun {
            id: r.get(0)?,
            task_id: r.get(1)?,
            task_name: r.get(2)?,
            variant: r.get(3)?,
            extra_args: r.get(4)?,
            status: status_from(&status),
            exit_code: r.get(6)?,
            started_at: r.get(7)?,
            finished_at: r.get(8)?,
            duration_ms: r.get(9)?,
            log_path: r.get(10)?,
        })
    })?;
    rows.collect()
}

pub fn list_bookmarks(conn: &Connection) -> rusqlite::Result<Vec<crate::types::Bookmark>> {
    let mut stmt = conn.prepare("SELECT id, name, path FROM bookmarks ORDER BY name ASC")?;
    let rows = stmt.query_map([], |r| {
        Ok(crate::types::Bookmark {
            id: r.get(0)?,
            name: r.get(1)?,
            path: r.get(2)?,
        })
    })?;
    rows.collect()
}

pub fn insert_bookmark(conn: &Connection, id: &str, name: &str, path: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO bookmarks (id, name, path) VALUES (?1,?2,?3)",
        params![id, name, path],
    )?;
    Ok(())
}

pub fn delete_bookmark(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM bookmarks WHERE id=?1", params![id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("sp-db-{}-{}", tag, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 手造一份「上一版」的库：`user_version=1`、groups 还带着 start_order 死列，
    /// 服务一条走加密、一条留明文（对应还没加密时存下去的老记录）。
    fn write_v1_db(dir: &std::path::Path) {
        let conn = Connection::open(dir.join("panel.db")).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE groups (
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                start_order INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE services (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, group_id TEXT,
                sort_order INTEGER NOT NULL DEFAULT 0, enabled INTEGER NOT NULL DEFAULT 0,
                spec_json TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
            );
            CREATE TABLE service_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT, service_id TEXT NOT NULL,
                ts INTEGER NOT NULL, from_state TEXT, to_state TEXT NOT NULL,
                reason TEXT, exit_code INTEGER
            );
            PRAGMA user_version=1;
            "#,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO groups (id, name, start_order) VALUES ('g1','旧分组',7)",
            [],
        )
        .unwrap();
        let mut env = HashMap::new();
        env.insert("TOKEN".to_string(), "v1-secret".to_string());
        let spec = ServiceSpec {
            env,
            ..ServiceSpec::default()
        };
        insert_service(&conn, "s1", "加密旧服务", Some("g1"), 1, true, &spec).unwrap();
        // 老版本还没加密时写下的明文记录：当年也是完整 JSON，不是空对象
        let legacy = ServiceSpec {
            command: "node".to_string(),
            args: vec!["server.js".to_string()],
            ..ServiceSpec::default()
        };
        conn.execute(
            "INSERT INTO services (id, name, group_id, sort_order, enabled, spec_json, created_at, updated_at)
             VALUES ('s2','明文旧服务',NULL,2,0,?1,1,1)",
            params![serde_json::to_string(&legacy).unwrap()],
        )
        .unwrap();
    }

    /// 用户机器上的老库必须能被新版打开：版本号推进、死列删掉、数据一行不少。
    /// 这条挂了就是「升级即丢配置」，没有第二次机会。
    #[test]
    fn v1_db_upgrades_without_losing_rows() {
        let dir = temp_dir("v1");
        write_v1_db(&dir);

        let conn = open(&dir).unwrap();

        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 2, "迁移后 user_version 应推进到 2");

        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(groups)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .flatten()
            .collect();
        assert!(
            !cols.contains(&"start_order".to_string()),
            "start_order 是只写不读的死列，迁移应删掉；实际列：{cols:?}"
        );

        let groups = list_groups(&conn).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "旧分组", "删列不能把整行一起带走");

        let services = list_services(&conn).unwrap();
        assert_eq!(services.len(), 2);
        let enc = services.iter().find(|s| s.id == "s1").unwrap();
        assert!(enc.config_ok, "加密的老 spec 应能被新版解密");
        assert_eq!(enc.spec.env.get("TOKEN").map(|s| s.as_str()), Some("v1-secret"));
        let plain = services.iter().find(|s| s.id == "s2").unwrap();
        assert!(plain.config_ok, "明文老记录也要能读，不能被判成配置损坏");
        assert_eq!(plain.spec.command, "node");
        assert_eq!(plain.spec.args, vec!["server.js".to_string()]);

        // 迁移必须幂等：第二次打开不能因为列已经没了而报错
        drop(conn);
        let again = open(&dir).unwrap();
        assert_eq!(list_groups(&again).unwrap().len(), 1);

        drop(again);
        std::fs::remove_dir_all(&dir).ok();
    }
}
