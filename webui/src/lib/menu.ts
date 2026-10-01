// ------------ Menu Keyboard Navigation ------------
// Arrow key, Home and End handling so pop-out menus can be used without a mouse.
import type { KeyboardEvent } from "react";

const ITEM_SELECTOR = '[role="menuitem"]:not(:disabled)';

export function focusFirstMenuItem(menu: HTMLElement | null): void {
  menu?.querySelector<HTMLElement>(ITEM_SELECTOR)?.focus({ preventScroll: true });
}

export function handleMenuArrowKeys(e: KeyboardEvent<HTMLElement>): void {
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp" && e.key !== "Home" && e.key !== "End") return;
  const entries = Array.from(e.currentTarget.querySelectorAll<HTMLElement>(ITEM_SELECTOR));
  if (entries.length === 0) return;
  e.preventDefault();
  const current = entries.indexOf(document.activeElement as HTMLElement);
  let next: number;
  if (e.key === "Home") next = 0;
  else if (e.key === "End") next = entries.length - 1;
  else if (e.key === "ArrowDown") next = current < 0 ? 0 : (current + 1) % entries.length;
  else next = current < 0 ? entries.length - 1 : (current - 1 + entries.length) % entries.length;
  entries[next].focus({ preventScroll: true });
}
