import { defineConfig, devices } from "@playwright/test";

// UI tests on the real React app, in WebKit and Chromium, with the Tauri IPC mocked
// (LIVE-MODEL-SPEC §10.1). Run through `npm run test:e2e`, which starts them inside the
// official Playwright image (Playwright's WebKit does not run natively on Fedora).
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  // Each worker runs its own browser. With one per core (16 here) the pages load slowly
  // enough for tests to time out, and a timed-out worker's browser is killed — its WPE
  // web processes then abort (SIGTRAP) and leave core dumps on the host. Four keep every
  // test well inside its timeout.
  workers: 4,
  reporter: [["list"]],
  use: {
    baseURL: "http://127.0.0.1:5199",
    deviceScaleFactor: 1,
  },
  // WebKit is the Linux webview (WebKitGTK); Chromium is Windows' (WebView2). The geometric
  // tests run in both; the reference screenshots only in WebKit (LIVE-MODEL-SPEC §10.2).
  projects: [
    { name: "webkit", use: { ...devices["Desktop Safari"], deviceScaleFactor: 1 } },
    { name: "chromium", use: { ...devices["Desktop Chrome"], deviceScaleFactor: 1 }, grepInvert: /@screenshot/ },
  ],
  webServer: {
    command: "npx vite --port 5199 --strictPort --host 127.0.0.1",
    url: "http://127.0.0.1:5199/e2e/harness.html",
    reuseExistingServer: false,
  },
});
