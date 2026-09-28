use crate::logs::{LogEmitter, Sink};
use crate::types::{Desired, ServiceSpec, State};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// 一个正在被面板托管的进程（当前只有 Command 驱动需要）
pub struct LiveProc {
    pub child: Child,
    pub pid: u32,
    /// Job Object 句柄，仅用于 Drop 时回收整棵进程树，不会被主动读取
    #[cfg(windows)]
    #[allow(dead_code)]
    pub job: Option<crate::drivers::command::JobHandle>,
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
    /// spec 快照，重启时不需要重查 DB
    pub spec: ServiceSpec,
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
    /// 最近一次采样的资源占用（supervisor 每 tick 刷新）
    pub cpu: f32,
    pub mem_mb: f32,
    /// spec 解密/解析是否成功（启动时算一次并缓存，避免每轮轮询重复 DPAPI 解密）
    pub config_ok: bool,
    /// 进入 Running 的时间戳（毫秒），用于"稳定运行一段时间后清零重试计数"
    pub running_since: i64,
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
            spec,
            live: None,
            adopted: None,
            tail: None,
            tail_stop: None,
            tail_join: None,
            sink: None,
            cpu: 0.0,
            mem_mb: 0.0,
            config_ok: true,
            running_since: 0,
        }
    }

    /// 面板"看见"的进程号：自己 spawn 的优先，其次是接管的。UI 与事件都取这个。
    pub fn observed_pid(&self) -> Option<u32> {
        match &self.live {
            Some(l) => Some(l.pid),
            None => self.adopted.as_ref().map(|a| a.pid),
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
        }
    }

    pub fn log_dir(&self, id: &str) -> PathBuf {
        self.log_root.join(id)
    }

    /// 取（必要时创建）该服务唯一的日志写入端。全局唯一是滚动安全的前提：
    /// 同一时刻只有一个句柄开着这个日志文件，才能按累计字节切分片。
    /// 返回 None 表示服务不存在，或日志目录创建/文件打开失败。
    pub fn log_sink(&self, id: &str) -> Option<Sink> {
        let mut rts = self.runtimes.lock().unwrap();
        let rt = rts.get_mut(id)?;
        if rt.sink.is_none() {
            rt.sink = crate::logs::LogSink::open(&self.log_dir(id)).ok();
        }
        rt.sink.clone()
    }

    pub fn current_emitter(&self) -> Option<LogEmitter> {
        self.emitter.lock().unwrap().clone()
    }

    pub fn push_state(&self, ev: StateEvent) {
        if let Some(tx) = self.state_tx.lock().unwrap().as_ref() {
            let _ = tx.send(ev);
        }
    }
}
