import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri dev expects a fixed port; matches tauri.conf.json -> build.devUrl
const TAURI_DEV_PORT = 1420;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: TAURI_DEV_PORT,
    strictPort: true,
    host: "127.0.0.1",
    hmr: { port: TAURI_DEV_PORT + 1 },
    watch: { ignored: ["**/src-tauri/**"] },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    target: "es2022",
    minify: "esbuild",
    sourcemap: false,
  },
});
