// Reference screenshots of the whole window (LIVE-MODEL-SPEC §10.1): 3 densities × learn
// off/on × panel shown/hidden at two window sizes. Tagged @screenshot: they run only on
// the reference machine (fonts differ elsewhere); CI runs the geometric tests.
import { expect, test } from "@playwright/test";
import { DENSITIES, enterLearn, openDemo, VIEWPORTS } from "./helpers";

for (const viewport of VIEWPORTS) {
  for (const density of DENSITIES) {
    for (const learn of [false, true]) {
      for (const panelHidden of [false, true]) {
        const name = `${viewport.width}x${viewport.height}-${density}${learn ? "-learn" : ""}${panelHidden ? "-nopanel" : ""}`;
        test(`${name} @screenshot`, async ({ page }) => {
          await openDemo(page, { density, viewport, panelHidden });
          if (learn) await enterLearn(page);
          await expect(page).toHaveScreenshot(`${name}.png`, { animations: "disabled", caret: "hide", timeout: 20_000 });
        });
      }
    }
  }
}

// The track's ⋯ popover and its settings dialog (§10.3 item 1), each on its own.
test("track popover @screenshot", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  await page.locator(".pr-thead[data-track]").nth(3).locator(".pr-thead-more").click();
  const pop = page.locator(".pr-tsettings");
  await expect(pop).toBeVisible();
  await expect(pop).toHaveScreenshot("track-popover.png", { animations: "disabled", caret: "hide" });
});

test("track settings dialog @screenshot", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  await page.locator(".pr-thead[data-track]").nth(3).dblclick();
  const dialog = page.locator(".pr-trackdlg");
  await expect(dialog).toBeVisible();
  await expect(dialog).toHaveScreenshot("track-dialog.png", { animations: "disabled", caret: "hide" });
});
