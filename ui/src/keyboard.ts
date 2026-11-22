import type { FocusKind, KeyBinding, KeyCombo } from "./api";

// Keyboard shortcuts (LIVE-MODEL-SPEC §8.2). The decision — is a shortcut bound to this
// combo, and may it fire with the focus where it is — is made in Rust, in one function
// (`prisma_core::keys::dispatch`, tested there). This file only turns a DOM event into
// what that function takes: the combo and the kind of element that has the focus.

/** The key's name as the shortcuts store it: a letter upper-cased, a main-row digit by its
 *  position (Shift+1 is still "1"), "Space", or the DOM name of a named key. */
export function keyName(e: KeyboardEvent): string {
  if (/^Digit\d$/.test(e.code)) return e.code.slice(5);
  if (e.key === " ") return "Space";
  if (e.key.length === 1) return e.key.toUpperCase();
  return e.key;
}

export function comboOf(e: KeyboardEvent): KeyCombo {
  return { key: keyName(e), ctrl: e.ctrlKey || e.metaKey, shift: e.shiftKey, alt: e.altKey };
}

/** Pure modifier presses are never combos. */
export function isModifierOnly(e: KeyboardEvent): boolean {
  return e.key === "Shift" || e.key === "Control" || e.key === "Alt" || e.key === "Meta" || e.key === "AltGraph";
}

const TEXT_INPUTS = new Set(["text", "search", "email", "url", "tel", "password", ""]);

/** What kind of element has the focus. A numeric field is an `<input type=number>`, a
 *  spin button, or an input marked `inputmode="decimal|numeric"` (the app's NumberField). */
export function focusKind(el: Element | null): FocusKind {
  if (!el || !(el instanceof HTMLElement)) return "other";
  if (el.isContentEditable || el.tagName === "TEXTAREA") return "text";
  if (el.getAttribute("role") === "spinbutton") return "number";
  if (el.tagName === "INPUT") {
    const input = el as HTMLInputElement;
    const mode = input.inputMode;
    if (input.type === "number" || mode === "decimal" || mode === "numeric") return "number";
    if (TEXT_INPUTS.has(input.type)) return "text";
  }
  if (el.tagName === "SELECT") return "text";
  return "other";
}

const sameCombo = (a: KeyCombo, b: KeyCombo) =>
  a.key === b.key && a.ctrl === b.ctrl && a.shift === b.shift && a.alt === b.alt;

/** Whether some shortcut uses `combo` — only to stop the browser's own action for it
 *  (Space scrolling, Enter clicking a focused button) before Rust decides. */
export function isBound(keys: KeyBinding[], combo: KeyCombo): boolean {
  return keys.some((k) => sameCombo(k.combo, combo));
}

/** A combo as the operator reads it: "Ctrl+Retroceso", "Mayús+B", "Espacio". */
export function comboLabel(c: KeyCombo): string {
  const names: Record<string, string> = {
    Space: "Espacio",
    Escape: "Esc",
    Backspace: "Retroceso",
    Enter: "Enter",
    ArrowUp: "↑",
    ArrowDown: "↓",
    ArrowLeft: "←",
    ArrowRight: "→",
    Delete: "Supr",
    Tab: "Tab",
  };
  const parts: string[] = [];
  if (c.ctrl) parts.push("Ctrl");
  if (c.alt) parts.push("Alt");
  if (c.shift) parts.push("Mayús");
  parts.push(names[c.key] ?? c.key);
  return parts.join("+");
}

/** The keys the menu keeps (mirrors `KeyCombo::is_reserved`): offered in the capture
 *  field as an explanation, the backend refuses them either way. */
export function isReserved(c: KeyCombo): boolean {
  const f = ["F3", "F4", "F7", "F8", "F9", "F10", "F11"].includes(c.key);
  const menu = c.ctrl && !c.alt && !c.shift && ["N", "O", "S", "M"].includes(c.key);
  return f || menu;
}
