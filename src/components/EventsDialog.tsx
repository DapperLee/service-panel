import { useEffect, useState } from "react";
import { api } from "../lib/api";
import type { ServiceView } from "../types";

interface EventRow {
  serviceId: string;
  ts: number;
  from: string | null;
  to: string;
  reason: string | null;
  exitCode: number | null;
}

const STATE_LABEL: Record<string, string> = {
  stopped: "已停止",
  starting: "启动中",
  running: "运行中",
  stopping: "停止中",
  unhealthy: "不健康",
  crashed: "已崩溃",
  restarting: "重启中",
};

function fmt(ts: number) {
  return new Date(ts * 1000).toLocaleString("zh-CN", { hour12: false });
}

function colorFor(s: string): string {
  const map: Record<string, string> = {
    running: "var(--green)",
    crashed: "var(--red)",
    unhealthy: "var(--red)",
    starting: "var(--yellow)",
    stopping: "var(--yellow)",
    restarting: "var(--yellow)",
    stopped: "var(--gray)",
  };
  return map[s] ?? "var(--text-dim)";
}

interface Props {
  services: ServiceView[];
  onClose: () => void;
}

export default function EventsDialog({ services, onClose }: Props) {
  const [rows, setRows] = useState<EventRow[]>([]);
  const [loading, setLoading] = useState(true);
  const nameOf = (id: string) => services.find((s) => s.id === id)?.name ?? id.slice(0, 8);

  useEffect(() => {
    api
      .events(300)
      .then((r) => setRows(r as unknown as EventRow[]))
      .catch(() => setRows([]))
      .finally(() => setLoading(false));
  }, []);

  return (
    <div className="overlay" onClick={onClose}>
      <div
        className="modal"
        style={{ width: "min(720px, 92vw)", maxHeight: "86vh" }}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>事件时间线</h2>
        <div style={{ color: "var(--text-dim)", fontSize: 12, marginBottom: 10 }}>
          最近 {rows.length} 条状态变化（保留 7 天）。这是排查"昨晚几点挂了几次"的地方。
        </div>
        {loading ? (
          <div className="empty">加载中…</div>
        ) : rows.length === 0 ? (
          <div className="empty">暂无事件记录。</div>
        ) : (
          <div className="event-list">
            {rows.map((r, i) => (
              <div className="event-row" key={i}>
                <span className="event-time">{fmt(r.ts)}</span>
                <span className="event-svc">{nameOf(r.serviceId)}</span>
                <span className="event-flow">
                  {r.from ? (
                    <span style={{ color: colorFor(r.from) }}>
                      {STATE_LABEL[r.from] ?? r.from}
                    </span>
                  ) : (
                    <span className="event-dim">—</span>
                  )}
                  <span className="event-arrow">→</span>
                  <span style={{ color: colorFor(r.to) }}>{STATE_LABEL[r.to] ?? r.to}</span>
                </span>
                <span className="event-reason">
                  {r.reason ?? ""}
                  {r.exitCode != null && r.exitCode !== 0 ? ` (exit ${r.exitCode})` : ""}
                </span>
              </div>
            ))}
          </div>
        )}
        <div className="footer">
          <button className="primary" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
