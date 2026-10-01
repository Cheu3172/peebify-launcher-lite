// ------------ Mod Rows ------------
// The rows used in the mod lists. Each shows a thumbnail, name, source and an on/off switch, plus an Update
// button and a menu with things like Remove from profile and Uninstall. Also the ghost that follows the cursor
// while dragging.
import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  ArrowUpCircle,
  ListMinus,
  ListPlus,
  Loader2,
  MoreHorizontal,
  Puzzle,
  Search,
  Trash2,
} from "lucide-react";
import type { ModEntry } from "../../lib/ipc";
import type { ModUpdate } from "../../lib/gamebanana";
import { fmtBytes } from "../../lib/format";
import { ActionButton } from "../ui/ActionButton";
import { PopoverMenu } from "../ui/PopoverMenu";
import { Toggle } from "../ui/Toggle";

const SOURCE_LABEL: Record<string, string> = {
  gamebanana: "GameBanana",
  archive: "Imported",
  manual: "Added manually",
};

export type RowDragHandlers = Pick<
  React.DOMAttributes<HTMLDivElement>,
  "onPointerDown" | "onPointerMove" | "onPointerUp" | "onPointerCancel"
>;

function Thumb({ url, dim }: { url: string | null; dim: boolean }) {
  const [failed, setFailed] = useState(false);
  useEffect(() => setFailed(false), [url]);
  return (
    <span
      className={`grid h-[40px] w-[64px] shrink-0 place-items-center overflow-hidden rounded-[6px] bg-white/[0.06] ${
        dim ? "opacity-50 grayscale" : ""
      }`}
    >
      {url && !failed ? (
        <img
          src={url}
          alt=""
          loading="lazy"
          decoding="async"
          draggable={false}
          onError={() => setFailed(true)}
          className="h-full w-full object-cover"
        />
      ) : (
        <Puzzle size={16} className="text-white/35" />
      )}
    </span>
  );
}

function RowBody({ mod, update }: { mod: ModEntry; update?: ModUpdate | null }) {
  const meta = [
    SOURCE_LABEL[mod.source.kind] ?? mod.source.kind,
    mod.version ? `v${mod.version}` : "",
    mod.sizeBytes === null || mod.sizeBytes <= 0 ? "" : fmtBytes(mod.sizeBytes),
  ]
    .filter(Boolean)
    .join(", ");
  return (
    <>
      <Thumb url={mod.thumbnailUrl} dim={!mod.enabled} />
      <div className="min-w-0 flex-1">
        <div className="flex min-w-0 items-center gap-2">
          <span
            className={`truncate text-[13px] font-medium ${mod.enabled ? "text-white" : "text-white/55"}`}
            title={mod.name}
          >
            {mod.name}
          </span>
          {update && (
            <span
              className="shrink-0 rounded-full border border-(--accent-b)/50 bg-(--accent-b)/15 px-2 py-[1px] text-[10.5px] font-medium text-white/85"
              title={update.version ? `Version ${update.version} is on GameBanana` : "A newer file is on GameBanana"}
            >
              Update{update.version ? ` v${update.version}` : ""}
            </span>
          )}
        </div>
        <div className="truncate text-[11.5px] text-white/55">{meta}</div>
      </div>
    </>
  );
}

function UpdateButton({
  updating,
  disabled,
  onClick,
}: {
  updating: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <ActionButton
      variant="accent"
      icon={updating ? <Loader2 size={14} className="animate-spin" /> : <ArrowUpCircle size={14} />}
      disabled={updating || disabled}
      onClick={onClick}
    >
      {updating ? "Updating…" : "Update"}
    </ActionButton>
  );
}

export interface RowToggle {
  checked: boolean;
  label: string;
  disabled: boolean;
  onChange: (on: boolean) => void;
}

interface MenuItem {
  label: string;
  icon: ReactNode;
  danger?: boolean;
  onClick: () => void;
}

function RowMenu({ label, items }: { label: string; items: MenuItem[] }) {
  const [open, setOpen] = useState(false);
  const btnRef = useRef<HTMLButtonElement>(null);

  return (
    <>
      <button
        ref={btnRef}
        type="button"
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        className="grid h-[30px] w-[30px] shrink-0 place-items-center rounded-ui border border-white/[0.08] bg-white/[0.04] text-white/55 transition hover:bg-white/[0.1] hover:text-white/85"
      >
        <MoreHorizontal size={15} />
      </button>
      <PopoverMenu
        open={open}
        onClose={() => setOpen(false)}
        anchorRef={btnRef}
        width={220}
        estimatedHeight={items.length * 42 + 12}
      >
        {items.map((item) => (
          <button
            key={item.label}
            type="button"
            role="menuitem"
            onClick={item.onClick}
            className={`flex w-full items-center gap-3 rounded-[7px] px-[10px] py-[8px] text-left text-[12.5px] font-medium transition-colors hover:bg-white/[0.08] ${
              item.danger ? "text-[#fca5a5]" : "text-white"
            }`}
          >
            <span className={item.danger ? "text-[#fca5a5]/80" : "text-white/60"}>
              {item.icon}
            </span>
            {item.label}
          </button>
        ))}
      </PopoverMenu>
    </>
  );
}

export function ProfileModRow({
  mod,
  update,
  updating,
  anyUpdating,
  dragging,
  dragHandlers,
  busy,
  toggle,
  onUpdate,
  onRemoveFromProfile,
  onUninstall,
  onOpenGameBanana,
}: {
  mod: ModEntry;
  update?: ModUpdate | null;
  updating?: boolean;
  anyUpdating?: boolean;
  dragging: boolean;
  dragHandlers: RowDragHandlers;
  busy: boolean;
  toggle: RowToggle;
  onUpdate?: () => void;
  onRemoveFromProfile: () => void;
  onUninstall: () => void;
  onOpenGameBanana?: () => void;
}) {
  const menu: MenuItem[] = [];
  if (!busy) {
    menu.push({
      label: "Remove from profile",
      icon: <ListMinus size={14} />,
      onClick: onRemoveFromProfile,
    });
  }
  if (onOpenGameBanana) {
    menu.push({ label: "Show in Browse", icon: <Search size={14} />, onClick: onOpenGameBanana });
  }
  menu.push({ label: "Uninstall", icon: <Trash2 size={14} />, danger: true, onClick: onUninstall });

  return (
    <div
      {...dragHandlers}
      className={`flex cursor-grab touch-none select-none items-center gap-3 px-3 py-[9px] transition-opacity active:cursor-grabbing ${
        dragging ? "opacity-40" : ""
      }`}
    >
      <RowBody mod={mod} update={update} />
      {update && onUpdate && (
        <UpdateButton updating={!!updating} disabled={busy || !!anyUpdating} onClick={onUpdate} />
      )}
      <Toggle
        checked={toggle.checked}
        disabled={toggle.disabled}
        ariaLabel={toggle.label}
        onChange={toggle.onChange}
      />
      <RowMenu label={`More for ${mod.name}`} items={menu} />
    </div>
  );
}

export function LocalModRow({
  mod,
  update,
  updating,
  anyUpdating,
  profileName,
  inProfile,
  draggable,
  dragging,
  dragHandlers,
  busy,
  toggle,
  onAddToProfile,
  onRemoveFromProfile,
  onUninstall,
  onOpenGameBanana,
  onUpdate,
}: {
  mod: ModEntry;
  update?: ModUpdate | null;
  updating?: boolean;
  anyUpdating?: boolean;
  profileName: string | null;
  inProfile: boolean;
  draggable: boolean;
  dragging: boolean;
  dragHandlers: RowDragHandlers;
  busy: boolean;
  toggle: RowToggle;
  onAddToProfile?: () => void;
  onRemoveFromProfile?: () => void;
  onUninstall: () => void;
  onOpenGameBanana?: () => void;
  onUpdate?: () => void;
}) {
  const menu: MenuItem[] = [];
  if (profileName && !busy) {
    if (inProfile && onRemoveFromProfile) {
      menu.push({
        label: "Remove from profile",
        icon: <ListMinus size={14} />,
        onClick: onRemoveFromProfile,
      });
    } else if (!inProfile && onAddToProfile) {
      menu.push({ label: `Add to ${profileName}`, icon: <ListPlus size={14} />, onClick: onAddToProfile });
    }
  }
  if (onOpenGameBanana) {
    menu.push({ label: "Show in Browse", icon: <Search size={14} />, onClick: onOpenGameBanana });
  }
  menu.push({ label: "Uninstall", icon: <Trash2 size={14} />, danger: true, onClick: onUninstall });

  return (
    <div
      {...dragHandlers}
      className={`flex touch-none select-none items-center gap-3 px-3 py-[9px] transition-opacity ${
        draggable ? "cursor-grab active:cursor-grabbing" : ""
      } ${dragging ? "opacity-40" : ""}`}
    >
      <RowBody mod={mod} update={update} />
      {update && onUpdate && (
        <UpdateButton updating={!!updating} disabled={busy || !!anyUpdating} onClick={onUpdate} />
      )}
      <Toggle
        checked={toggle.checked}
        disabled={toggle.disabled}
        ariaLabel={toggle.label}
        onChange={toggle.onChange}
      />
      <RowMenu label={`More for ${mod.name}`} items={menu} />
    </div>
  );
}

export function DragGhost({
  mod,
  ghostRef,
}: {
  mod: ModEntry;
  ghostRef: React.Ref<HTMLDivElement>;
}) {
  return (
    <div
      ref={ghostRef}
      className="glass pointer-events-none fixed left-0 top-0 z-(--z-banner) flex max-w-[280px] items-center gap-2 rounded-ui px-3 py-2 shadow-2xl will-change-transform"
    >
      <Thumb url={mod.thumbnailUrl} dim={false} />
      <span className="truncate text-[12.5px] font-medium text-white">{mod.name}</span>
    </div>
  );
}
