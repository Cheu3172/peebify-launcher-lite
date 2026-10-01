// ------------ Mods Library Lists ------------
// The mod lists inside the Library: the mods in the chosen profile and every mod installed. Has search and
// sorting, and mods can be dragged between the two.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { AlertTriangle, Layers, Puzzle } from "lucide-react";
import type { ModEntry, ModProfile } from "../../lib/ipc";
import type { ModUpdate } from "../../lib/gamebanana";
import type { Game } from "../../types/game";
import { ActionButton } from "../ui/ActionButton";
import { GroupBox } from "../ui/SettingsGroup";
import { SearchField } from "../ui/SearchField";
import { Select } from "../ui/Select";
import { ModProgressRow } from "./ModProgressRow";
import {
  DragGhost,
  LocalModRow,
  ProfileModRow,
  type RowDragHandlers,
  type RowToggle,
} from "./ModsHubRow";

type SortKey = "name" | "newest" | "largest";

const SORTS: { value: SortKey; label: string }[] = [
  { value: "name", label: "Sort by name" },
  { value: "newest", label: "Newest first" },
  { value: "largest", label: "Largest first" },
];

const DRAG_THRESHOLD = 6;

type DragSource = "profile" | "installed";

interface DragState {
  modId: string;
  from: DragSource;
  over: boolean;
}

function Heading({ title, count }: { title: string; count: number | null }) {
  return (
    <h2 className="mb-2 flex items-baseline gap-2 text-[12.5px] font-medium text-white/55">
      {title}
      {count !== null && <span className="tabular-nums text-white/30">{count}</span>}
    </h2>
  );
}

function Empty({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex flex-col items-center gap-3 rounded-ui border border-dashed border-white/[0.12] bg-white/[0.02] px-6 py-8 text-center text-[13px] text-white/50">
      {children}
    </div>
  );
}

function Skeleton({ rows }: { rows: number }) {
  return (
    <div className="grid animate-pulse gap-2" aria-label="Loading mods">
      {Array.from({ length: rows }, (_, i) => (
        <div key={i} className="h-[58px] rounded-ui border border-white/[0.08] bg-white/[0.04]" />
      ))}
    </div>
  );
}

function sortMods(list: ModEntry[], sort: SortKey): ModEntry[] {
  const sorted = [...list];
  if (sort === "name") sorted.sort((a, b) => a.name.localeCompare(b.name));
  else if (sort === "newest")
    sorted.sort((a, b) => {
      if (!a.installedAt && !b.installedAt) return a.name.localeCompare(b.name);
      if (!a.installedAt) return 1;
      if (!b.installedAt) return -1;
      return b.installedAt.localeCompare(a.installedAt);
    });
  else sorted.sort((a, b) => (b.sizeBytes ?? 0) - (a.sizeBytes ?? 0));
  return sorted;
}

export function ModsHubList({
  game,
  loaded,
  mods,
  modsError,
  profile,
  profileIsActive,
  busy,
  togglingModIds,
  downloadingKey,
  updates,
  updatingModId,
  onBrowse,
  onRetryMods,
  onToggleEnabled,
  onAddToProfile,
  onRemoveFromProfile,
  onUninstall,
  onOpenGameBanana,
  onUpdate,
}: {
  game: Game;
  loaded: boolean;
  mods: ModEntry[];
  modsError: boolean;
  profile: ModProfile | null;
  profileIsActive: boolean;
  busy: boolean;
  togglingModIds: ReadonlySet<string>;
  downloadingKey: string | null;
  updates: Map<string, ModUpdate>;
  updatingModId: string | null;
  onBrowse?: () => void;
  onRetryMods: () => void;
  onToggleEnabled: (mod: ModEntry, on: boolean) => void;
  onAddToProfile: (mod: ModEntry) => void;
  onRemoveFromProfile: (mod: ModEntry) => void;
  onUninstall: (mod: ModEntry) => void;
  onOpenGameBanana?: (mod: ModEntry) => void;
  onUpdate: (mod: ModEntry) => void;
}) {
  const [search, setSearch] = useState("");
  const [sort, setSort] = useState<SortKey>("name");
  const [drag, setDrag] = useState<DragState | null>(null);
  const dropRef = useRef<HTMLElement>(null);
  const installedRef = useRef<HTMLElement>(null);
  const pressRef = useRef<{
    modId: string;
    from: DragSource;
    pointerId: number;
    x: number;
    y: number;
  } | null>(null);
  const startedRef = useRef(false);
  const ghostRef = useRef<HTMLDivElement>(null);
  const posRef = useRef({ x: 0, y: 0 });
  const frameRef = useRef(0);

  const placeGhost = () => {
    frameRef.current = 0;
    const el = ghostRef.current;
    if (el) el.style.transform = `translate(${posRef.current.x + 14}px, ${posRef.current.y + 14}px)`;
  };

  const endDrag = () => {
    pressRef.current = null;
    startedRef.current = false;
    if (frameRef.current) cancelAnimationFrame(frameRef.current);
    frameRef.current = 0;
    setDrag(null);
  };

  useEffect(() => {
    setSearch("");
    setDrag(null);
    pressRef.current = null;
    startedRef.current = false;
  }, [game.id, profile?.id]);

  useEffect(
    () => () => {
      if (frameRef.current) cancelAnimationFrame(frameRef.current);
    },
    [],
  );

  const dragging = drag !== null;

  useLayoutEffect(() => {
    if (!dragging) return;
    const el = ghostRef.current;
    if (el) el.style.transform = `translate(${posRef.current.x + 14}px, ${posRef.current.y + 14}px)`;
  });

  useEffect(() => {
    if (!dragging) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      pressRef.current = null;
      startedRef.current = false;
      if (frameRef.current) cancelAnimationFrame(frameRef.current);
      frameRef.current = 0;
      setDrag(null);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [dragging]);

  const inProfile = useMemo(() => new Set(profile?.modIds ?? []), [profile]);
  const members = useMemo(
    () => sortMods(mods.filter((m) => inProfile.has(m.modId)), "name"),
    [mods, inProfile],
  );
  const query = search.trim().toLowerCase();
  const shownMembers = useMemo(
    () => (query ? members.filter((m) => m.name.toLowerCase().includes(query)) : members),
    [members, query],
  );
  const visible = useMemo(
    () => sortMods(query ? mods.filter((m) => m.name.toLowerCase().includes(query)) : mods, sort),
    [mods, query, sort],
  );
  const draggedMod = drag ? (mods.find((m) => m.modId === drag.modId) ?? null) : null;

  const inside = (el: HTMLElement | null, x: number, y: number) => {
    const r = el?.getBoundingClientRect();
    return !!r && x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
  };
  const isOverTarget = (from: DragSource, x: number, y: number) =>
    inside(from === "installed" ? dropRef.current : installedRef.current, x, y);

  const anyUpdating = updatingModId !== null || downloadingKey !== null;
  const toggleFor = (mod: ModEntry): RowToggle =>
    !profile || profileIsActive
      ? {
          checked: mod.enabled,
          label: `Enable ${mod.name}`,
          disabled: busy || togglingModIds.has(mod.modId),
          onChange: (on) => onToggleEnabled(mod, on),
        }
      : {
          checked: inProfile.has(mod.modId),
          label: `Include ${mod.name} in ${profile.name}`,
          disabled: busy,
          onChange: (on) => (on ? onAddToProfile(mod) : onRemoveFromProfile(mod)),
        };

  const dragHandlersFor = (mod: ModEntry, from: DragSource): RowDragHandlers => ({
    onPointerDown: (e) => {
      if (e.button !== 0 || busy || !profile) return;
      if (from === "installed" && inProfile.has(mod.modId)) return;
      if ((e.target as HTMLElement).closest("button, input, a")) return;
      pressRef.current = {
        modId: mod.modId,
        from,
        pointerId: e.pointerId,
        x: e.clientX,
        y: e.clientY,
      };
      startedRef.current = false;
      e.currentTarget.setPointerCapture(e.pointerId);
    },
    onPointerMove: (e) => {
      const press = pressRef.current;
      if (!press || press.pointerId !== e.pointerId) return;
      if (!startedRef.current) {
        if (Math.hypot(e.clientX - press.x, e.clientY - press.y) < DRAG_THRESHOLD) return;
        startedRef.current = true;
      }
      posRef.current = { x: e.clientX, y: e.clientY };
      if (!frameRef.current) frameRef.current = requestAnimationFrame(placeGhost);
      const over = isOverTarget(press.from, e.clientX, e.clientY);
      setDrag((prev) =>
        prev && prev.modId === press.modId && prev.from === press.from && prev.over === over
          ? prev
          : { modId: press.modId, from: press.from, over },
      );
    },
    onPointerUp: (e) => {
      const press = pressRef.current;
      if (!press || press.pointerId !== e.pointerId) return;
      const dropped = startedRef.current && isOverTarget(press.from, e.clientX, e.clientY);
      endDrag();
      if (!dropped) return;
      if (press.from === "installed") onAddToProfile(mod);
      else onRemoveFromProfile(mod);
    },
    onPointerCancel: endDrag,
  });

  const tone = (active: boolean) =>
    !active
      ? "border-transparent"
      : drag?.over
        ? "border-(--accent-b) bg-(--accent-b)/10"
        : "border-(--accent-b)/50";
  const dropTone = tone(drag?.from === "installed");
  const installedTone = tone(drag?.from === "profile");

  return (
    <div className="flex min-w-0 flex-col gap-6">
      <ModProgressRow gameId={game.id} />

      {loaded && !modsError && mods.length > 0 && (
        <div className="-mb-2 flex flex-wrap items-center gap-2">
          <SearchField
            value={search}
            onChange={setSearch}
            placeholder="Search your mods…"
            ariaLabel="Search mods"
            className="min-w-[200px] flex-1"
          />
          <Select ariaLabel="Sort" options={SORTS} value={sort} onChange={(v) => setSort(v as SortKey)} />
        </div>
      )}

      <section
        ref={dropRef}
        className={`-m-2 rounded-ui border border-dashed p-2 transition-colors ${dropTone}`}
      >
        <Heading
          title="Mods in Profile"
          count={loaded && !modsError && profile ? members.length : null}
        />
        {!loaded ? (
          <Skeleton rows={1} />
        ) : !profile ? (
          <Empty>
            <Layers size={20} className="opacity-60" />
            <p>Pick a profile to see the mods in it.</p>
          </Empty>
        ) : modsError ? (
          <Empty>
            <p>Couldn't read the mods folder.</p>
          </Empty>
        ) : members.length === 0 ? (
          <Empty>
            <Layers size={20} className="opacity-60" />
            <p>Add installed mods from the menu on each row, or drag them here.</p>
          </Empty>
        ) : shownMembers.length === 0 ? (
          <Empty>
            <p>Nothing matches. Try a different search to see every mod again.</p>
          </Empty>
        ) : (
          <GroupBox>
            {shownMembers.map((mod) => (
              <ProfileModRow
                key={mod.modId}
                mod={mod}
                update={updates.get(mod.modId) ?? null}
                updating={updatingModId === mod.modId}
                anyUpdating={anyUpdating}
                dragging={drag?.modId === mod.modId}
                dragHandlers={dragHandlersFor(mod, "profile")}
                busy={busy}
                toggle={toggleFor(mod)}
                onUpdate={() => onUpdate(mod)}
                onRemoveFromProfile={() => onRemoveFromProfile(mod)}
                onUninstall={() => onUninstall(mod)}
                onOpenGameBanana={
                  onOpenGameBanana && mod.source.gbModId != null
                    ? () => onOpenGameBanana(mod)
                    : undefined
                }
              />
            ))}
          </GroupBox>
        )}
      </section>

      <section
        ref={installedRef}
        className={`-m-2 rounded-ui border border-dashed p-2 transition-colors ${installedTone}`}
      >
        <Heading title="Mods Installed" count={loaded && !modsError ? mods.length : null} />
        {!loaded ? (
          <Skeleton rows={3} />
        ) : modsError ? (
          <Empty>
            <AlertTriangle size={20} className="opacity-60" />
            <p className="max-w-[46ch]">
              Peebify couldn't read the {game.name} mods folder. If it's on another drive, check
              that the drive is connected.
            </p>
            <ActionButton onClick={onRetryMods}>Retry</ActionButton>
          </Empty>
        ) : mods.length === 0 ? (
          <Empty>
            <Puzzle size={20} className="opacity-60" />
            <p className="max-w-[46ch]">
              No mods for {game.name} on this PC. Import a .zip, .7z or .rar file, drop one onto this
              window{onBrowse ? ", or find something new in Browse" : ""}.
            </p>
            {onBrowse && <ActionButton onClick={onBrowse}>Browse mods</ActionButton>}
          </Empty>
        ) : visible.length === 0 ? (
          <Empty>
            <p>Nothing matches. Try a different search to see every mod again.</p>
          </Empty>
        ) : (
          <GroupBox>
            {visible.map((mod) => (
              <LocalModRow
                key={mod.modId}
                mod={mod}
                update={updates.get(mod.modId) ?? null}
                updating={updatingModId === mod.modId}
                anyUpdating={anyUpdating}
                onUpdate={() => onUpdate(mod)}
                profileName={profile?.name ?? null}
                inProfile={inProfile.has(mod.modId)}
                draggable={!!profile && !inProfile.has(mod.modId)}
                dragging={drag?.modId === mod.modId}
                dragHandlers={dragHandlersFor(mod, "installed")}
                busy={busy}
                toggle={toggleFor(mod)}
                onAddToProfile={profile ? () => onAddToProfile(mod) : undefined}
                onRemoveFromProfile={profile ? () => onRemoveFromProfile(mod) : undefined}
                onUninstall={() => onUninstall(mod)}
                onOpenGameBanana={
                  onOpenGameBanana && mod.source.gbModId != null
                    ? () => onOpenGameBanana(mod)
                    : undefined
                }
              />
            ))}
          </GroupBox>
        )}
      </section>

      {drag && draggedMod && <DragGhost mod={draggedMod} ghostRef={ghostRef} />}
    </div>
  );
}
