// ------------ Capture Viewer ------------
// The full-size viewer that opens over the gallery for a screenshot or clip. Has previous and next arrows, file
// details, and buttons to open the file, show it in its folder or delete it.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ChevronLeft, ChevronRight, ExternalLink, FolderOpen, X } from "lucide-react";
import { openCapture, revealCapture, type MediaItem } from "../../lib/ipc";
import { iconFallback, iconSrcFor, localFileSrc } from "../../lib/customMedia";
import { fmtBytes, fmtDateTime, fmtDuration } from "../../lib/format";
import { useTimeFormat } from "../../store/settingsStore";
import { useCustomizationStore } from "../../store/customizationStore";
import { useEscapeKey } from "../../lib/useClickOutside";
import { useFocusTrap } from "../../lib/useFocusTrap";
import { gameMeta } from "../../data/games";
import { DeleteButton } from "./DeleteButton";

function ViewerButton({ onClick, children }: { onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex shrink-0 items-center gap-[6px] rounded-ui border border-white/15 bg-white/[0.06] px-3 py-[7px] text-[12px] font-medium text-white transition-colors duration-150 hover:bg-white/[0.12]"
    >
      {children}
    </button>
  );
}

function Arrow({
  side,
  onClick,
  children,
}: {
  side: "left" | "right";
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={side === "left" ? "Previous" : "Next"}
      onClick={onClick}
      className={`absolute top-1/2 grid h-[42px] w-[42px] -translate-y-1/2 place-items-center rounded-full bg-white/[0.08] text-white/70 hover:bg-white/[0.16] hover:text-white ${
        side === "left" ? "left-5" : "right-5"
      }`}
    >
      {children}
    </button>
  );
}

function Info({ label, value }: { label: string; value: string }) {
  if (!value) return null;
  return (
    <div>
      <dt className="text-[10.5px] font-medium uppercase tracking-[0.08em] text-white/35">{label}</dt>
      <dd className="mt-[2px] break-words text-[12.5px] text-white/85">{value}</dd>
    </div>
  );
}

interface LoadedMeta {
  path: string;
  resolution: string;
  durationMs: number | null;
}

export function MediaViewer({
  items,
  path,
  onPath,
  onClose,
}: {
  items: MediaItem[];
  path: string;
  onPath: (path: string) => void;
  onClose: () => void;
}) {
  const timeFormat = useTimeFormat();
  const icons = useCustomizationStore((s) => s.gameIcons);
  const [meta, setMeta] = useState<LoadedMeta | null>(null);
  const video = useRef<HTMLVideoElement>(null);
  const root = useRef<HTMLDivElement>(null);

  const index = useMemo(() => {
    const found = items.findIndex((i) => i.path === path);
    return found < 0 ? 0 : found;
  }, [items, path]);
  const current = items[index];

  const step = useCallback(
    (delta: number) => {
      if (items.length === 0) return;
      const next = (index + delta + items.length) % items.length;
      onPath(items[next].path);
    },
    [items, index, onPath],
  );

  const isTop = useEscapeKey(onClose);
  useFocusTrap(root, true, true);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
      if (!isTop()) return;
      const target = e.target instanceof Element ? e.target : null;
      if (target?.closest("video, input, textarea, select, [role=menu], [role=slider]")) return;
      e.preventDefault();
      e.stopPropagation();
      step(e.key === "ArrowLeft" ? -1 : 1);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [isTop, step]);

  useEffect(() => {
    const element = video.current;
    return () => {
      if (!element) return;
      element.pause();
      element.removeAttribute("src");
      element.load();
    };
  }, [path]);

  if (!current) return null;

  const source = localFileSrc(current.path) ?? undefined;
  const game = gameMeta(current.gameId);
  const loaded = meta?.path === current.path ? meta : null;
  const nextAfterDelete = items[index + 1]?.path ?? items[index - 1]?.path ?? null;

  return createPortal(
    <div
      ref={root}
      data-viewer-root
      role="dialog"
      aria-modal="true"
      aria-label={current.name}
      tabIndex={-1}
      className="fixed inset-0 z-(--z-viewer) flex flex-col outline-none"
      style={{ background: "#07070b" }}
    >
      <header className="drag-region flex shrink-0 items-center gap-2 border-b border-white/[0.08] px-5 py-3">
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13.5px] font-medium">{current.name}</div>
          <div className="mt-[2px] truncate text-[11px] text-white/45">
            {game.name} · {fmtDateTime(current.takenAt, timeFormat)} · {index + 1} of {items.length}
          </div>
        </div>
        <ViewerButton onClick={() => void openCapture(current.path)}>
          <ExternalLink size={14} />
          Open
        </ViewerButton>
        <ViewerButton onClick={() => void revealCapture(current.path)}>
          <FolderOpen size={14} />
          Show in folder
        </ViewerButton>
        <DeleteButton
          items={[current]}
          onDeleted={() => {
            if (nextAfterDelete) onPath(nextAfterDelete);
            else onClose();
          }}
        />
        <button
          type="button"
          aria-label="Close"
          onClick={onClose}
          className="ml-1 grid h-[30px] w-[30px] shrink-0 place-items-center rounded-[8px] text-white/60 hover:bg-white/[0.08] hover:text-white"
        >
          <X size={18} />
        </button>
      </header>

      <div className="flex min-h-0 flex-1">
        <div className="relative flex min-h-0 min-w-0 flex-1 items-center justify-center p-6">
          {items.length > 1 && (
            <Arrow side="left" onClick={() => step(-1)}>
              <ChevronLeft size={22} />
            </Arrow>
          )}

          {current.kind === "clip" ? (
            <video
              ref={video}
              key={current.path}
              src={source}
              controls
              autoPlay
              onLoadedMetadata={(e) => {
                const el = e.currentTarget;
                setMeta({
                  path: current.path,
                  resolution: `${el.videoWidth} x ${el.videoHeight}`,
                  durationMs: Number.isFinite(el.duration) ? el.duration * 1000 : null,
                });
              }}
              className="max-h-full max-w-full rounded-[10px]"
            />
          ) : (
            <img
              key={current.path}
              src={source}
              alt={current.name}
              draggable={false}
              onLoad={(e) =>
                setMeta({
                  path: current.path,
                  resolution: `${e.currentTarget.naturalWidth} x ${e.currentTarget.naturalHeight}`,
                  durationMs: null,
                })
              }
              className="max-h-full max-w-full rounded-[10px] object-contain"
            />
          )}

          {items.length > 1 && (
            <Arrow side="right" onClick={() => step(1)}>
              <ChevronRight size={22} />
            </Arrow>
          )}
        </div>

        <aside className="w-[264px] shrink-0 overflow-y-auto border-l border-white/[0.08] px-5 py-5">
          <dl className="flex flex-col gap-3">
            <div className="flex items-center gap-2">
              {game.icon && current.gameId && (
                <img
                  src={iconSrcFor(current.gameId, icons)}
                  onError={iconFallback(current.gameId)}
                  alt=""
                  className="h-6 w-6 rounded-[6px] object-cover"
                />
              )}
              <Info label="Game" value={game.name} />
            </div>
            <Info label="Taken" value={fmtDateTime(current.takenAt, timeFormat)} />
            <Info label="Size" value={current.sizeBytes > 0 ? fmtBytes(current.sizeBytes) : ""} />
            <Info label="Resolution" value={loaded?.resolution ?? ""} />
            {current.kind === "clip" && (
              <Info label="Duration" value={fmtDuration(loaded?.durationMs ?? null)} />
            )}
            <Info label="Kind" value={current.kind === "clip" ? "Clip" : "Screenshot"} />
            <Info label="File" value={current.path} />
          </dl>
        </aside>
      </div>
    </div>,
    document.body,
  );
}
