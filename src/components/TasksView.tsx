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
}

export default function TasksView({ services, logs, onClearLog }: Props) {
  const [tasks, setTasks] = useState<TaskView[]>([]);
  const [runs, setRuns] = useState<TaskRun[]>([]);
  const [bookmarks, setBookmarks] = useState<Bookmark[]>([]);
  const [selectedRun, setSelectedRun] = useState<string | null>(null);
  const [editOpen, setEditOpen] = useState(false);
  const [editing, setEditing] = useState<TaskView | null>(null);
  const [runFor, setRunFor] = useState<TaskView | null>(null);

  const load = useCallback(async () => {
    try {
      const [t, r, b] = await Promise.all([
        api.listTasks(),
        api.listTaskRuns(50),
        api.listBookmarks(),
      ]);
      setTasks(t);
      setRuns(r);
      setBookmarks(b);
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

  return (
    <div className="body">
      <div className="grid-wrap">
        <div className="section-head">
          <strong>任务</strong>
          <span className="spacer" style={{ flex: 1 }} />
          <button className="primary" onClick={() => { setEditing(null); setEditOpen(true); }}>
            + 新建任务
          </button>
        </div>

        {tasks.length === 0 ? (
          <div className="empty">还没有任务。新建一个，把常用的构建/命令存起来，一键就跑。</div>
        ) : (
          <div className="grid">
            {tasks.map((t) => (
              <div className="card" key={t.id}>
                <div className="head">
                  <span className="name">{t.name}</span>
                  {t.group && <span className="tag">{t.group}</span>}
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
            ))}
          </div>
        )}

        <div className="section-head" style={{ marginTop: 18 }}>
          <strong>运行历史</strong>
        </div>
        {runs.length === 0 ? (
          <div style={{ color: "var(--text-dim)", padding: "8px 2px" }}>暂无运行记录。</div>
        ) : (
          <div className="event-list">
            {runs.map((r) => (
              <div
                className="event-row"
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
