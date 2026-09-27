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
  cpu: number;
  memMb: number;
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
