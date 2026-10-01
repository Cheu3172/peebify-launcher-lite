// ------------ Selection Bar ------------
// The floating bar at the bottom of the gallery while captures are selected. Shows how many are selected and
// offers select all, open, show in folder, delete and clear.
import { m } from "framer-motion";
import { CheckSquare, ExternalLink, FolderOpen, X } from "lucide-react";
import type { MediaItem } from "../../lib/ipc";
import { openCapture, revealCapture } from "../../lib/ipc";
import { ActionButton } from "../ui/ActionButton";
import { DeleteButton } from "./DeleteButton";

export function SelectionBar({
  items,
  visibleCount,
  onSelectAll,
  onClear,
}: {
  items: MediaItem[];
  visibleCount: number;
  onSelectAll: () => void;
  onClear: () => void;
}) {
  const single = items.length === 1 ? items[0] : null;

  return (
    <m.div
      initial={{ opacity: 0, y: 16 }}
      animate={{ opacity: 1, y: 0 }}
      exit={{ opacity: 0, y: 16 }}
      transition={{ duration: 0.16 }}
      className="pointer-events-none fixed bottom-6 left-[60px] right-0 z-(--z-banner) flex justify-center px-6"
    >
      <div className="glass pointer-events-auto flex max-w-full flex-wrap items-center gap-2 rounded-ui px-3 py-2 shadow-2xl">
        <span className="px-2 text-[13px] font-medium tabular-nums text-white/85">
          {items.length} selected
        </span>
        {items.length < visibleCount && (
          <ActionButton icon={<CheckSquare size={14} />} onClick={onSelectAll}>
            Select all {visibleCount}
          </ActionButton>
        )}
        {single && (
          <>
            <ActionButton
              icon={<ExternalLink size={14} />}
              onClick={() => void openCapture(single.path)}
            >
              Open
            </ActionButton>
            <ActionButton
              icon={<FolderOpen size={14} />}
              onClick={() => void revealCapture(single.path)}
            >
              Show in folder
            </ActionButton>
          </>
        )}
        <DeleteButton items={items} onDeleted={onClear} />
        <button
          type="button"
          aria-label="Clear selection"
          onClick={onClear}
          className="grid h-[34px] w-[34px] place-items-center rounded-ui text-white/60 transition-colors hover:bg-white/[0.08] hover:text-white"
        >
          <X size={16} />
        </button>
      </div>
    </m.div>
  );
}
