import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { appShellPlugin } from "./scripts/app-shell-plugin.mjs";

function webBackendProxyTarget() {
  if (process.env.LANTOR_WEB_PROXY_TARGET) {
    return process.env.LANTOR_WEB_PROXY_TARGET;
  }
  const bind = process.env.LANTOR_WEB_BIND?.trim() || "127.0.0.1:8787";
  const match = bind.match(/^(?:\[(.*)\]|([^:]+)):(\d+)$/);
  if (!match) {
    return "http://127.0.0.1:8787";
  }
  const host = match[1] || match[2];
  const port = match[3];
  if (host === "0.0.0.0") {
    return `http://127.0.0.1:${port}`;
  }
  if (host === "::") {
    return `http://[::1]:${port}`;
  }
  return host.includes(":") ? `http://[${host}]:${port}` : `http://${host}:${port}`;
}

export default defineConfig(({ mode }) => ({
  plugins: [react(), appShellPlugin()],
  resolve: {
    alias: mode === "bench"
      ? [
        { find: /^react-dom\/client$/, replacement: "react-dom/profiling" },
      ]
      : [],
  },
  server: {
    host: "127.0.0.1",
    port: 5173,
    strictPort: true,
    proxy: {
      "/api": {
        target: webBackendProxyTarget(),
        changeOrigin: true,
      },
    },
  },
}));
