// A cell's grid colour (LIVE-MODEL-SPEC §7): a hand-picked label colour, else its
// effect's main colour, else the zone's. The effect's colour is computed from the current
// values, so the grid follows every edit — not only the first.
import { expect, test, type Page } from "@playwright/test";
import { openDemo } from "./helpers";

const cell = (page: Page, name: string) => page.locator(".pr-cell", { hasText: name }).first();
const shown = (page: Page, name: string) =>
  cell(page, name).evaluate((el) => (el as HTMLElement).style.getPropertyValue("--cell-rgb").replace(/\s/g, ""));

async function setRgb(page: Page, r: number, g: number, b: number) {
  for (const [label, v] of [["R", r], ["G", g], ["B", b]] as const) {
    const input = page.locator(".pr-workrow-panel .pr-color-fields").getByLabel(label, { exact: true });
    await input.fill(String(v));
    await input.press("Enter");
  }
}

test("the grid follows a Color cell's colour on every change", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  await cell(page, "Blanco cálido").click();
  await setRgb(page, 255, 0, 0);
  await expect.poll(() => shown(page, "Blanco cálido")).toBe("255,0,0");
  await setRgb(page, 0, 0, 255);
  await expect.poll(() => shown(page, "Blanco cálido")).toBe("0,0,255");

  // Another cell, then back: it still follows.
  await cell(page, "Plasma lento").click();
  await cell(page, "Blanco cálido").click();
  await setRgb(page, 0, 255, 0);
  await expect.poll(() => shown(page, "Blanco cálido")).toBe("0,255,0");
  await setRgb(page, 255, 255, 0);
  await expect.poll(() => shown(page, "Blanco cálido")).toBe("255,255,0");
});

test("Enter in a colour value box never fires a shortcut (it would launch the selected clip)", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  await cell(page, "Blanco cálido").click();
  await page.evaluate(() => (window.__calls.length = 0));
  const input = page.locator(".pr-workrow-panel .pr-color-fields").getByLabel("G", { exact: true });
  await input.fill("10");
  await input.press("Enter");
  const keys = await page.evaluate(() => window.__calls.filter((c) => c.cmd === "main_key").map((c) => c.args));
  expect(keys, "the key is not offered as a shortcut").toEqual([]);
});

test("a hand-picked label colour wins over the effect's colour", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  await cell(page, "Blanco cálido").click({ button: "right" });
  await page.locator(".pr-ctx-menu button", { hasText: "Color…" }).click();
  const swatch = page.locator(".pr-cellcolor-swatch").first();
  const label = ((await swatch.getAttribute("data-rgb")) ?? "").replace(/\s/g, "");
  await swatch.click();
  await expect(page.locator(".pr-cellcolor-quick")).toHaveCount(0);
  await expect.poll(() => shown(page, "Blanco cálido")).toBe(label);
  expect(await page.evaluate(() => window.__calls.some((c) => c.cmd === "main_set_cell_color"))).toBe(true);

  // The effect's colour changes; the label keeps the cell.
  await cell(page, "Blanco cálido").click();
  await setRgb(page, 0, 0, 255);
  await page.waitForTimeout(300);
  expect(await shown(page, "Blanco cálido")).toBe(label);
});
