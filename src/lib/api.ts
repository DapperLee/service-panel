import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { devApi } from "./devPreview";
import type {
  Bookmark,
  Group,
  GroupResult,
  ImportOutcome,
  LogEvent,
  LogLine,
  PortAudit,
  ServiceInput,
  ServiceView,
  StateChange,
  TaskInput,
  TaskRun,
  TaskView,
} from "../types";

// 浏览器预览开关：真壳里窗口带 __TAURI_INTERNALS__，打包构建又不是 DEV，两条都不成立。
const browserPreview =
  import.meta.env.DEV &&
  typeof window !== "undefined" &&
  !("__TAURI_INTERNALS__" in window);

const realApi = {
  list: () => invoke<ServiceView[]>("list_services"),
  add: (input: ServiceInput) => invoke<string>("add_service", { input }),
  update: (input: ServiceInput) => invoke<void>("update_service", { input }),
  remove: (id: string) => invoke<void>("delete_service", { id }),
  start: (id: string) => invoke<void>("start_service", { id }),
  stop: (id: string) => invoke<void>("stop_service", { id }),
  restart: (id: string) => invoke<void>("restart_service", { id }),
  startAll: () => invoke<void>("start_all"),
  stopAll: () => invoke<void>("stop_all"),
  events: (limit: number) => invoke<unknown[]>("recent_events", { limit }),
  getAutostart: () => invoke<boolean>("get_autostart"),
  setAutostart: (enabled: boolean) =>
    invoke<void>("set_autostart", { enabled }),
  quit: () => invoke<void>("quit_app"),
  isAdmin: () => invoke<boolean>("is_admin"),
  restartAsAdmin: () => invoke<void>("restart_as_admin"),
  // —— 分组 ——
  listGroups: () => invoke<Group[]>("list_groups"),
  saveGroup: (id: string | null, name: string) =>
    invoke<string>("save_group", { id, name }),
  deleteGroup: (id: string) => invoke<void>("delete_group", { id }),
  groupOrder: (id: string) => invoke<string[]>("group_order", { id }),
  startGroup: (id: string) => invoke<void>("start_group", { id }),
  stopGroup: (id: string) => invoke<void>("stop_group", { id }),
  // —— 配置导出/导入 ——
  exportConfig: (path: string) => invoke<number>("export_config", { path }),
  importConfig: (path: string, replace: boolean) =>
    invoke<ImportOutcome>("import_config", { path, replace }),
  // —— 端口体检 ——
  portAudit: () => invoke<PortAudit[]>("port_audit"),
  // —— 任务 ——
  listTasks: () => invoke<TaskView[]>("list_tasks"),
  addTask: (input: TaskInput) => invoke<string>("add_task", { input }),
  updateTask: (input: TaskInput) => invoke<void>("update_task", { input }),
  deleteTask: (id: string) => invoke<void>("delete_task", { id }),
  runTask: (taskId: string, variantIndex: number | null, extraArgs: string) =>
    invoke<string>("run_task", { taskId, variantIndex, extraArgs }),
  cancelTaskRun: (runId: string) => invoke<void>("cancel_task_run", { runId }),
  listTaskRuns: (limit: number) => invoke<TaskRun[]>("list_task_runs", { limit }),
  readTaskLog: (runId: string) => invoke<LogLine[]>("read_task_log", { runId }),
  readServiceLog: (id: string) => invoke<LogLine[]>("read_service_log", { id }),
  getTaskMaxParallel: () => invoke<number>("get_task_max_parallel"),
  setTaskMaxParallel: (n: number) => invoke<void>("set_task_max_parallel", { n }),
  openInExplorer: (path: string) => invoke<void>("open_in_explorer", { path }),
  openInTerminal: (path: string) => invoke<void>("open_in_terminal", { path }),
  // —— 书签 ——
  listBookmarks: () => invoke<Bookmark[]>("list_bookmarks"),
  addBookmark: (name: string, path: string) =>
    invoke<string>("add_bookmark", { name, path }),
  deleteBookmark: (id: string) => invoke<void>("delete_bookmark", { id }),
};

export const api = (browserPreview ? devApi : realApi) as typeof realApi;

const noListen: Promise<UnlistenFn> = Promise.resolve(() => {});

export function onLog(cb: (events: LogEvent[]) => void): Promise<UnlistenFn> {
  if (browserPreview) return noListen;
  return listen<LogEvent[]>("log://batch", (ev) => cb(ev.payload));
}

export function onStateChange(
  cb: (e: StateChange) => void
): Promise<UnlistenFn> {
  if (browserPreview) return noListen;
  return listen<StateChange>("state://change", (ev) => cb(ev.payload));
}

export function onGroupResult(
  cb: (e: GroupResult) => void
): Promise<UnlistenFn> {
  if (browserPreview) return noListen;
  return listen<GroupResult>("group://result", (ev) => cb(ev.payload));
}

export interface GroupProgress {
  group: string;
  op: "start" | "stop";
  current: string;
  name: string;
  index: number;
  total: number;
}

export function onGroupProgress(
  cb: (e: GroupProgress) => void
): Promise<UnlistenFn> {
  if (browserPreview) return noListen;
  return listen<GroupProgress>("group://progress", (ev) => cb(ev.payload));
}
