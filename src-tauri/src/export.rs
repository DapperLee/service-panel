//! 配置导出/导入。导出文件里 spec 是**明文 JSON**（含 env 密钥），
//! 请提醒用户妥善保管。导入支持两种模式：merge（只加新 id）/ replace（清空重建）。
use crate::db;
use crate::manager;
use crate::state::AppState;
use crate::types::ServiceSpec;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const FORMAT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportGroup {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportService {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub enabled: bool,
    pub spec: ServiceSpec,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportFile {
    pub version: u32,
    pub exported_at: String,
    pub app_version: String,
    pub groups: Vec<ExportGroup>,
    pub services: Vec<ExportService>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub services_added: usize,
    pub services_skipped: usize,
    pub groups_added: usize,
    pub stopped: usize,
}

/// 导出所有分组与服务到指定路径（明文 JSON，pretty）。返回导出的服务数。
pub fn export_to_file(state: &Arc<AppState>, path: &str) -> Result<usize, String> {
    let (groups, services) = {
        let conn = state.db.lock().unwrap();
        let gs = db::list_groups(&conn).map_err(|e| e.to_string())?;
        let sv = db::list_services(&conn).map_err(|e| e.to_string())?;
        (gs, sv)
    };

    // 有服务配置解析失败（DPAPI 不可用/损坏）时中止导出，
    // 否则会把解出来的空 spec 当真实配置写进备份，等于静默清空。
    let broken: Vec<String> = services
        .iter()
        .filter(|s| !s.config_ok)
        .map(|s| s.name.clone())
        .collect();
    if !broken.is_empty() {
        return Err(format!(
            "有 {} 个服务配置解析失败（{}），已中止导出以免备份丢失配置",
            broken.len(),
            broken.join("、")
        ));
    }

    let file = ExportFile {
        version: FORMAT_VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        groups: groups
            .into_iter()
            .map(|g| ExportGroup { id: g.id, name: g.name })
            .collect(),
        services: services
            .into_iter()
            .map(|s| ExportService {
                id: s.id,
                name: s.name,
                group_id: s.group_id,
                sort_order: s.sort_order,
                enabled: s.enabled,
                spec: s.spec,
            })
            .collect(),
    };

    let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("写入失败: {e}"))?;
    Ok(file.services.len())
}

/// 从指定路径导入。replace=true 会先停所有服务并清空 DB 再重建。
pub fn import_from_file(
    state: &Arc<AppState>,
    path: &str,
    replace: bool,
) -> Result<ImportOutcome, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("读取失败: {e}"))?;
    let file: ExportFile =
        serde_json::from_str(&raw).map_err(|e| format!("解析失败（不是有效的导出文件）: {e}"))?;
    if file.version != FORMAT_VERSION {
        return Err(format!(
            "不支持的导出格式版本 {}（当前支持 {}）",
            file.version, FORMAT_VERSION
        ));
    }

    let mut stopped = 0usize;
    if replace {
        // 先停所有正在跑的服务（走 manager::stop 保证日志源卸载 + Job Object 回收）
        let ids: Vec<String> = state.runtimes.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let _ = manager::stop(state, &id);
            stopped += 1;
        }
        {
            let conn = state.db.lock().unwrap();
            conn.execute("DELETE FROM services", [])
                .map_err(|e| e.to_string())?;
            conn.execute("DELETE FROM groups", [])
                .map_err(|e| e.to_string())?;
            conn.execute("DELETE FROM service_events", [])
                .map_err(|e| e.to_string())?;
        }
        state.runtimes.lock().unwrap().clear();
    }

    // 分组：merge 模式下若同 id 已存在则跳过（保留现有名字）
    let mut groups_added = 0usize;
    {
        let conn = state.db.lock().unwrap();
        for g in &file.groups {
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM groups WHERE id=?1",
                    [&g.id],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if !exists {
                db::create_group(&conn, &g.id, &g.name).map_err(|e| e.to_string())?;
                groups_added += 1;
            }
        }
    }

    let mut services_added = 0usize;
    let mut services_skipped = 0usize;
    for s in file.services {
        let exists = {
            let conn = state.db.lock().unwrap();
            conn.query_row(
                "SELECT 1 FROM services WHERE id=?1",
                [&s.id],
                |_| Ok(true),
            )
            .unwrap_or(false)
        };
        if exists {
            services_skipped += 1;
            continue;
        }
        {
            let conn = state.db.lock().unwrap();
            db::insert_service(
                &conn,
                &s.id,
                &s.name,
                s.group_id.as_deref(),
                s.sort_order,
                s.enabled,
                &s.spec,
            )
            .map_err(|e| e.to_string())?;
        }
        manager::ensure_runtime(state, &s.id, s.spec, true);
        services_added += 1;
    }

    Ok(ImportOutcome {
        services_added,
        services_skipped,
        groups_added,
        stopped,
    })
}
