import { useCallback, useEffect, useRef, useState } from "react";
import { api, onGroupProgress, onGroupResult, onLog, onStateChange } from "./lib/api";
import type { Group, LogLine, ServiceView } from "./types";
import ServiceCard from "./components/ServiceCard";
import LogConsole from "./components/LogConsole";
import AddServiceDialog from "./components/AddServiceDialog";
import SettingsDialog from "./components/SettingsDialog";
import GroupManagerDialog from "./components/GroupManagerDialog";
import DependencyGraph from "./components/DependencyGraph";
import EventsDialog from "./components/EventsDialog";
import TasksView from "./components/TasksView";

// 每个服务在内存里保留的日志行数上限（与 LogConsole 的显示上限一致）
const MAX_LOG_LINES = 2000;

export default function App() {
  const [services, setServices] = useState<ServiceView[]>([]);
  const [groups, setGroups] = useState<Group[]>([]);
  const [selectedGroupId, setSelectedGroupId] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [logs, setLogs] = useState<Record<string, LogLine[]>>({});
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editing, setEditing] = useState<ServiceView | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [groupMgrOpen, setGroupMgrOpen] = useState(false);
  const [graphOpen, setGraphOpen] = useState(false);
  const [eventsOpen, setEventsOpen] = useState(false);
  const [view, setView] = useState<"services" | "tasks">("services");
  const [notAdmin, setNotAdmin] = useState(false);
  const [busy, setBusy] = useState(false);
  const [toast, setToast] = useState<string | null>(null);

  const loadGroups = useCallback(async () => {
    try {
      setGroups(await api.listGroups());
    } catch (e) {
      console.error(e);
    }
  }, []);

  // 事件回调里用 ref 读取最新 id，避免闭包过期
  const refresh = useCallback(async () => {
    try {
      const list = await api.list();
      // 直接信任后端状态：start/stop 在命令返回前已同步更新状态，
      // 且状态变化会经 state://change 事件即时推送，无需前端再做“保留过渡态”
      // 的防抖（那会导致 stopping 永远回不到 stopped）。
      setServices(list);
    } catch (e) {
      console.error(e);
    }
  }, []);

  // 初始加载 + 轮询（资源数值）
  useEffect(() => {
    refresh();
    loadGroups();
    api.isAdmin().then((ok) => setNotAdmin(!ok)).catch(() => {});
    const t = setInterval(refresh, 2500);
    return () => clearInterval(t);
  }, [refresh, loadGroups]);

  // 订阅日志流
  useEffect(() => {
    const un = onLog((events) => {
      if (events.length === 0) return;
      // 先按 serviceId 归并，再对每个服务只做一次数组拷贝。
      // 逐事件 spread 会让一批 N 行退化成 N 次 O(2000) 拷贝——省了 IPC 却加重了拷贝。
      const grouped: Record<string, LogLine[]> = {};
      for (const ev of events) {
        (grouped[ev.serviceId] ??= []).push(ev.line);
      }
      setLogs((prev) => {
        const next = { ...prev };
        for (const [id, lines] of Object.entries(grouped)) {
          const merged = [...(next[id] ?? []), ...lines];
          next[id] =
            merged.length > MAX_LOG_LINES
              ? merged.slice(merged.length - MAX_LOG_LINES)
              : merged;
        }
        return next;
      });
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  // 订阅状态变化：立即刷新一次
  const lastStateRefresh = useRef(0);
  useEffect(() => {
    const un = onStateChange(() => {
      const now = Date.now();
      if (now - lastStateRefresh.current > 400) {
        lastStateRefresh.current = now;
        refresh();
      }
    });
    return () => {
      un.then((f) => f());
    };
  }, [refresh]);

  // 订阅分组批量操作结果
  useEffect(() => {
    const un = onGroupResult((e) => {
      refresh();
      setToast(
        e.ok
          ? `${e.group} ${e.op === "start" ? "已全部启动" : "已全部停止"}`
          : `${e.group} ${e.op === "start" ? "启动" : "停止"}中断：${e.error}`
      );
      setTimeout(() => setToast(null), 4000);
    });
    return () => {
      un.then((f) => f());
    };
  }, [refresh]);

  // 订阅分组批量操作进度（逐服务，避免长链静默数十秒）
  useEffect(() => {
    const un = onGroupProgress((e) => {
      setToast(
        `${e.group}：${e.op === "start" ? "启动" : "停止"}中 ${e.index}/${e.total} · ${e.name}`
      );
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  const selected = services.find((s) => s.id === selectedId) ?? null;
  const runningCount = services.filter((s) => s.state === "running").length;

  const bulk = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await fn();
      await refresh();
    } finally {
      setBusy(false);
    }
  };

  const visible =
    selectedGroupId === null
      ? services
      : services.filter((s) => s.groupId === selectedGroupId);
  const groupName = groups.find((g) => g.id === selectedGroupId)?.name ?? null;

  return (
    <div className="app">
      <div className="topbar">
        <h1>本地服务管理面板</h1>
        <div className="viewswitch">
          <button
            className={view === "services" ? "active" : "ghost"}
            onClick={() => setView("services")}
          >
            服务
          </button>
          <button
            className={view === "tasks" ? "active" : "ghost"}
            onClick={() => setView("tasks")}
          >
            任务
          </button>
        </div>
        <div className="summary">
          <span>
            {runningCount}/{services.length} 运行中
          </span>
        </div>
        <div className="spacer" />
        {view === "services" && (
          <>
            <button disabled={busy} onClick={() => bulk(() => api.startAll())}>
              全部启动
            </button>
            <button disabled={busy} onClick={() => bulk(() => api.stopAll())}>
              全部停止
            </button>
            <button className="ghost" onClick={() => setGroupMgrOpen(true)}>
              管理分组
            </button>
            <button className="ghost" onClick={() => setEventsOpen(true)}>
              时间线
            </button>
          </>
        )}
        <button className="ghost" onClick={() => setSettingsOpen(true)}>
          设置
        </button>
        {view === "services" && (
          <button className="primary" onClick={() => { setEditing(null); setDialogOpen(true); }}>
            + 添加服务
          </button>
        )}
      </div>

      {notAdmin && (
        <div className="admin-banner">
          <span>当前非管理员身份，无法启停 Windows 服务（sc 会报错误码 5）。</span>
          <button
            className="primary"
            onClick={() => {
              if (confirm("以管理员身份重启面板？会弹出 UAC 确认。")) {
                api.restartAsAdmin().catch((e) => setToast(String(e)));
              }
            }}
          >
            以管理员重启
          </button>
          <button className="ghost" onClick={() => setNotAdmin(false)}>
            忽略
          </button>
        </div>
      )}

      {view === "services" && (
        <>
      {/* 分组标签栏 */}
      <div className="groupbar">
        <button
          className={`tab${selectedGroupId === null ? " active" : ""}`}
          onClick={() => setSelectedGroupId(null)}
        >
          全部
        </button>
        {groups.map((g) => {
          const cnt = services.filter((s) => s.groupId === g.id).length;
          return (
            <button
              key={g.id}
              className={`tab${selectedGroupId === g.id ? " active" : ""}`}
              onClick={() => setSelectedGroupId(g.id)}
            >
              {g.name}
              <span className="tab-count">{cnt}</span>
            </button>
          );
        })}
        <button
          className="ghost"
          onClick={() => setGraphOpen(true)}
          disabled={services.length === 0}
        >
          依赖图
        </button>
        <div className="spacer" />
        {selectedGroupId !== null && (
          <>
            <span className="group-hint">
              按依赖顺序{groupName ? `「${groupName}」` : ""}
            </span>
            <button
              disabled={busy}
              onClick={() => bulk(() => api.startGroup(selectedGroupId))}
            >
              ▶ 本组启动
            </button>
            <button
              disabled={busy}
              onClick={() => bulk(() => api.stopGroup(selectedGroupId))}
            >
              ■ 本组停止
            </button>
          </>
        )}
      </div>

      <div className="body">
        <div className="grid-wrap">
          {visible.length === 0 ? (
            <div className="grid">
              <div className="empty">
                {selectedGroupId === null
                  ? "还没有服务。点击右上角「+ 添加服务」，把你的 Python / Node / Go 服务或 MySQL、Redis 加进来。"
                  : "该分组下还没有服务。用「+ 添加服务」或编辑服务，把它归入本组。"}
              </div>
            </div>
          ) : (
            <div className="grid">
              {visible.map((svc) => (
                <ServiceCard
                  key={svc.id}
                  svc={svc}
                  selected={svc.id === selectedId}
                  onSelect={setSelectedId}
                  onEdit={(s) => {
                    setEditing(s);
                    setDialogOpen(true);
                  }}
                  onChanged={refresh}
                />
              ))}
            </div>
          )}
        </div>

        <LogConsole
          serviceId={selectedId}
          serviceName={selected?.name ?? null}
          logs={logs}
          onClear={(id) => setLogs((p) => ({ ...p, [id]: [] }))}
        />
      </div>
        </>
      )}

      {view === "tasks" && (
        <TasksView
          services={services}
          logs={logs}
          onClearLog={(id) => setLogs((p) => ({ ...p, [id]: [] }))}
        />
      )}

      {toast && <div className="toast">{toast}</div>}

      {dialogOpen && (
        <AddServiceDialog
          editing={editing}
          groups={groups}
          allServices={services}
          onClose={() => setDialogOpen(false)}
          onSaved={() => {
            refresh();
            loadGroups();
          }}
          onGroupsChanged={loadGroups}
        />
      )}

      {groupMgrOpen && (
        <GroupManagerDialog
          groups={groups}
          onClose={() => setGroupMgrOpen(false)}
          onChanged={loadGroups}
        />
      )}

      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}

      {graphOpen && (
        <DependencyGraph
          services={visible}
          groupName={groupName}
          onClose={() => setGraphOpen(false)}
        />
      )}

      {eventsOpen && (
        <EventsDialog services={services} onClose={() => setEventsOpen(false)} />
      )}
    </div>
  );
}
