import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  base: "./",
  clearScreen: false,
  // 显式绑 127.0.0.1（防御性设置，不是某个已定位 bug 的修复）：
  // Node 17+ 解析 `localhost` 时不再优先 IPv4，vite 可能只监听 ::1，
  // 而 WebView2/Chromium 对 `localhost` 是 IPv4 优先 —— 两边对不上时
  // dev 模式会加载失败。绑死 IPv4 可消除这类不确定性。
  // 注意 devUrl 仍须保持 `http://localhost:1420`，不要写成 127.0.0.1：
  // Chromium 对 `localhost` 有隐式代理豁免，对字面 IP 没有，写成 IP 时
  // 若环境设了 HTTP_PROXY 会走代理而加载失败（实测过）。
  server: { port: 1420, strictPort: true, host: "127.0.0.1" },
  build: { outDir: "dist", target: "es2021" },
});
