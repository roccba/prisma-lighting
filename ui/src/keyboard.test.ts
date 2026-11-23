// The window's half of the keyboard rule (LIVE-MODEL-SPEC §8.2): a key pressed in a text or
// numeric field is never offered to the shortcuts — including a field that blurs itself on
// Enter (§10.2) — nor while learn mode or a dialog / menu owns the keyboard.
import { describe, expect, it } from "vitest";
import { focusKind, shortcutFor } from "./keyboard";
import type { KeyBinding } from "./api";

const KEYS: KeyBinding[] = [
  { combo: { key: "Space", ctrl: false, shift: false, alt: false }, path: "/arm/go" },
  { combo: { key: "Enter", ctrl: false, shift: false, alt: false }, path: "/select/launch" },
  { combo: { key: "1", ctrl: false, shift: false, alt: false }, path: "/scene/0/launch" },
];

function element(html: string): HTMLElement {
  const host = document.createElement("div");
  host.innerHTML = html;
  document.body.appendChild(host);
  return host.firstElementChild as HTMLElement;
}

function press(target: EventTarget, init: KeyboardEventInit): KeyboardEvent {
  const e = new KeyboardEvent("keydown", { bubbles: true, ...init });
  Object.defineProperty(e, "target", { value: target });
  return e;
}

describe("focusKind", () => {
  it("tells text, numeric and other elements apart", () => {
    expect(focusKind(element("<input>"))).toBe("text");
    expect(focusKind(element('<input type="search">'))).toBe("text");
    expect(focusKind(element("<textarea></textarea>"))).toBe("text");
    expect(focusKind(element("<select></select>"))).toBe("text");
    expect(focusKind(element('<input type="number">'))).toBe("number");
    expect(focusKind(element('<input inputmode="numeric">'))).toBe("number");
    expect(focusKind(element('<input inputmode="decimal">'))).toBe("number");
    expect(focusKind(element('<div role="spinbutton"></div>'))).toBe("number");
    expect(focusKind(element("<button>x</button>"))).toBe("other");
    expect(focusKind(element('<input type="checkbox">'))).toBe("other");
    expect(focusKind(document.body)).toBe("other");
    expect(focusKind(null)).toBe("other");
  });
});

describe("shortcutFor", () => {
  it("offers a bound key pressed outside any field", () => {
    expect(shortcutFor(press(document.body, { key: " ", code: "Space" }), KEYS, false)).toEqual({
      combo: { key: "Space", ctrl: false, shift: false, alt: false },
      focus: "other",
    });
    expect(shortcutFor(press(element("<button>GO</button>"), { key: "1", code: "Digit1" }), KEYS, false)?.combo.key).toBe("1");
  });

  it("never fires in a text or numeric field", () => {
    for (const html of ["<input>", "<textarea></textarea>", '<input type="number">', '<input inputmode="numeric">']) {
      expect(shortcutFor(press(element(html), { key: " ", code: "Space" }), KEYS, false), html).toBeNull();
      expect(shortcutFor(press(element(html), { key: "1", code: "Digit1" }), KEYS, false), html).toBeNull();
    }
  });

  it("judges the field the key was pressed in, even after it blurred itself", () => {
    const box = element('<input inputmode="numeric" value="10">');
    box.focus();
    box.blur(); // the R / G / B box on Enter: the focus is on the body now
    expect(document.activeElement).toBe(document.body);
    expect(shortcutFor(press(box, { key: "Enter", code: "Enter" }), KEYS, false)).toBeNull();
  });

  it("stays out while blocked, for repeats, lone modifiers, menu keys and unbound keys", () => {
    const body = document.body;
    expect(shortcutFor(press(body, { key: " ", code: "Space" }), KEYS, true), "learn / dialog").toBeNull();
    expect(shortcutFor(press(body, { key: " ", code: "Space", repeat: true }), KEYS, false), "repeat").toBeNull();
    expect(shortcutFor(press(body, { key: "Shift", code: "ShiftLeft", shiftKey: true }), KEYS, false)).toBeNull();
    expect(shortcutFor(press(body, { key: "F3", code: "F3" }), [{ combo: { key: "F3", ctrl: false, shift: false, alt: false }, path: "/x" }], false), "menu key").toBeNull();
    expect(shortcutFor(press(body, { key: "q", code: "KeyQ" }), KEYS, false), "unbound").toBeNull();
  });
});
