// ------------ Notification Panel ------------
// The panel that drops down from the bell in the title bar, listing recent notifications with relative times.
// Individual ones can be dismissed or all of them cleared.
import { useRef, useState, type ReactNode } from "react";
import { Trash2, X } from "lucide-react";
import { useNotificationStore } from "../../store/notificationStore";
import { NOTIF_ICONS, NOTIF_ICON_COLORS } from "../../lib/notifIcons";
import { useNow } from "../../lib/useRelativeTime";
import { fmtRelTime } from "../../lib/format";
import { useClickOutside, useEscapeKey } from "../../lib/useClickOutside";
import { notificationBellRef } from "./WindowControls";
import { TooltipPortal, useAnchoredTip } from "../ui/Tooltip";

function RelTime({ ts }: { ts: number }) {
  const now = useNow();
  return <>{fmtRelTime(ts, now)}</>;
}

function IconButton({
  label,
  disabled,
  className,
  onClick,
  children,
}: {
  label: string;
  disabled?: boolean;
  className: string;
  onClick: () => void;
  children: ReactNode;
}) {
  const tip = useAnchoredTip<HTMLButtonElement>("bottom");
  return (
    <button
      ref={tip.anchorRef}
      {...tip.bind}
      onClick={onClick}
      aria-label={label}
      disabled={disabled}
      className={className}
    >
      {children}
      {tip.shown && !disabled && (
        <TooltipPortal x={tip.pos.x} y={tip.pos.y} placement="bottom">
          {label}
        </TooltipPortal>
      )}
    </button>
  );
}

export function NotificationPanel() {
  const items = useNotificationStore((s) => s.items);
  const open = useNotificationStore((s) => s.open);
  const dismiss = useNotificationStore((s) => s.dismiss);
  const clearAll = useNotificationStore((s) => s.clearAll);
  const setOpen = useNotificationStore((s) => s.setOpen);

  const ref = useRef<HTMLElement>(null);
  const [listMounted, setListMounted] = useState(open);
  if (open && !listMounted) setListMounted(true);

  useClickOutside(ref, () => setOpen(false), open, (t) => !!t.closest("[data-window-controls]"));
  function close() {
    const restore = !!ref.current?.contains(document.activeElement);
    setOpen(false);
    if (restore) notificationBellRef.current?.focus();
  }

  useEscapeKey(close, open);

  return (
    <aside
      ref={ref}
      className="glass absolute bottom-[8px] right-[8px] top-[52px] z-(--z-panel) flex w-[360px] flex-col rounded-ui shadow-2xl transition-transform duration-300"
      style={{ transform: open ? "translateX(0)" : "translateX(calc(100% + 16px))" }}
      aria-label="Notifications"
      aria-hidden={!open}
      inert={!open}
      onTransitionEnd={(e) => {
        if (!open && e.target === e.currentTarget && e.propertyName === "transform") setListMounted(false);
      }}
    >
      <header className="flex items-center justify-between gap-3 border-b border-white/[0.08] px-[18px] py-[14px]">
        <h3 className="text-[15px] font-semibold">Notifications</h3>
        <div className="flex items-center gap-1">
          <IconButton
            label="Clear all notifications"
            onClick={clearAll}
            disabled={items.length === 0}
            className="grid h-[28px] w-[28px] place-items-center rounded-md text-white/55 transition-colors hover:bg-white/10 hover:text-white disabled:opacity-40 disabled:hover:bg-transparent disabled:hover:text-white/55"
          >
            <Trash2 size={14} />
          </IconButton>
          <IconButton
            label="Close"
            onClick={close}
            className="grid h-[28px] w-[28px] place-items-center rounded-md text-white/55 transition-colors hover:bg-white/10 hover:text-white"
          >
            <X size={15} />
          </IconButton>
        </div>
      </header>

      <div className="flex-1 overflow-y-auto overscroll-contain px-3 py-3">
        {listMounted && (
          <>
            {items.length === 0 && (
              <div className="px-5 py-16 text-center text-[13px] text-white/45">
                No notifications yet.
              </div>
            )}

            {items.map((n) => {
              const Icon = NOTIF_ICONS[n.type];
              const c = NOTIF_ICON_COLORS[n.type];
              return (
                <div
                  key={n.id}
                  className="mb-[6px] flex gap-[10px] rounded-ui border px-[10px] py-[10px]"
                  style={{
                    background: n.read ? "rgba(255,255,255,.03)" : "rgba(var(--accent-a-rgb), .07)",
                    borderColor: n.read ? "rgba(255,255,255,.05)" : "rgba(var(--accent-a-rgb), .18)",
                  }}
                >
                  <span
                    className="mt-[1px] grid h-[26px] w-[26px] shrink-0 place-items-center rounded-md"
                    style={{ background: c.bg, color: c.fg }}
                  >
                    <Icon size={14} />
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="text-[13px] font-medium leading-tight">{n.title}</div>
                    <div className="mt-[2px] select-text break-words text-[12px] leading-snug text-white/65">
                      {n.text}
                    </div>
                    {n.action && (
                      <button
                        onClick={() => {
                          setOpen(false);
                          n.action?.run();
                        }}
                        className="mt-[7px] rounded-md bg-white/10 px-[10px] py-[4px] text-[12px] font-medium text-white transition-colors hover:bg-white/[0.16]"
                      >
                        {n.action.label}
                      </button>
                    )}
                    <div className="mt-[4px] text-[11px] text-white/55"><RelTime ts={n.time} /></div>
                  </div>
                  <IconButton
                    label="Dismiss"
                    onClick={() => dismiss(n.id)}
                    className="grid h-[22px] w-[22px] shrink-0 place-items-center self-start rounded-md text-white/45 transition-colors hover:bg-white/10 hover:text-white"
                  >
                    <X size={12} />
                  </IconButton>
                </div>
              );
            })}
          </>
        )}
      </div>
    </aside>
  );
}
