pub mod command;
pub mod docker;
pub mod winservice;

/// 外部驱动（系统服务 / 容器）一次状态探测的返回值。
///
/// 光有 `running` 一个布尔值时，面板只知道服务在不在跑，不知道它是哪个进程，
/// 于是 CPU/内存永远只能报 0。这里把定位进程的线索一起带回来：
/// - `pid`：宿主进程号，监督线程按它采样进程树（Windows 服务、systemd 单元）；
/// - `metrics`：驱动自己算好的 (cpu%, mem MB)。Docker 在 macOS 上容器进程位于
///   Docker VM 内部，宿主的 sysinfo 看不见，只能由 `docker stats` 直接给数。
///
/// 两者都拿不到时上层显示「—」而不是 0，不把「测不到」伪装成「没占用」。
#[derive(Clone, Copy, Debug, Default)]
pub struct ExtStatus {
    pub running: Option<bool>,
    pub pid: Option<u32>,
    pub metrics: Option<(f32, f32)>,
}
