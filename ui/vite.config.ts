import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Relative base so the built assets resolve under Tauri's asset protocol.
export default defineConfig({
  base: "./",
  plugins: [react()],
  clearScreen: false,
  build: {
    outDir: "dist",
    // The editor is a config tool; a readable, unminified-enough build is fine and
    // targets the modern webview Tauri ships.
    target: "es2021",
  },
});
