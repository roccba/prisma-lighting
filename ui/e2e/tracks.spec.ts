// Track order and layout lock (LIVE-MODEL-SPEC §10.2): a track is dragged only from its
// header and only past a few pixels; Vista → Bloquear layout stops dragging, adding and
// deleting tracks and changing destinations, is shown in the top bar and is remembered.
import { expect, test, type Page } from "@playwright/test";
import { box, calls, clearCalls, drag, openDemo } from "./helpers";

const header = (page: Page, i: number) => page.locator(".pr-thead").nth(i);

async function centre(page: Page, selector: string, i = 0) {
  const b = await page.locator(selector).nth(i).boundingBox();
  if (!b) throw new Error(`${selector} has no box`);
  return { x: b.x + b.width / 2, y: b.y + b.height / 2 };
}

test("a track moves only when its header is dragged past the threshold", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  const second = await box(page, ".pr-thead:nth-child(2)");

  await clearCalls(page);
  await drag(page, await centre(page, ".pr-thead", 0), 3);
  expect(await calls(page, "main_move_track"), "3 px is a click, not a drag").toEqual([]);

  await drag(page, await centre(page, ".pr-cell.is-assigned", 0), second.w * 1.5);
  expect(await calls(page, "main_move_track"), "a cell never drags its track").toEqual([]);

  await drag(page, await centre(page, ".pr-thead .pr-thead-more", 0), second.w * 1.5);
  expect(await calls(page, "main_move_track"), "the ⋯ button never drags").toEqual([]);

  await drag(page, await centre(page, ".pr-thead", 0), second.w * 1.5);
  const moves = await calls(page, "main_move_track");
  expect(moves.length, "a header dragged past the next one moves its track").toBe(1);
});

test("with the layout locked nothing structural can change", async ({ page }) => {
  await openDemo(page, { density: "normal", layoutLocked: true });
  await expect(page.locator(".pr-lock-ind"), "shown in the top bar").toBeVisible();
  const width = (await box(page, ".pr-thead")).w;

  await clearCalls(page);
  await drag(page, await centre(page, ".pr-thead", 0), width * 1.5);
  expect(await calls(page, "main_move_track"), "no drag").toEqual([]);

  await expect(page.locator(".pr-tgrid-addbtn"), "no adding").toBeDisabled();

  await header(page, 0).click({ button: "right" });
  await expect(page.locator(".pr-ctx-menu button", { hasText: "Eliminar track" }), "no deleting").toBeDisabled();
  await page.keyboard.press("Escape");

  await header(page, 0).dblclick();
  const dialog = page.getByRole("dialog", { name: /Ajustes del track/ });
  await expect(dialog).toBeVisible();
  for (const zone of await dialog.locator(".pr-trackdlg-zone .pr-chk").all()) {
    await expect(zone, "no destination change").toHaveAttribute("aria-disabled", "true");
  }
  await expect(dialog.getByRole("button", { name: "Aplicar destino" })).toBeDisabled();
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();

  // A click on the top-bar indicator unlocks.
  await page.locator(".pr-lock-ind").click();
  await expect(page.locator(".pr-lock-ind")).toBeHidden();
  await expect(page.locator(".pr-tgrid-addbtn")).toBeEnabled();
  await drag(page, await centre(page, ".pr-thead", 0), width * 1.5);
  expect((await calls(page, "main_move_track")).length, "unlocked: the drag moves the track").toBe(1);
});

test("Vista → Bloquear layout locks, and the lock is remembered", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  await expect(page.locator(".pr-lock-ind")).toBeHidden();
  await page.getByText("Vista", { exact: true }).click();
  await page.getByText("Bloquear layout", { exact: true }).click();
  await expect(page.locator(".pr-lock-ind")).toBeVisible();
  await page.reload();
  await expect(page.locator(".pr-cell").first()).toBeVisible();
  await expect(page.locator(".pr-lock-ind"), "remembered per machine").toBeVisible();
  await expect(page.locator(".pr-tgrid-addbtn")).toBeDisabled();
});

test("while dragging: a header copy follows the pointer, the column dims, a line marks the drop", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  const heads = page.locator(".pr-thead[data-track]");
  const id = await heads.nth(0).getAttribute("data-track");
  const name = (await heads.nth(0).locator(".pr-thead-name").textContent()) ?? "";
  const h0 = (await heads.nth(0).boundingBox())!;
  const h2 = (await heads.nth(2).boundingBox())!;
  const h3 = (await heads.nth(3).boundingBox())!;

  // Press on track 1's header and move to the middle of track 4's left half, no release.
  const start = { x: h0.x + h0.width / 2, y: h0.y + h0.height / 2 };
  const to = { x: h3.x + h3.width / 4, y: start.y + 12 };
  await page.mouse.move(start.x, start.y);
  await page.mouse.down();
  await page.mouse.move(to.x, to.y, { steps: 10 });

  // The copy: shows the track, semi-transparent, kept under the pointer at the grip point.
  const ghost = page.locator(".pr-thead-ghost");
  await expect(ghost).toBeVisible();
  await expect(ghost).toContainText(name);
  const g = (await ghost.boundingBox())!;
  expect(Math.abs(g.x - (to.x - (start.x - h0.x)))).toBeLessThan(2);
  expect(Math.abs(g.y - (to.y - (start.y - h0.y)))).toBeLessThan(2);
  expect(Number(await ghost.evaluate((el) => getComputedStyle(el).opacity))).toBeLessThan(1);
  expect(await ghost.evaluate((el) => getComputedStyle(el).pointerEvents)).toBe("none");

  // The dragged column dims — header, cells, footer — and only that one. Darkened, never
  // made transparent: the sticky header and footer stay opaque over the scrolled rows.
  const looks = await page.evaluate(() =>
    Array.from(document.querySelectorAll(".pr-tgrid [data-track]")).map((el) => ({
      track: el.getAttribute("data-track"),
      filter: getComputedStyle(el).filter,
      opacity: Number(getComputedStyle(el).opacity),
    })),
  );
  const source = looks.filter((o) => o.track === id);
  expect(source.length, "header, cells and footer").toBeGreaterThan(10);
  for (const o of source) {
    expect(o.filter, "dimmed").toContain("brightness");
    expect(o.opacity, "still opaque").toBe(1);
  }
  for (const o of looks.filter((o) => o.track !== id)) expect(o.filter).toBe("none");

  // The line: in the gap between tracks 3 and 4 (it lands before track 4), top to bottom.
  const line = page.locator(".pr-tgrid-dropline");
  await expect(line).toBeVisible();
  const l = (await line.boundingBox())!;
  expect(l.x).toBeGreaterThanOrEqual(h2.x + h2.width - 0.5);
  expect(l.x + l.width).toBeLessThanOrEqual(h3.x + 0.5);
  const foot = (await page.locator(".pr-tfoot").first().boundingBox())!;
  expect(l.y).toBeLessThanOrEqual(h3.y + 0.5);
  expect(l.y + l.height).toBeGreaterThanOrEqual(foot.y + foot.height - 0.5);

  // Release: everything goes away and the track moves before track 4.
  await clearCalls(page);
  await page.mouse.up();
  await expect(ghost).toHaveCount(0);
  await expect(line).toHaveCount(0);
  expect(await heads.nth(0).evaluate((el) => getComputedStyle(el).filter)).toBe("none");
  const before = await heads.nth(3).getAttribute("data-track");
  expect(await calls(page, "main_move_track")).toEqual([{ track: id, before }]);
});

test("the layout lock indicator is an icon from icons.tsx, not an emoji", async ({ page }) => {
  await openDemo(page, { density: "normal", layoutLocked: true });
  const ind = page.locator(".pr-lock-ind");
  await expect(ind.locator("svg")).toHaveCount(1);
  expect((await ind.textContent())?.trim()).toBe("Layout bloqueado");
});
