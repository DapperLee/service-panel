import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 期望前端固定端口，clearScreen false 让终端只保留 Rust 的错误
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    // 默认 5273；测试实例可用 TAURI_DEV_PORT 指定别的端口，避免和正式实例抢端口。
    // 注意：改端口要同时让 tauri 的 devUrl 指到同一端口（见 package.json 的 tauri:dev:test）。
    port: Number(process.env.TAURI_DEV_PORT) || 5273,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2021",
    minify: "esbuild",
    sourcemap: false,
  },
});
