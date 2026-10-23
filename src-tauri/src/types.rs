use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 驱动类型：决定 start/stop/status 走哪条实现路径
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    /// 命令行进程（Python / Node / Go / 绿色版 exe）——面板亲自托管
    Command,
    /// 已安装为 Windows 服务（MySQL/Redis 装成服务时用）
    WindowsService,
    /// Docker 容器
    Docker,
}

/// 健康检查类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthKind {
    /// 不做健康检查，仅看进程存活
    None,
    /// TCP 端口能握手即健康
    Tcp,
    /// HTTP GET 返回 2xx/3xx 即健康；支持 http:// 与 https://
    Http,
    /// 跑一条命令，退出码 0 即健康
    Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheck {
    pub kind: HealthKind,
    /// tcp: "host:port"；http: 完整 url；command: shell 命令
    pub target: String,
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// 连续失败多少次判定为不健康
    #[serde(default = "default_threshold")]
    pub unhealthy_threshold: u32,
    /// https 探测是否放过自签证书与域名不匹配（本地 dev 证书常需要）。
    /// 对 http:// 无意义。
    #[serde(default)]
    pub tls_insecure: bool,
}

fn default_interval() -> u64 {
    10
}
fn default_timeout() -> u64 {
    3
}
fn default_threshold() -> u32 {
    3
}

impl Default for HealthCheck {
    fn default() -> Self {
        Self {
            kind: HealthKind::None,
            target: String::new(),
            interval_secs: 10,
            timeout_secs: 3,
            unhealthy_threshold: 3,
            tls_insecure: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartMode {
    /// 永不自动重启
    Never,
    /// 进程异常退出（非用户主动停止）时重启
    OnFailure,
    /// 只要不为 Running 就尝试拉起（含健康检查失败）
    Always,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartPolicy {
    pub mode: RestartMode,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
    /// 指数退避基数（秒）：backoff = base * 2^(attempt-1)，上限 60s
    #[serde(default = "default_backoff")]
    pub backoff_base_secs: u64,
}

fn default_max_retries() -> u32 {
    5
}
fn default_backoff() -> u64 {
    2
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            mode: RestartMode::OnFailure,
            max_retries: 5,
            backoff_base_secs: 2,
        }
    }
}

/// 服务的完整声明式配置。DB 中以 JSON 存 spec_json 列。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceSpec {
    pub driver: DriverKind,
    // —— Command 驱动字段 ——
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// 子进程 stdout 编码：utf-8 | gbk
    #[serde(default = "default_encoding")]
    pub encoding: String,
    /// 优雅停止等待秒数，超时后强杀进程树
    #[serde(default = "default_stop_timeout")]
    pub stop_timeout_secs: u64,
    // —— WindowsService 驱动字段 ——
    #[serde(default)]
    pub service_name: String,
    // —— Docker 驱动字段 ——
    #[serde(default)]
    pub container: String,
    /// 日志文件路径：Windows 服务（无 stdout）尾随该文件；也可给命令型服务用。
    /// Docker 留空则用 `docker logs -f`，填了则尾随该文件。
    #[serde(default)]
    pub log_path: String,
    // —— 自动端口 ——
    /// 默认端口被占时，自动往上找一个空闲端口启动
    #[serde(default)]
    pub auto_port: bool,
    /// 首选端口（被占则从它往上找空闲端口）
    #[serde(default)]
    pub default_port: u16,
    /// 把选定的端口注入到该环境变量（很多服务读 PORT）；留空则不注入 env
    #[serde(default = "default_port_env_key")]
    pub port_env_key: String,
    // —— 通用 ——
    #[serde(default)]
    pub healthcheck: HealthCheck,
    #[serde(default)]
    pub restart_policy: RestartPolicy,
    /// 启动前必须先健康的服务 id（拓扑排序）
    #[serde(default)]
    pub after: Vec<String>,
}

fn default_encoding() -> String {
    "utf-8".to_string()
}
fn default_stop_timeout() -> u64 {
    10
}
fn default_port_env_key() -> String {
    "PORT".to_string()
}

impl Default for ServiceSpec {
    fn default() -> Self {
        Self {
            driver: DriverKind::Command,
            command: String::new(),
            args: Vec::new(),
            cwd: String::new(),
            env: HashMap::new(),
            encoding: default_encoding(),
            stop_timeout_secs: default_stop_timeout(),
            service_name: String::new(),
            container: String::new(),
            log_path: String::new(),
            auto_port: false,
            default_port: 0,
            port_env_key: default_port_env_key(),
            healthcheck: HealthCheck::default(),
            restart_policy: RestartPolicy::default(),
            after: Vec::new(),
        }
    }
}

/// 运行时状态机
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Stopped,
    Starting,
    Running,
    Stopping,
    /// 进程在但健康检查连续失败
    Unhealthy,
    /// 非预期退出，等待自动重启
    Crashed,
    /// 正在执行自动重启
    Restarting,
}

/// 用户期望态——与真实态分离，防止手动停止后被重启策略又拉起
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Desired {
    Running,
    Stopped,
}

/// 暴露给前端的服务视图对象
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceView {
    pub id: String,
    pub name: String,
    pub group_id: Option<String>,
    pub sort_order: i64,
    pub enabled: bool,
    pub spec: ServiceSpec,
    pub state: State,
    pub desired: Desired,
    pub pid: Option<u32>,
    /// None = 测不到（外部驱动拿不到宿主进程），前端显示「—」而不是 0
    pub cpu: Option<f32>,
    pub mem_mb: Option<f32>,
    pub last_error: Option<String>,
    /// spec 解密/解析是否成功。false 表示配置损坏（如 DPAPI 不可用），
    /// 列表会标红且导出会中止，避免把空配置当真实配置写进备份。
    #[serde(default = "default_true")]
    pub config_ok: bool,
    /// 自动端口模式下本次实际选定的端口（非自动为 None）
    #[serde(default)]
    pub chosen_port: Option<u16>,
}

fn default_true() -> bool {
    true
}

/// 一行日志
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    pub ts: i64,
    pub stream: String, // stdout | stderr | system
    pub text: String,
}

/// 新建/更新服务时的入参（前端表单 -> 后端）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInput {
    pub id: Option<String>,
    pub name: String,
    pub group_id: Option<String>,
    pub sort_order: i64,
    pub enabled: bool,
    pub spec: ServiceSpec,
}

// ============================ 一次性任务 ============================

/// 任务变体：同一命令的一组预设参数（如 android / ios / release）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskVariant {
    pub label: String,
    #[serde(default)]
    pub args: Vec<String>,
}

/// 任务配置（存 config_json，含 env，DPAPI 加密）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskConfig {
    #[serde(default)]
    pub command: String,
    /// 无变体时的默认参数
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    /// true 则经 shell 执行（npx、&&、管道等需要）；false 直接 spawn
    #[serde(default)]
    pub use_shell: bool,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default = "default_encoding")]
    pub encoding: String,
    #[serde(default)]
    pub variants: Vec<TaskVariant>,
    /// 运行前必须处于 Running 的服务 id（依赖服务）
    #[serde(default)]
    pub requires: Vec<String>,
}

impl Default for TaskConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            cwd: String::new(),
            use_shell: false,
            env: HashMap::new(),
            encoding: default_encoding(),
            variants: Vec::new(),
            requires: Vec::new(),
        }
    }
}

/// 暴露给前端 + 存库的任务视图
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub id: String,
    pub name: String,
    pub group: Option<String>,
    pub config: TaskConfig,
    #[serde(default = "default_true")]
    pub config_ok: bool,
}

/// 任务运行状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskRunStatus {
    Queued,
    Running,
    Success,
    Failed,
    Cancelled,
}

/// 一次运行记录
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskRun {
    pub id: String,
    pub task_id: String,
    pub task_name: String,
    pub variant: Option<String>,
    pub extra_args: String,
    pub status: TaskRunStatus,
    pub exit_code: Option<i32>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub log_path: String,
}

/// 目录书签（一键打开终端/资源管理器）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bookmark {
    pub id: String,
    pub name: String,
    pub path: String,
}

/// 新建/更新任务入参
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInput {
    pub id: Option<String>,
    pub name: String,
    pub group: Option<String>,
    pub config: TaskConfig,
}

/// 队列内部：一次运行请求（不序列化）
#[derive(Debug, Clone)]
pub struct TaskRunRequest {
    pub run_id: String,
    pub config: TaskConfig,
    pub variant_index: Option<usize>,
    pub extra_args: Vec<String>,
}
