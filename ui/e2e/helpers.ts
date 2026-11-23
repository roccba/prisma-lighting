// Shared steps for the UI tests (LIVE-MODEL-SPEC §10.1): open the demo show in the
// harness at a density / window size, enter MIDI learn, and measure boxes in the page.
import { expect, type Page } from "@playwright/test";
import type { Density } from "../src/density";

export const DENSITIES: Density[] = ["compact", "normal", "large"];
export const VIEWPORTS = [
  { width: 1920, height: 1200 },
  { width: 1280, height: 800 },
];

export interface OpenOptions {
  density: Density;
  viewport?: { width: number; height: number };
  panelHidden?: boolean;
  layoutLocked?: boolean;
}

/** Open the demo show in the harness. Density and panel state are the per-machine
 *  settings the window reads from localStorage at start. */
export async function openDemo(page: Page, o: OpenOptions) {
  await page.setViewportSize(o.viewport ?? VIEWPORTS[0]);
  // Seeds the settings once per page; `relayout` changes them later (a reload keeps them).
  await page.addInitScript(
    ([density, hidden, locked]) => {
      if (sessionStorage.getItem("prisma-test-seeded")) return;
      sessionStorage.setItem("prisma-test-seeded", "1");
      localStorage.setItem("prisma-grid-density", density);
      localStorage.setItem("prisma-panel-hidden", hidden ? "1" : "0");
      localStorage.setItem("prisma-layout-locked", locked ? "1" : "0");
    },
    [o.density, o.panelHidden ?? false, o.layoutLocked ?? false] as const,
  );
  await page.goto("/e2e/harness.html");
  await expect(page.locator(".pr-cell").first()).toBeVisible();
  await expect(page.locator(".pr-zonecard").first()).toBeVisible();
  // Let the layout settle (the grid measures its rows after the first paint).
  await page.waitForTimeout(300);
}

/** Change the per-machine layout settings and reload the window (same page: faster than
 *  a new one, and one browser page per test). */
export async function relayout(page: Page, o: { density: Density; panelHidden?: boolean }) {
  await page.evaluate(
    ([density, hidden]) => {
      localStorage.setItem("prisma-grid-density", density);
      localStorage.setItem("prisma-panel-hidden", hidden ? "1" : "0");
    },
    [o.density, o.panelHidden ?? false] as const,
  );
  await page.reload();
  await expect(page.locator(".pr-cell").first()).toBeVisible();
  await page.waitForTimeout(300);
}

/** The IPC commands the UI sent since the last `clearCalls`, by name. */
export async function calls(page: Page, cmd: string) {
  return page.evaluate((cmd) => window.__calls.filter((c) => c.cmd === cmd).map((c) => c.args), cmd);
}

export async function clearCalls(page: Page) {
  await page.evaluate(() => (window.__calls.length = 0));
}

/** Press at `from`, move by `dx` in a few steps, release. */
export async function drag(page: Page, from: { x: number; y: number }, dx: number) {
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + dx, from.y, { steps: 8 });
  await page.mouse.up();
}

/** MIDI learn on (Ctrl+M, as the operator does). */
export async function enterLearn(page: Page) {
  await page.keyboard.press("Control+m");
  await expect(page.locator(".pr-learn-ind")).toBeVisible();
  // Learn mode re-reads the mappings asynchronously: wait for their labels to be drawn, or
  // a screenshot can catch some controls before and some after (a flaky reference).
  await expect(page.locator("[data-learn-badge]").first()).toBeAttached();
  await page.waitForTimeout(150);
}

export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Problem {
  region: string;
  what: string;
}

/**
 * Within each region (a CSS selector, one or more elements), every visible control or
 * text box must lie inside the region's box and no two of them may overlap (an element
 * and its own ancestor are not an overlap). Returns the problems found, described so a
 * failure says what and where.
 */
export async function layoutProblems(page: Page, regions: Record<string, string>): Promise<Problem[]> {
  return page.evaluate((regions) => {
    const EPS = 0.5;
    const LEAVES = [
      "button",
      "input",
      "select",
      ".pr-fader",
      ".pr-hslider",
      ".pr-chk",
      ".pr-dropdown",
      ".pr-num",
      "canvas",
      ".pr-cell-num",
      ".pr-cell-title",
      ".pr-cell-effect",
      ".pr-thead-name",
      ".pr-thead-sub",
      "[class*='-value']",
      "[class*='-label']",
      "[class*='faderval']",
    ].join(",");
    // Parts that live inside a composite control are not separate leaves.
    const INSIDE_CONTROL = ".pr-fader *, .pr-hslider *, .pr-dropdown *, .pr-num *, .pr-chk *, button *";
    const visible = (el: Element) => {
      const r = el.getBoundingClientRect();
      const cs = getComputedStyle(el);
      return r.width > 0 && r.height > 0 && cs.visibility !== "hidden" && cs.display !== "none" && Number(cs.opacity) > 0;
    };
    const describe = (el: Element) => {
      const c = (el.getAttribute("class") ?? "").split(/\s+/).filter(Boolean).slice(0, 2).join(".");
      const t = (el.textContent ?? "").trim().slice(0, 24);
      return `${el.tagName.toLowerCase()}${c ? "." + c : ""}${t ? ` "${t}"` : ""}`;
    };
    const out: { region: string; what: string }[] = [];
    for (const [name, sel] of Object.entries(regions)) {
      document.querySelectorAll(sel).forEach((region, ri) => {
        if (!visible(region)) return;
        const rb = region.getBoundingClientRect();
        // A region that scrolls vertically holds rows above / below its box on purpose:
        // there only the horizontal extent is checked.
        const oy = getComputedStyle(region).overflowY;
        const scrolls = (oy === "auto" || oy === "scroll") && region.scrollHeight > region.clientHeight;
        const leaves = Array.from(region.querySelectorAll(LEAVES)).filter((el) => visible(el) && !el.matches(INSIDE_CONTROL));
        for (const el of leaves) {
          const b = el.getBoundingClientRect();
          const outY = !scrolls && (b.top < rb.top - EPS || b.bottom > rb.bottom + EPS);
          if (b.left < rb.left - EPS || b.right > rb.right + EPS || outY) {
            out.push({ region: `${name}[${ri}]`, what: `${describe(el)} sticks out of the region` });
          }
          // Text cut without an ellipsis.
          const cs = getComputedStyle(el);
          const html = el as HTMLElement;
          if (html.scrollWidth > html.clientWidth + 1 && cs.textOverflow !== "ellipsis" && el.children.length === 0) {
            out.push({ region: `${name}[${ri}]`, what: `${describe(el)} text cut (no ellipsis)` });
          }
          if (html.scrollHeight > html.clientHeight + 1 && el.children.length === 0 && cs.overflowY !== "visible") {
            out.push({ region: `${name}[${ri}]`, what: `${describe(el)} text cut vertically` });
          }
        }
        for (let i = 0; i < leaves.length; i++) {
          for (let j = i + 1; j < leaves.length; j++) {
            const a = leaves[i];
            const c = leaves[j];
            if (a.contains(c) || c.contains(a)) continue;
            const p = a.getBoundingClientRect();
            const q = c.getBoundingClientRect();
            const w = Math.min(p.right, q.right) - Math.max(p.left, q.left);
            const h = Math.min(p.bottom, q.bottom) - Math.max(p.top, q.top);
            // Line boxes of adjacent text touch by a pixel or two without the glyphs meeting.
            if (w > 2 && h > 2) out.push({ region: `${name}[${ri}]`, what: `${describe(a)} overlaps ${describe(c)} by ${w.toFixed(1)}×${h.toFixed(1)}` });
          }
        }
      });
    }
    return out;
  }, regions);
}

export async function box(page: Page, selector: string): Promise<Box> {
  const b = await page.locator(selector).first().boundingBox();
  if (!b) throw new Error(`${selector} has no box`);
  return { x: b.x, y: b.y, w: b.width, h: b.height };
}

export interface LearnFrame {
  name: string;
  frame: Box | null;
  element: Box;
}

/**
 * Every learnable control's learn frame next to the control's own box. A learnable is an
 * element with `data-learn` (the frame is drawn on the element itself), or — the old
 * scheme — a `.pr-learn-host` wrapper whose overlay is the frame and whose first child is
 * the control.
 */
export async function learnFrames(page: Page): Promise<LearnFrame[]> {
  return page.evaluate(() => {
    const rect = (el: Element) => {
      const r = el.getBoundingClientRect();
      return { x: r.x, y: r.y, w: r.width, h: r.height };
    };
    const out: { name: string; frame: { x: number; y: number; w: number; h: number } | null; element: { x: number; y: number; w: number; h: number } }[] = [];
    document.querySelectorAll("[data-learn]").forEach((el) => {
      const cs = getComputedStyle(el);
      const drawn = cs.outlineStyle !== "none" && parseFloat(cs.outlineWidth) > 0;
      const inside = parseFloat(cs.outlineOffset) <= -parseFloat(cs.outlineWidth) + 0.01;
      out.push({ name: el.getAttribute("data-learn") ?? "?", frame: drawn && inside ? rect(el) : null, element: rect(el) });
    });
    document.querySelectorAll(".pr-learn-host").forEach((host) => {
      const control = Array.from(host.children).find((c) => !c.classList.contains("pr-learn-overlay"));
      const overlay = host.querySelector(":scope > .pr-learn-overlay");
      if (!control) return;
      out.push({ name: overlay?.getAttribute("title") ?? "?", frame: overlay ? rect(overlay) : null, element: rect(control) });
    });
    return out;
  });
}

/** Open the patch editor on the demo rig (the harness's `?window=editor`). */
export async function openEditor(page: Page, viewport = VIEWPORTS[0]) {
  await page.setViewportSize(viewport);
  await page.goto("/e2e/harness.html?window=editor");
  await expect(page.locator(".groups-sector")).toBeVisible();
  await expect(page.locator(".fixture-list .pr-listrow").first()).toBeVisible();
}
