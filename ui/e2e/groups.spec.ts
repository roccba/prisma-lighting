// Groups in the patch editor (LIVE-MODEL-SPEC §8.5 step 1): created from the selection in
// selection order, renamed in place, members added / removed / reordered from the
// selection, deleted — every edit sent as one undoable command.
import { expect, test } from "@playwright/test";
import { calls, clearCalls, layoutProblems, openEditor, VIEWPORTS } from "./helpers";

// The demo rig already has three groups (Tubos pares, Tubos impares, Bordes de cortinas);
// the tests add theirs after them.
const DEMO_GROUPS = 3;

test("a group is made from the selection, in the order it was selected", async ({ page }) => {
  await openEditor(page);
  const fixtures = page.locator(".fixture-list .pr-listrow");
  const ids = await fixtures.evaluateAll((rows) => rows.map((r) => r.textContent ?? ""));
  expect(ids.length).toBeGreaterThan(3);

  // Select the 1st, the 4th, then the 2nd fixture.
  await fixtures.nth(0).click();
  await fixtures.nth(3).click({ modifiers: ["Control"] });
  await fixtures.nth(1).click({ modifiers: ["Control"] });
  await clearCalls(page);
  await page.getByRole("button", { name: "Crear grupo" }).click();
  const [created] = (await calls(page, "editor_add_group")) as { members: string[] }[];
  expect(created.members).toHaveLength(3);

  // The new group opens a name field; Enter renames it.
  const name = page.getByRole("textbox", { name: "Nombre del grupo" });
  await expect(name).toBeFocused();
  await name.fill("Pares");
  await name.press("Enter");
  expect(await calls(page, "editor_rename_group")).toEqual([
    { group: expect.any(String), name: "Pares" },
  ]);
  await expect(page.locator(".group-list .pr-listrow .nm")).toHaveCount(DEMO_GROUPS + 1);
  await expect(page.locator(".group-list .pr-listrow .nm").last()).toHaveText("Pares");

  // Members, numbered, in selection order: what was 1st, 4th, 2nd in the zone's list.
  const members = page.locator(".group-members .pr-listrow .nm");
  const names = await fixtures.evaluateAll((rows) => rows.map((r) => r.querySelector(".nm")?.textContent));
  await expect(members).toHaveText([names[0]!, names[3]!, names[1]!]);
  await expect(page.locator(".group-members .group-pos")).toHaveText(["1", "2", "3"]);
});

test("members are reordered, added and removed from the selection; the group is deleted", async ({ page }) => {
  await openEditor(page);
  const fixtures = page.locator(".fixture-list .pr-listrow");
  const names = await fixtures.evaluateAll((rows) => rows.map((r) => r.querySelector(".nm")?.textContent ?? ""));
  await fixtures.nth(0).click();
  await fixtures.nth(1).click({ modifiers: ["Control"] });
  await fixtures.nth(2).click({ modifiers: ["Control"] });
  await page.getByRole("button", { name: "Crear grupo" }).click();
  await page.getByRole("textbox", { name: "Nombre del grupo" }).press("Escape");
  const members = page.locator(".group-members .pr-listrow .nm");
  await expect(members).toHaveText(names.slice(0, 3));

  // Down: the selected first member moves after the second.
  await page.locator(".group-members .pr-listrow").nth(0).click();
  await clearCalls(page);
  await page.getByRole("button", { name: "Bajar" }).click();
  await expect(members).toHaveText([names[1], names[0], names[2]]);
  expect(await calls(page, "editor_set_group_members")).toHaveLength(1);
  // Up at the top does nothing (no command).
  await page.getByRole("button", { name: "Subir" }).click();
  await page.getByRole("button", { name: "Subir" }).click();
  await expect(members).toHaveText([names[0], names[1], names[2]]);
  expect(await calls(page, "editor_set_group_members")).toHaveLength(2);

  // − Selección removes the selected member; + Selección appends what is not in it.
  await page.getByRole("button", { name: "Selección" }).nth(1).click();
  await expect(members).toHaveText([names[1], names[2]]);
  await fixtures.nth(4).click();
  await fixtures.nth(1).click({ modifiers: ["Control"] });
  await page.getByRole("button", { name: "Selección" }).nth(0).click();
  await expect(members).toHaveText([names[1], names[2], names[4]]);

  // Picking the group selects its fixtures (in the zone's list too).
  await page.locator(".group-list .pr-listrow").last().click();
  await expect(page.locator(".fixture-list .pr-listrow.is-sel")).toHaveCount(3);

  await clearCalls(page);
  await page.getByRole("button", { name: "Eliminar grupo" }).click();
  expect(await calls(page, "editor_remove_group")).toHaveLength(1);
  await expect(page.locator(".group-list .pr-listrow")).toHaveCount(DEMO_GROUPS);
  await expect(page.locator(".group-members")).toHaveCount(0);
});

for (const viewport of VIEWPORTS) {
  test(`the groups sector fits at ${viewport.width}×${viewport.height}`, async ({ page }) => {
    await openEditor(page, viewport);
    const fixtures = page.locator(".fixture-list .pr-listrow");
    for (let i = 0; i < 6; i++) await fixtures.nth(i).click({ modifiers: i ? ["Control"] : [] });
    await page.getByRole("button", { name: "Crear grupo" }).click();
    await page.getByRole("textbox", { name: "Nombre del grupo" }).press("Enter");
    expect(
      await layoutProblems(page, {
        head: ".groups-sector .sector-head",
        list: ".groups-sector .group-list",
        actions: ".group-member-actions",
      }),
    ).toEqual([]);
    // The whole sector is on screen, members included.
    const sector = (await page.locator(".groups-sector").boundingBox())!;
    expect(sector.y + sector.height).toBeLessThanOrEqual(viewport.height + 0.5);
    await expect(page.locator(".group-members .pr-listrow").first()).toBeInViewport();
  });
}

test("each new group gets its own colour; the app's colour selector changes it", async ({ page }) => {
  await openEditor(page);
  const fixtures = page.locator(".fixture-list .pr-listrow");
  for (const i of [0, 1]) {
    await fixtures.nth(i).click();
    await page.getByRole("button", { name: "Crear grupo" }).click();
    await page.getByRole("textbox", { name: "Nombre del grupo" }).press("Enter");
  }
  const swatches = page.locator(".group-list .group-swatch");
  const colours = await swatches.evaluateAll((s) => s.map((e) => getComputedStyle(e).backgroundColor));
  expect(colours).toHaveLength(DEMO_GROUPS + 2);
  expect(new Set(colours).size, "no two groups share a colour").toBe(colours.length);

  // The swatch opens the app's selector (not the system one); a quick colour picks and
  // closes, stored as one edit.
  await clearCalls(page);
  await swatches.nth(DEMO_GROUPS + 1).click();
  const picker = page.getByRole("dialog", { name: "Selector de color" });
  await expect(picker).toBeVisible();
  await expect(page.locator("input[type=color]")).toHaveCount(0);
  await picker.locator('[data-cpick="rgb"][data-rgb="255,0,0"]').first().click();
  await expect(picker).toHaveCount(0);
  expect(await calls(page, "editor_set_group_color")).toEqual([{ group: expect.any(String), color: [255, 0, 0] }]);
  await expect(swatches.nth(DEMO_GROUPS + 1)).toHaveCSS("background-color", "rgb(255, 0, 0)");
  // Opening the selector does not pick the group (the row click is not triggered).
  await expect(page.locator(".group-list .pr-listrow.is-sel")).toHaveCount(1);
});
