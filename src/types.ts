// 与 Rust 端 types.rs 对齐（serde 用 camelCase，枚举用 snake_case）
export type DriverKind = "command" | "windows_service" | "docker";
export type HealthKind = "none" | "tcp" | "http" | "command";
export type RestartMode = "never" | "on_failure" | "always";
export type Desired = "running" | "stopped";
export type State =
  | "stopped"
  | "starting"
  | "running"
  | "stopping"
  | "unhealthy"
  | "crashed"
  | "restarting";

export interface HealthCheck {
  kind: HealthKind;
  target: string;
  intervalSecs: number;
  timeoutSecs: number;
  unhealthyThreshold: number;
  /** https 探测放过自签证书/域名不匹配（本地 dev 证书）。省略=不放过 */
  tlsInsecure?: boolean;
}

/** 端口冲突体检结果：服务声明的端口被别的进程占着 */
export interface PortAudit {
  serviceId: string;
  serviceName: string;
  port: number;
  listenerPid: number | null;
  listenerName: string | null;
  conflict: boolean;
}

export interface RestartPolicy {
  mode: RestartMode;
  maxRetries: number;
  backoffBaseSecs: number;
}

export interface ServiceSpec {
  driver: DriverKind;
  command: string;
  args: string[];
  cwd: string;
  env: Record<string, string>;
  encoding: string;
  stopTimeoutSecs: number;
  serviceName: string;
  container: string;
  logPath: string;
  healthcheck: HealthCheck;
  restartPolicy: RestartPolicy;
  after: string[];
}

export interface ServiceView {
  id: string;
  name: string;
  groupId: string | null;
  sortOrder: number;
  enabled: boolean;
  spec: ServiceSpec;
  state: State;
  desired: Desired;
  pid: number | null;
  /** null = 测不到（外部驱动拿不到宿主进程），界面显示「—」而不是 0 */
  cpu: number | null;
  memMb: number | null;
  lastError: string | null;
  configOk: boolean;
}

export interface ServiceInput {
  id?: string | null;
  name: string;
  groupId: string | null;
  sortOrder: number;
  enabled: boolean;
  spec: ServiceSpec;
}

export interface LogLine {
  ts: number;
  stream: string;
  text: string;
}

export interface LogEvent {
  serviceId: string;
  line: LogLine;
}

export interface StateChange {
  id: string;
  state: State;
  pid: number | null;
  error: string | null;
}

export interface Group {
  id: string;
  name: string;
}

export interface GroupResult {
  group: string;
  op: "start" | "stop";
  ok: boolean;
  error: string | null;
}

export interface ImportOutcome {
  servicesAdded: number;
  servicesSkipped: number;
  groupsAdded: number;
  stopped: number;
}

// ============================ 任务 ============================
export interface TaskVariant {
  label: string;
  args: string[];
}

export interface TaskConfig {
  command: string;
  args: string[];
  cwd: string;
  useShell: boolean;
  env: Record<string, string>;
  encoding: string;
  variants: TaskVariant[];
  requires: string[];
}

export interface TaskView {
  id: string;
  name: string;
  group: string | null;
  config: TaskConfig;
  configOk: boolean;
}

export type TaskRunStatus =
  | "queued"
  | "running"
  | "success"
  | "failed"
  | "cancelled";

export interface TaskRun {
  id: string;
  taskId: string;
  taskName: string;
  variant: string | null;
  extraArgs: string;
  status: TaskRunStatus;
  exitCode: number | null;
  startedAt: number;
  finishedAt: number | null;
  durationMs: number | null;
  logPath: string;
}

export interface TaskInput {
  id?: string | null;
  name: string;
  group: string | null;
  config: TaskConfig;
}

export interface Bookmark {
  id: string;
  name: string;
  path: string;
}

export function defaultTaskConfig(): TaskConfig {
  return {
    command: "",
    args: [],
    cwd: "",
    useShell: true,
    env: {},
    encoding: "auto",
    variants: [],
    requires: [],
  };
}

export function defaultSpec(): ServiceSpec {
  return {
    driver: "command",
    command: "",
    args: [],
    cwd: "",
    env: {},
    encoding: "auto",
    stopTimeoutSecs: 10,
    serviceName: "",
    container: "",
    logPath: "",
    healthcheck: {
      kind: "none",
      target: "",
      intervalSecs: 10,
      timeoutSecs: 3,
      unhealthyThreshold: 3,
    },
    restartPolicy: {
      mode: "on_failure",
      maxRetries: 5,
      backoffBaseSecs: 2,
    },
    after: [],
  };
}
