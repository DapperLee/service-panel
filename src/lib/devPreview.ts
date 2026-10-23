// 浏览器预览用的假后端（`npm run dev` 直接在普通浏览器里看布局与显示规则）。
// 生效条件：DEV 构建 + 窗口没有 __TAURI_INTERNALS__。真壳与打包版永远进不来这个分支。
import type { Bookmark, PortAudit, ServiceView, TaskRun, TaskView } from "../types";
import { defaultSpec } from "../types";

type Patch = Partial<Pick<ServiceView, "state" | "desired" | "pid" | "cpu" | "memMb" | "lastError">>;

function svc(
  id: string,
  name: string,
  driver: ServiceView["spec"]["driver"],
  over: Patch & { enabled?: boolean; sortOrder?: number; groupId?: string | null; configOk?: boolean }
): ServiceView {
  return {
    id,
    name,
    groupId: over.groupId ?? null,
    sortOrder: over.sortOrder ?? 0,
    enabled: over.enabled ?? true,
    spec: { ...defaultSpec(), driver, ...extra(driver) },
    state: "stopped",
    desired: "stopped",
    pid: null,
    cpu: null,
    memMb: null,
    lastError: null,
    configOk: over.configOk ?? true,
    ...over,
  };
}

function extra(driver: ServiceView["spec"]["driver"]) {
  if (driver === "windows_service") return { serviceName: "MySQL84", command: "", args: [] };
  if (driver === "docker") return { container: "search-1", command: "", args: [] };
  return { command: "python", args: ["-m", "http.server", "8000"] };
}

// 一份刻意凑齐各种边界的样本：正常数字、取不到指标、不健康、坏配置、已停止。
// 注意：这里不能在模块顶层构造（顶层函数调用会让 Rollup 认定有副作用，
// 生产包里摇不掉这个模块），所以懒到第一次用到才建。
let list: ServiceView[] = [];
let groups: { id: string; name: string }[] = [];

// 任务/运行/书签的预览数据（懒填充，样本在 ensure() 里）
let tasks: TaskView[] = [];
let runs: TaskRun[] = [];
let bookmarks: Bookmark[] = [];

function ensure() {
  if (list.length) return;
  list = [
    svc("s1", "API 网关", "command", { state: "running", desired: "running", pid: 41001, cpu: 1.4, memMb: 42.5, sortOrder: 1, groupId: "g1" }),
    svc("s2", "MySQL84（系统服务）", "windows_service", { state: "running", desired: "running", pid: 41002, cpu: null, memMb: null, sortOrder: 2, groupId: "g1" }),
    svc("s3", "搜索（Docker）", "docker", { state: "running", desired: "running", pid: null, cpu: null, memMb: null, sortOrder: 3 }),
    svc("s4", "爬虫", "command", {
      state: "unhealthy",
      desired: "running",
      pid: null,
      cpu: null,
      memMb: null,
      sortOrder: 4,
      lastError: "健康检查连续 3 次失败（tcp 127.0.0.1:9009 连接被拒绝）",
    }),
    svc("s5", "报表（配置损坏）", "command", { sortOrder: 5, configOk: false, lastError: "配置解析失败（DPAPI 不可用或数据损坏），请重新编辑保存；启动与导出已临时禁止" }),
    svc("s6", "静态站", "command", { sortOrder: 6 }),
  ];
  groups = [
    { id: "g1", name: "后端" },
    { id: "g2", name: "数据" },
  ];
  tasks = [
    {
      id: "t1",
      name: "Android 打包",
      group: null,
      configOk: true,
      config: {
        command: "npx", args: ["tauri", "android", "build"], cwd: "D:\\code\\app",
        useShell: true, env: {}, encoding: "auto",
        variants: [
          { label: "debug", args: ["--debug"] },
          { label: "release", args: [] },
        ],
        requires: [],
      },
    },
  ];
  bookmarks = [
    { id: "b1", name: "quant", path: "D:\\code\\develop\\gx\\quant" },
  ];
}

const find = (id: string) => list.find((s) => s.id === id);
const tick = () => new Promise((r) => setTimeout(r, 60));
const now = () => Math.floor(Date.now() / 1000);

function setState(id: string, patch: Patch) {
  const s = find(id);
  if (s) Object.assign(s, patch);
}

const audit: PortAudit[] = [
  {
    serviceId: "s4",
    serviceName: "爬虫",
    port: 8000,
    listenerPid: 99999,
    listenerName: "python3",
    conflict: true,
  },
];

// 同样懒建：`Date.now()` 写在顶层会让整个模块被判定有副作用，生产包就摇不掉了
const events = () => [
  { serviceId: "s4", ts: now() - 420, from: "running", to: "unhealthy", reason: "健康检查连续 3 次失败", exitCode: null },
  { serviceId: "s4", ts: now() - 900, from: "starting", to: "running", reason: null, exitCode: null },
  { serviceId: "s1", ts: now() - 1800, from: "crashed", to: "restarting", reason: "进程退出", exitCode: -1 },
];

export const devApi = {
  list: async () => (ensure(), await tick(), list.map((s) => ({ ...s, spec: { ...s.spec } }))),
  add: async (input: any) => {
    ensure();
    const id = `s${Math.random().toString(36).slice(2, 8)}`;
    list.push({ ...input, id, state: "stopped", desired: "stopped", pid: null, cpu: null, memMb: null, lastError: null, configOk: true });
    return id;
  },
  update: async (input: any) => {
    ensure();
    const s = find(input.id);
    if (s) Object.assign(s, { name: input.name, spec: input.spec, enabled: input.enabled, groupId: input.groupId, sortOrder: input.sortOrder });
  },
  remove: async (id: string) => {
    ensure();
    list = list.filter((s) => s.id !== id);
  },
  start: async (id: string) => {
    ensure();
    await tick();
    const s = find(id);
    setState(id, {
      state: "running",
      desired: "running",
      pid: 50000 + (s ? s.sortOrder : 0),
      // 外部驱动在浏览器预览里也保持「测不到」，好让「—」这条规则可见
      cpu: s?.spec.driver === "command" ? 0.8 : null,
      memMb: s?.spec.driver === "command" ? 28.4 : null,
      lastError: null,
    });
  },
  stop: async (id: string) => {
    ensure();
    await tick();
    setState(id, { state: "stopped", desired: "stopped", pid: null, cpu: null, memMb: null });
  },
  restart: async (id: string) => {
    await devApi.stop(id);
    await devApi.start(id);
  },
  startAll: async () => {
    ensure();
    for (const s of list) await devApi.start(s.id);
  },
  stopAll: async () => {
    ensure();
    for (const s of list) await devApi.stop(s.id);
  },
  events: async (limit: number) => events().slice(0, limit),
  getAutostart: async () => false,
  setAutostart: async () => {},
  quit: async () => {},
  isAdmin: async () => true,
  restartAsAdmin: async () => {},
  listGroups: async () => (ensure(), groups),
  saveGroup: async (id: string | null, name: string) => {
    ensure();
    const gid = id ?? `g${Math.random().toString(36).slice(2, 6)}`;
    const hit = groups.find((g) => g.id === gid);
    if (hit) hit.name = name;
    else groups.push({ id: gid, name });
    return gid;
  },
  deleteGroup: async (id: string) => {
    ensure();
    groups = groups.filter((g) => g.id !== id);
    for (const s of list) if (s.groupId === id) s.groupId = null;
  },
  groupOrder: async (id: string) => (ensure(), list.filter((s) => s.groupId === id).map((s) => s.id)),
  startGroup: async (id: string) => {
    ensure();
    for (const s of list.filter((x) => x.groupId === id)) await devApi.start(s.id);
  },
  stopGroup: async (id: string) => {
    ensure();
    for (const s of list.filter((x) => x.groupId === id)) await devApi.stop(s.id);
  },
  exportConfig: async () => (ensure(), list.length),
  importConfig: async () => (ensure(), { servicesAdded: 0, servicesSkipped: list.length, groupsAdded: 0, stopped: 0 }),
  portAudit: async () => audit,
  // —— 任务（浏览器预览桩）——
  listTasks: async () => (ensure(), tasks),
  addTask: async (input: any) => {
    const id = `t${Math.random().toString(36).slice(2, 8)}`;
    tasks.push({ id, name: input.name, group: input.group ?? null, config: input.config, configOk: true });
    return id;
  },
  updateTask: async (input: any) => {
    const t = tasks.find((x) => x.id === input.id);
    if (t) Object.assign(t, { name: input.name, group: input.group ?? null, config: input.config });
  },
  deleteTask: async (id: string) => {
    tasks = tasks.filter((t) => t.id !== id);
  },
  runTask: async (taskId: string, variantIndex: number | null, extraArgs: string) => {
    const t = tasks.find((x) => x.id === taskId);
    const runId = `r${Math.random().toString(36).slice(2, 8)}`;
    runs.unshift({
      id: runId, taskId, taskName: t?.name ?? taskId,
      variant: variantIndex != null && t ? t.config.variants[variantIndex]?.label ?? null : null,
      extraArgs, status: "running", exitCode: null,
      startedAt: now(), finishedAt: null, durationMs: null, logPath: "",
    });
    setTimeout(() => {
      const r = runs.find((x) => x.id === runId);
      if (r) Object.assign(r, { status: "success", exitCode: 0, finishedAt: now(), durationMs: 1200 });
    }, 1200);
    return runId;
  },
  cancelTaskRun: async () => {},
  listTaskRuns: async (limit: number) => runs.slice(0, limit),
  openInExplorer: async () => {},
  openInTerminal: async () => {},
  listBookmarks: async () => (ensure(), bookmarks),
  addBookmark: async (name: string, path: string) => {
    const id = `b${Math.random().toString(36).slice(2, 6)}`;
    bookmarks.push({ id, name, path });
    return id;
  },
  deleteBookmark: async (id: string) => {
    bookmarks = bookmarks.filter((b) => b.id !== id);
  },
};
