import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Component-logic tests (LIVE-MODEL-SPEC §10.3 item 2): React in jsdom, the Tauri IPC
// mocked by the same module the Playwright harness uses (e2e/mock.ts, demo-show fixture).
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    setupFiles: ["./src/test-setup.ts"],
  },
});
