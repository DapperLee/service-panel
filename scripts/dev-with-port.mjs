// 带端口启动面板开发实例，避免 dev 端口和其它服务冲突。
//
// 用法:
//   node scripts/dev-with-port.mjs            -> 用默认端口 5273（等价 npm run tauri dev）
//   node scripts/dev-with-port.mjs 6000       -> 用 6000 端口
//   node scripts/dev-with-port.mjs 6000 --dry-run -> 只打印将使用的配置，不启动
//
// 原理:
//   1) 设置 TAURI_DEV_PORT 环境变量（vite.config.ts 读它决定 vite 绑哪个端口）
//   2) 生成一份 Tauri 配置覆盖文件（devUrl 指向同一端口；identifier 按端口区分，
//      让不同端口的实例各自拥有独立的数据目录与单实例锁，互不干扰）
//   3) 启动 `tauri dev --config <覆盖文件>`
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const BASE_ID = "com.local.servicepanel";
const DEFAULT_PORT = 5273;

const args = process.argv.slice(2);
const dry = args.includes("--dry-run");
const portArg = args.find((a) => /^\d+$/.test(a));
const port = Number(portArg) || DEFAULT_PORT;
if (port < 1024 || port > 65535) {
  console.error(`端口 ${port} 不在 1024-65535 范围内`);
  process.exit(1);
}

// 不同端口 -> 不同 identifier：独立数据目录 + 独立单实例锁，
// 多个实例才能同时存在，也不会共用同一个 SQLite 互相污染。
const identifier = port === DEFAULT_PORT ? BASE_ID : `${BASE_ID}.p${port}`;

const override = {
  build: { devUrl: `http://localhost:${port}` },
  identifier,
  app: {
    windows: [
      {
        title: port === DEFAULT_PORT ? "本地服务管理面板" : `本地服务管理面板 :${port}`,
      },
    ],
  },
};

// 覆盖文件写到系统临时目录，不污染仓库
const confPath = path.join(os.tmpdir(), `service-panel.dev.${port}.conf.json`);
fs.writeFileSync(confPath, JSON.stringify(override, null, 2));

console.log(`[dev] 端口 ${port}  identifier ${identifier}`);
console.log(`[dev] 覆盖配置 ${confPath}`);

if (dry) {
  console.log(JSON.stringify(override, null, 2));
  process.exit(0);
}

const r = spawnSync("npx", ["tauri", "dev", "--config", confPath], {
  stdio: "inherit",
  env: { ...process.env, TAURI_DEV_PORT: String(port) },
  shell: process.platform === "win32",
});
process.exit(r.status ?? 1);
