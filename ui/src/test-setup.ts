// Vitest setup: what jsdom lacks, the mocked Tauri IPC, and a clean DOM between tests.
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

class NoResizeObserver {
  observe() {}
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver ??= NoResizeObserver as unknown as typeof ResizeObserver;

// jsdom has no PointerEvent: a MouseEvent carries what the grid reads (button, Shift).
if (!("PointerEvent" in globalThis)) {
  class PointerEvent extends MouseEvent {
    pointerId: number;
    constructor(type: string, init: PointerEventInit = {}) {
      super(type, init);
      this.pointerId = init.pointerId ?? 1;
    }
  }
  (globalThis as unknown as { PointerEvent: typeof MouseEvent }).PointerEvent = PointerEvent;
}

await import("../e2e/mock");

afterEach(() => {
  cleanup();
  window.__calls.length = 0;
  localStorage.clear();
});
