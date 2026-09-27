import { useMemo } from "react";
import type { ServiceView, State } from "../types";
import { buildGraph } from "../lib/deps";

interface Props {
  services: ServiceView[];
  groupName: string | null;
  onClose: () => void;
}

const NODE_W = 176;
const NODE_H = 48;
const COL_GAP = 250;
const ROW_GAP = 74;
const PAD = 36;

const STATE_COLOR: Record<State, string> = {
  running: "#2ecc71",
  starting: "#f1c40f",
  restarting: "#f1c40f",
  stopping: "#f1c40f",
  unhealthy: "#e74c3c",
  crashed: "#e74c3c",
  stopped: "#6b7688",
};

export default function DependencyGraph({ services, groupName, onClose }: Props) {
  const graph = useMemo(() => buildGraph(services), [services]);
  const stateById = useMemo(
    () => new Map(services.map((s) => [s.id, s.state])),
    [services]
  );

  // 按层分组并排布
  const { pos, width, height } = useMemo(() => {
    const byLayer = new Map<number, string[]>();
    for (const n of graph.nodes) {
      if (!byLayer.has(n.layer)) byLayer.set(n.layer, []);
      byLayer.get(n.layer)!.push(n.id);
    }
    const pos = new Map<string, { x: number; y: number }>();
    let maxRows = 0;
    for (const [layer, ids] of byLayer) {
      ids.sort();
      maxRows = Math.max(maxRows, ids.length);
      ids.forEach((id, i) => {
        pos.set(id, { x: PAD + layer * COL_GAP, y: PAD + i * ROW_GAP });
      });
    }
    const width = PAD * 2 + Math.max(0, graph.layerCount - 1) * COL_GAP + NODE_W;
    const height = PAD * 2 + Math.max(0, maxRows - 1) * ROW_GAP + NODE_H;
    return { pos, width: Math.max(width, 320), height: Math.max(height, 200) };
  }, [graph]);

  return (
    <div className="overlay" onClick={onClose}>
      <div
        className="modal"
        style={{ width: "min(900px, 92vw)", maxHeight: "88vh" }}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>依赖关系图{groupName ? ` · ${groupName}` : ""}</h2>

        {graph.nodes.length === 0 ? (
          <div className="empty">当前视图没有服务。</div>
        ) : (
          <>
            <div className="dep-legend">
              <span>
                <i className="dep-arrow" />箭头方向 = 启动先后（被依赖 → 依赖方）
              </span>
              <span>停止时按箭头反方向</span>
              {graph.cycleIds.size > 0 && (
                <span className="dep-cycle-warn">
                  ⚠ 有 {graph.cycleIds.size} 个服务陷入循环依赖（红框）
                </span>
              )}
            </div>

            <div className="dep-scroll">
              <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`}>
                <defs>
                  <marker
                    id="dep-arrow"
                    markerWidth="10"
                    markerHeight="10"
                    refX="8"
                    refY="3"
                    orient="auto"
                    markerUnits="strokeWidth"
                  >
                    <path d="M0,0 L8,3 L0,6 Z" fill="#5b6b86" />
                  </marker>
                </defs>

                {/* 边 */}
                {graph.edges.map((e, i) => {
                  const a = pos.get(e.from);
                  const b = pos.get(e.to);
                  if (!a || !b) return null;
                  const x1 = a.x + NODE_W;
                  const y1 = a.y + NODE_H / 2;
                  const x2 = b.x;
                  const y2 = b.y + NODE_H / 2;
                  const mx = (x1 + x2) / 2;
                  const inCycle =
                    graph.cycleIds.has(e.from) || graph.cycleIds.has(e.to);
                  return (
                    <path
                      key={i}
                      d={`M${x1},${y1} C${mx},${y1} ${mx},${y2} ${x2},${y2}`}
                      fill="none"
                      stroke={inCycle ? "#e74c3c" : "#5b6b86"}
                      strokeWidth={1.5}
                      markerEnd="url(#dep-arrow)"
                    />
                  );
                })}

                {/* 节点 */}
                {graph.nodes.map((n) => {
                  const p = pos.get(n.id)!;
                  const st = stateById.get(n.id) ?? "stopped";
                  return (
                    <g key={n.id} transform={`translate(${p.x},${p.y})`}>
                      <rect
                        width={NODE_W}
                        height={NODE_H}
                        rx={9}
                        fill="#1e2637"
                        stroke={n.inCycle ? "#e74c3c" : "#2a3348"}
                        strokeWidth={n.inCycle ? 2 : 1}
                      />
                      <circle
                        cx={16}
                        cy={NODE_H / 2}
                        r={5}
                        fill={STATE_COLOR[st]}
                      />
                      <text
                        x={30}
                        y={NODE_H / 2 - 2}
                        fill="#e6ebf5"
                        fontSize={13}
                        dominantBaseline="middle"
                      >
                        {n.name.length > 14
                          ? n.name.slice(0, 14) + "…"
                          : n.name}
                      </text>
                      <text
                        x={30}
                        y={NODE_H / 2 + 14}
                        fill="#8a95ad"
                        fontSize={10}
                        dominantBaseline="middle"
                      >
                        第 {n.layer + 1} 层
                      </text>
                    </g>
                  );
                })}
              </svg>
            </div>

            {graph.edges.length === 0 && (
              <div style={{ color: "var(--text-dim)", marginTop: 10 }}>
                这些服务之间还没有依赖关系。编辑服务，在「依赖服务」里勾选，
                这里就会出现箭头。
              </div>
            )}
          </>
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
