// Density (LIVE-MODEL-SPEC §10.1): in Compacta / Normal / Grande nothing in the window is
// cut or overlaps, compact cells show the name on one line, the panel and the master have
// the density's fixed sizes at any window size, and the master stays anchored at the
// bottom right whatever the density or the panel.
import { expect, test } from "@playwright/test";
import { DENSITIES as SIZES } from "../src/density";
import { box, DENSITIES, layoutProblems, openDemo, relayout, VIEWPORTS } from "./helpers";

const REGIONS = {
  "zone card": ".pr-zonecard",
  "track header": ".pr-thead",
  cell: ".pr-cell",
  "track footer": ".pr-tfoot",
  scene: ".pr-tgrid-scene",
  "stop all": ".pr-tfoot-all",
  panel: ".pr-workrow-panel",
  master: ".pr-workrow-master",
};

for (const viewport of VIEWPORTS) {
  for (const density of DENSITIES) {
    test.describe(`${density} @ ${viewport.width}×${viewport.height}`, () => {
      test("nothing is cut or overlaps", async ({ page }) => {
        await openDemo(page, { density, viewport });
        const problems = await layoutProblems(page, REGIONS);
        expect(problems, problems.map((p) => `${p.region}: ${p.what}`).join("\n")).toEqual([]);
      });

      test("the panel and the master have the density's sizes", async ({ page }) => {
        await openDemo(page, { density, viewport });
        const s = SIZES[density];
        expect((await box(page, ".pr-workrow-panel")).h, "panel height").toBe(s.panel);
        expect(s.masterFader, "the density defines the master fader height").toBeGreaterThan(0);
        expect((await box(page, ".pr-master-fader .pr-fader")).h, "master fader height").toBe(s.masterFader);
        expect(s.slider, "the density defines the panel's slider height").toBeGreaterThan(0);
        expect((await box(page, ".pr-workrow-panel .pr-hslider")).h, "panel slider height").toBe(s.slider);
        expect(s.field, "the density defines the panel's field height").toBeGreaterThan(0);
        expect((await box(page, ".pr-workrow-panel input.pr-field-value")).h, "panel field height").toBe(s.field);
      });
    });
  }

  test(`the master stays anchored @ ${viewport.width}×${viewport.height}`, async ({ page }) => {
    const corners: string[] = [];
    await openDemo(page, { density: "normal", viewport });
    for (const density of DENSITIES) {
      const sizes: string[] = [];
      for (const panelHidden of [false, true]) {
        await relayout(page, { density, panelHidden });
        const m = await box(page, ".pr-workrow-master");
        corners.push(`${density}${panelHidden ? " (panel hidden)" : ""}: right ${m.x + m.w}, bottom ${m.y + m.h}`);
        sizes.push(`${m.w}×${m.h}`);
      }
      expect(sizes[1], `${density}: the master keeps its size with the panel hidden`).toBe(sizes[0]);
    }
    const anchor = (c: string) => c.split(": ")[1];
    for (const c of corners) expect(anchor(c), `bottom-right corner — ${c}`).toBe(anchor(corners[0]));
  });
}

test("compact cells show the name on one line, no effect subtitle", async ({ page }) => {
  await openDemo(page, { density: "compact" });
  const cells = await page.evaluate(() =>
    Array.from(document.querySelectorAll(".pr-cell.is-assigned")).map((c) => {
      const title = c.querySelector(".pr-cell-title") as HTMLElement | null;
      const sub = c.querySelector(".pr-cell-effect") as HTMLElement | null;
      const lh = title ? parseFloat(getComputedStyle(title).lineHeight) || 16 : 0;
      return {
        name: title?.textContent ?? "",
        oneLine: !!title && title.getBoundingClientRect().height <= lh * 1.3,
        ellipsis: !!title && getComputedStyle(title).textOverflow === "ellipsis",
        tooltip: (c as HTMLElement).title.includes(title?.textContent ?? "\u0000"),
        subtitle: !!sub && sub.getBoundingClientRect().height > 0 && getComputedStyle(sub).display !== "none",
      };
    }),
  );
  expect(cells.length).toBeGreaterThan(20);
  for (const c of cells) {
    expect(c.oneLine, `${c.name}: one line`).toBe(true);
    expect(c.ellipsis, `${c.name}: a long name ends in …`).toBe(true);
    expect(c.tooltip, `${c.name}: full name in the tooltip`).toBe(true);
    expect(c.subtitle, `${c.name}: no effect subtitle in compact`).toBe(false);
  }
});

test("compact footer: no opacity number, it shows while the fader moves", async ({ page }) => {
  await openDemo(page, { density: "compact" });
  const value = page.locator(".pr-tfoot").first().locator("[class*='faderval']");
  await expect(value).toBeHidden();
  const fader = await box(page, ".pr-tfoot .pr-fader");
  await page.mouse.move(fader.x + fader.w / 2, fader.y + 4);
  await page.mouse.down();
  await page.mouse.move(fader.x + fader.w / 2, fader.y + fader.h / 2, { steps: 4 });
  await expect(value).toBeVisible();
  await page.mouse.up();
  await expect(value).toBeHidden();
});
