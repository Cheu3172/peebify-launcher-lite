// ------------ Hotkey Helpers ------------
// Turns a key press into a shortcut string like "Ctrl+Shift+P" for the shortcut pickers, and rejects
// combinations that can't be used as a global shortcut.
import type { KeyboardEvent } from "react";

export const MODIFIER_KEYS: ReadonlySet<string> = new Set([
  "Control",
  "Alt",
  "AltGraph",
  "Shift",
  "Meta",
  "OS",
  "CapsLock",
  "NumLock",
  "ScrollLock",
]);
const NAMED_CODES = new Set([
  "Space",
  "Tab",
  "Insert",
  "Delete",
  "Home",
  "End",
  "PageUp",
  "PageDown",
  "ArrowUp",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "Enter",
  "Backspace",
]);

function functionKey(text: string): string | null {
  const match = /^F(\d{1,2})$/.exec(text);
  if (!match) return null;
  const n = Number(match[1]);
  return n >= 1 && n <= 24 ? `F${n}` : null;
}

function keyName(e: KeyboardEvent): string | null {
  if (/^[a-z]$/i.test(e.key)) return e.key.toUpperCase();
  const fn = functionKey(e.code) ?? functionKey(e.key);
  if (fn) return fn;
  const letter = /^Key([A-Z])$/.exec(e.code);
  if (letter) return letter[1];
  const digit = /^Digit(\d)$/.exec(e.code);
  if (digit) return digit[1];
  if (NAMED_CODES.has(e.code)) return e.code;
  return null;
}

export function acceleratorOf(e: KeyboardEvent): string | null {
  if (MODIFIER_KEYS.has(e.key)) return null;

  const key = keyName(e);
  if (!key) return null;

  const parts: string[] = [];
  if (e.ctrlKey) parts.push("Ctrl");
  if (e.altKey) parts.push("Alt");
  if (e.shiftKey) parts.push("Shift");
  if (e.metaKey) parts.push("Win");

  const fn = functionKey(key) !== null;
  if (!parts.length && !fn) return null;
  parts.push(key);
  return parts.join("+");
}

export function captureHint(e: KeyboardEvent): string | null {
  if (MODIFIER_KEYS.has(e.key)) return null;
  return e.ctrlKey || e.altKey || e.shiftKey || e.metaKey
    ? "That key cannot be used in a shortcut. Try a letter, digit, function or navigation key."
    : "Add Ctrl, Alt or Shift, or use a function key.";
}

export function spacedAccelerator(accelerator: string): string {
  return accelerator
    .split("+")
    .map((part) => part.trim())
    .filter(Boolean)
    .join(" + ");
}
