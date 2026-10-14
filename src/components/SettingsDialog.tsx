import { useEffect, useState } from "react";
import { save, open } from "@tauri-apps/plugin-dialog";
import { api } from "../lib/api";
import { PLATFORM } from "../lib/os";
import type { PortAudit } from "../types";

interface Props {
  onClose: () => void;
  onChanged?: () => void;
}

export default function SettingsDialog({ onClose, onChanged }: Props) {
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  const [audit, setAudit] = useState<PortAudit[] | null>(null);
  const [auditing, setAuditing] = useState(false);

  useEffect(() => {
    api
      .getAutostart()
      .then(setAutostart)
      .catch((e) => setError(String(e)));
  }, []);

  const toggle = async (v: boolean) => {
    setSaving(true);
    setError(null);
    try {
      await api.setAutostart(v);
      setAutostart(v);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const doExport = async () => {
    setError(null);
    setMsg(null);
    setBusy(true);
    try {
      const defaultName = `service-panel-${new Date()
        .toISOString()
        .slice(0, 10)}.json`;
      const path = await save({
        defaultPath: defaultName,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!path) return;
      const n = await api.exportConfig(path);
      setMsg(`已导出 ${n} 个服务到：${path}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const doImport = async () => {
    setError(null);
    setMsg(null);
    setBusy(true);
    try {
      const path = await open({
        multiple: false,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (!path || typeof path !== "string") return;
      const replace = window.confirm(
        "选择导入模式：\n\n" +
          "【确定】= 覆盖：先停止所有正在运行的服务、清空现有配置，再按文件重建。\n" +
          "【取消】= 合并：保留现有配置，只添加文件中不存在的 id。"
      );
      const r = await api.importConfig(path, replace);
      setMsg(
        `导入完成：新增 ${r.servicesAdded} 个服务、跳过 ${r.servicesSkipped} 个已存在、` +
          `新增 ${r.groupsAdded} 个分组${replace ? `、停止 ${r.stopped} 个服务` : ""}。`
      );
      onChanged?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const doAudit = async () => {
    setError(null);
    setAuditing(true);
    try {
      setAudit(await api.portAudit());
    } catch (e) {
      setError(String(e));
    } finally {
      setAuditing(false);
    }
  };

  return (
    <div className="overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2>设置</h2>

        <div className="field">
          <label style={{ display: "flex", alignItems: "center", gap: 10 }}>
            <input
              type="checkbox"
              style={{ width: "auto" }}
              checked={autostart ?? false}
              disabled={autostart === null || saving}
              onChange={(e) => toggle(e.target.checked)}
            />
            开机自动启动面板
          </label>
          <div style={{ color: "var(--text-dim)", fontSize: 12, marginTop: 6 }}>
            开启后面板会在登录时于托盘后台启动，并自动拉起所有勾选了
            「开机随面板自动启动」的服务，无需你手动一个个打开。
          </div>
        </div>

        <div className="section-title" style={{ borderTop: "none", marginTop: 4 }}>
          关于关闭窗口
        </div>
        <div style={{ color: "var(--text-dim)", fontSize: 13, lineHeight: 1.7 }}>
          点窗口右上角 × 不会退出，只会<strong>隐藏到系统托盘</strong>。
          托盘左键唤起面板，右键菜单可单个启停 / 全部启停 / 退出。
          需要真正结束面板，用托盘菜单或下方按钮的「退出」。
        </div>

        {error && <div className="err" style={{ marginTop: 12 }}>{error}</div>}
        {msg && (
          <div style={{ marginTop: 12, color: "var(--green)", fontSize: 13 }}>{msg}</div>
        )}

        <div className="section-title">配置备份</div>
        <div style={{ color: "var(--text-dim)", fontSize: 12, marginBottom: 10, lineHeight: 1.6 }}>
          导出为明文 JSON，含服务定义、分组、依赖关系与 env
          <strong style={{ color: "var(--red)" }}>（密钥明文！）</strong>
          ，请妥善保管。导入支持"合并"（只加新 id）与"覆盖"（先停所有再重建）。
          跨机迁移走这个通道，别直接拷 panel.db（DPAPI 机器绑定，换了机器解不开）。
        </div>
        <div className="row">
          <button disabled={busy} onClick={doExport}>
            导出配置…
          </button>
          <button disabled={busy} onClick={doImport}>
            导入配置…
          </button>
        </div>

        <div className="section-title">端口体检</div>
        <div
          style={{
            color: "var(--text-dim)",
            fontSize: 12,
            marginBottom: 10,
            lineHeight: 1.6,
          }}
        >
          端口占用不等于进程就是它。这里直接问内核要监听表（GetExtendedTcpTable）
          拿到「端口 → PID → 进程名」，检查各服务声明的健康端口是不是被<strong>别的</strong>
          进程占着——启动器 fork 出的子进程（npm 之类）算自己人，不会误报。
        </div>
        <div className="row">
          <button disabled={auditing} onClick={doAudit}>
            {auditing ? "检查中…" : "重新检查"}
          </button>
        </div>
        {audit && audit.length === 0 && (
          <div style={{ marginTop: 8, color: "var(--green)", fontSize: 13 }}>
            没有发现端口冲突。
          </div>
        )}
        {audit && audit.length > 0 && (
          <div style={{ marginTop: 8 }}>
            {audit.map((a) => (
              <div className="err" key={`${a.serviceId}-${a.port}`} style={{ marginTop: 4 }}>
                端口 {a.port} 被 {a.listenerName ?? "未知进程"}（PID{" "}
                {a.listenerPid ?? "?"}）占用，但服务「{a.serviceName}」并未运行它
              </div>
            ))}
          </div>
        )}

        <div className="footer">
          <button
            className="danger"
            onClick={() => {
              if (
                confirm(
                  "退出面板？\n\n被面板托管的进程会一并结束（" +
                    PLATFORM.orphanGuard +
                    "）。" +
                    PLATFORM.card +
                    " 与 Docker 容器不受影响。"
                )
              ) {
                api.quit().catch(() => {});
              }
            }}
          >
            退出面板
          </button>
          <button className="primary" onClick={onClose}>
            完成
          </button>
        </div>
      </div>
    </div>
  );
}
