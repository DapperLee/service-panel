use crate::db;
use crate::health;
use crate::manager;
use crate::state::AppState;
use crate::types::{HealthKind, ServiceSpec, State};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

/// 分组操作结果事件（推给前端 toast）
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupResult {
    pub group: String,
    pub op: String, // start | stop
    pub ok: bool,
    pub error: Option<String>,
}

fn emit_result(app: &AppHandle, group: &str, op: &str, res: Result<(), String>) {
    let payload = GroupResult {
        group: group.to_string(),
        op: op.to_string(),
        ok: res.is_ok(),
        error: res.err(),
    };
    let _ = app.emit("group://result", &payload);
}

/// 分组批量操作的逐服务进度事件（避免一条链最多静默 45 秒）
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupProgress {
    pub group: String,
    pub op: String, // start | stop
    pub current: String,
    pub name: String,
    pub index: usize, // 1-based
    pub total: usize,
}

fn emit_progress(app: &AppHandle, group: &str, op: &str, current: &str, name: &str, index: usize, total: usize) {
    let _ = app.emit(
        "group://progress",
        &GroupProgress {
            group: group.to_string(),
            op: op.to_string(),
            current: current.to_string(),
            name: name.to_string(),
            index,
            total,
        },
    );
}

/// id -> 服务名（供进度事件显示）
fn names_of(state: &Arc<AppState>) -> HashMap<String, String> {
    let conn = state.db.lock().unwrap();
    db::list_services(&conn)
        .unwrap_or_default()
        .into_iter()
        .map(|v| (v.id, v.name))
        .collect()
}

/// 构建 id -> spec 映射（以 DB 为准）
fn spec_map(state: &Arc<AppState>) -> HashMap<String, ServiceSpec> {
    let views = {
        let conn = state.db.lock().unwrap();
        db::list_services(&conn).unwrap_or_default()
    };
    views.into_iter().map(|v| (v.id, v.spec)).collect()
}

/// 组内成员（按 sort_order 稳定排序）
fn members_of(state: &Arc<AppState>, gid: &str) -> (Vec<String>, String) {
    let conn = state.db.lock().unwrap();
    let name: String = conn
        .query_row("SELECT name FROM groups WHERE id=?1", [gid], |r| r.get(0))
        .unwrap_or_else(|_| "分组".to_string());
    let mut rows: Vec<(String, i64)> = conn
        .prepare("SELECT id, sort_order FROM services WHERE group_id=?1 ORDER BY sort_order ASC, name ASC")
        .and_then(|mut st| {
            let it = st.query_map([gid], |r| Ok((r.get(0)?, r.get(1)?)))?;
            it.collect::<rusqlite::Result<_>>()
        })
        .unwrap_or_default();
    rows.sort_by_key(|(_, s)| *s);
    (rows.into_iter().map(|(id, _)| id).collect(), name)
}

/// Kahn 拓扑排序；只考虑成员之间的依赖边。返回顺序，或环内成员列表。
pub fn topo_order(
    members: &[String],
    specs: &HashMap<String, ServiceSpec>,
) -> Result<Vec<String>, Vec<String>> {
    let set: HashSet<&String> = members.iter().collect();
    let mut indeg: HashMap<&String, usize> = members.iter().map(|m| (m, 0usize)).collect();
    let mut adj: HashMap<&String, Vec<&String>> = HashMap::new();

    for m in members {
        if let Some(sp) = specs.get(m) {
            for dep in &sp.after {
                // 只连成员内部的边；忽略自环
                if dep != m && set.contains(dep) {
                    *indeg.get_mut(m).unwrap() += 1;
                    adj.entry(dep).or_default().push(m);
                }
            }
        }
    }

    // 初始就绪队列按 members 给定顺序（sort_order）填充，保证确定性
    let mut queue: VecDeque<&String> = VecDeque::new();
    for m in members {
        if indeg[m] == 0 {
            queue.push_back(m);
        }
    }

    let mut order: Vec<String> = Vec::with_capacity(members.len());
    while let Some(n) = queue.pop_front() {
        order.push(n.clone());
        if let Some(dependents) = adj.get(n) {
            for d in dependents {
                let c = indeg.get_mut(d).unwrap();
                *c = c.saturating_sub(1);
                if *c == 0 {
                    queue.push_back(d);
                }
            }
        }
    }

    if order.len() != members.len() {
        // 剩余未排入的就是环上的节点
        let done: HashSet<&String> = order.iter().collect();
        let cycle: Vec<String> = members.iter().filter(|m| !done.contains(m)).cloned().collect();
        return Err(cycle);
    }
    Ok(order)
}

fn state_is_up(state: &Arc<AppState>, id: &str) -> bool {
    let rts = state.runtimes.lock().unwrap();
    matches!(
        rts.get(id).map(|r| r.state),
        Some(State::Running | State::Starting | State::Restarting | State::Unhealthy)
    )
}

/// 等待一个服务就绪：Command 需保持存活；配了健康检查则需探通。
fn wait_ready(state: &Arc<AppState>, id: &str, spec: &ServiceSpec) -> Result<(), String> {
    let timeout = Duration::from_secs(45);
    let start = Instant::now();
    loop {
        let up = state_is_up(state, id);
        // 3 秒内就掉了 -> 判定启动失败，尽早中断整条链
        if !up && start.elapsed() > Duration::from_secs(3) {
            return Err(format!("{id} 未能保持运行"));
        }
        if spec.healthcheck.kind == HealthKind::None {
            if up {
                return Ok(());
            }
        } else if up && health::check(&spec.healthcheck).is_ok() {
            return Ok(());
        }
        if start.elapsed() > timeout {
            return Err(format!("{id} 健康检查在 {}s 内未通过", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(800));
    }
}

fn run_start(app: &AppHandle, state: &Arc<AppState>, ordered: &[String], group: &str) {
    let specs = spec_map(state);
    let names = names_of(state);
    let total = ordered.len();
    for (i, id) in ordered.iter().enumerate() {
        emit_progress(app, group, "start", id, names.get(id).map(|s| s.as_str()).unwrap_or(id), i + 1, total);
        let already = {
            let rts = state.runtimes.lock().unwrap();
            matches!(
                rts.get(id).map(|r| r.state),
                Some(State::Running | State::Starting | State::Restarting)
            )
        };
        if !already {
            if let Err(e) = manager::start(state, id) {
                emit_result(app, group, "start", Err(format!("{id} 启动失败：{e}")));
                return;
            }
        }
        if let Some(spec) = specs.get(id) {
            if let Err(e) = wait_ready(state, id, spec) {
                emit_result(app, group, "start", Err(e));
                return;
            }
        }
    }
    emit_result(app, group, "start", Ok(()));
}

fn run_stop(app: &AppHandle, state: &Arc<AppState>, ordered: &[String], group: &str) {
    let names = names_of(state);
    let total = ordered.len();
    // 逆序停止，先起的后停，避免依赖方还在用被依赖方
    for (i, id) in ordered.iter().rev().enumerate() {
        emit_progress(app, group, "stop", id, names.get(id).map(|s| s.as_str()).unwrap_or(id), i + 1, total);
        if let Err(e) = manager::stop(state, id) {
            emit_result(app, group, "stop", Err(format!("{id} 停止失败：{e}")));
            return;
        }
    }
    emit_result(app, group, "stop", Ok(()));
}

/// 计算某组的拓扑顺序（供命令层做环检测与前端展示）
pub fn ordered_members(state: &Arc<AppState>, gid: &str) -> Result<(Vec<String>, String), Vec<String>> {
    let (members, name) = members_of(state, gid);
    let specs = spec_map(state);
    let order = topo_order(&members, &specs)?;
    Ok((order, name))
}

/// 全局（所有服务）的拓扑顺序，用于「全部启动」按依赖定序
pub fn ordered_all(state: &Arc<AppState>) -> Result<Vec<String>, Vec<String>> {
    let members: Vec<String> = {
        let conn = state.db.lock().unwrap();
        db::list_services(&conn)
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.id)
            .collect()
    };
    let specs = spec_map(state);
    topo_order(&members, &specs)
}

/// 分组批量操作互斥（RAII）：同一组（或"全部"）同一时刻只跑一条链，
/// 防止用户连点并发起多条 run_start 互相抢服务的 op 锁。
struct GroupOpGuard {
    state: Arc<AppState>,
    key: String,
}
impl Drop for GroupOpGuard {
    fn drop(&mut self) {
        self.state.group_ops.lock().unwrap().remove(&self.key);
    }
}
fn try_lock_group(state: &Arc<AppState>, key: &str) -> Option<GroupOpGuard> {
    let mut set = state.group_ops.lock().unwrap();
    if set.contains(key) {
        return None;
    }
    set.insert(key.to_string());
    Some(GroupOpGuard {
        state: state.clone(),
        key: key.to_string(),
    })
}

pub fn start_group(app: &AppHandle, state: Arc<AppState>, gid: String) -> Result<(), String> {
    let _g = try_lock_group(&state, &gid).ok_or("该分组正在执行批量操作，请稍候")?;
    let (ordered, name) = ordered_members(&state, &gid)
        .map_err(|cycle| cycle_err(&cycle))?;
    let app2 = app.clone();
    std::thread::spawn(move || run_start(&app2, &state, &ordered, &name));
    Ok(())
}

pub fn stop_group(app: &AppHandle, state: Arc<AppState>, gid: String) -> Result<(), String> {
    let _g = try_lock_group(&state, &gid).ok_or("该分组正在执行批量操作，请稍候")?;
    let (ordered, name) = ordered_members(&state, &gid)
        .map_err(|cycle| cycle_err(&cycle))?;
    let app2 = app.clone();
    std::thread::spawn(move || run_stop(&app2, &state, &ordered, &name));
    Ok(())
}

pub fn cycle_err(cycle: &[String]) -> String {
    format!("存在循环依赖，无法定序：{}", cycle.join(" → "))
}

/// 全局拓扑顺序；有环时退回 DB 原始顺序并忽略依赖约束（best-effort）
fn ordered_all_or_fallback(state: &Arc<AppState>) -> Vec<String> {
    if let Ok(o) = ordered_all(state) {
        return o;
    }
    let conn = state.db.lock().unwrap();
    db::list_services(&conn)
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.id)
        .collect()
}

pub fn start_all_seq(app: &AppHandle, state: Arc<AppState>) -> Result<(), String> {
    let _g = try_lock_group(&state, "all").ok_or("正在执行批量操作，请稍候")?;
    let ordered = ordered_all_or_fallback(&state);
    let app2 = app.clone();
    std::thread::spawn(move || run_start(&app2, &state, &ordered, "全部"));
    Ok(())
}

pub fn stop_all_seq(app: &AppHandle, state: Arc<AppState>) -> Result<(), String> {
    let _g = try_lock_group(&state, "all").ok_or("正在执行批量操作，请稍候")?;
    let ordered = ordered_all_or_fallback(&state);
    let app2 = app.clone();
    std::thread::spawn(move || run_stop(&app2, &state, &ordered, "全部"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::topo_order;
    use crate::types::ServiceSpec;
    use std::collections::HashMap;

    fn spec_with_after(after: &[&str]) -> ServiceSpec {
        let mut s = ServiceSpec::default();
        s.after = after.iter().map(|x| x.to_string()).collect();
        s
    }

    fn map(pairs: &[(&str, ServiceSpec)]) -> HashMap<String, ServiceSpec> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn linear_chain_orders_by_dependency() {
        let specs = map(&[
            ("a", spec_with_after(&[])),
            ("b", spec_with_after(&["a"])),
            ("c", spec_with_after(&["b"])),
        ]);
        let members: Vec<String> = ["c", "b", "a"].iter().map(|s| s.to_string()).collect();
        let order = topo_order(&members, &specs).unwrap();
        // a 在 b 前，b 在 c 前
        let pos = |id: &str| order.iter().position(|x| x == id).unwrap();
        assert!(pos("a") < pos("b"));
        assert!(pos("b") < pos("c"));
    }

    #[test]
    fn detects_cycle() {
        let specs = map(&[
            ("a", spec_with_after(&["b"])),
            ("b", spec_with_after(&["a"])),
        ]);
        let members: Vec<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let err = topo_order(&members, &specs).unwrap_err();
        assert_eq!(err.len(), 2); // 环上两个节点都报出来
    }

    #[test]
    fn ignores_edges_to_non_members() {
        // b 依赖一个不在集合里的 x，应被忽略，仍能排序
        let specs = map(&[
            ("a", spec_with_after(&[])),
            ("b", spec_with_after(&["a", "x"])),
        ]);
        let members: Vec<String> = ["a", "b"].iter().map(|s| s.to_string()).collect();
        let order = topo_order(&members, &specs).unwrap();
        assert_eq!(order, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn self_edge_ignored() {
        let specs = map(&[("a", spec_with_after(&["a"]))]);
        let members = vec!["a".to_string()];
        assert_eq!(topo_order(&members, &specs).unwrap(), vec!["a".to_string()]);
    }
}
