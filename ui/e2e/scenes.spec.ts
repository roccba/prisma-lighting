// The scene column (LIVE-MODEL-SPEC §10.3): right-click a scene → Renombrar (in place) or
// Armar escena; a plain click launches it. Scene buttons are triggers: no colour, no state —
// a fired scene flashes once, whatever fired it.
import { expect, test } from "@playwright/test";
import { calls, clearCalls, openDemo } from "./helpers";

test("a scene is renamed in place and armed from its menu; a click launches it", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  const scene = (i: number) => page.locator(".pr-tgrid-scene").nth(i);
  const menuItem = (label: string) => page.locator(".pr-ctx-menu button", { hasText: label });

  // A plain click launches (the menu is only on right-click).
  await clearCalls(page);
  await scene(0).locator(".pr-scene-btn").click();
  expect(await calls(page, "main_launch_scene")).toEqual([{ row: 0, arm: false }]);
  await expect(page.locator(".pr-ctx-menu")).toHaveCount(0);

  // Renombrar: an input in place of the button; Enter saves.
  await scene(1).locator(".pr-scene-btn").click({ button: "right" });
  await menuItem("Renombrar").click();
  const input = scene(1).locator("input");
  await expect(input).toBeFocused();
  await input.fill("Intro");
  await input.press("Enter");
  await expect(scene(1).locator(".pr-scene-btn")).toHaveText("▶ Intro");
  expect(await calls(page, "main_rename_scene")).toEqual([{ row: 1, name: "Intro" }]);

  // Escape cancels a rename.
  await scene(1).locator(".pr-scene-btn").click({ button: "right" });
  await menuItem("Renombrar").click();
  await scene(1).locator("input").fill("No");
  await scene(1).locator("input").press("Escape");
  await expect(scene(1).locator(".pr-scene-btn")).toHaveText("▶ Intro");

  // Armar escena.
  await clearCalls(page);
  await scene(2).locator(".pr-scene-btn").click({ button: "right" });
  await menuItem("Armar escena").click();
  expect(await calls(page, "main_launch_scene")).toEqual([{ row: 2, arm: true }]);

  // An empty name goes back to the row number.
  await scene(1).locator(".pr-scene-btn").click({ button: "right" });
  await menuItem("Renombrar").click();
  await scene(1).locator("input").fill("");
  await scene(1).locator("input").press("Enter");
  await expect(scene(1).locator(".pr-scene-btn")).toHaveText("▶ 2");
});

test("the menu offers Renombrar and Armar escena only; a scene button shows no colour", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  // The demo's scenes carry colours in the show: they stay stored, never drawn.
  const btn = page.locator(".pr-tgrid-scene .pr-scene-btn").first();
  expect(await btn.evaluate((el) => getComputedStyle(el).boxShadow)).toBe("none");
  await btn.click({ button: "right" });
  await expect(page.locator(".pr-ctx-menu button")).toHaveText(["Renombrar", "Armar escena"]);
});

test("a fired scene flashes once, under half a second, whoever fired it", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  const scene = (i: number) => page.locator(".pr-tgrid-scene .pr-scene-btn").nth(i);

  // From the screen. The flash is timed in the page, from when it appears to when it goes.
  const lasted = page.evaluate(
    () =>
      new Promise<number>((done) => {
        let start = 0;
        const watch = () => {
          const lit = document.querySelector(".pr-tgrid-scene:nth-child(3) .pr-scene-btn.is-fired");
          if (lit && !start) start = performance.now();
          if (!lit && start) return done(performance.now() - start);
          requestAnimationFrame(watch);
        };
        watch();
      }),
  );
  await scene(2).click();
  expect(await lasted, "the flash lasts under half a second").toBeLessThan(500);

  // From elsewhere — the keyboard or a controller: the engine reports the fire.
  await page.evaluate(() => (window as unknown as { __emit: (e: string, p: unknown) => Promise<void> }).__emit("scene-fired", 4));
  await expect(scene(4)).toHaveClass(/is-fired/);
  await expect(scene(4)).not.toHaveClass(/is-fired/, { timeout: 1000 });

  // Not a state: nothing stays lit.
  await expect(page.locator(".pr-scene-btn.is-fired, .pr-scene-btn.is-active")).toHaveCount(0);
});
