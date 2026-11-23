// MIDI learn (LIVE-MODEL-SPEC §10.1): with learn on, every learnable control draws its own
// frame on its own box — no overlay layers — in every density, with the grid scrolled
// both ways and with the panel hidden.
import { expect, test, type Page } from "@playwright/test";
import { DENSITIES, enterLearn, learnFrames, openDemo } from "./helpers";

async function expectFramesOnTheirElements(page: Page, where: string) {
  const frames = await learnFrames(page);
  expect(frames.length, `${where}: learnable controls found`).toBeGreaterThan(40);
  const off = frames.filter(
    (f) =>
      !f.frame ||
      Math.abs(f.frame.x - f.element.x) > 0.5 ||
      Math.abs(f.frame.y - f.element.y) > 0.5 ||
      Math.abs(f.frame.w - f.element.w) > 0.5 ||
      Math.abs(f.frame.h - f.element.h) > 0.5,
  );
  const fmt = (b: { x: number; y: number; w: number; h: number } | null) =>
    b ? `${b.x.toFixed(1)},${b.y.toFixed(1)} ${b.w.toFixed(1)}×${b.h.toFixed(1)}` : "no frame";
  expect(
    off.map((f) => `${f.name}: frame ${fmt(f.frame)} ≠ element ${fmt(f.element)}`),
    `${where}: ${off.length} of ${frames.length} frames off their element`,
  ).toEqual([]);
  expect(await page.locator(".pr-learn-overlay").count(), `${where}: no overlay layers`).toBe(0);
}

for (const density of DENSITIES) {
  test(`learn frames match their elements — ${density}`, async ({ page }) => {
    await openDemo(page, { density });
    await enterLearn(page);
    await expectFramesOnTheirElements(page, "at rest");

    const scroll = page.locator(".pr-tgrid-scroll");
    await scroll.evaluate((el) => el.scrollTo({ left: el.scrollWidth, top: el.scrollHeight / 3 }));
    await page.waitForTimeout(100);
    await expectFramesOnTheirElements(page, "grid scrolled");
  });

  test(`learn frames match their elements, panel hidden — ${density}`, async ({ page }) => {
    await openDemo(page, { density, panelHidden: true });
    await enterLearn(page);
    await expectFramesOnTheirElements(page, "panel hidden");
  });
}

/** Every control's box, keyed by position in the document. */
async function controlBoxes(page: Page) {
  return page.evaluate(() =>
    Array.from(document.querySelectorAll(".pr-mainwin-body button, .pr-mainwin-body .pr-cell, .pr-mainwin-body .pr-fader, .pr-mainwin-body input")).map(
      (el, i) => {
        const r = el.getBoundingClientRect();
        return `${i} ${el.tagName} ${r.x.toFixed(1)},${r.y.toFixed(1)} ${r.width.toFixed(1)}×${r.height.toFixed(1)}`;
      },
    ),
  );
}

for (const density of DENSITIES) {
  test(`turning learn on moves nothing — ${density}`, async ({ page }) => {
    await openDemo(page, { density });
    const before = await controlBoxes(page);
    await enterLearn(page);
    expect(await controlBoxes(page)).toEqual(before);
  });
}

test("in learn a click arms the target and never operates the control", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  await enterLearn(page);
  await page.evaluate(() => (window.__calls.length = 0));
  const cell = page.locator(".pr-cell.is-assigned").nth(1);
  await cell.click();
  await cell.dblclick();
  const cmds = await page.evaluate(() => window.__calls.map((c) => c.cmd));
  expect(cmds).toContain("midi_learn_arm");
  for (const forbidden of ["main_select", "main_trigger_cell", "main_arm_cell"]) {
    expect(cmds, `${forbidden} must not run in learn mode`).not.toContain(forbidden);
  }
  await expect(cell).toHaveAttribute("data-learn-state", /armed/);
});
