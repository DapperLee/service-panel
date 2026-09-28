import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  Group,
  GroupResult,
  ImportOutcome,
  LogEvent,
  PortAudit,
  ServiceInput,
  ServiceView,
  StateChange,
} from "../types";

export const api = {
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
};

export function onLog(cb: (events: LogEvent[]) => void): Promise<UnlistenFn> {
  return listen<LogEvent[]>("log://batch", (ev) => cb(ev.payload));
}

export function onStateChange(
  cb: (e: StateChange) => void
): Promise<UnlistenFn> {
  return listen<StateChange>("state://change", (ev) => cb(ev.payload));
}

export function onGroupResult(
  cb: (e: GroupResult) => void
): Promise<UnlistenFn> {
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
  return listen<GroupProgress>("group://progress", (ev) => cb(ev.payload));
}
