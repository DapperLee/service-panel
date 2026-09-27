use crate::logs::LogEvent;
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
    /// Docker 日志尾随子进程
    pub tail: Option<LogTail>,
    /// 文件尾随线程的停止标志
    pub file_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
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
            tail: None,
            file_stop: None,
            cpu: 0.0,
            mem_mb: 0.0,
            config_ok: true,
            running_since: 0,
        }
    }
}

/// 状态变化事件（监督线程通过 channel 发给 UI 广播循环）
#[derive(Clone)]
pub struct StateEvent {
    pub id: String,
    pub state: State,
    pub pid: Option<u32>,
    pub error: Option<String>,
}

/// 全局共享状态，通过 Tauri 的 manage 注入
pub struct AppState {
    pub db: Arc<Mutex<rusqlite::Connection>>,
    pub runtimes: Arc<Mutex<HashMap<String, Runtime>>>,
    pub log_root: PathBuf,
    /// 日志行广播函数，app.setup 里注入（闭包捕获 AppHandle）
    pub emitter: Arc<Mutex<Option<Arc<dyn Fn(LogEvent) + Send + Sync>>>>,
    /// 状态变化发送端，setup 里注入
    pub state_tx: Arc<Mutex<Option<Sender<StateEvent>>>>,
    /// 正在执行生命周期操作（启/停/重启）的服务 id，用于串行化同一服务的并发操作
    pub ops: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 正在执行批量启停的分组 key（组 id 或 "all"），防止并发起多条 run_start 链
    pub group_ops: Arc<Mutex<std::collections::HashSet<String>>>,
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
        }
    }

    pub fn log_dir(&self, id: &str) -> PathBuf {
        self.log_root.join(id)
    }

    pub fn current_emitter(&self) -> Option<Arc<dyn Fn(LogEvent) + Send + Sync>> {
        self.emitter.lock().unwrap().clone()
    }

    pub fn push_state(&self, ev: StateEvent) {
        if let Some(tx) = self.state_tx.lock().unwrap().as_ref() {
            let _ = tx.send(ev);
        }
    }
}
