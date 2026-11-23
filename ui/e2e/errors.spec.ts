// No error brings the app down (LIVE-MODEL-SPEC §10.3 item 5): errors the backend reports
// on its own (`app-error`: autosave, MIDI mappings, a panic on any thread) and commands that
// failed with nobody handling them both reach the status line.
import { expect, test } from "@playwright/test";
import { openDemo } from "./helpers";

type Emit = { __emit: (e: string, p: unknown) => Promise<void> };

test("backend errors and unhandled command failures reach the status line", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  const status = page.locator(".pr-statusline");

  await page.evaluate(() => (window as unknown as Emit).__emit("app-error", "El autoguardado falló: disco lleno"));
  await expect(status).toHaveText("Error: El autoguardado falló: disco lleno");
  await expect(status).toHaveClass(/is-error/);

  // A rejected command promise that no caller catches (as a Rust `Err` arrives).
  await page.evaluate(() => {
    void Promise.reject("No se pudo escribir el archivo");
  });
  await expect(status).toHaveText("Error: No se pudo escribir el archivo");

  // The window is still alive and usable.
  await expect(page.locator(".pr-tgrid-scene").first()).toBeVisible();
});
