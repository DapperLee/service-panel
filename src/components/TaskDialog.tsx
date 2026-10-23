import { useEffect, useState } from "react";
import {
  defaultTaskConfig,
  type ServiceView,
  type TaskConfig,
  type TaskInput,
  type TaskVariant,
  type TaskView,
} from "../types";
import { api } from "../lib/api";

interface Props {
  editing: TaskView | null;
  services: ServiceView[];
  onClose: () => void;
  onSaved: () => void;
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
const formatEnv = (env: Record<string, string>) =>
  Object.entries(env).map(([k, v]) => `${k}=${v}`).join("\n");

export default function TaskDialog({ editing, services, onClose, onSaved }: Props) {
  const [name, setName] = useState("");
  const [group, setGroup] = useState("");
  const [config, setConfig] = useState<TaskConfig>(defaultTaskConfig());
  const [argsText, setArgsText] = useState("");
  const [envText, setEnvText] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (editing) {
      setName(editing.name);
      setGroup(editing.group ?? "");
      setConfig(editing.config);
      setArgsText(editing.config.args.join(" "));
      setEnvText(formatEnv(editing.config.env));
    } else {
      setName("");
      setGroup("");
      setConfig(defaultTaskConfig());
      setArgsText("");
      setEnvText("");
    }
    setError(null);
  }, [editing]);

  const patch = (p: Partial<TaskConfig>) => setConfig((c) => ({ ...c, ...p }));

  const setVariant = (i: number, p: Partial<TaskVariant>) =>
    setConfig((c) => ({
      ...c,
      variants: c.variants.map((v, idx) => (idx === i ? { ...v, ...p } : v)),
    }));
  const addVariant = () =>
    setConfig((c) => ({ ...c, variants: [...c.variants, { label: "", args: [] }] }));
  const delVariant = (i: number) =>
    setConfig((c) => ({ ...c, variants: c.variants.filter((_, idx) => idx !== i) }));

  const toggleRequire = (id: string) =>
    setConfig((c) => ({
      ...c,
      requires: c.requires.includes(id)
        ? c.requires.filter((x) => x !== id)
        : [...c.requires, id],
    }));

  const save = async () => {
    setError(null);
    if (!name.trim()) return setError("请填写任务名称");
    if (!config.command.trim()) return setError("请填写命令");
    const finalConfig: TaskConfig = {
      ...config,
      command: config.command.trim(),
      args: argsText.trim() ? argsText.trim().split(/\s+/) : [],
      env: parseEnv(envText),
      variants: config.variants
        .map((v) => ({ label: v.label.trim(), args: v.args }))
        .filter((v) => v.label),
    };
    const input: TaskInput = {
      id: editing?.id ?? null,
      name: name.trim(),
      group: group.trim() || null,
      config: finalConfig,
    };
    setSaving(true);
    try {
      if (editing) await api.updateTask(input);
      else await api.addTask(input);
      onSaved();
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2>{editing ? "编辑任务" : "新建任务"}</h2>

        <div className="row">
          <div className="field">
            <label>任务名称</label>
            <input value={name} onChange={(e) => setName(e.target.value)} placeholder="例如 Android 打包" />
          </div>
          <div className="field">
            <label>分组（可选）</label>
            <input value={group} onChange={(e) => setGroup(e.target.value)} placeholder="例如 移动端" />
          </div>
        </div>

        <div className="field">
          <label>命令（可执行文件或 npx/npm/cargo 等）</label>
          <input value={config.command} onChange={(e) => patch({ command: e.target.value })} placeholder="npx / cargo / dotnet" />
        </div>
        <div className="field">
          <label>默认参数（空格分隔；选了变体时用变体参数）</label>
          <input value={argsText} onChange={(e) => setArgsText(e.target.value)} placeholder="tauri android build" />
        </div>
        <div className="field">
          <label>工作目录（运行前自动 cd）</label>
          <input value={config.cwd} onChange={(e) => patch({ cwd: e.target.value })} placeholder="D:\code\myproject" />
        </div>

        <div className="row">
          <div className="field">
            <label style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <input type="checkbox" style={{ width: "auto" }} checked={config.useShell} onChange={(e) => patch({ useShell: e.target.checked })} />
              经 shell 执行（npx、&amp;&amp;、管道需要）
            </label>
          </div>
          <div className="field">
            <label>输出编码</label>
            <select value={config.encoding} onChange={(e) => patch({ encoding: e.target.value })}>
              <option value="auto">自动</option>
              <option value="utf-8">utf-8</option>
              <option value="gbk">gbk</option>
            </select>
          </div>
        </div>

        <div className="field">
          <label>环境变量（每行 KEY=VALUE）</label>
          <textarea rows={2} value={envText} onChange={(e) => setEnvText(e.target.value)} placeholder={"ANDROID_HOME=C:\\..."} />
        </div>

        <div className="section-title">变体（可选，如 debug / release）</div>
        {config.variants.map((v, i) => (
          <div className="row" key={i} style={{ alignItems: "center" }}>
            <input style={{ flex: "0 0 30%" }} value={v.label} onChange={(e) => setVariant(i, { label: e.target.value })} placeholder="名称" />
            <input value={v.args.join(" ")} onChange={(e) => setVariant(i, { args: e.target.value.trim() ? e.target.value.trim().split(/\s+/) : [] })} placeholder="该变体的参数" />
            <button className="ghost danger" style={{ flex: "0 0 auto" }} onClick={() => delVariant(i)}>删</button>
          </div>
        ))}
        <button className="ghost" onClick={addVariant}>+ 添加变体</button>

        {services.length > 0 && (
          <>
            <div className="section-title">依赖服务（运行前这些服务需在运行）</div>
            <div style={{ display: "flex", flexWrap: "wrap", gap: "6px 16px", maxHeight: 110, overflow: "auto" }}>
              {services.map((s) => (
                <label key={s.id} style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 13, cursor: "pointer" }}>
                  <input type="checkbox" style={{ width: "auto" }} checked={config.requires.includes(s.id)} onChange={() => toggleRequire(s.id)} />
                  {s.name}
                </label>
              ))}
            </div>
          </>
        )}

        {error && <div className="err" style={{ marginTop: 12 }}>{error}</div>}

        <div className="footer">
          <button className="ghost" onClick={onClose}>取消</button>
          <button className="primary" disabled={saving} onClick={save}>{saving ? "保存中…" : "保存"}</button>
        </div>
      </div>
    </div>
  );
}
