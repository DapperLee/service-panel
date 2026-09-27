import type { ServiceView } from "../types";

export interface GraphNode {
  id: string;
  name: string;
  layer: number;
  inCycle: boolean;
}
export interface GraphEdge {
  from: string; // 被依赖（先启动）
  to: string; // 依赖方（后启动）
}
export interface Graph {
  nodes: GraphNode[];
  edges: GraphEdge[];
  layerCount: number;
  cycleIds: Set<string>;
}

/**
 * 从服务的 spec.after（依赖边）构建 DAG 布局数据。
 * 用 Kahn 拓扑 + 最长路径给节点分层；无法入队的节点即处于环中。
 */
export function buildGraph(services: ServiceView[]): Graph {
  const ids = new Set(services.map((s) => s.id));
  const depsOf = new Map<string, string[]>();
  const dependentsOf = new Map<string, string[]>();
  const edges: GraphEdge[] = [];

  for (const s of services) {
    const deps = (s.spec.after || []).filter((d) => ids.has(d) && d !== s.id);
    depsOf.set(s.id, deps);
    for (const d of deps) {
      edges.push({ from: d, to: s.id });
      if (!dependentsOf.has(d)) dependentsOf.set(d, []);
      dependentsOf.get(d)!.push(s.id);
    }
  }

  // 入度 = 依赖数
  const indeg = new Map<string, number>();
  for (const s of services) indeg.set(s.id, (depsOf.get(s.id) ?? []).length);

  const layer = new Map<string, number>();
  const queue: string[] = [];
  for (const s of services) {
    if ((indeg.get(s.id) ?? 0) === 0) {
      layer.set(s.id, 0);
      queue.push(s.id);
    }
  }

  const visited = new Set<string>();
  const localIndeg = new Map(indeg);
  while (queue.length) {
    const n = queue.shift()!;
    visited.add(n);
    for (const m of dependentsOf.get(n) ?? []) {
      const cand = (layer.get(n) ?? 0) + 1;
      layer.set(m, Math.max(layer.get(m) ?? 0, cand));
      localIndeg.set(m, (localIndeg.get(m) ?? 1) - 1);
      if (localIndeg.get(m) === 0) queue.push(m);
    }
  }

  const cycleIds = new Set(
    services.map((s) => s.id).filter((id) => !visited.has(id))
  );

  const nodes: GraphNode[] = services.map((s) => ({
    id: s.id,
    name: s.name,
    layer: layer.get(s.id) ?? 0,
    inCycle: cycleIds.has(s.id),
  }));

  const layerCount = nodes.length
    ? Math.max(...nodes.map((n) => n.layer)) + 1
    : 0;

  return { nodes, edges, layerCount, cycleIds };
}
