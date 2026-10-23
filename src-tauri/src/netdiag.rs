//! 本机 TCP 监听表与端口反查。
//!
//! 设计文档避坑 #1 说的是「端口占用不等于进程就是它」：`netstat` 的文本输出既依赖
//! 系统语言又要解析，慢且脆。这里直接问内核要 `GetExtendedTcpTable`（端口 -> PID），
//! 再用 sysinfo 反查进程名/命令行。两个消费方：
//! - 端口冲突检测：服务声明的端口是不是被**别的**进程占着；
//! - 进程接管：面板重启后按命令行/端口找回原来那个进程，而不是把它当没启动。

use crate::state::AppState;
use serde::Serialize;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use sysinfo::{Pid, System};

/// 一条监听记录
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listener {
    /// 本地地址；`0.0.0.0` 表示通配监听
    pub addr: String,
    pub port: u16,
    pub pid: u32,
}

/// 端口字节序转换：DWORD 低位那个 WORD 是网络序的端口。
/// 例：80 -> 网络序字节 00 50 -> 落在小端 WORD 里是 0x5000 -> from_be 还原成 80。
#[cfg_attr(not(windows), allow(dead_code))] // 仅 Windows FFI 路径与单元测试使用
fn port_from_raw(raw: u32) -> u16 {
    u16::from_be((raw & 0xFFFF) as u16)
}

/// IPv4 地址转换：DWORD 里按网络序存放，小端机器上第 1 个八位组在最低字节。
/// 例：127.0.0.1 -> 0x0100_007F。Windows 只有小端，这里不处理大端。
#[cfg_attr(not(windows), allow(dead_code))]
fn ipv4_from_raw(raw: u32) -> Ipv4Addr {
    let b = raw.to_le_bytes();
    Ipv4Addr::new(b[0], b[1], b[2], b[3])
}

#[cfg(windows)]
mod ffi {
    use std::ffi::c_void;

    const AF_INET: u32 = 2;
    /// TCP_TABLE_CLASS 里带属主 PID 的**监听**表。
    /// 真机实测的映射：0/1/2 是不带 PID 的 basic 系列（结构体只有 20 字节，
    /// 按 24 字节的 OWNER_PID 结构跨行读会读到垃圾），3 = OWNER_PID_LISTENER，
    /// 4 = OWNER_PID_CONNECTIONS（**不含监听**），5 = OWNER_PID_ALL。
    const TABLE_CLASS: u32 = 3;
    const ERROR_SUCCESS: u32 = 0;
    /// 两次调用之间可能新增连接，留点余量避免刚好卡在边界上重试
    const SLACK: usize = 1024;

    #[repr(C)]
    #[derive(Clone, Copy)]
    #[allow(non_snake_case)]
    // 布局必须与内核一致，所以字段一个都不能少，哪怕只读其中几个
    #[allow(dead_code)]
    struct MibTcpRowOwnerPid {
        dwState: u32,
        dwLocalAddr: u32,
        dwLocalPort: u32,
        dwRemoteAddr: u32,
        dwRemotePort: u32,
        dwOwningPid: u32,
    }

    #[link(name = "iphlpapi")]
    extern "system" {
        fn GetExtendedTcpTable(
            p_tcp_table: *mut c_void,
            pdw_size: *mut u32,
            sorted: i32,
            ul_af: u32,
            class: u32,
            reserved: u32,
        ) -> u32;
    }

    /// 取所有处于 LISTEN 的 IPv4 连接及其属主 PID
    pub fn listeners() -> Vec<super::Listener> {
        let mut size: u32 = 0;
        // 第一次调用只为拿所需缓冲区大小，返回值必然是 ERROR_INSUFFICIENT_BUFFER
        unsafe {
            GetExtendedTcpTable(std::ptr::null_mut(), &mut size, 0, AF_INET, TABLE_CLASS, 0);
        }
        if size == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; size as usize + SLACK];
        let mut sz = buf.len() as u32;
        let code = unsafe {
            GetExtendedTcpTable(
                buf.as_mut_ptr() as *mut c_void,
                &mut sz,
                0,
                AF_INET,
                TABLE_CLASS,
                0,
            )
        };
        if code != ERROR_SUCCESS || (sz as usize) < std::mem::size_of::<u32>() {
            return Vec::new();
        }
        let count = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let row_size = std::mem::size_of::<MibTcpRowOwnerPid>();
        let avail = (sz as usize - std::mem::size_of::<u32>()) / row_size;
        // 防御：不要相信内核报回来的条目数超过缓冲区实际容得下的行数
        let n = count.min(avail);
        let rows = unsafe {
            std::slice::from_raw_parts(
                buf.as_ptr().add(std::mem::size_of::<u32>()) as *const MibTcpRowOwnerPid,
                n,
            )
        };
        // 不再按 dwState 过滤：取的就是监听表，本身只含 LISTEN 行。
        // （之前在这里加了一道 dwState == 1 的过滤，而 MIB_TCP_STATE 的 LISTEN 其实是 2、
        // 1 是 CLOSED —— 这道"防御"把结果全滤空了。按数字猜枚举是最容易翻车的地方。）
        rows.iter()
            .map(|r| super::Listener {
                addr: super::ipv4_from_raw(r.dwLocalAddr).to_string(),
                port: super::port_from_raw(r.dwLocalPort),
                pid: r.dwOwningPid,
            })
            .collect()
    }
}

#[cfg(not(windows))]
pub fn listeners() -> Vec<Listener> {
    // 本项目按 Windows 单平台设计；其它平台编译通过即可，探测返回空。
    Vec::new()
}

#[cfg(windows)]
pub fn listeners() -> Vec<Listener> {
    ffi::listeners()
}

/// 哪个 PID 在监听该端口。通配监听（0.0.0.0 / ::）也算。
pub fn listener_pid(port: u16) -> Option<u32> {
    listeners()
        .into_iter()
        .find(|l| l.port == port)
        .map(|l| l.pid)
}

/// 端口是否空闲：用 bind 测试最准（能捕获监听/占用/TIME_WAIT 等各种占用）。
/// 测试后立即释放，存在极小的 TOCTOU 窗口，本机单用户场景可接受。
pub fn is_port_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// 从 start 往上找最多 tries 个端口里的第一个空闲端口。
pub fn find_free_port(start: u16, tries: u16) -> Option<u16> {
    (0..tries).find_map(|d| {
        let p = start.checked_add(d)?;
        if is_port_free(p) {
            Some(p)
        } else {
            None
        }
    })
}

/// 把字符串里的 `{port}` 占位符替换为实际端口。
pub fn resolve_port_token(s: &str, port: u16) -> String {
    s.replace("{port}", &port.to_string())
}

/// 从健康检查 target 里抽端口：支持 "host:port"、"http(s)://host:port/p"，
/// URL 省略端口时按方案取 80 / 443。抽不到返回 None。
pub fn port_of(target: &str) -> Option<u16> {
    let t = target.trim();
    if t.is_empty() {
        return None;
    }
    let (rest, default_port) = if let Some(r) = t.strip_prefix("https://") {
        (r, Some(443u16))
    } else if let Some(r) = t.strip_prefix("http://") {
        (r, Some(80u16))
    } else {
        (t, None)
    };
    // 去掉 path / query，只留 authority
    let authority = match rest.find('/').or(rest.find('?')) {
        Some(i) => &rest[..i],
        None => rest,
    };
    // IPv6 字面量形如 [::1]:8080
    let after_bracket = if authority.contains(']') {
        authority.rsplit(']').next().unwrap_or("")
    } else {
        authority
    };
    if let Some(pos) = after_bracket.rfind(':') {
        return after_bracket[pos + 1..].parse::<u16>().ok();
    }
    default_port
}

/// 进程名（一次性快照，只用于按需调用，别放进 2 秒 tick）
fn system_snapshot() -> System {
    let mut sys = System::new_all();
    sys.refresh_processes();
    sys
}

pub fn proc_name(pid: u32) -> Option<String> {
    let sys = system_snapshot();
    sys.process(Pid::from_u32(pid))
        .map(|p| p.name().to_string())
}

/// `pid` 是否是 `ancestor` 的后代（含自身）。
/// npm/pnpm 这类启动器会 fork 出真正监听的子进程，只看自身 PID 会误报冲突。
pub fn is_descendant_of(ancestor: u32, pid: u32) -> bool {
    let sys = system_snapshot();
    let mut cur = Pid::from_u32(pid);
    let anc = Pid::from_u32(ancestor);
    // 限步数，防御异常父子环
    for _ in 0..32 {
        if cur == anc {
            return true;
        }
        match sys.process(cur).and_then(|p| p.parent()) {
            Some(p) => cur = p,
            None => return false,
        }
    }
    false
}

/// 进程是否还活着（接管的进程没有 Child 句柄，只能这样判）
pub fn process_alive(pid: u32) -> bool {
    let sys = system_snapshot();
    sys.process(Pid::from_u32(pid)).is_some()
}

/// 按命令行找**疑似**该服务的进程（面板重启后的接管场景）。
/// 返回全部候选而不是一个：调用方只在「唯一候选」时才敢认领——认领错了，
/// 后续停止服务就意味着去结束别人的进程。要求命令行同时含程序名与全部参数。
pub fn find_candidates(command: &str, args: &[String]) -> Vec<u32> {
    let cmd = command.trim().to_lowercase();
    if cmd.is_empty() {
        return Vec::new();
    }
    let me = std::process::id();
    let sys = system_snapshot();
    let stem = cmd.trim_end_matches(".exe");
    let mut out: Vec<u32> = sys
        .processes()
        .iter()
        .filter_map(|(pid, p)| {
            let id = pid.as_u32();
            // 排除面板自己与系统进程
            if id == me || id <= 4 {
                return None;
            }
            let joined = p
                .cmd()
                .iter()
                .map(|s| s.to_lowercase())
                .collect::<Vec<_>>()
                .join(" ");
            if !joined.contains(stem) {
                return None;
            }
            let all_args_present = args
                .iter()
                .map(|a| a.trim().to_lowercase())
                .filter(|a| !a.is_empty())
                .all(|a| joined.contains(&a));
            if all_args_present {
                Some(id)
            } else {
                None
            }
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// 某个服务的端口占用结论
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortAudit {
    pub service_id: String,
    pub service_name: String,
    pub port: u16,
    /// 监听该端口的进程（None 表示当前无人监听，通常就是"没起来"）
    pub listener_pid: Option<u32>,
    pub listener_name: Option<String>,
    /// true = 端口被别人占着，启这个服务大概率会撞端口
    pub conflict: bool,
}

/// 审计所有配了 TCP/HTTP 健康检查且能推出端口的服务。
pub fn audit(state: &Arc<AppState>) -> Vec<PortAudit> {
    // 展示名在 services 表里（spec 只有驱动字段），取轻量列，不为审计去解密 spec
    let names: HashMap<String, String> = {
        let conn = state.db.lock().unwrap();
        crate::db::list_static(&conn)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, name, _, _, _)| (id, name))
            .collect()
    };
    let snapshot: Vec<(String, crate::types::HealthKind, String, Option<u32>)> = {
        let rts = state.runtimes.lock().unwrap();
        rts.iter()
            .map(|(id, r)| {
                (
                    id.clone(),
                    r.spec.healthcheck.kind,
                    r.spec.healthcheck.target.clone(),
                    r.live.as_ref().map(|l| l.pid),
                )
            })
            .collect()
    };
    let ls = listeners();
    let mut out = Vec::new();
    for (id, kind, target, own_pid) in snapshot {
        let is_http = kind == crate::types::HealthKind::Http;
        if !is_http && kind != crate::types::HealthKind::Tcp {
            continue;
        }
        let Some(port) = port_of(&target) else {
            continue;
        };
        let Some(l) = ls.iter().find(|l| l.port == port) else {
            continue; // 无人监听：没有冲突可报，启停状态本身已经说明问题
        };
        // 自己的进程、或自己的子进程在监听，都不算冲突
        let mine =
            own_pid == Some(l.pid) || own_pid.map(|p| is_descendant_of(p, l.pid)) == Some(true);
        if mine {
            continue;
        }
        out.push(PortAudit {
            service_name: names.get(&id).cloned().unwrap_or_else(|| id.clone()),
            service_id: id,
            port,
            listener_pid: Some(l.pid),
            listener_name: proc_name(l.pid),
            conflict: true,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{ipv4_from_raw, listeners, port_from_raw, port_of};
    use std::net::Ipv4Addr;

    #[test]
    fn listeners_table_is_well_formed() {
        let ls = listeners();
        #[cfg(windows)]
        assert!(
            !ls.is_empty(),
            "Windows 上必然有监听（RPC 135 等）；空表说明 FFI 调用或表类选错了"
        );
        for l in &ls {
            assert!(l.port > 0, "端口不应为 0: {l:?}");
            assert!(l.pid > 0, "PID 不应为 0: {l:?}");
            assert!(l.addr.parse::<Ipv4Addr>().is_ok(), "地址应可解析: {l:?}");
        }
    }

    #[test]
    fn port_decoded_from_network_order_low_word() {
        // 80 = 0x0050，网络序字节 00 50，落在小端 WORD 里是 0x5000
        assert_eq!(port_from_raw(0x5000), 80);
        // 3306 = 0x0CEA -> 0xEA0C
        assert_eq!(port_from_raw(0xEA0C), 3306);
        // 443 = 0x01BB -> 0xBB01
        assert_eq!(port_from_raw(0xBB01), 443);
        assert_eq!(port_from_raw(0), 0);
    }

    #[test]
    fn ipv4_decoded_octet_order() {
        assert_eq!(ipv4_from_raw(0x0100_007F), Ipv4Addr::new(127, 0, 0, 1));
        assert_eq!(ipv4_from_raw(0), Ipv4Addr::new(0, 0, 0, 0));
        assert_eq!(ipv4_from_raw(0x0101_A8C0), Ipv4Addr::new(192, 168, 1, 1));
    }

    #[test]
    fn port_extracted_from_targets() {
        assert_eq!(port_of("127.0.0.1:3306"), Some(3306));
        assert_eq!(port_of("localhost:6379"), Some(6379));
        assert_eq!(port_of("http://localhost:8080/health"), Some(8080));
        assert_eq!(port_of("https://api.example.com/v1/ping"), Some(443));
        assert_eq!(port_of("http://127.0.0.1"), Some(80));
        assert_eq!(port_of("[::1]:9000"), Some(9000));
        // 无端口且非 URL：推不出，交给上层忽略
        assert_eq!(port_of("just-a-name"), None);
        assert_eq!(port_of("  "), None);
    }
}
