// ------------ Gallery Page ------------
// The Gallery tab. Lists the screenshots and clips the overlay saved, grouped by day, with filters for game and
// type, a select mode for bulk actions, and a viewer to open any capture.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, m } from "framer-motion";
import { CheckSquare, FolderOpen, Images, RefreshCw, Settings } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { useMediaStore, type KindFilter } from "../../store/mediaStore";
import { useSettingsStore } from "../../store/settingsStore";
import { useSessionUiStore } from "../../store/sessionUiStore";
import { useUiStore } from "../../store/uiStore";
import { getOverlayConfig, onCapturesChanged, openCaptureFolder } from "../../lib/ipc";
import { useEscapeKey } from "../../lib/useClickOutside";
import { useTauriEvent } from "../../lib/useTauriEvent";
import { fadeVariants } from "../../lib/motion";
import { GAMES } from "../../data/games";
import { ActionButton } from "../ui/ActionButton";
import { GlassPage } from "../ui/GlassPage";
import { Select } from "../ui/Select";
import { MediaTile } from "./MediaTile";
import { MediaViewer } from "./MediaViewer";
import { SelectionBar } from "./SelectionBar";
import { groupByDay } from "./media";

const PAGE = 96;

const KIND_CHIPS: { value: KindFilter; label: string }[] = [
  { value: "all", label: "All" },
  { value: "screenshot", label: "Screenshots" },
  { value: "clip", label: "Clips" },
];

const GAME_OPTIONS = [
  { value: "all", label: "All games" },
  ...GAMES.map((g) => ({ value: g.id, label: g.name })),
];

const noop = () => {};

const KEY_IGNORE =
  "input, textarea, select, [contenteditable=true], [role=menu], [role=listbox], [aria-expanded=true]";

function CaptureHint() {
  const overlayOn = useSettingsStore((s) => s.values.overlayEnabled === "true");
  const setView = useUiStore((s) => s.setView);
  const setSettingsCat = useSessionUiStore((s) => s.setSettingsCat);
  const [shotKey, setShotKey] = useState("Alt+S");

  useEffect(() => {
    let cancelled = false;
    void getOverlayConfig().then((config) => {
      const key = config?.hotkeys.find((h) => h.id === "overlayShotHotkey")?.accelerator;
      if (!cancelled && key) setShotKey(key);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <>
      <p className="max-w-[52ch] text-[12.5px] text-white/45">
        {overlayOn
          ? `Press ${shotKey} in game to take a screenshot.`
          : `Turn on the game overlay, then press ${shotKey} in game to take a screenshot.`}
      </p>
      <ActionButton
        icon={<Settings size={14} />}
        onClick={() => {
          setSettingsCat("overlay");
          setView("settings");
        }}
      >
        Open overlay settings
      </ActionButton>
    </>
  );
}

function Chip({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={`rounded-ui border px-[12px] py-[7px] text-[12.5px] font-medium transition-colors duration-150 ${
        active
          ? "border-white/[0.16] bg-white/[0.10] text-white"
          : "border-white/[0.08] text-white/55 hover:bg-white/[0.05] hover:text-white"
      }`}
    >
      {children}
    </button>
  );
}

function CapturesHeader() {
  const folder = useMediaStore((s) => s.folder);
  return (
    <span className="inline-flex" title={folder || undefined}>
      <ActionButton icon={<FolderOpen size={14} />} onClick={() => void openCaptureFolder()}>
        Open folder
      </ActionButton>
    </span>
  );
}

function CapturesTab() {
  const { items, hydrated, loading, error, kind, gameId, selectMode, selected } = useMediaStore(
    useShallow((s) => ({
      items: s.items,
      hydrated: s.hydrated,
      loading: s.loading,
      error: s.error,
      kind: s.kind,
      gameId: s.gameId,
      selectMode: s.selectMode,
      selected: s.selected,
    })),
  );
  const load = useMediaStore((s) => s.load);
  const refresh = useMediaStore((s) => s.refresh);
  const setKind = useMediaStore((s) => s.setKind);
  const setGame = useMediaStore((s) => s.setGame);
  const setSelectMode = useMediaStore((s) => s.setSelectMode);
  const toggleSelected = useMediaStore((s) => s.toggleSelected);
  const selectMany = useMediaStore((s) => s.selectMany);
  const clearSelection = useMediaStore((s) => s.clearSelection);

  const [viewing, setViewing] = useState<string | null>(null);
  const [limit, setLimit] = useState(PAGE);
  const viewedIndex = useRef(0);
  const anchor = useRef<string | null>(null);
  const sentinel = useRef<HTMLDivElement>(null);

  useEffect(() => {
    void load();
  }, [load]);
  const missedChange = useRef(false);
  useTauriEvent(onCapturesChanged, () => {
    if (document.visibilityState === "hidden") missedChange.current = true;
    else refresh();
  });
  useEffect(() => {
    const onVisible = () => {
      if (document.visibilityState === "hidden" || !missedChange.current) return;
      missedChange.current = false;
      refresh();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [refresh]);

  useEffect(() => setLimit(PAGE), [kind, gameId]);

  const visible = useMemo(
    () =>
      items.filter(
        (i) => (kind === "all" || i.kind === kind) && (gameId === "all" || i.gameId === gameId),
      ),
    [items, kind, gameId],
  );
  const shown = useMemo(() => visible.slice(0, limit), [visible, limit]);
  const groups = useMemo(() => groupByDay(shown), [shown]);
  const dayPaths = useMemo(() => {
    const byDay = new Map<string, string[]>();
    for (const group of groupByDay(visible)) {
      const paths = group.items.map((i) => i.path);
      const known = byDay.get(group.key);
      if (known) known.push(...paths);
      else byDay.set(group.key, paths);
    }
    return byDay;
  }, [visible]);
  const selectedItems = useMemo(() => visible.filter((i) => selected[i.path]), [visible, selected]);
  const hasMore = visible.length > shown.length;

  useEffect(() => {
    if (!viewing) return;
    const at = visible.findIndex((i) => i.path === viewing);
    if (at >= 0) {
      viewedIndex.current = at;
      return;
    }
    const fallback = visible[Math.min(viewedIndex.current, visible.length - 1)];
    setViewing(fallback ? fallback.path : null);
  }, [viewing, visible]);

  useEffect(() => {
    const el = sentinel.current;
    if (!el || !hasMore) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) setLimit((l) => l + PAGE);
      },
      { root: el.closest(".overflow-y-auto"), rootMargin: "500px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [hasMore, groups.length]);

  const keysActive = selectMode && !viewing;
  const isTopLayer = useEscapeKey(noop, keysActive);

  useEffect(() => {
    if (!keysActive) return;
    const onKey = (e: KeyboardEvent) => {
      if (!isTopLayer()) return;
      const target = e.target instanceof Element ? e.target : null;
      if (target?.closest(KEY_IGNORE)) return;
      if (e.key === "Escape") {
        setSelectMode(false);
        return;
      }
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "a") {
        e.preventDefault();
        selectMany(visible.map((i) => i.path));
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [keysActive, isTopLayer, setSelectMode, selectMany, visible]);

  const onToggle = useCallback(
    (path: string, range: boolean) => {
      const from = anchor.current;
      anchor.current = path;
      if (range && from && from !== path) {
        const a = visible.findIndex((i) => i.path === from);
        const b = visible.findIndex((i) => i.path === path);
        if (a >= 0 && b >= 0) {
          window.getSelection()?.removeAllRanges();
          selectMany(visible.slice(Math.min(a, b), Math.max(a, b) + 1).map((i) => i.path));
          return;
        }
      }
      toggleSelected(path);
    },
    [visible, selectMany, toggleSelected],
  );
  const nothingCaptured = kind === "all" && gameId === "all";

  return (
    <>
      <div className="mb-5 flex flex-wrap items-center gap-3">
        <div className="flex flex-wrap gap-[6px]">
          {KIND_CHIPS.map((c) => (
            <Chip key={c.value} active={kind === c.value} onClick={() => setKind(c.value)}>
              {c.label}
            </Chip>
          ))}
        </div>
        <Select ariaLabel="Game" value={gameId} onChange={setGame} options={GAME_OPTIONS} />
        <span className="ml-auto text-[12px] tabular-nums text-white/40">
          {hydrated ? `${visible.length} item${visible.length === 1 ? "" : "s"}` : ""}
        </span>
        <ActionButton
          icon={<CheckSquare size={14} />}
          variant={selectMode ? "accent" : "neutral"}
          onClick={() => setSelectMode(!selectMode)}
        >
          {selectMode ? "Done" : "Select"}
        </ActionButton>
      </div>

      {error && (
        <div className="mb-4 flex items-center gap-3 rounded-ui border border-(--color-danger,#f87171)/30 bg-(--color-danger,#f87171)/[0.08] px-4 py-3 text-[12.5px] text-(--color-danger-soft,#fca5a5)">
          <span className="min-w-0 flex-1">{error}</span>
          <ActionButton
            icon={<RefreshCw size={14} className={loading ? "animate-spin" : undefined} />}
            disabled={loading}
            onClick={() => void load()}
          >
            Retry
          </ActionButton>
        </div>
      )}

      {!hydrated ? (
        <div
          role="status"
          aria-label="Loading captures"
          className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(200px,1fr))]"
        >
          {Array.from({ length: 8 }, (_, i) => (
            <div key={i} className="aspect-video animate-pulse rounded-ui bg-white/[0.05]" />
          ))}
        </div>
      ) : error && visible.length === 0 ? null : visible.length === 0 ? (
        <div className="flex flex-col items-center justify-center gap-3 rounded-ui border border-dashed border-white/15 bg-white/[0.04] px-6 py-12 text-center">
          <Images size={22} className="opacity-60" />
          <p className="max-w-[52ch] text-[13.5px] text-white/60">
            {nothingCaptured ? "Nothing captured yet." : "Nothing matches these filters."}
          </p>
          {nothingCaptured && <CaptureHint />}
        </div>
      ) : (
        <AnimatePresence initial={false}>
          <m.div
            key={`${kind}-${gameId}`}
            variants={fadeVariants}
            initial="initial"
            animate="animate"
            className="flex flex-col gap-7 pb-24"
          >
            {groups.map((group) => (
              <section key={group.key}>
                <div className="sticky top-0 z-[2] -mx-2 mb-3 flex items-baseline gap-2 rounded-[8px] bg-[rgba(12,12,16,0.92)] px-2 py-2">
                  <h2 className="text-[14px] font-semibold text-white/90">{group.label}</h2>
                  <span className="text-[11.5px] tabular-nums text-white/35">
                    {dayPaths.get(group.key)?.length ?? group.items.length}
                  </span>
                  {selectMode && (
                    <button
                      type="button"
                      onClick={() =>
                        selectMany(dayPaths.get(group.key) ?? group.items.map((i) => i.path))
                      }
                      className="ml-auto text-[11.5px] font-medium text-white/50 hover:text-white"
                    >
                      Select day
                    </button>
                  )}
                </div>
                <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(200px,1fr))]">
                  {group.items.map((item) => (
                    <MediaTile
                      key={item.path}
                      item={item}
                      selectMode={selectMode}
                      selected={!!selected[item.path]}
                      onOpen={setViewing}
                      onToggle={onToggle}
                    />
                  ))}
                </div>
              </section>
            ))}
            {hasMore && (
              <div ref={sentinel} className="flex items-center justify-center py-4">
                <ActionButton onClick={() => setLimit((l) => l + PAGE)}>Load more</ActionButton>
              </div>
            )}
          </m.div>
        </AnimatePresence>
      )}

      <AnimatePresence>
        {selectMode && selectedItems.length > 0 && (
          <SelectionBar
            key="selection"
            items={selectedItems}
            visibleCount={visible.length}
            onSelectAll={() => selectMany(visible.map((i) => i.path))}
            onClear={clearSelection}
          />
        )}
      </AnimatePresence>

      {viewing && (
        <MediaViewer
          items={visible}
          path={viewing}
          onPath={setViewing}
          onClose={() => setViewing(null)}
        />
      )}
    </>
  );
}

export function GalleryPage() {
  return (
    <GlassPage
      title="Gallery"
      subtitle="Screenshots and clips from the overlay on this PC."
      headerRight={
        <div className="flex flex-wrap items-center justify-end gap-3">
          <CapturesHeader />
        </div>
      }
    >
      <CapturesTab />
    </GlassPage>
  );
}
