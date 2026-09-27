import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 期望前端固定端口，clearScreen false 让终端只保留 Rust 的错误
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    // 避开 vite 默认的 5173：你机器上常常同时跑着多个项目的 dev server
    port: 5273,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2021",
    minify: "esbuild",
    sourcemap: false,
  },
});
