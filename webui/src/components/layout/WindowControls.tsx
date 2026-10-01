// ------------ Window Controls ------------
// The notification bell plus minimize, maximize and close buttons in the top right corner. The launcher draws
// its own title bar, so these replace the normal Windows ones.
import { createRef } from "react";
import type { ReactNode, RefObject } from "react";
import { Bell, Minus, Square, Copy, X } from "lucide-react";
import { minimizeWindow, closeWindow } from "../../lib/tauri";
import { toggleMaximizeWindow } from "../../lib/ipc";
import { useNotificationStore } from "../../store/notificationStore";
import { useUiStore } from "../../store/uiStore";
import { TooltipPortal, useAnchoredTip } from "../ui/Tooltip";

export const notificationBellRef = createRef<HTMLButtonElement>();

function WinButton({
  label,
  spoken,
  expanded,
  buttonRef,
  onClick,
  children,
}: {
  label: string;
  spoken?: string;
  expanded?: boolean;
  buttonRef?: RefObject<HTMLButtonElement | null>;
  onClick: () => void;
  children: ReactNode;
}) {
  const tip = useAnchoredTip<HTMLButtonElement>("bottom");
  return (
    <button
      ref={(el) => {
        tip.anchorRef.current = el;
        if (buttonRef) buttonRef.current = el;
      }}
      onClick={onClick}
      {...tip.bind}
      aria-label={spoken ?? label}
      aria-expanded={expanded}
      className="relative grid h-[30px] w-[30px] place-items-center rounded-[7px] transition-colors hover:bg-white/10"
    >
      {children}
      {tip.shown && (
        <TooltipPortal x={tip.pos.x} y={tip.pos.y} placement="bottom">
          {label}
        </TooltipPortal>
      )}
    </button>
  );
}

export function WindowControls({ hideBell = false }: { hideBell?: boolean }) {
  const togglePanel = useNotificationStore((s) => s.togglePanel);
  const panelOpen = useNotificationStore((s) => s.open);
  const unread = useNotificationStore((s) => s.items.filter((i) => !i.read).length);
  const maximized = useUiStore((s) => s.maximized);
  const setMaximized = useUiStore((s) => s.setMaximized);

  return (
    <div
      data-window-controls
      className="glass absolute right-[14px] top-[12px] z-(--z-controls) flex items-center rounded-ui px-[6px] py-[2px]"
    >
      {!hideBell && (
        <WinButton
          label="Notifications"
          spoken={unread > 0 ? `Notifications, ${unread} unread` : undefined}
          expanded={panelOpen}
          buttonRef={notificationBellRef}
          onClick={togglePanel}
        >
          <Bell size={15} />
          {unread > 0 && (
            <span className="accent-grad absolute right-[1px] top-[1px] grid h-[15px] min-w-[15px] place-items-center rounded-full border-[1.5px] border-[rgba(18,18,22,0.8)] px-[3px] text-[9px] font-medium leading-none text-(--on-accent)">
              {unread}
            </span>
          )}
        </WinButton>
      )}
      <WinButton label="Minimize" onClick={() => void minimizeWindow()}>
        <Minus size={15} />
      </WinButton>
      <WinButton
        label={maximized ? "Restore" : "Maximize"}
        onClick={() => void toggleMaximizeWindow().then(setMaximized)}
      >
        {maximized ? <Copy size={14} className="-scale-x-100" /> : <Square size={14} />}
      </WinButton>
      <WinButton label="Close" onClick={() => void closeWindow()}>
        <X size={15} />
      </WinButton>
    </div>
  );
}
