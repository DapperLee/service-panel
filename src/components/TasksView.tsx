import { useCallback, useEffect, useState } from "react";
import { api } from "../lib/api";
import type {
  Bookmark,
  LogLine,
  ServiceView,
  TaskRun,
  TaskRunStatus,
  TaskView,
} from "../types";
import TaskDialog from "./TaskDialog";
import LogConsole from "./LogConsole";

const STATUS_LABEL: Record<TaskRunStatus, string> = {
  queued: "排队",
  running: "运行中",
  success: "成功",
  failed: "失败",
  cancelled: "已取消",
};
const STATUS_COLOR: Record<TaskRunStatus, string> = {
  queued: "var(--gray)",
  running: "var(--yellow)",
  success: "var(--green)",
  failed: "var(--red)",
  cancelled: "var(--gray)",
};

function fmtDur(ms: number | null): string {
  if (ms == null) return "—";
  if (ms < 1000) return `${ms}ms`;
  const s = ms / 1000;
  return s < 60 ? `${s.toFixed(1)}s` : `${Math.floor(s / 60)}m${Math.round(s % 60)}s`;
}

interface Props {
  services: ServiceView[];
  logs: Record<string, LogLine[]>;
  onClearLog: (id: string) => void;
  /** 把从文件读回的历史日志注入共享日志流（仅当该 run 内存里还没有时） */
  onInjectLog: (id: string, lines: LogLine[]) => void;
}

export default function TasksView({ services, logs, onClearLog, onInjectLog }: Props) {
  const [tasks, setTasks] = useState<TaskView[]>([]);
  const [runs, setRuns] = useState<TaskRun[]>([]);
  const [bookmarks, setBookmarks] = useState<Bookmark[]>([]);
  const [selectedRun, setSelectedRun] = useState<string | null>(null);
  const [editOpen, setEditOpen] = useState(false);
  const [editing, setEditing] = useState<TaskView | null>(null);
  const [runFor, setRunFor] = useState<TaskView | null>(null);
  const [selGroup, setSelGroup] = useState<string | null>(null);
  const [maxParallel, setMaxParallel] = useState(1);
  const [runningGroup, setRunningGroup] = useState(false);
  const [historyTask, setHistoryTask] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const [t, r, b, mp] = await Promise.all([
        api.listTasks(),
        api.listTaskRuns(50),
        api.listBookmarks(),
        api.getTaskMaxParallel(),
      ]);
      setTasks(t);
      setRuns(r);
      setBookmarks(b);
      setMaxParallel(mp);
    } catch (e) {
      console.error(e);
    }
  }, []);

  useEffect(() => {
    load();
    const timer = setInterval(load, 2000);
    return () => clearInterval(timer);
  }, [load]);

  const del = async (t: TaskView) => {
    if (confirm(`删除任务「${t.name}」及其运行历史？`)) {
      await api.deleteTask(t.id);
      load();
    }
  };

  const cancel = async (runId: string) => {
    try {
      await api.cancelTaskRun(runId);
    } catch (e) {
      alert(String(e));
    }
    load();
  };

  const selectedRunObj = runs.find((r) => r.id === selectedRun) || null;

  // 从任务的 group 字段派生分组列表（去重、非空）
  const groups = Array.from(
    new Set(tasks.map((t) => t.group).filter((g): g is string => !!g))
  ).sort();
  const filtered = selGroup === null ? tasks : tasks.filter((t) => t.group === selGroup);

  const runGroup = async () => {
    const list = filtered.filter((t) => t.configOk);
    if (list.length === 0) return;
    if (!confirm(`运行「${selGroup ?? "全部"}」下的 ${list.length} 个任务？（各按其上次选的变体）`)) return;
    setRunningGroup(true);
    try {
      for (const t of list) {
        // 用该任务上次运行的变体（没跑过则默认参数）
        await api.runTask(t.id, t.lastVariantIndex ?? null, "");
      }
      load();
    } catch (e) {
      alert(String(e));
    } finally {
      setRunningGroup(false);
    }
  };

  const changeParallel = async (n: number) => {
    const v = Math.max(1, Math.min(16, n || 1));
    setMaxParallel(v);
    try {
      await api.setTaskMaxParallel(v);
    } catch (e) {
      alert(String(e));
    }
  };

  // 每个任务最近一次运行（用于卡片结果徽章）
  const lastRunByTask = new Map<string, TaskRun>();
  for (const r of runs) if (!lastRunByTask.has(r.taskId)) lastRunByTask.set(r.taskId, r);

  // 历史按任务筛选
  const visibleRuns = historyTask ? runs.filter((r) => r.taskId === historyTask) : runs;

  // 选中一个已结束的运行时，若内存无日志则从文件读回
  useEffect(() => {
    if (!selectedRun) return;
    if (logs[selectedRun] && logs[selectedRun].length > 0) return;
    const r = runs.find((x) => x.id === selectedRun);
    if (!r || r.status === "running" || r.status === "queued") return;
    api
      .readTaskLog(selectedRun)
      .then((lines) => {
        if (lines.length) onInjectLog(selectedRun, lines);
      })
      .catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedRun, runs]);

  return (
    <div className="body">
      <div className="grid-wrap">
        <div className="section-head">
          <strong>任务</strong>
          <span className="spacer" style={{ flex: 1 }} />
          <label className="conc" title="同时运行的任务数（1=串行）。构建吃资源，按需调大。">
            并发数
            <input
              type="number"
              min={1}
              max={16}
              value={maxParallel}
              onChange={(e) => changeParallel(Number(e.target.value))}
            />
          </label>
          <button className="primary" onClick={() => { setEditing(null); setEditOpen(true); }}>
            + 新建任务
          </button>
        </div>

        <div className="groupbar">
          <button
            className={`tab${selGroup === null ? " active" : ""}`}
            onClick={() => setSelGroup(null)}
          >
            全部
          </button>
          {groups.map((g) => {
            const cnt = tasks.filter((t) => t.group === g).length;
            return (
              <button
                key={g}
                className={`tab${selGroup === g ? " active" : ""}`}
                onClick={() => setSelGroup(g)}
              >
                {g}
                <span className="tab-count">{cnt}</span>
              </button>
            );
          })}
          <div className="spacer" />
          {selGroup !== null && (
            <button disabled={runningGroup} onClick={runGroup}>
              ▶ 运行本组
            </button>
          )}
        </div>

        {filtered.length === 0 ? (
          <div className="empty">
            {selGroup === null
              ? "还没有任务。新建一个，把常用的构建/命令存起来，一键就跑。"
              : "该分组下还没有任务。编辑任务填「分组」即可归入本组。"}
          </div>
        ) : (
          <div className="grid">
            {filtered.map((t) => {
              const last = lastRunByTask.get(t.id);
              return (
              <div className="card" key={t.id}>
                <div className="head">
                  <span className="name">{t.name}</span>
                  {t.group && <span className="tag">{t.group}</span>}
                  {last && (
                    <span
                      className="tag"
                      style={{ color: STATUS_COLOR[last.status] }}
                      title={`上次：${STATUS_LABEL[last.status]} ${new Date(
                        last.startedAt * 1000
                      ).toLocaleString("zh-CN")}`}
                    >
                      {last.status === "success"
                        ? "✓"
                        : last.status === "failed"
                        ? "✗"
                        : last.status === "cancelled"
                        ? "⊘"
                        : "●"}{" "}
                      {fmtDur(last.durationMs)}
                    </span>
                  )}
                </div>
                <div className="meta">
                  <span className="mono" title={t.config.command}>
                    {t.config.command} {t.config.args.join(" ")}
                  </span>
                </div>
                {!t.configOk && <div className="err">配置解析失败，请重新编辑</div>}
                <div className="actions">
                  <button
                    className="primary"
                    disabled={!t.configOk}
                    onClick={() => setRunFor(t)}
                  >
                    运行
                  </button>
                  <button className="ghost" onClick={() => { setEditing(t); setEditOpen(true); }}>
                    编辑
                  </button>
                  <button
                    className="ghost"
                    disabled={!t.config.cwd}
                    onClick={() => api.openInTerminal(t.config.cwd)}
                  >
                    终端
                  </button>
                  <button
                    className="ghost"
                    disabled={!t.config.cwd}
                    onClick={() => api.openInExplorer(t.config.cwd)}
                  >
                    目录
                  </button>
                  <button className="ghost danger" onClick={() => del(t)}>
                    删除
                  </button>
                </div>
              </div>
              );
            })}
          </div>
        )}

        <div className="section-head" style={{ marginTop: 18 }}>
          <strong>运行历史</strong>
          {historyTask && (
            <button
              className="ghost"
              style={{ marginLeft: 10 }}
              onClick={() => setHistoryTask(null)}
            >
              只看：{tasks.find((t) => t.id === historyTask)?.name ?? historyTask} ✕
            </button>
          )}
        </div>
        {visibleRuns.length === 0 ? (
          <div style={{ color: "var(--text-dim)", padding: "8px 2px" }}>暂无运行记录。</div>
        ) : (
          <div className="event-list">
            {visibleRuns.map((r) => (
              <div
                className="event-row run-row"
                key={r.id}
                style={{ cursor: "pointer" }}
                onClick={() => setSelectedRun(r.id)}
              >
                <span style={{ color: STATUS_COLOR[r.status], fontWeight: 600 }}>
                  {STATUS_LABEL[r.status]}
                </span>
                <span className="event-svc">
                  {r.taskName}
                  {r.variant ? ` · ${r.variant}` : ""}
                </span>
                <span className="event-time">{fmtDur(r.durationMs)}</span>
                <span className="event-reason">
                  {r.exitCode != null && r.exitCode !== 0 ? `exit ${r.exitCode}` : ""}
                  {r.extraArgs ? ` ${r.extraArgs}` : ""}
                </span>
                <span className="run-actions" onClick={(e) => e.stopPropagation()}>
                  <button
                    className="ghost"
                    title="只看该任务的历史"
                    onClick={() => setHistoryTask(r.taskId)}
                  >
                    筛选
                  </button>
                  <button
                    className="ghost"
                    title="按这次运行的变体和参数重跑"
                    onClick={() =>
                      api
                        .runTask(r.taskId, r.variantIndex, r.extraArgs)
                        .then((id) => {
                          setSelectedRun(id);
                          load();
                        })
                        .catch((e) => alert(String(e)))
                    }
                  >
                    重跑
                  </button>
                </span>
              </div>
            ))}
          </div>
        )}

        <div className="section-head" style={{ marginTop: 18 }}>
          <strong>目录书签</strong>
          <span className="spacer" style={{ flex: 1 }} />
          <button
            className="ghost"
            onClick={async () => {
              const name = prompt("书签名称");
              if (!name) return;
              const path = prompt("目录路径");
              if (!path) return;
              await api.addBookmark(name, path);
              load();
            }}
          >
            + 添加
          </button>
        </div>
        {bookmarks.length === 0 ? (
          <div style={{ color: "var(--text-dim)", padding: "8px 2px" }}>
            添加常用项目目录，一键打开终端 / 资源管理器。
          </div>
        ) : (
          <div className="bookmark-list">
            {bookmarks.map((b) => (
              <div className="bookmark-row" key={b.id}>
                <span className="event-svc">{b.name}</span>
                <span className="mono" title={b.path}>{b.path}</span>
                <button className="ghost" onClick={() => api.openInTerminal(b.path)}>终端</button>
                <button className="ghost" onClick={() => api.openInExplorer(b.path)}>目录</button>
                <button
                  className="ghost danger"
                  onClick={async () => { await api.deleteBookmark(b.id); load(); }}
                >
                  删
                </button>
              </div>
            ))}
          </div>
        )}
      </div>

      <LogConsole
        serviceId={selectedRun}
        serviceName={selectedRunObj ? `${selectedRunObj.taskName} · ${STATUS_LABEL[selectedRunObj.status]}` : null}
        logs={logs}
        onClear={onClearLog}
        extra={
          selectedRunObj && selectedRunObj.status === "running" ? (
            <button className="ghost danger" onClick={() => cancel(selectedRunObj.id)}>
              取消运行
            </button>
          ) : null
        }
      />

      {editOpen && (
        <TaskDialog
          editing={editing}
          services={services}
          onClose={() => setEditOpen(false)}
          onSaved={load}
        />
      )}

      {runFor && (
        <RunDialog
          task={runFor}
          onClose={() => setRunFor(null)}
          onRun={async (variantIndex, extraArgs) => {
            try {
              const runId = await api.runTask(runFor.id, variantIndex, extraArgs);
              setSelectedRun(runId);
              setRunFor(null);
              load();
            } catch (e) {
              alert(String(e));
            }
          }}
        />
      )}
    </div>
  );
}

// —— 运行对话框：选变体 + 追加参数 ——
function RunDialog({
  task,
  onClose,
  onRun,
}: {
  task: TaskView;
  onClose: () => void;
  onRun: (variantIndex: number | null, extraArgs: string) => void;
}) {
  const [variant, setVariant] = useState<number | null>(
    task.config.variants.length ? 0 : null
  );
  const [extra, setExtra] = useState("");

  return (
    <div className="overlay" onClick={onClose}>
      <div className="modal" style={{ width: 460 }} onClick={(e) => e.stopPropagation()}>
        <h2>运行：{task.name}</h2>
        <div className="mono" style={{ color: "var(--text-dim)", marginBottom: 12 }}>
          {task.config.command}{" "}
          {variant != null
            ? task.config.variants[variant]?.args.join(" ")
            : task.config.args.join(" ")}
        </div>

        {task.config.variants.length > 0 && (
          <div className="field">
            <label>变体</label>
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
              {task.config.variants.map((v, i) => (
                <button
                  key={i}
                  className={variant === i ? "primary" : "ghost"}
                  onClick={() => setVariant(i)}
                >
                  {v.label}
                </button>
              ))}
            </div>
          </div>
        )}

        <div className="field">
          <label>追加参数（可选，空格分隔）</label>
          <input value={extra} onChange={(e) => setExtra(e.target.value)} placeholder="--debug --verbose" />
        </div>

        <div className="footer">
          <button className="ghost" onClick={onClose}>取消</button>
          <button className="primary" onClick={() => onRun(variant, extra)}>开始运行</button>
        </div>
      </div>
    </div>
  );
}
