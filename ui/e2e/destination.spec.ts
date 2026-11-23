// Groups as a track destination (LIVE-MODEL-SPEC §8.5 step 2): the track dialog offers the
// patch's groups next to the zones; a track can reach groups only, zones only, or both;
// at least one of either stays checked; "Aplicar destino" sends both and the header
// shows the new destination.
import { expect, test } from "@playwright/test";
import { calls, clearCalls, openDemo } from "./helpers";

test("a track's destination takes groups next to zones", async ({ page }) => {
  await openDemo(page, { density: "normal" });
  const head = page.locator(".pr-thead[data-track]").first();
  const trackId = await head.getAttribute("data-track");
  await head.dblclick();
  const dialog = page.getByRole("dialog", { name: /Ajustes del track/ });
  await expect(dialog).toBeVisible();
  const groups = dialog.locator(".pr-trackdlg-groups .pr-trackdlg-zone");
  await expect(groups).toHaveText([/Tubos pares$/, /Tubos impares$/, /Bordes de cortinas$/]);
  const apply = dialog.getByRole("button", { name: "Aplicar destino" });
  await expect(apply).toBeDisabled();

  // Add a group, then drop the zone: the group alone is a destination.
  await groups.filter({ hasText: "Tubos pares" }).locator(".pr-chk").click();
  const zone = dialog.locator(".pr-trackdlg-zones:not(.pr-trackdlg-groups) .pr-trackdlg-zone").first();
  await zone.locator(".pr-chk").click();
  // The only one left cannot be unchecked.
  await expect(groups.filter({ hasText: "Tubos pares" }).locator(".pr-chk")).toHaveAttribute("aria-disabled", "true");
  await clearCalls(page);
  await apply.click();
  const [sent] = (await calls(page, "main_set_track_destination")) as { track: string; zones: string[]; groups: string[] }[];
  expect(sent.track).toBe(trackId);
  expect(sent.zones).toEqual([]);
  expect(sent.groups).toEqual(["00000000-0000-0000-0000-0000de300201"]);
  await page.keyboard.press("Escape");
  await expect(head.locator(".pr-thead-sub").first()).toHaveText("→ Tubos pares");
});

test("with the layout locked the groups cannot be changed either", async ({ page }) => {
  await openDemo(page, { density: "normal", layoutLocked: true });
  await page.locator(".pr-thead[data-track]").first().dblclick();
  const dialog = page.getByRole("dialog", { name: /Ajustes del track/ });
  for (const g of await dialog.locator(".pr-trackdlg-groups .pr-chk").all()) {
    await expect(g).toHaveAttribute("aria-disabled", "true");
  }
});
