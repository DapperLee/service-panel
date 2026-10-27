use crate::logs::{LogEmitter, Sink};
use crate::types::{Desired, ServiceSpec, State};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// 系统通知回调类型（title, body）
pub type NotifyFn = Arc<dyn Fn(String, String) + Send + Sync>;

/// 一个正在被面板托管的进程（当前只有 Command 驱动需要）
pub struct LiveProc {
    pub child: Child,
    pub pid: u32,
    /// Job Object 句柄，仅用于 Drop 时回收整棵进程树，不会被主动读取
    #[cfg(windows)]
    #[allow(dead_code)]
    pub job: Option<crate::drivers::command::JobHandle>,
    /// unix 看门狗子进程（--guard 副本）。任何对进程组的操作前都要先杀掉它，
    /// 否则它可能在 pgid 被系统复用后醒来补刀，误杀无关进程组。
    #[cfg(unix)]
    pub guard: Option<Child>,
}

/// 外部服务（Docker）的日志尾随子进程（如 `docker logs -f`）
pub struct LogTail {
    pub child: Child,
    #[cfg(windows)]
    #[allow(dead_code)]
    pub job: Option<crate::winjob::Job>,
}

/// 被接管的、非面板启动的进程
pub struct Adopted {
    pub pid: u32,
    /// 只为了 Drop：关闭句柄即让 OS 结束该进程树
    #[cfg(windows)]
    #[allow(dead_code)]
    pub job: Option<crate::drivers::command::JobHandle>,
}

/// 服务的运行时信息（状态机 + 重启退避计数）
pub struct Runtime {
    pub state: State,
    pub desired: Desired,
    pub last_error: Option<String>,
    pub unhealthy_streak: u32,
    pub fail_streak: u32,
    /// 下一次允许重启的时间戳（unix 毫秒），实现指数退避
    pub next_attempt_at: i64,
    /// 下一次健康探测的时间戳（unix 毫秒），按 spec.healthcheck.interval_secs 排期。
    /// 0 表示立刻可探（刚启动、或该服务本轮还没排过程）。
    pub next_health_at: i64,
    /// spec 快照，重启时不需要重查 DB。自动端口模式下这是**已注入实际端口**的解析结果，
    /// 供监督线程做健康检查直接用。原始模板（含 {port} 占位符）见 spec_template。
    pub spec: ServiceSpec,
    /// 原始 spec 模板（含 {port} 占位符），每次启动据此重新解析端口
    pub spec_template: ServiceSpec,
    /// 自动端口模式本次选定的端口
    pub chosen_port: Option<u16>,
    pub live: Option<LiveProc>,
    /// 面板重启后**接管**的进程：面板之外启动、但命令行与端口都指向本服务。
    /// 没有 Child 句柄，所以只能靠内核快照判存活；停止它的唯一手段是 drop 下面这个
    /// KILL_ON_JOB_CLOSE 的 Job 句柄（由操作系统连带结束整棵进程树）。
    /// 用附加字段而不是把 LiveProc 改成枚举，是为了不惊动所有 `rt.live` 分支——
    /// 接管是低频路径，改动面小得多。
    pub adopted: Option<Adopted>,
    /// Docker 日志尾随子进程
    pub tail: Option<LogTail>,
    /// 文件尾随线程的停止信号发送端 + JoinHandle：detach 时 send 再 join，
    /// 确保线程真的退出。旧实现只置一个 AtomicBool 标志、不 join，紧邻的重启
    /// 会让新旧两个尾随线程重叠约 1 秒，把同一行日志推两遍。
    pub tail_stop: Option<std::sync::mpsc::Sender<()>>,
    pub tail_join: Option<std::thread::JoinHandle<()>>,
    /// 该服务唯一的日志写入端（按天 + 按字节滚动都在它内部判）
    pub sink: Option<Sink>,
    /// 最近一次采样的资源占用（supervisor 每 tick 刷新）。
    /// None = 测不到（外部驱动拿不到宿主进程、或进程已消失），UI 显示「—」而不是 0。
    pub cpu: Option<f32>,
    pub mem_mb: Option<f32>,
    /// 外部驱动（系统服务）探测回来的宿主进程号，供下一拍按进程树采样
    pub ext_pid: Option<u32>,
    /// 外部驱动（Docker）自己算好的 (cpu%, mem MB)，优先于按 PID 采样
    pub ext_metrics: Option<(f32, f32)>,
    /// spec 解密/解析是否成功（启动时算一次并缓存，避免每轮轮询重复 DPAPI 解密）
    pub config_ok: bool,
    /// 进入 Running 的时间戳（毫秒），用于"稳定运行一段时间后清零重试计数"
    pub running_since: i64,
    /// 自上次手动启动以来观察到的崩溃次数
    pub crash_count: u32,
    /// 最近一次崩溃痕迹（重启回 Running 后仍保留，手动启动才清）
    pub last_crash: Option<crate::types::CrashInfo>,
}

impl Runtime {
    pub fn new(spec: ServiceSpec) -> Self {
        Self {
            state: State::Stopped,
            desired: Desired::Stopped,
            last_error: None,
            unhealthy_streak: 0,
            fail_streak: 0,
            next_attempt_at: 0,
            next_health_at: 0,
            spec: spec.clone(),
            spec_template: spec,
            chosen_port: None,
            live: None,
            adopted: None,
            tail: None,
            tail_stop: None,
            tail_join: None,
            sink: None,
            cpu: None,
            mem_mb: None,
            ext_pid: None,
            ext_metrics: None,
            config_ok: true,
            running_since: 0,
            crash_count: 0,
            last_crash: None,
        }
    }

    /// 面板"看见"的进程号：自己 spawn 的优先，其次是接管的，最后是外部驱动探测回来的宿主
    /// 进程号（只为展示与采样，面板并不拥有它，不能拿它去结束进程）。
    pub fn observed_pid(&self) -> Option<u32> {
        match &self.live {
            Some(l) => Some(l.pid),
            None => self.adopted.as_ref().map(|a| a.pid).or(self.ext_pid),
        }
    }
}

/// 状态变化事件（监督线程通过 channel 发给 UI 广播循环）
#[derive(Clone)]
pub struct StateEvent {
    pub id: String,
    pub from: Option<State>,
    pub state: State,
    pub pid: Option<u32>,
    pub error: Option<String>,
    /// 进程退出码（仅托管进程意外退出时有值），写进 service_events.exit_code
    pub exit_code: Option<i32>,
}

/// 全局共享状态，通过 Tauri 的 manage 注入
pub struct AppState {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub runtimes: Arc<Mutex<HashMap<String, Runtime>>>,
    pub log_root: PathBuf,
    /// 日志行广播函数，app.setup 里注入（闭包捕获批量队列）
    pub emitter: Arc<Mutex<Option<LogEmitter>>>,
    /// 状态变化发送端，setup 里注入
    pub state_tx: Arc<Mutex<Option<Sender<StateEvent>>>>,
    /// 正在执行生命周期操作（启/停/重启）的服务 id，用于串行化同一服务的并发操作
    pub ops: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 正在执行批量启停的分组 key（组 id 或 "all"），防止并发起多条 run_start 链
    pub group_ops: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 上一次重建托盘菜单时的签名；签名没变就不必重建整个菜单
    pub tray_sig: Arc<Mutex<String>>,
    /// 任务队列的发送端（setup 里注入）
    pub task_tx: Arc<Mutex<Option<Sender<crate::types::TaskRunRequest>>>>,
    /// 每个正在运行任务的取消标志（run_id -> flag），支持并行下按运行精确取消
    pub task_cancel_flags: Arc<Mutex<HashMap<String, Arc<std::sync::atomic::AtomicBool>>>>,
    /// 当前正在运行的任务 run id 集合（供 UI 显示"运行中 N"）
    pub task_active: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 最大并发运行数（1 = 串行；可调大做并行）。会话级设置，重启回默认。
    pub task_max_parallel: Arc<std::sync::atomic::AtomicUsize>,
    /// 系统通知回调（title, body），setup 里注入
    pub notify: Arc<Mutex<Option<NotifyFn>>>,
}

impl AppState {
    pub fn new(db: rusqlite::Connection, log_root: PathBuf) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            runtimes: Arc::new(Mutex::new(HashMap::new())),
            log_root,
            emitter: Arc::new(Mutex::new(None)),
            state_tx: Arc::new(Mutex::new(None)),
            ops: Arc::new(Mutex::new(std::collections::HashSet::new())),
            group_ops: Arc::new(Mutex::new(std::collections::HashSet::new())),
            tray_sig: Arc::new(Mutex::new(String::new())),
            task_tx: Arc::new(Mutex::new(None)),
            task_cancel_flags: Arc::new(Mutex::new(HashMap::new())),
            task_active: Arc::new(Mutex::new(std::collections::HashSet::new())),
            task_max_parallel: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
            notify: Arc::new(Mutex::new(None)),
        }
    }

    pub fn log_dir(&self, id: &str) -> PathBuf {
        self.log_root.join(id)
    }

    /// 取（必要时创建）该服务唯一的日志写入端。全局唯一是滚动安全的前提：
    /// 同一时刻只有一个句柄开着这个日志文件，才能按累计字节切分片。
    /// 返回 None 表示服务不存在，或日志目录创建/文件打开失败。
    pub fn log_sink(&self, id: &str) -> Option<Sink> {
        let mut rts = self.runtimes.lock().unwrap_or_else(|e| e.into_inner());
        let rt = rts.get_mut(id)?;
        if rt.sink.is_none() {
            rt.sink = crate::logs::LogSink::open(&self.log_dir(id)).ok();
        }
        rt.sink.clone()
    }

    pub fn current_emitter(&self) -> Option<LogEmitter> {
        self.emitter
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn push_state(&self, ev: StateEvent) {
        if let Some(tx) = self
            .state_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = tx.send(ev);
        }
    }
}
