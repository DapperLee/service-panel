import { useEffect, useState } from "react";
import {
  defaultSpec,
  type DriverKind,
  type Group,
  type ServiceInput,
  type ServiceSpec,
  type ServiceView,
} from "../types";
import { api } from "../lib/api";

interface Props {
  editing: ServiceView | null;
  groups: Group[];
  allServices: ServiceView[];
  onClose: () => void;
  onSaved: () => void;
  onGroupsChanged: () => void;
}

function parseEnv(text: string): Record<string, string> {
  const env: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const t = line.trim();
    if (!t) continue;
    const i = t.indexOf("=");
    if (i > 0) env[t.slice(0, i)] = t.slice(i + 1);
  }
  return env;
}
function formatEnv(env: Record<string, string>): string {
  return Object.entries(env)
    .map(([k, v]) => `${k}=${v}`)
    .join("\n");
}

export default function AddServiceDialog({
  editing,
  groups,
  allServices,
  onClose,
  onSaved,
  onGroupsChanged,
}: Props) {
  const [spec, setSpec] = useState<ServiceSpec>(defaultSpec());
  const [name, setName] = useState("");
  const [groupId, setGroupId] = useState<string | null>(null);
  const [enabled, setEnabled] = useState(false);
  const [argsText, setArgsText] = useState("");
  const [envText, setEnvText] = useState("");
  const [newGroupName, setNewGroupName] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (editing) {
      setSpec(editing.spec);
      setName(editing.name);
      setGroupId(editing.groupId);
      setEnabled(editing.enabled);
      setArgsText(editing.spec.args.join(" "));
      setEnvText(formatEnv(editing.spec.env));
    } else {
      setSpec(defaultSpec());
      setName("");
      setGroupId(null);
      setEnabled(false);
      setArgsText("");
      setEnvText("");
    }
    setNewGroupName("");
  }, [editing]);

  const createGroupInline = async () => {
    const nn = newGroupName.trim();
    if (!nn) return;
    try {
      const gid = await api.saveGroup(null, nn);
      onGroupsChanged();
      setGroupId(gid);
      setNewGroupName("");
    } catch (e) {
      setError(String(e));
    }
  };

  const toggleDep = (id: string) => {
    setSpec((s) => {
      const has = s.after.includes(id);
      return {
        ...s,
        after: has ? s.after.filter((x) => x !== id) : [...s.after, id],
      };
    });
  };

  const patch = (p: Partial<ServiceSpec>) =>
    setSpec((s) => ({ ...s, ...p }));

  const save = async () => {
    setError(null);
    if (!name.trim()) return setError("请填写服务名称");
    if (spec.driver === "command" && !spec.command.trim())
      return setError("请填写启动命令");
    if (spec.driver === "windows_service" && !spec.serviceName.trim())
      return setError("请填写 Windows 服务名");
    if (spec.driver === "docker" && !spec.container.trim())
      return setError("请填写容器名/ID");

    const finalSpec: ServiceSpec = {
      ...spec,
      args: argsText.trim() ? argsText.trim().split(/\s+/) : [],
      env: parseEnv(envText),
    };
    const input: ServiceInput = {
      id: editing?.id ?? null,
      name: name.trim(),
      groupId,
      sortOrder: editing?.sortOrder ?? Date.now() % 1000000,
      enabled,
      spec: finalSpec,
    };
    setSaving(true);
    try {
      if (editing) await api.update(input);
      else await api.add(input);
      onSaved();
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const driver = spec.driver;

  return (
    <div className="overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2>{editing ? "编辑服务" : "添加服务"}</h2>

        <div className="field">
          <label>服务名称</label>
          <input value={name} onChange={(e) => setName(e.target.value)} placeholder="例如 订单服务" />
        </div>

        <div className="field">
          <label>类型</label>
          <select
            value={driver}
            onChange={(e) => patch({ driver: e.target.value as DriverKind })}
          >
            <option value="command">命令行进程（Python / Node / Go / exe）</option>
            <option value="windows_service">Windows 服务</option>
            <option value="docker">Docker 容器</option>
          </select>
        </div>

        <div className="field">
          <label>归属分组（用于一键批量启停，可空）</label>
          <div className="row">
            <select
              value={groupId ?? ""}
              onChange={(e) => setGroupId(e.target.value || null)}
            >
              <option value="">未分组</option>
              {groups.map((g) => (
                <option key={g.id} value={g.id}>
                  {g.name}
                </option>
              ))}
            </select>
          </div>
          <div className="row" style={{ marginTop: 6 }}>
            <input
              value={newGroupName}
              onChange={(e) => setNewGroupName(e.target.value)}
              placeholder="快速新建分组，如：局域网套件"
              onKeyDown={(e) => e.key === "Enter" && createGroupInline()}
            />
            <button
              className="ghost"
              style={{ flex: "0 0 auto" }}
              onClick={createGroupInline}
              disabled={!newGroupName.trim()}
            >
              + 新建
            </button>
          </div>
        </div>

        {driver === "command" && (
          <>
            <div className="field">
              <label>启动命令（可执行文件或解释器）</label>
              <input
                value={spec.command}
                onChange={(e) => patch({ command: e.target.value })}
                placeholder="python / node / go / 绝对路径 exe"
              />
            </div>
            <div className="field">
              <label>参数（空格分隔）</label>
              <input
                value={argsText}
                onChange={(e) => setArgsText(e.target.value)}
                placeholder="app.py --port 8000"
              />
            </div>
            <div className="field">
              <label>工作目录 cwd</label>
              <input
                value={spec.cwd}
                onChange={(e) => patch({ cwd: e.target.value })}
                placeholder="D:\code\myproj"
              />
            </div>
            <div className="field">
              <label>环境变量（每行 KEY=VALUE）</label>
              <textarea
                rows={3}
                value={envText}
                onChange={(e) => setEnvText(e.target.value)}
                placeholder={"PORT=8000\nNODE_ENV=development"}
              />
            </div>
            <div className="row">
              <div className="field">
                <label>输出编码</label>
                <select
                  value={spec.encoding}
                  onChange={(e) => patch({ encoding: e.target.value })}
                >
                  <option value="auto">自动（UTF-8 优先，回退 GBK）</option>
                  <option value="utf-8">utf-8</option>
                  <option value="gbk">gbk（中文老工具）</option>
                </select>
              </div>
              <div className="field">
                <label>优雅停止超时(秒)</label>
                <input
                  type="number"
                  value={spec.stopTimeoutSecs}
                  onChange={(e) =>
                    patch({ stopTimeoutSecs: Number(e.target.value) || 10 })
                  }
                />
              </div>
            </div>
          </>
        )}

        {driver === "windows_service" && (
          <div className="field">
            <label>Windows 服务名（sc query 用的名字）</label>
            <input
              value={spec.serviceName}
              onChange={(e) => patch({ serviceName: e.target.value })}
              placeholder="MySQL80 / Redis"
            />
          </div>
        )}

        {driver === "docker" && (
          <div className="field">
            <label>容器名或 ID</label>
            <input
              value={spec.container}
              onChange={(e) => patch({ container: e.target.value })}
              placeholder="mysql-dev"
            />
          </div>
        )}

        <div className="field">
          <label>
            {driver === "windows_service"
              ? "日志文件路径（服务无标准输出，尾随此文件看日志）"
              : driver === "docker"
              ? "日志文件路径（可选，留空则用 docker logs -f）"
              : "日志文件路径（可选，额外尾随文件日志）"}
          </label>
          <input
            value={spec.logPath}
            onChange={(e) => patch({ logPath: e.target.value })}
            placeholder="D:\\ProgramData\\MySQL\\MySQL Server 8.0\\Data\\mysqld.err"
          />
        </div>

        <div className="section-title">健康检查</div>
        <div className="row">
          <div className="field">
            <label>方式</label>
            <select
              value={spec.healthcheck.kind}
              onChange={(e) =>
                patch({
                  healthcheck: {
                    ...spec.healthcheck,
                    kind: e.target.value as any,
                  },
                })
              }
            >
              <option value="none">不检查</option>
              <option value="tcp">TCP 端口</option>
              <option value="http">HTTP</option>
              <option value="command">命令</option>
            </select>
          </div>
          <div className="field">
            <label>目标</label>
            <input
              value={spec.healthcheck.target}
              onChange={(e) =>
                patch({
                  healthcheck: { ...spec.healthcheck, target: e.target.value },
                })
              }
              placeholder="127.0.0.1:8000 或 http://localhost:8000/health"
            />
          </div>
        </div>
        <div className="row">
          <div className="field">
            <label>间隔(秒)</label>
            <input
              type="number"
              value={spec.healthcheck.intervalSecs}
              onChange={(e) =>
                patch({
                  healthcheck: {
                    ...spec.healthcheck,
                    intervalSecs: Number(e.target.value) || 10,
                  },
                })
              }
            />
          </div>
          <div className="field">
            <label>失败阈值</label>
            <input
              type="number"
              value={spec.healthcheck.unhealthyThreshold}
              onChange={(e) =>
                patch({
                  healthcheck: {
                    ...spec.healthcheck,
                    unhealthyThreshold: Number(e.target.value) || 3,
                  },
                })
              }
            />
          </div>
        </div>

        <div className="section-title">重启策略</div>
        <div className="row">
          <div className="field">
            <label>模式</label>
            <select
              value={spec.restartPolicy.mode}
              onChange={(e) =>
                patch({
                  restartPolicy: {
                    ...spec.restartPolicy,
                    mode: e.target.value as any,
                  },
                })
              }
            >
              <option value="never">从不自动重启</option>
              <option value="on_failure">异常时重启</option>
              <option value="always">总是保持运行</option>
            </select>
          </div>
          <div className="field">
            <label>最大重试</label>
            <input
              type="number"
              value={spec.restartPolicy.maxRetries}
              onChange={(e) =>
                patch({
                  restartPolicy: {
                    ...spec.restartPolicy,
                    maxRetries: Number(e.target.value) || 5,
                  },
                })
              }
            />
          </div>
          <div className="field">
            <label>退避基数(秒)</label>
            <input
              type="number"
              value={spec.restartPolicy.backoffBaseSecs}
              onChange={(e) =>
                patch({
                  restartPolicy: {
                    ...spec.restartPolicy,
                    backoffBaseSecs: Number(e.target.value) || 2,
                  },
                })
              }
            />
          </div>
        </div>

        <div className="section-title">依赖服务（启动顺序）</div>
        {allServices.filter((s) => s.id !== editing?.id).length === 0 ? (
          <div style={{ color: "var(--text-dim)", fontSize: 12 }}>
            暂无其它服务可选。勾选后，本服务会在这些依赖服务健康就绪后才启动；
            分组一键启动会据此做拓扑排序，停止按逆序。
          </div>
        ) : (
          <div
            style={{
              display: "flex",
              flexWrap: "wrap",
              gap: "6px 16px",
              maxHeight: 120,
              overflow: "auto",
            }}
          >
            {allServices
              .filter((s) => s.id !== editing?.id)
              .map((s) => (
                <label
                  key={s.id}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 6,
                    fontSize: 13,
                    cursor: "pointer",
                  }}
                >
                  <input
                    type="checkbox"
                    style={{ width: "auto" }}
                    checked={spec.after.includes(s.id)}
                    onChange={() => toggleDep(s.id)}
                  />
                  {s.name}
                </label>
              ))}
          </div>
        )}

        <div className="field" style={{ marginTop: 12 }}>
          <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <input
              type="checkbox"
              style={{ width: "auto" }}
              checked={enabled}
              onChange={(e) => setEnabled(e.target.checked)}
            />
            开机随面板自动启动
          </label>
        </div>

        {error && <div className="err">{error}</div>}

        <div className="footer">
          <button className="ghost" onClick={onClose}>
            取消
          </button>
          <button className="primary" disabled={saving} onClick={save}>
            {saving ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}
