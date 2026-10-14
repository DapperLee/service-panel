//! macOS/Linux 崩溃看门狗。
//!
//! mac 没有 PR_SET_PDEATHSIG：面板被 `kill -9`/abort 异常杀死时，它 spawn 的托管
//! 服务会留在原地继续占端口。解法是独立监督进程——面板给每个 command 型服务拉起
//! 一个自身副本（`--guard <面板pid> <目标pgid>`），本模块就是该副本的入口：轮询
//! getppid()，一旦被 reparent（说明面板没了）就 kill(-pgid) 整组。
//! kqueue 方案不可行：事件队列属于面板进程，面板死了队列也死了。
//!
//! 主体写成平台中立（unix 专属调用藏在两个薄 shim 里），这样 Windows 上的
//! `cargo check` 也能完整校验这里的类型与借用。

use std::time::Duration;

const POLL_MS: u64 = 500;
/// 与面板正常退出的 reap 保持一致：先 TERM 给宽限，再 KILL
const GRACE_MS: u64 = 1700;

#[cfg(unix)]
fn my_ppid() -> i32 {
    extern "C" {
        fn getppid() -> i32;
    }
    unsafe { getppid() }
}
#[cfg(not(unix))]
fn my_ppid() -> i32 {
    -1 // 非 unix 平台只参与编译校验，不会被调用
}

#[cfg(unix)]
fn signal_group(pgid: u32, force: bool) {
    crate::drivers::command::kill_group(pgid, force);
}
#[cfg(not(unix))]
fn signal_group(_pgid: u32, _force: bool) {}

/// 带 `--guard` 启动时进入看护循环并返回退出码；普通 GUI 启动返回 None。
/// 参数残缺同样返回 Some(2)（拒启），不能让畸形参数误开出第二个面板。
#[allow(dead_code)] // 非 unix 平台保留同一份代码仅用于编译校验
pub fn serve_if_guard_mode() -> Option<i32> {
    let args: Vec<String> = std::env::args().collect();
    let idx = args.iter().position(|a| a == "--guard")?;
    let parent = match args.get(idx + 1).and_then(|s| s.parse::<i32>().ok()) {
        Some(v) => v,
        None => {
            eprintln!("[guard] 参数缺失或非法，拒绝看护");
            return Some(2);
        }
    };
    let pgid = match args.get(idx + 2).and_then(|s| s.parse::<u32>().ok()) {
        Some(v) => v,
        None => {
            eprintln!("[guard] 参数缺失或非法，拒绝看护");
            return Some(2);
        }
    };

    let reap = || {
        signal_group(pgid, false);
        std::thread::sleep(Duration::from_millis(GRACE_MS));
        signal_group(pgid, true);
    };
    // 面板可能在 guard 完成第一次轮询前就死了：进门先查一次
    if my_ppid() != parent {
        reap();
        return Some(0);
    }
    loop {
        std::thread::sleep(Duration::from_millis(POLL_MS));
        if my_ppid() != parent {
            reap();
            return Some(0);
        }
    }
}
