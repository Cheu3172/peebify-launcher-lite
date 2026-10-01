// ------------ Capture Tile ------------
// One screenshot or clip in the gallery grid. Click to open it, or to tick it while in select mode (shift-click
// selects a range). Clips only load their preview when they scroll into view.
import { memo, useEffect, useRef, useState } from "react";
import { Check, Film, Image as ImageIcon } from "lucide-react";
import type { MediaItem } from "../../lib/ipc";
import { tileMedia } from "./media";
import { gameMeta } from "../../data/games";
import { iconFallback, iconSrcFor } from "../../lib/customMedia";
import { useCustomizationStore } from "../../store/customizationStore";

export const MediaTile = memo(function MediaTile({
  item,
  selectMode,
  selected,
  onOpen,
  onToggle,
}: {
  item: MediaItem;
  selectMode: boolean;
  selected: boolean;
  onOpen: (path: string) => void;
  onToggle: (path: string, range: boolean) => void;
}) {
  const media = tileMedia(item);
  const game = gameMeta(item.gameId);
  const icons = useCustomizationStore((s) => s.gameIcons);

  return (
    <div
      className={`group relative overflow-hidden rounded-ui border bg-black/40 transition-[border-color,box-shadow] duration-150 has-[>button:focus-visible]:outline-2 has-[>button:focus-visible]:outline-offset-2 has-[>button:focus-visible]:outline-(--accent-a) ${
        selected
          ? "border-(--accent-b)/70 shadow-[0_0_0_1px_rgba(var(--accent-b-rgb),0.5)]"
          : "border-white/[0.08] hover:border-white/20"
      }`}
    >
      <button
        type="button"
        onClick={(e) => (selectMode ? onToggle(item.path, e.shiftKey) : onOpen(item.path))}
        aria-label={selectMode ? `Select ${item.name}` : `Open ${item.name}`}
        aria-pressed={selectMode ? selected : undefined}
        className={`relative block aspect-video w-full ${selectMode ? "cursor-pointer" : "cursor-zoom-in"}`}
      >
        {media.src ? (
          media.video ? (
            <LazyClip src={media.src} />
          ) : (
            <img
              src={media.src}
              alt={item.name}
              loading="lazy"
              decoding="async"
              draggable={false}
              className="h-full w-full object-cover transition-transform duration-300 group-hover:scale-[1.03]"
            />
          )
        ) : (
          <span className="grid h-full w-full place-items-center text-white/25">
            {item.kind === "clip" ? <Film size={26} /> : <ImageIcon size={26} />}
          </span>
        )}
        <span className="pointer-events-none absolute inset-x-0 bottom-0 h-[46px] bg-gradient-to-t from-black/70 to-transparent" />
        {selectMode && selected && <span className="absolute inset-0 bg-(--accent-b)/15" />}
      </button>

      <div className="pointer-events-none absolute left-[7px] top-[7px] flex items-center gap-[5px]">
        {selectMode ? (
          <span
            className={`grid h-[20px] w-[20px] place-items-center rounded-[6px] border ${
              selected
                ? "accent-grad border-transparent text-(--on-accent)"
                : "border-white/50 bg-black/45"
            }`}
          >
            {selected && <Check size={13} strokeWidth={3} />}
          </span>
        ) : (
          item.kind === "clip" && (
            <span className="rounded-[5px] bg-black/65 px-[6px] py-[2px] text-[9.5px] font-semibold uppercase tracking-[0.08em] text-white/85">
              Clip
            </span>
          )
        )}
      </div>

      <div className="pointer-events-none absolute bottom-[7px] left-[8px] flex max-w-[65%] items-center gap-[6px]">
        {game.icon && item.gameId && (
          <img
            src={iconSrcFor(item.gameId, icons)}
            onError={iconFallback(item.gameId)}
            alt=""
            className="h-[16px] w-[16px] rounded-[4px] object-cover"
          />
        )}
        <span className="truncate text-[11px] text-white/80 opacity-0 transition-opacity duration-150 group-focus-within:opacity-100 group-hover:opacity-100">
          {game.name}
        </span>
      </div>
    </div>
  );
});

function LazyClip({ src }: { src: string }) {
  const box = useRef<HTMLSpanElement>(null);
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    const node = box.current;
    if (!node || visible) return;
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setVisible(true);
          observer.disconnect();
        }
      },
      { rootMargin: "200px" },
    );
    observer.observe(node);
    return () => observer.disconnect();
  }, [visible]);

  if (visible) {
    return (
      <video src={src} muted playsInline preload="metadata" className="h-full w-full object-cover" />
    );
  }
  return (
    <span ref={box} className="grid h-full w-full place-items-center text-white/25">
      <Film size={26} />
    </span>
  );
}
