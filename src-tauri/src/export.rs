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
        let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
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
            .map(|g| ExportGroup {
                id: g.id,
                name: g.name,
            })
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
        let ids: Vec<String> = state
            .runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        for id in ids {
            let _ = manager::stop(state, &id);
            stopped += 1;
        }
        {
            let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
            conn.execute("DELETE FROM services", [])
                .map_err(|e| e.to_string())?;
            conn.execute("DELETE FROM groups", [])
                .map_err(|e| e.to_string())?;
            conn.execute("DELETE FROM service_events", [])
                .map_err(|e| e.to_string())?;
        }
        state
            .runtimes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    // 分组：merge 模式下若同 id 已存在则跳过（保留现有名字）
    let mut groups_added = 0usize;
    {
        let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
        for g in &file.groups {
            let exists: bool = conn
                .query_row("SELECT 1 FROM groups WHERE id=?1", [&g.id], |_| Ok(true))
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
            let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
            conn.query_row("SELECT 1 FROM services WHERE id=?1", [&s.id], |_| Ok(true))
                .unwrap_or(false)
        };
        if exists {
            services_skipped += 1;
            continue;
        }
        {
            let conn = state.db.lock().unwrap_or_else(|e| e.into_inner());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DriverKind;
    use std::collections::HashMap;

    /// 真 SQLite 文件（要跑建表迁移），不是内存库——内存库没有 schema。
    fn state() -> (Arc<AppState>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("sp-export-{}", uuid::Uuid::new_v4()));
        let conn = db::open(&dir).unwrap();
        (Arc::new(AppState::new(conn, dir.clone())), dir)
    }

    fn spec_with_token(tok: &str) -> ServiceSpec {
        let mut env = HashMap::new();
        env.insert("TOKEN".to_string(), tok.to_string());
        ServiceSpec {
            driver: DriverKind::Command,
            command: "python".to_string(),
            env,
            ..ServiceSpec::default()
        }
    }

    /// 备份的本职工作：导出去、导入回，env 里的凭据必须逐字符回来。
    /// 这条挂了等于用户恢复配置时静默拿到空 env，服务起不来还查不出原因。
    #[test]
    fn export_then_import_restores_env_values_verbatim() {
        let (src, src_dir) = state();
        {
            let conn = src.db.lock().unwrap_or_else(|e| e.into_inner());
            db::create_group(&conn, "g1", "分组一").unwrap();
            db::insert_service(
                &conn,
                "s1",
                "服务一",
                Some("g1"),
                1,
                true,
                &spec_with_token("abc-123"),
            )
            .unwrap();
            db::insert_service(
                &conn,
                "s2",
                "服务二",
                None,
                2,
                false,
                &spec_with_token("中文密钥 ±*/\"'"),
            )
            .unwrap();
        }

        let path = std::env::temp_dir().join(format!("sp-export-{}.json", uuid::Uuid::new_v4()));
        let n = export_to_file(&src, path.to_str().unwrap()).unwrap();
        assert_eq!(n, 2);

        // 按设计导出文件是明文 JSON（文件头注释承诺的）；哪天改成加密导出，这条会逼着同步改文档
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("abc-123"),
            "导出文件应为可读明文，便于人工核对备份内容"
        );

        let (dst, dst_dir) = state();
        let out = import_from_file(&dst, path.to_str().unwrap(), false).unwrap();
        assert_eq!(
            (out.services_added, out.services_skipped, out.groups_added),
            (2, 0, 1)
        );

        let back = {
            let conn = dst.db.lock().unwrap_or_else(|e| e.into_inner());
            db::list_services(&conn).unwrap()
        };
        assert_eq!(back.len(), 2);
        let s1 = back.iter().find(|s| s.id == "s1").unwrap();
        assert_eq!(
            s1.spec.env.get("TOKEN").map(|s| s.as_str()),
            Some("abc-123")
        );
        assert_eq!(s1.group_id.as_deref(), Some("g1"), "分组归属要跟着回来");
        assert!(s1.enabled, "enabled 不能在 round-trip 里翻转");
        let s2 = back.iter().find(|s| s.id == "s2").unwrap();
        assert_eq!(
            s2.spec.env.get("TOKEN").map(|s| s.as_str()),
            Some("中文密钥 ±*/\"'")
        );

        // 同一份文件再导入一次：merge 的语义是跳过已有 id，既不重复也不覆盖
        let again = import_from_file(&dst, path.to_str().unwrap(), false).unwrap();
        assert_eq!(
            (
                again.services_added,
                again.services_skipped,
                again.groups_added
            ),
            (0, 2, 0)
        );

        std::fs::remove_file(&path).ok();
        drop(src);
        drop(dst);
        std::fs::remove_dir_all(src_dir).ok();
        std::fs::remove_dir_all(dst_dir).ok();
    }

    /// 有一条 spec 读不出来就必须中止导出：否则空 spec 被当成真配置写进备份，
    /// 用户拿它恢复时等于静默清空全部配置。
    #[test]
    fn export_aborts_when_any_service_config_is_unreadable() {
        let (src, src_dir) = state();
        {
            let conn = src.db.lock().unwrap_or_else(|e| e.into_inner());
            db::insert_service(
                &conn,
                "ok",
                "正常服务",
                None,
                1,
                false,
                &spec_with_token("x"),
            )
            .unwrap();
            conn.execute(
                "INSERT INTO services (id, name, sort_order, enabled, spec_json, created_at, updated_at)
                 VALUES ('bad','坏配置服务',2,0,'{ 这不是 JSON',1,1)",
                [],
            )
            .unwrap();
        }

        let path = std::env::temp_dir().join(format!("sp-export-{}.json", uuid::Uuid::new_v4()));
        let err = export_to_file(&src, path.to_str().unwrap()).unwrap_err();
        assert!(
            err.contains("坏配置服务"),
            "报错要点出是哪个服务坏了：{err}"
        );
        assert!(!path.exists(), "中止导出不能留下半截备份文件");

        drop(src);
        std::fs::remove_dir_all(src_dir).ok();
    }
}
