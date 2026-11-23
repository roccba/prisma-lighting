// The scene column (LIVE-MODEL-SPEC §10.3): right-click a scene → Renombrar (in place),
// Color…, Armar escena; a plain click still launches it.
import { expect, test } from "@playwright/test";
import { calls, clearCalls, openDemo } from "./helpers";

test("a scene is renamed in place, coloured and armed from its menu; a click launches it", async ({ page }) => {
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

  // Color…: a quick colour, saved, shown as the scene's stripe.
  await scene(5).locator(".pr-scene-btn").click({ button: "right" });
  await menuItem("Color…").click();
  const swatch = page.locator(".pr-cellcolor-swatch").first();
  const rgb = ((await swatch.getAttribute("data-rgb")) ?? "").split(",").map((v) => Number(v));
  await swatch.click();
  await expect.poll(() => calls(page, "main_set_scene_color")).toEqual([{ row: 5, color: rgb }]);
  await expect(scene(5).locator(".pr-scene-btn")).toHaveClass(/has-color/);

  // An empty name goes back to the row number.
  await scene(1).locator(".pr-scene-btn").click({ button: "right" });
  await menuItem("Renombrar").click();
  await scene(1).locator("input").fill("");
  await scene(1).locator("input").press("Enter");
  await expect(scene(1).locator(".pr-scene-btn")).toHaveText("▶ 2");
});
