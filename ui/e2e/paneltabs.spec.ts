// The bottom panel's tabs and the Grupos tab (LIVE-MODEL-SPEC §8.5 step 4): Parámetros |
// Grupos in the strip over the panel, Alt+1… pick them, the choice is remembered per
// machine and a cell click never changes it; Grupos shows a fader per group master and the
// presets as buttons (click applies, right-click renames / sets the fade / deletes, + Preset
// stores the masters).
import { expect, test, type Page } from "@playwright/test";
import { calls, clearCalls, DENSITIES, layoutProblems, openDemo, relayout, VIEWPORTS } from "./helpers";

const tab = (page: Page, name: string) => page.getByRole("tab", { name });
const panel = (page: Page) => page.getByRole("tabpanel");

test("the tabs switch the panel, Alt+n picks one, and the choice is remembered", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  await expect(page.getByRole("tablist", { name: "Panel" }).getByRole("tab")).toHaveText(["Parámetros", "Grupos"]);
  await expect(tab(page, "Parámetros")).toHaveAttribute("aria-selected", "true");

  await tab(page, "Grupos").click();
  await expect(panel(page)).toHaveAttribute("data-tab", "groups");
  await expect(page.locator(".pr-groupstrip .pr-groupstrip-name")).toHaveText([
    "Tubos pares",
    "Tubos impares",
    "Bordes de cortinas",
  ]);

  // A cell click selects the cell; the tab stays.
  await page.locator(".pr-cell").first().click();
  await expect(panel(page)).toHaveAttribute("data-tab", "groups");

  // Alt+1 / Alt+2 (the default shortcuts).
  await page.keyboard.press("Alt+1");
  await expect(panel(page)).toHaveAttribute("data-tab", "params");
  await page.keyboard.press("Alt+2");
  await expect(panel(page)).toHaveAttribute("data-tab", "groups");

  // Remembered per machine.
  await relayout(page, { density: "normal" });
  await expect(panel(page)).toHaveAttribute("data-tab", "groups");
});

test("with the panel hidden, a tab (or its shortcut) shows it on that tab", async ({ page }) => {
  await openDemo(page, { density: "normal", panelHidden: true });
  await expect(page.getByRole("tabpanel")).toHaveCount(0);
  await page.keyboard.press("Alt+2");
  await expect(panel(page)).toHaveAttribute("data-tab", "groups");
  await relayout(page, { density: "normal", panelHidden: true });
  await tab(page, "Parámetros").click();
  await expect(panel(page)).toHaveAttribute("data-tab", "params");
});

test("group masters and presets send their commands", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  await tab(page, "Grupos").click();
  const strips = page.locator(".pr-groupstrip");
  await expect(strips).toHaveCount(3);

  // A master's fader: drag it down.
  await clearCalls(page);
  const fader = strips.first().getByLabel("Master Tubos pares");
  const b = (await fader.boundingBox())!;
  await page.mouse.move(b.x + b.width / 2, b.y + 4);
  await page.mouse.down();
  await page.mouse.move(b.x + b.width / 2, b.y + b.height - 2, { steps: 6 });
  await page.mouse.up();
  const levels = (await calls(page, "main_set_group_level")) as { group: string; level: number }[];
  expect(levels.length).toBeGreaterThan(0);
  expect(levels[levels.length - 1].level).toBeLessThan(0.1);
  await expect(strips.first().locator(".pr-groupstrip-value")).toHaveText(/^\d$|^1\d$/);

  // Presets: the demo's two, with their fades; a click applies.
  const presets = page.locator(".pr-preset-btn");
  await expect(presets.locator(".pr-preset-name")).toHaveText(["Sólo pares", "Todo"]);
  await expect(presets.locator(".pr-preset-fade")).toHaveText(["1,5 s", "0,5 s"]);
  await clearCalls(page);
  await presets.first().click();
  expect(await calls(page, "main_apply_group_preset")).toEqual([{ preset: "00000000-0000-0000-0000-0000de300301" }]);

  // + Preset stores the masters as a new preset.
  await page.getByRole("button", { name: "+ Preset" }).click();
  expect(await calls(page, "main_add_group_preset")).toHaveLength(1);
  await expect(presets.locator(".pr-preset-name")).toHaveText(["Sólo pares", "Todo", "Preset 1"]);

  // Right-click: rename in place, set the fade, delete.
  const menuItem = (label: string) => page.locator(".pr-ctx-menu button", { hasText: label });
  await presets.nth(2).click({ button: "right" });
  await menuItem("Renombrar").click();
  const name = page.getByRole("textbox", { name: "Nombre del preset" });
  await name.fill("Cierre");
  await name.press("Enter");
  expect(await calls(page, "main_rename_group_preset")).toEqual([{ preset: expect.any(String), name: "Cierre" }]);
  await expect(presets.nth(2).locator(".pr-preset-name")).toHaveText("Cierre");

  await presets.nth(2).click({ button: "right" });
  await menuItem("Fundido: 2 s").click();
  expect(await calls(page, "main_set_group_preset_fade")).toEqual([{ preset: expect.any(String), seconds: 2 }]);
  await expect(presets.nth(2).locator(".pr-preset-fade")).toHaveText("2 s");

  await presets.nth(2).click({ button: "right" });
  await menuItem("Regrabar con los niveles actuales").click();
  expect(await calls(page, "main_rerecord_group_preset")).toHaveLength(1);

  await presets.nth(2).click({ button: "right" });
  await menuItem("Eliminar").click();
  expect(await calls(page, "main_delete_group_preset")).toHaveLength(1);
  await expect(presets).toHaveCount(2);
});

for (const density of DENSITIES) {
  for (const viewport of VIEWPORTS) {
    test(`the Grupos tab fits — ${density} at ${viewport.width}×${viewport.height}`, async ({ page }) => {
      await openDemo(page, { density, viewport });
      await tab(page, "Grupos").click();
      await expect(page.locator(".pr-groupstrip")).toHaveCount(3);
      // The tabs and the handle sit in the gap between the grid and the panel, touching
      // neither region.
      const gap = await page.evaluate(() => {
        const above = document.querySelector(".pr-region-grid")!.getBoundingClientRect().bottom;
        const below = document.querySelector(".pr-workrow-panel")!.getBoundingClientRect().top;
        return Array.from(document.querySelectorAll(".pr-paneltab, .pr-panelbar-btn"))
          .map((el) => el.getBoundingClientRect())
          .filter((r) => r.top < above - 0.5 || r.bottom > below + 0.5)
          .map((r) => `${r.top}–${r.bottom} outside ${above}–${below}`);
      });
      expect(gap).toEqual([]);
      expect(
        await layoutProblems(page, {
          masters: ".pr-groupstab-masters",
          presets: ".pr-groupstab-presets",
          panel: ".pr-workrow-panel",
        }),
      ).toEqual([]);
      // Every fader has room to move and its value shows.
      for (const f of await page.locator(".pr-groupstrip .pr-fader").all()) {
        expect((await f.boundingBox())!.height).toBeGreaterThan(24);
      }
      await expect(page.locator(".pr-groupstrip-value").first()).toBeVisible();
    });
  }
}
