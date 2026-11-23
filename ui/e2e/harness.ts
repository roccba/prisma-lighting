// The Playwright harness: the real UI with the Tauri IPC mocked (LIVE-MODEL-SPEC §10.1).
// The mock is installed first (module order), then the app's own entry runs unchanged.
import "./mock";
import "../src/main.tsx";
