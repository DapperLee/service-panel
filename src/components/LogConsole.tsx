import { useEffect, useMemo, useRef, useState } from "react";
import type { LogLine } from "../types";

const RENDER_CAP = 800; // 只渲染最后 N 行，控制 DOM 规模（缓冲仍由 App 保留 2000）

interface Props {
  serviceId: string | null;
  serviceName: string | null;
  /** 由 App 汇聚的所有日志流（按 serviceId 过滤） */
  logs: Record<string, LogLine[]>;
  onClear: (id: string) => void;
  /** 头部额外操作（如任务运行控制台的"取消"按钮） */
  extra?: React.ReactNode;
}

function fmt(ts: number) {
  const d = new Date(ts);
  return d.toLocaleTimeString("zh-CN", { hour12: false });
}

export default function LogConsole({
  serviceId,
  serviceName,
  logs,
  onClear,
  extra,
}: Props) {
  const [follow, setFollow] = useState(true);
  const [q, setQ] = useState("");
  const [stream, setStream] = useState<"all" | "stdout" | "stderr" | "system">(
    "all"
  );
  const bodyRef = useRef<HTMLDivElement>(null);
  const allLines = serviceId ? logs[serviceId] ?? [] : [];

  // 切换服务时重置过滤条件
  useEffect(() => {
    setQ("");
    setStream("all");
  }, [serviceId]);

  const lines = useMemo(() => {
    const kw = q.trim().toLowerCase();
    return allLines.filter((l) => {
      if (stream !== "all" && l.stream !== stream) return false;
      if (kw && !l.text.toLowerCase().includes(kw)) return false;
      return true;
    });
  }, [allLines, q, stream]);

  useEffect(() => {
    if (follow && bodyRef.current) {
      bodyRef.current.scrollTop = bodyRef.current.scrollHeight;
    }
  }, [lines.length, follow]);

  const onScroll = () => {
    const el = bodyRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 30;
    setFollow(atBottom);
  };

  const copyVisible = () => {
    const text = lines.map((l) => `${fmt(l.ts)} [${l.stream}] ${l.text}`).join("\n");
    navigator.clipboard?.writeText(text).catch(() => {});
  };

  if (!serviceId) {
    return (
      <div className="logs">
        <div className="log-head">
          <strong>日志</strong>
        </div>
        <div className="empty">从左侧选择一个服务查看实时日志</div>
      </div>
    );
  }

  return (
    <div className="logs">
      <div className="log-head">
        <strong>{serviceName}</strong>
        {extra}
        <span className="spacer" style={{ flex: 1 }} />
        <label style={{ fontSize: 12, color: "var(--text-dim)" }}>
          <input
            type="checkbox"
            checked={follow}
            style={{ width: "auto" }}
            onChange={(e) => setFollow(e.target.checked)}
          />{" "}
          跟随
        </label>
        <button className="ghost" onClick={copyVisible}>
          复制
        </button>
        <button className="ghost" onClick={() => onClear(serviceId)}>
          清空
        </button>
      </div>

      <div className="log-filter">
        <input
          className="log-search"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="过滤关键字…"
        />
        <select
          value={stream}
          onChange={(e) => setStream(e.target.value as any)}
        >
          <option value="all">全部</option>
          <option value="stdout">stdout</option>
          <option value="stderr">stderr</option>
          <option value="system">系统</option>
        </select>
        <span className="log-count">
          {lines.length}
          {lines.length !== allLines.length ? `/${allLines.length}` : ""}
        </span>
      </div>

      <div className="log-body" ref={bodyRef} onScroll={onScroll}>
        {lines.length === 0 ? (
          <div style={{ color: "var(--text-dim)" }}>
            {allLines.length === 0 ? "（暂无输出）" : "（无匹配行）"}
          </div>
        ) : (
          lines.slice(-RENDER_CAP).map((l, i) => (
            <div key={`${l.ts}-${i}`} className={`log-line ${l.stream}`}>
              <span className="t">{fmt(l.ts)}</span>
              {l.text}
            </div>
          ))
        )}
      </div>
    </div>
  );
}
