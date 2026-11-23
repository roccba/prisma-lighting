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
