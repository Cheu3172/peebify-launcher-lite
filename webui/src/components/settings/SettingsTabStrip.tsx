// ------------ Settings Tab Strip ------------
// The row of tabs at the top of Settings. Works with the mouse and with the arrow, Home and End keys.
import { useRef, type KeyboardEvent, type ReactNode } from "react";
import { m } from "framer-motion";
import { springThumb } from "../../lib/motion";

export interface TabItem {
  id: string;
  label: string;
  icon: ReactNode;
}

export const SETTINGS_PANEL_ID = "settings-panel";
export const settingsTabId = (id: string): string => `settings-tab-${id}`;

export function SettingsTabStrip({
  items,
  active,
  onSelect,
}: {
  items: TabItem[];
  active: string;
  onSelect: (id: string) => void;
}) {
  const refs = useRef<(HTMLButtonElement | null)[]>([]);

  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const last = items.length - 1;
    let next: number;
    switch (e.key) {
      case "ArrowRight":
        next = index === last ? 0 : index + 1;
        break;
      case "ArrowLeft":
        next = index === 0 ? last : index - 1;
        break;
      case "Home":
        next = 0;
        break;
      case "End":
        next = last;
        break;
      default:
        return;
    }
    e.preventDefault();
    onSelect(items[next].id);
    refs.current[next]?.focus();
  };

  return (
    <div
      role="tablist"
      aria-orientation="horizontal"
      className="flex items-center gap-1 border-b border-white/[0.08]"
    >
      {items.map((it, index) => {
        const on = it.id === active;
        return (
          <button
            key={it.id}
            ref={(el) => {
              refs.current[index] = el;
            }}
            type="button"
            role="tab"
            id={settingsTabId(it.id)}
            aria-controls={SETTINGS_PANEL_ID}
            aria-selected={on}
            tabIndex={on ? 0 : -1}
            onClick={() => onSelect(it.id)}
            onKeyDown={(e) => onKeyDown(e, index)}
            className={`relative flex items-center gap-[7px] whitespace-nowrap px-[14px] pb-[11px] pt-[9px] text-[13px] transition-colors duration-150 ${
              on ? "font-semibold text-white" : "font-medium text-white/60 hover:text-white/80"
            }`}
          >
            {it.icon}
            {it.label}
            {on && (
              <m.span
                layoutId="settings-tab-underline"
                className="absolute inset-x-0 -bottom-px h-[2px] bg-white"
                transition={springThumb}
              />
            )}
          </button>
        );
      })}
    </div>
  );
}
