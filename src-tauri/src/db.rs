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

    conn.pragma_update(None, "user_version", 2)?;
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
        cpu: 0.0,
        mem_mb: 0.0,
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
