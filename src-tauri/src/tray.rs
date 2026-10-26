use crate::manager;
use crate::state::AppState;
use crate::types::{DriverKind, ServiceSpec, State};
use std::sync::Arc;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

pub const TRAY_ID: &str = "main-tray";

fn spec_name(spec: &ServiceSpec) -> String {
    match spec.driver {
        DriverKind::Command => {
            if spec.command.trim().is_empty() {
                "(未配置命令)".to_string()
            } else {
                spec.command.clone()
            }
        }
        DriverKind::WindowsService => spec.service_name.clone(),
        DriverKind::Docker => spec.container.clone(),
    }
}

/// 托盘圆点语义：进程在不在。Running/Starting/Restarting 都算「在」。
/// 特意把 Unhealthy 也算「在」——健康抖动不该触发整菜单重建。
fn is_on(state: State) -> bool {
    matches!(state, State::Running | State::Starting | State::Restarting)
}

/// 当前该显示在托盘菜单里的服务列表（按名字排序，保证顺序稳定）
type Snapshot = Vec<(String, String, bool)>;

fn snapshot(state: &Arc<AppState>) -> Snapshot {
    let mut v: Snapshot = {
        let rts = state.runtimes.lock().unwrap();
        rts.iter()
            .map(|(id, r)| (id.clone(), spec_name(&r.spec), is_on(r.state)))
            .collect()
    };
    v.sort_by(|a, b| a.1.cmp(&b.1));
    v
}

/// 菜单的可见内容签名。只有它变了才值得重建整个菜单。
fn signature(snap: &Snapshot) -> String {
    snap.iter()
        .map(|(id, name, on)| format!("{id}|{name}|{on}"))
        .collect::<Vec<_>>()
        .join(";")
}

fn build_menu(app: &AppHandle, snap: &Snapshot) -> tauri::Result<Menu<Wry>> {
    let show = MenuItem::with_id(app, "show", "打开面板", true, None::<&str>)?;
    let start_all = MenuItem::with_id(app, "start_all", "▶ 全部启动", true, None::<&str>)?;
    let stop_all = MenuItem::with_id(app, "stop_all", "■ 全部停止", true, None::<&str>)?;
    let sep1 = PredefinedMenuItem::separator(app)?;

    // 每个服务一项：点击 = 启停切换
    let mut svc_items: Vec<MenuItem<Wry>> = Vec::with_capacity(snap.len());
    for (id, name, on) in snap {
        let label = format!("{} {}", if *on { "●" } else { "○" }, name);
        let item = MenuItem::with_id(app, format!("svc::{}", id), label, true, None::<&str>)?;
        svc_items.push(item);
    }

    let sep2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

    let mut items: Vec<&dyn IsMenuItem<Wry>> = Vec::with_capacity(5 + svc_items.len());
    items.push(&show);
    items.push(&start_all);
    items.push(&stop_all);
    items.push(&sep1);
    for it in &svc_items {
        items.push(it);
    }
    items.push(&sep2);
    items.push(&quit);

    Menu::with_items(app, &items)
}

/// 按需刷新托盘菜单：可见内容没变就什么都不做。
/// 之前是每个状态事件都重建一次整菜单，健康抖动中的服务每 2 秒就能触发一轮，
/// Windows 上重建菜单会让展开中的菜单闪烁甚至丢失。
pub fn refresh(app: &AppHandle, state: &Arc<AppState>) {
    let snap = snapshot(state);
    let sig = signature(&snap);
    {
        let mut last = state.tray_sig.lock().unwrap();
        if *last == sig {
            return;
        }
        *last = sig;
    }
    if let Ok(menu) = build_menu(app, &snap) {
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            let _ = tray.set_menu(Some(menu));
        }
    }
}

fn toggle_service(app: &AppHandle, state: &Arc<AppState>, sid: &str) {
    let running = state
        .runtimes
        .lock()
        .unwrap()
        .get(sid)
        .map(|r| r.state == State::Running)
        .unwrap_or(false);
    // 启停可能阻塞很久（Windows 服务停止轮询最长 30s+），不能在托盘菜单事件线程上同步跑，
    // 否则整个托盘/事件循环被冻结。丢到后台线程，完成后再刷新菜单。
    let st = state.clone();
    let app2 = app.clone();
    let sid = sid.to_string();
    std::thread::spawn(move || {
        if running {
            let _ = manager::stop(&st, &sid);
        } else {
            let _ = manager::start(&st, &sid);
        }
        refresh(&app2, &st);
    });
}

fn handle_menu(app: &AppHandle, state: &Arc<AppState>, id: &str) {
    match id {
        "show" => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }
        // 与顶栏按钮走同一条路：按依赖拓扑定序、有批量互斥、进度经事件回报。
        // 旧的托盘实现是原地 for 循环 + sleep(150ms)，既忽略依赖顺序，
        // 又会把菜单事件线程按住若干秒。
        "start_all" => {
            let _ = crate::group::start_all_seq(app, state.clone());
        }
        "stop_all" => {
            let _ = crate::group::stop_all_seq(app, state.clone());
        }
        "quit" => {
            // 真正退出：绕过 close-to-tray
            app.exit(0);
        }
        s => {
            if let Some(sid) = s.strip_prefix("svc::") {
                toggle_service(app, state, sid);
            }
        }
    }
}

pub fn install(app: &AppHandle, state: Arc<AppState>) -> tauri::Result<()> {
    let snap = snapshot(&state);
    let menu = build_menu(app, &snap)?;
    *state.tray_sig.lock().unwrap() = signature(&snap);
    // 构建期已注入 bundle 图标，default_window_icon 必然存在
    let icon = app
        .default_window_icon()
        .cloned()
        .expect("缺少应用图标（应由 generate_context 注入）");

    let app_handle = app.clone();
    TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .icon(icon)
        .tooltip("本地服务管理面板")
        .on_menu_event(move |_app, event| {
            handle_menu(&app_handle, &state, event.id().as_ref());
        })
        .on_tray_icon_event(|tray, event| {
            // 左键点击：切换主窗口显示/隐藏
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                if let Some(w) = tray.app_handle().get_webview_window("main") {
                    if w.is_visible().unwrap_or(true) {
                        let _ = w.hide();
                    } else {
                        let _ = w.show();
                        let _ = w.unminimize();
                        let _ = w.set_focus();
                    }
                }
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::signature;

    #[test]
    fn signature_ignores_order_but_not_content() {
        let a = vec![
            ("1".to_string(), "mysql".to_string(), true),
            ("2".to_string(), "redis".to_string(), false),
        ];
        let b = a.clone();
        assert_eq!(signature(&a), signature(&b));

        let mut c = a.clone();
        c[0].2 = false; // 某个服务停了 -> 签名变
        assert_ne!(signature(&a), signature(&c));

        let mut d = a.clone();
        d[1].1 = "redis-7".to_string(); // 改名 -> 签名变
        assert_ne!(signature(&a), signature(&d));
    }
}
