import { useEffect, useState } from "react";
import type { Group } from "../types";
import { api } from "../lib/api";

interface Props {
  groups: Group[];
  onClose: () => void;
  onChanged: () => void;
}

export default function GroupManagerDialog({
  groups,
  onClose,
  onChanged,
}: Props) {
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => setError(null), [groups]);

  const add = async () => {
    if (!name.trim()) return;
    try {
      await api.saveGroup(null, name.trim());
      setName("");
      onChanged();
    } catch (e) {
      setError(String(e));
    }
  };

  const rename = async (g: Group) => {
    const nn = window.prompt("重命名分组", g.name);
    if (nn && nn.trim() && nn !== g.name) {
      await api.saveGroup(g.id, nn.trim()).catch((e) => setError(String(e)));
      onChanged();
    }
  };

  const del = async (g: Group) => {
    if (confirm(`删除分组「${g.name}」？组内服务不会被删除，只是移出该组。`)) {
      await api.deleteGroup(g.id).catch((e) => setError(String(e)));
      onChanged();
    }
  };

  return (
    <div className="overlay" onClick={onClose}>
      <div className="modal" style={{ width: 460 }} onClick={(e) => e.stopPropagation()}>
        <h2>管理分组</h2>

        <div className="field">
          <label>新建分组</label>
          <div className="row">
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="例如 电商后端"
              onKeyDown={(e) => e.key === "Enter" && add()}
            />
            <button className="primary" style={{ flex: "0 0 auto" }} onClick={add}>
              添加
            </button>
          </div>
        </div>

        {groups.length === 0 ? (
          <div style={{ color: "var(--text-dim)", padding: "8px 0" }}>
            还没有分组。分组用于把相关服务归拢，一键按依赖顺序启停。
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
            {groups.map((g) => (
              <div
                key={g.id}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 8,
                  padding: "8px 10px",
                  background: "var(--bg-elev-2)",
                  border: "1px solid var(--border)",
                  borderRadius: 8,
                }}
              >
                <span style={{ flex: 1 }}>{g.name}</span>
                <button className="ghost" onClick={() => rename(g)}>
                  重命名
                </button>
                <button className="ghost danger" onClick={() => del(g)}>
                  删除
                </button>
              </div>
            ))}
          </div>
        )}

        {error && <div className="err" style={{ marginTop: 10 }}>{error}</div>}

        <div className="footer">
          <button className="primary" onClick={onClose}>
            完成
          </button>
        </div>
      </div>
    </div>
  );
}
