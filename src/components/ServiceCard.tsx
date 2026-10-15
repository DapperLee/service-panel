import type { ServiceView, State } from "../types";
import { api } from "../lib/api";
import { PLATFORM } from "../lib/os";

const STATE_LABEL: Record<State, string> = {
  stopped: "已停止",
  starting: "启动中",
  running: "运行中",
  stopping: "停止中",
  unhealthy: "不健康",
  crashed: "已崩溃",
  restarting: "重启中",
};

const DRIVER_LABEL: Record<string, string> = {
  command: "进程",
  windows_service: PLATFORM.card,
  docker: "Docker",
};

interface Props {
  svc: ServiceView;
  selected: boolean;
  onSelect: (id: string) => void;
  onEdit: (svc: ServiceView) => void;
  onChanged: () => void;
}

// 后端拿不到宿主进程时给 null（共享宿主服务、macOS 上的容器），显示「—」而不是 0，
// 不把「测不到」伪装成「没占用」
const metric = (v: number | null, unit: string) => (v === null ? "—" : `${v.toFixed(0)}${unit}`);

export default function ServiceCard({
  svc,
  selected,
  onSelect,
  onEdit,
  onChanged,
}: Props) {
  const running = svc.state === "running";
  // 坏配置（DPAPI 解不开/数据损坏）时后端会拒绝启动，这里同步置灰，避免无效点击
  const broken = !svc.configOk;
  const transitioning =
    svc.state === "starting" ||
    svc.state === "stopping" ||
    svc.state === "restarting";

  const target = async (fn: () => Promise<unknown>) => {
    try {
      await fn();
    } finally {
      onChanged();
    }
  };

  return (
    <div
      className={`card${selected ? " selected" : ""}`}
      onClick={() => onSelect(svc.id)}
    >
      <div className="head">
        <span className={`dot ${svc.state}`} />
        <span className="name">{svc.name}</span>
        <span className="tag">{DRIVER_LABEL[svc.spec.driver]}</span>
      </div>

      <div className="meta">
        <span>{STATE_LABEL[svc.state]}</span>
        {svc.pid ? <span>PID {svc.pid}</span> : null}
        {running && (
          <>
            <span>CPU {metric(svc.cpu, "%")}</span>
            <span>MEM {metric(svc.memMb, "MB")}</span>
          </>
        )}
      </div>

      {svc.lastError && <div className="err">{svc.lastError}</div>}

      <div className="actions">
        {running || svc.desired === "running" ? (
          <button
            disabled={transitioning}
            onClick={(e) => {
              e.stopPropagation();
              target(() => api.stop(svc.id));
            }}
          >
            停止
          </button>
        ) : (
          <button
            className="primary"
            disabled={transitioning || broken}
            onClick={(e) => {
              e.stopPropagation();
              target(() => api.start(svc.id));
            }}
          >
            启动
          </button>
        )}
        <button
          disabled={transitioning || broken}
          onClick={(e) => {
            e.stopPropagation();
            target(() => api.restart(svc.id));
          }}
        >
          重启
        </button>
        <button
          className="ghost"
          onClick={(e) => {
            e.stopPropagation();
            onEdit(svc);
          }}
        >
          编辑
        </button>
        <button
          className="ghost danger"
          onClick={(e) => {
            e.stopPropagation();
            if (confirm(`删除服务「${svc.name}」？`)) {
              target(() => api.remove(svc.id));
            }
          }}
        >
          删除
        </button>
      </div>
    </div>
  );
}
