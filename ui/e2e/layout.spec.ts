// Grid layout (LIVE-MODEL-SPEC §10.3 item 1): every track's header, cells and footer share
// one column; the scene column never covers a cell or a button; the grid shows whole rows
// at any window height. In every density, at both window sizes, scrolled or not.
import { expect, test, type Page } from "@playwright/test";
import { DENSITIES, openDemo, relayout, VIEWPORTS } from "./helpers";

/** Per track: the x and width of its header, cells and footer. */
async function columns(page: Page) {
  return page.evaluate(() => {
    const out: Record<string, { what: string; x: number; w: number }[]> = {};
    document.querySelectorAll(".pr-tgrid [data-track]").forEach((el) => {
      const id = el.getAttribute("data-track")!;
      const r = el.getBoundingClientRect();
      const what = el.classList.contains("pr-thead") ? "header" : el.classList.contains("pr-tfoot") ? "footer" : "cell";
      (out[id] ??= []).push({ what, x: r.x, w: r.width });
    });
    return out;
  });
}

async function expectAligned(page: Page, where: string) {
  const cols = await columns(page);
  expect(Object.keys(cols).length, where).toBeGreaterThan(5);
  for (const [id, parts] of Object.entries(cols)) {
    const head = parts.find((p) => p.what === "header")!;
    for (const p of parts) {
      expect(Math.abs(p.x - head.x), `${where}: ${p.what} of ${id} x`).toBeLessThanOrEqual(0.5);
      expect(Math.abs(p.w - head.w), `${where}: ${p.what} of ${id} width`).toBeLessThanOrEqual(0.5);
    }
  }
}

/** Every cell and track button visible in the grid's scroll box is the topmost element
 *  at its own centre and corners (insets 3 px): nothing — the scene column included —
 *  is drawn over it. Returns what is covered, and by what. */
async function covered(page: Page) {
  return page.evaluate(() => {
    // What is really in view: the scroll box clipped by the grid's wrap, which scrolls the
    // whole block when not even one row fits.
    const sb = document.querySelector(".pr-tgrid-scroll")!.getBoundingClientRect();
    const wr = document.querySelector(".pr-tgrid-wrap")!.getBoundingClientRect();
    const box = {
      left: Math.max(sb.left, wr.left),
      right: Math.min(sb.right, wr.right),
      top: Math.max(sb.top, wr.top),
      bottom: Math.min(sb.bottom, wr.bottom),
    };
    const head = document.querySelector(".pr-thead[data-track]")!.getBoundingClientRect();
    const foot = document.querySelector(".pr-tfoot[data-track]")!.getBoundingClientRect();
    const out: string[] = [];
    const check = (el: Element, top: number, bottom: number) => {
      const r = el.getBoundingClientRect();
      // Only what is wholly in view: scrolled-away parts and rows under the sticky header
      // or footer are hidden by design.
      if (r.left < box.left || r.right > box.right || r.top < top || r.bottom > bottom) return;
      for (const [x, y] of [
        [r.left + r.width / 2, r.top + r.height / 2],
        [r.left + 3, r.top + 3],
        [r.right - 3, r.top + 3],
        [r.left + 3, r.bottom - 3],
        [r.right - 3, r.bottom - 3],
      ]) {
        const hit = document.elementFromPoint(x, y);
        if (!hit || !(el === hit || el.contains(hit))) {
          out.push(`${el.className} at ${Math.round(x)},${Math.round(y)} covered by ${hit?.className ?? "nothing"}`);
          return;
        }
      }
    };
    document
      .querySelectorAll(".pr-tgrid-body .pr-cell")
      .forEach((el) => check(el, Math.max(head.bottom, box.top), Math.min(foot.top, box.bottom)));
    document.querySelectorAll(".pr-tfoot[data-track] button").forEach((el) => check(el, box.top, box.bottom));
    document.querySelectorAll(".pr-thead[data-track]").forEach((el) => check(el, box.top, box.bottom));
    return out;
  });
}

/** The clip rows in view between the sticky header and footer: none cut. */
async function cutRows(page: Page) {
  return page.evaluate(() => {
    const head = document.querySelector(".pr-thead[data-track]")!.getBoundingClientRect();
    const foot = document.querySelector(".pr-tfoot[data-track]")!.getBoundingClientRect();
    const first = document.querySelector(".pr-thead[data-track]")!.getAttribute("data-track");
    const cells = Array.from(document.querySelectorAll(`.pr-cell[data-track="${first}"]`)).map((c) => c.getBoundingClientRect());
    const visible = cells.filter((r) => r.bottom > head.bottom && r.top < foot.top);
    const cut = visible.filter((r) => r.top < head.bottom - 0.5 || r.bottom > foot.top + 0.5);
    return { visible: visible.length, cut: cut.length };
  });
}

for (const viewport of VIEWPORTS) {
  for (const density of DENSITIES) {
    test(`columns aligned, nothing covered — ${density} @ ${viewport.width}×${viewport.height}`, async ({ page }) => {
      await openDemo(page, { density, viewport });
      await expectAligned(page, "at rest");
      expect(await covered(page), "at rest").toEqual([]);

      const scroll = page.locator(".pr-tgrid-scroll");
      await scroll.evaluate((el) => el.scrollTo({ left: el.scrollWidth, top: 0 }));
      await page.waitForTimeout(100);
      await expectAligned(page, "scrolled right");
      expect(await covered(page), "scrolled right, the last track beside the scene column").toEqual([]);

      await scroll.evaluate((el) => el.scrollTo({ left: el.scrollWidth / 2, top: el.scrollHeight }));
      await page.waitForTimeout(100);
      await expectAligned(page, "scrolled down");
      expect(await covered(page), "scrolled down").toEqual([]);
    });
  }
}

for (const density of DENSITIES) {
  test(`whole rows at any window height — ${density}`, async ({ page }) => {
    await openDemo(page, { density, viewport: { width: 1600, height: 800 } });
    for (const height of [800, 860, 937, 1001, 1080, 1200]) {
      await page.setViewportSize({ width: 1600, height });
      await relayout(page, { density });
      const rows = await cutRows(page);
      expect(rows.visible, `${height} px: rows in view`).toBeGreaterThanOrEqual(1);
      expect(rows.cut, `${height} px: no row cut by the header or the footer`).toBe(0);
    }
  });
}

test("when not even one row fits, the whole grid scrolls as one block — nothing cut", async ({ page }) => {
  // Grande in the smallest window: header + one row + footer are taller than the region.
  await openDemo(page, { density: "large", viewport: { width: 1280, height: 800 } });
  const wrap = page.locator(".pr-tgrid-wrap");
  const sizes = await wrap.evaluate((el) => ({ scroll: el.scrollHeight, client: el.clientHeight }));
  expect(sizes.scroll, "the block is taller than the region: it scrolls").toBeGreaterThan(sizes.client);
  expect(await cutRows(page), "one whole row between header and footer").toEqual({ visible: 1, cut: 0 });

  // Top: the headers are whole and on top.
  expect(await covered(page)).toEqual([]);
  const head = await page.locator(".pr-thead[data-track]").first().boundingBox();
  const wrapBox = (await wrap.boundingBox())!;
  expect(head!.y).toBeGreaterThanOrEqual(wrapBox.y);

  // Bottom: the footer's buttons are reachable and nothing covers them.
  await wrap.evaluate((el) => el.scrollTo({ top: el.scrollHeight }));
  await page.waitForTimeout(100);
  const buttons = page.locator(".pr-tfoot[data-track] .pr-tfoot-btns button").first();
  const b = (await buttons.boundingBox())!;
  expect(b.y + b.height, "M / S / sel in view").toBeLessThanOrEqual(wrapBox.y + wrapBox.height + 0.5);
  expect(await covered(page)).toEqual([]);
  await buttons.click();
  expect(await page.evaluate(() => window.__calls.some((c) => c.cmd === "main_set_track_mute"))).toBe(true);
});
