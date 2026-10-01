// ------------ Overlay Mods ------------
// The Mods tab of the overlay: switch installed mods on and off, drag them into profiles, apply a profile,
// and remove mods without leaving the game.
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type DOMAttributes,
  type ReactNode,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";
import { Search } from "lucide-react";
import { useModsStore } from "../../store/modsStore";
import { useModProfilesStore } from "../../store/modProfilesStore";
import { useNotificationStore } from "../../store/notificationStore";
import { useOverlayModsSession } from "../modsSession";
import {
  deleteMod,
  onModProgress,
  setModsEnabledBulk,
  type ModEntry,
  type ModProfile,
} from "../ipc";
import { fmtBytes } from "../../lib/format";
import { OvBar, OvButton, OvChip, OvLabel, OvList, OvThumb, OvToggle } from "../ui";
import type { PanelProps } from "../types";

const SOURCE_LABEL: Record<string, string> = {
  gamebanana: "GameBanana",
  archive: "Imported",
  manual: "Added manually",
};

const DRAG_THRESHOLD = 6;

const INPUT =
  "w-[200px] rounded-[8px] border border-white/[0.28] bg-black/30 px-3 py-[6px] text-[12px] text-white outline-none placeholder:text-white/30";

type DragSource = "profile" | "installed";

interface DragState {
  modId: string;
  from: DragSource;
  x: number;
  y: number;
  over: boolean;
}

type DragHandlers = Pick<
  DOMAttributes<HTMLDivElement>,
  "onPointerDown" | "onPointerMove" | "onPointerUp" | "onPointerCancel"
>;

type RowMenu = { key: string; step: "menu" | "uninstall" } | null;

interface RowToggle {
  checked: boolean;
  label: string;
  disabled: boolean;
  onChange: (on: boolean) => void;
}

function byName(list: ModEntry[]): ModEntry[] {
  return [...list].sort((a, b) => a.name.localeCompare(b.name));
}

function Section({
  title,
  count,
  tone,
  sectionRef,
  action,
  children,
}: {
  title: string;
  count: number | null;
  tone: string;
  sectionRef?: RefObject<HTMLElement | null>;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section
      ref={sectionRef}
      className={`-mx-2 rounded-[12px] border border-dashed px-2 pb-2 pt-1 transition-colors duration-150 ${tone}`}
    >
      <div className="mb-2 flex items-baseline gap-2">
        <OvLabel>{title}</OvLabel>
        {count !== null && <span className="ov-mono text-[11px] text-white/30">{count}</span>}
        {action && <span className="ml-auto self-center">{action}</span>}
      </div>
      {children}
    </section>
  );
}

function Empty({ children }: { children: ReactNode }) {
  return (
    <div className="rounded-[10px] border border-dashed border-white/[0.12] bg-white/[0.02] px-4 py-5 text-center text-[12.5px] leading-relaxed text-white/50">
      {children}
    </div>
  );
}

function RowBody({ mod }: { mod: ModEntry }) {
  const meta = [
    SOURCE_LABEL[mod.source.kind] ?? mod.source.kind,
    mod.version ? `v${mod.version}` : "",
    mod.sizeBytes === null || mod.sizeBytes <= 0 ? "" : fmtBytes(mod.sizeBytes),
  ]
    .filter(Boolean)
    .join(", ");
  return (
    <>
      <span className={`flex shrink-0 ${mod.enabled ? "" : "opacity-50 grayscale"}`}>
        <OvThumb src={mod.thumbnailUrl} size={36} />
      </span>
      <div className="min-w-0 flex-1">
        <div
          className={`truncate text-[13px] font-medium ${mod.enabled ? "text-white" : "text-white/55"}`}
          title={mod.name}
        >
          {mod.name}
        </div>
        <div className="truncate text-[11px] text-white/40">{meta}</div>
      </div>
    </>
  );
}

function ProfileRow({
  mod,
  dragging,
  dragHandlers,
  busy,
  toggle,
  onRemove,
}: {
  mod: ModEntry;
  dragging: boolean;
  dragHandlers: DragHandlers;
  busy: boolean;
  toggle: RowToggle;
  onRemove: () => void;
}) {
  return (
    <div
      {...dragHandlers}
      className={`flex cursor-grab touch-none select-none items-center gap-3 border-b border-white/[0.055] px-4 py-[10px] transition-opacity active:cursor-grabbing ${
        dragging ? "opacity-40" : ""
      }`}
    >
      <RowBody mod={mod} />
      <OvButton disabled={busy} onClick={onRemove}>
        Remove
      </OvButton>
      <OvToggle
        checked={toggle.checked}
        disabled={toggle.disabled}
        ariaLabel={toggle.label}
        onChange={toggle.onChange}
      />
    </div>
  );
}

function InstalledRow({
  mod,
  canDrag,
  dragging,
  dragHandlers,
  busy,
  toggle,
  profileName,
  inProfile,
  onAddToProfile,
  onRemoveFromProfile,
  menu,
  onMenu,
  onUninstall,
}: {
  mod: ModEntry;
  canDrag: boolean;
  dragging: boolean;
  dragHandlers: DragHandlers;
  busy: boolean;
  toggle: RowToggle;
  profileName: string | null;
  inProfile: boolean;
  onAddToProfile: () => void;
  onRemoveFromProfile: () => void;
  menu: RowMenu;
  onMenu: (next: RowMenu) => void;
  onUninstall: () => void;
}) {
  const open = menu?.key === mod.modId ? menu : null;
  const close = () => onMenu(null);
  return (
    <div className={`border-b border-white/[0.055] transition-opacity ${dragging ? "opacity-40" : ""}`}>
      <div
        {...dragHandlers}
        className={`flex touch-none select-none items-center gap-3 px-4 py-[10px] ${
          canDrag ? "cursor-grab active:cursor-grabbing" : ""
        }`}
      >
        <RowBody mod={mod} />
        <OvToggle
          checked={toggle.checked}
          disabled={toggle.disabled}
          ariaLabel={toggle.label}
          onChange={toggle.onChange}
        />
        <button
          type="button"
          aria-label={`More for ${mod.name}`}
          aria-expanded={open !== null}
          disabled={busy}
          onClick={() => onMenu(open ? null : { key: mod.modId, step: "menu" })}
          className={`grid h-[26px] w-[26px] shrink-0 place-items-center rounded-[7px] border text-[14px] leading-none transition-colors duration-150 disabled:cursor-not-allowed disabled:opacity-40 ${
            open
              ? "border-white/[0.22] bg-white/10 text-white"
              : "border-white/10 bg-white/[0.04] text-white/55 hover:text-white/85"
          }`}
        >
          &middot;&middot;&middot;
        </button>
      </div>
      {open && (
        <div className="flex flex-wrap items-center gap-2 border-t border-white/[0.055] bg-black/20 px-4 py-2">
          {open.step === "menu" && (
            <>
              {profileName && (
                <OvButton
                  disabled={busy}
                  onClick={() => {
                    close();
                    if (inProfile) onRemoveFromProfile();
                    else onAddToProfile();
                  }}
                >
                  {inProfile ? "Remove from profile" : `Add to ${profileName}`}
                </OvButton>
              )}
              <OvButton tone="danger" onClick={() => onMenu({ key: mod.modId, step: "uninstall" })}>
                Uninstall
              </OvButton>
              <span className="ml-auto">
                <OvButton onClick={close}>Close</OvButton>
              </span>
            </>
          )}
          {open.step === "uninstall" && (
            <>
              <span className="min-w-0 flex-1 text-[12px] text-white/70">
                Uninstall? This deletes the mod files from this PC. There is no undo.
              </span>
              <OvButton tone="danger" onClick={onUninstall}>
                Yes
              </OvButton>
              <OvButton onClick={close}>No</OvButton>
            </>
          )}
        </div>
      )}
    </div>
  );
}

function ghostTransform(x: number, y: number): string {
  return `translate(${x + 14}px, ${y + 14}px)`;
}

function DragGhost({
  mod,
  x,
  y,
  ghostRef,
}: {
  mod: ModEntry;
  x: number;
  y: number;
  ghostRef: RefObject<HTMLDivElement | null>;
}) {
  return createPortal(
    <div
      ref={ghostRef}
      className="pointer-events-none fixed left-0 top-0 z-[130] flex max-w-[260px] items-center gap-2 rounded-[10px] border border-white/15 px-3 py-2 shadow-2xl"
      style={{ transform: ghostTransform(x, y), background: "rgba(8,8,11,0.86)" }}
    >
      <OvThumb src={mod.thumbnailUrl} size={28} />
      <span className="truncate text-[12.5px] font-medium text-white">{mod.name}</span>
    </div>,
    document.body,
  );
}

export function OverlayMods({ gameId, modsUsable, mods: status }: PanelProps) {
  const mods = useModsStore((s) => s.mods);
  const progress = useModsStore((s) => s.progress);
  const refreshMods = useModsStore((s) => s.refresh);
  const refreshModsIfCurrent = useModsStore((s) => s.refreshIfCurrent);
  const setProgress = useModsStore((s) => s.setProgress);
  const setLocal = useModsStore((s) => s.setModsEnabledLocal);
  const loaded = useModsStore((s) => s.loadedGameId) === gameId;
  const push = useNotificationStore((s) => s.push);
  const pending = useOverlayModsSession((s) => s.pending[gameId] ?? 0);
  const reloadKey = useOverlayModsSession((s) => s.reloadKeys[gameId] ?? "F10");
  const addPending = useOverlayModsSession((s) => s.addPending);
  const clearPending = useOverlayModsSession((s) => s.clearPending);

  const profiles = useModProfilesStore((s) => s.profiles);
  const activeId = useModProfilesStore((s) => s.activeId);
  const applying = useModProfilesStore((s) => s.applying);
  const profilesLoaded = useModProfilesStore((s) => s.loadedGameId) === gameId;
  const refreshProfiles = useModProfilesStore((s) => s.refresh);
  const refreshProfilesIfCurrent = useModProfilesStore((s) => s.refreshIfCurrent);
  const apply = useModProfilesStore((s) => s.apply);
  const rename = useModProfilesStore((s) => s.rename);
  const create = useModProfilesStore((s) => s.create);
  const duplicate = useModProfilesStore((s) => s.duplicate);
  const remove = useModProfilesStore((s) => s.remove);
  const setMembers = useModProfilesStore((s) => s.setMembers);

  const [search, setSearch] = useState("");
  const [rescanning, setRescanning] = useState(false);
  const [busy, setBusy] = useState(false);
  const [toggling, setToggling] = useState<Set<string>>(() => new Set());
  const [selectedProfileId, setSelectedProfileId] = useState<string | null>(null);
  const [mode, setMode] = useState<"new" | "rename" | null>(null);
  const [draft, setDraft] = useState("");
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [rowMenu, setRowMenu] = useState<RowMenu>(null);
  const [drag, setDrag] = useState<DragState | null>(null);

  const filter = useRef<HTMLInputElement>(null);
  const draftInput = useRef<HTMLInputElement>(null);
  const dropRef = useRef<HTMLElement | null>(null);
  const installedRef = useRef<HTMLElement | null>(null);
  const ghostRef = useRef<HTMLDivElement | null>(null);
  const pressRef = useRef<{
    modId: string;
    from: DragSource;
    pointerId: number;
    x: number;
    y: number;
  } | null>(null);
  const startedRef = useRef(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "/") return;
      const target = e.target as HTMLElement | null;
      if (target?.tagName === "INPUT" || target?.tagName === "TEXTAREA") return;
      e.preventDefault();
      filter.current?.focus();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    setSelectedProfileId(null);
    setMode(null);
    setConfirmDelete(false);
    setRowMenu(null);
    setSearch("");
    setDrag(null);
    pressRef.current = null;
    startedRef.current = false;
    if (!modsUsable) return;
    void refreshMods(gameId);
    void refreshProfiles(gameId);
  }, [gameId, modsUsable, refreshMods, refreshProfiles]);

  useEffect(() => {
    if (mode) draftInput.current?.select();
  }, [mode]);

  const refreshAll = useCallback(async () => {
    await Promise.all([refreshModsIfCurrent(gameId), refreshProfilesIfCurrent(gameId)]);
  }, [gameId, refreshModsIfCurrent, refreshProfilesIfCurrent]);

  const rescan = useCallback(async () => {
    setRescanning(true);
    try {
      await Promise.all([refreshMods(gameId), refreshProfiles(gameId)]);
    } finally {
      setRescanning(false);
    }
  }, [gameId, refreshMods, refreshProfiles]);

  useEffect(() => {
    let un: (() => void) | undefined;
    let cancelled = false;
    void onModProgress((p) => {
      if (p.gameId !== gameId) return;
      setProgress(p.done ? null : p);
      if (p.done && !p.failed) {
        void refreshAll();
      }
    }).then((f) => {
      if (cancelled) f();
      else un = f;
    });
    return () => {
      cancelled = true;
      un?.();
    };
  }, [gameId, refreshAll, setProgress]);

  const profile = useMemo<ModProfile | null>(
    () =>
      profiles.find((p) => p.id === selectedProfileId) ??
      profiles.find((p) => p.id === activeId) ??
      null,
    [profiles, selectedProfileId, activeId],
  );

  useEffect(() => {
    setDrag(null);
    pressRef.current = null;
    startedRef.current = false;
  }, [profile?.id]);

  const inProfile = useMemo(() => new Set(profile?.modIds ?? []), [profile]);
  const members = useMemo(
    () => byName(mods.filter((m) => inProfile.has(m.modId))),
    [mods, inProfile],
  );
  const installed = useMemo(() => byName(mods), [mods]);
  const query = search.trim().toLowerCase();
  const shownMembers = useMemo(
    () => (query ? members.filter((m) => m.name.toLowerCase().includes(query)) : members),
    [members, query],
  );
  const visible = useMemo(
    () => (query ? installed.filter((m) => m.name.toLowerCase().includes(query)) : installed),
    [installed, query],
  );
  const draggedMod = drag ? (mods.find((m) => m.modId === drag.modId) ?? null) : null;

  if (!modsUsable) {
    if (!status) return null;
    return (
      <p className="max-w-[70ch] text-[13px] leading-relaxed text-white/45">
        Mod support is off for this game. The mod loader injects when the game starts, so switch it
        on in the launcher and start the game again.
      </p>
    );
  }

  const locked = busy || !!applying || !loaded || !profilesLoaded;
  const canApply = !!status?.toolchainInstalled && !!status?.gameEnabled;
  const missing = profile ? profile.modCount - profile.resolvedCount : 0;

  const loaderLabel =
    status?.toolchainInstalled && status.variant
      ? `${status.variant.toUpperCase()}${
          status.versions?.[status.variant] ? ` ${status.versions[status.variant]}` : ""
        } · loaded`
      : `${(status?.variant ?? "xxmi").toUpperCase()} · not installed`;

  const warnFailed = (title: string, failed: { folderName: string; error: string }[]) => {
    if (failed.length === 0) return;
    push({
      type: "warning",
      title,
      text: failed.map((f) => `${f.folderName}: ${f.error}`).join("; "),
    });
  };

  const toggle = async (mod: ModEntry, on: boolean) => {
    if (toggling.has(mod.modId)) return;
    setToggling((s) => new Set(s).add(mod.modId));
    setLocal([mod.folderName], on);
    let changed = 0;
    try {
      const result = await setModsEnabledBulk(gameId, [mod.folderName], on);
      if (result) {
        changed = result.changed.length;
        addPending(gameId, changed);
        warnFailed(`${mod.name} could not be switched ${on ? "on" : "off"}`, result.failed);
      }
    } finally {
      setToggling((s) => {
        const next = new Set(s);
        next.delete(mod.modId);
        return next;
      });
    }
    if (changed === 0) void refreshModsIfCurrent(gameId);
  };

  const setMembership = async (mod: ModEntry, joining: boolean) => {
    if (!profile) return;
    setBusy(true);
    try {
      await setMembers(
        gameId,
        profile.id,
        joining ? [mod.modId] : [],
        joining ? [] : [mod.modId],
      );
    } finally {
      setBusy(false);
    }
  };

  const uninstall = async (mod: ModEntry) => {
    setRowMenu(null);
    setBusy(true);
    try {
      await deleteMod(gameId, mod.folderName, mod.modId);
    } finally {
      setBusy(false);
    }
    await refreshAll();
  };

  const commitDraft = async () => {
    const name = draft.trim();
    const which = mode;
    setMode(null);
    if (!name) return;
    if (which === "new") {
      const created = await create(gameId, name);
      if (created) setSelectedProfileId(created.id);
      return;
    }
    if (which === "rename" && profile && name !== profile.name) {
      await rename(gameId, profile.id, name);
    }
  };

  const onDraftKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key !== "Enter" && e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    if (e.key === "Enter") void commitDraft();
    else setMode(null);
  };

  const applySelected = async () => {
    if (!profile || applying) return;
    const result = await apply(gameId, profile.id);
    void refreshModsIfCurrent(gameId);
    if (!result) return;
    if (canApply) addPending(gameId, result.enabled.length + result.disabled.length);
    warnFailed(`Some mods in ${profile.name} did not switch`, result.failed);
  };

  const deleteSelected = async () => {
    if (!profile) return;
    setConfirmDelete(false);
    await remove(gameId, profile.id);
    setSelectedProfileId(null);
  };

  const inside = (el: HTMLElement | null, x: number, y: number) => {
    const r = el?.getBoundingClientRect();
    return !!r && x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
  };
  const isOverTarget = (from: DragSource, x: number, y: number) =>
    inside(from === "installed" ? dropRef.current : installedRef.current, x, y);

  const endDrag = () => {
    pressRef.current = null;
    startedRef.current = false;
    setDrag(null);
  };

  const toggleFor = (mod: ModEntry): RowToggle =>
    !profile || profile.id === activeId
      ? {
          checked: mod.enabled,
          label: `Use ${mod.name}`,
          disabled: locked || toggling.has(mod.modId),
          onChange: (on) => void toggle(mod, on),
        }
      : {
          checked: inProfile.has(mod.modId),
          label: `Include ${mod.name} in ${profile.name}`,
          disabled: locked,
          onChange: (on) => void setMembership(mod, on),
        };

  const dragHandlersFor = (mod: ModEntry, from: DragSource): DragHandlers => ({
    onPointerDown: (e) => {
      if (e.button !== 0 || locked || !profile) return;
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
        setRowMenu(null);
      }
      if (ghostRef.current) ghostRef.current.style.transform = ghostTransform(e.clientX, e.clientY);
      const over = isOverTarget(press.from, e.clientX, e.clientY);
      setDrag((prev) =>
        prev && prev.modId === press.modId && prev.from === press.from && prev.over === over
          ? prev
          : { modId: press.modId, from: press.from, x: e.clientX, y: e.clientY, over },
      );
    },
    onPointerUp: (e) => {
      const press = pressRef.current;
      if (!press || press.pointerId !== e.pointerId) return;
      const dropped = startedRef.current && isOverTarget(press.from, e.clientX, e.clientY);
      endDrag();
      if (!dropped) return;
      void setMembership(mod, press.from === "installed");
    },
    onPointerCancel: endDrag,
  });

  const tone = (armed: boolean) =>
    !armed
      ? "border-transparent"
      : drag?.over
        ? "border-(--accent-b) bg-(--accent-b)/10"
        : "border-(--accent-b)/50";
  const profileTone = tone(drag?.from === "installed");
  const installedTone = tone(drag?.from === "profile");

  const progressPercent = progress ? Math.max(0, Math.min(100, progress.percentage)) : 0;

  return (
    <div className="flex flex-col gap-[18px]">
      <div className="flex flex-col gap-[10px]">
        <div className="flex items-center justify-between gap-3">
          <OvLabel>Profiles</OvLabel>
          <div className="flex shrink-0 items-center gap-2 rounded-[8px] border border-white/15 bg-white/[0.07] px-[11px] py-[6px]">
            <span
              className={`h-[6px] w-[6px] rounded-full ${
                status?.toolchainInstalled ? "bg-white" : "bg-white/35"
              }`}
            />
            <span className="text-[11.5px] font-medium">{loaderLabel}</span>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          {!profilesLoaded ? (
            <span className="text-[12px] text-white/40">Loading…</span>
          ) : (
            profiles.map((p) => {
              const isSelected = profile?.id === p.id;
              if (mode === "rename" && isSelected) {
                return (
                  <input
                    key={p.id}
                    ref={draftInput}
                    value={draft}
                    autoFocus
                    maxLength={60}
                    onChange={(e) => setDraft(e.target.value)}
                    onBlur={() => void commitDraft()}
                    onKeyDown={onDraftKey}
                    className={INPUT}
                  />
                );
              }
              return (
                <OvChip
                  key={p.id}
                  active={isSelected}
                  disabled={!!applying}
                  onClick={() => {
                    setSelectedProfileId(p.id);
                    setConfirmDelete(false);
                    setRowMenu(null);
                  }}
                >
                  <span className="flex items-center gap-[7px]">
                    {p.id === activeId && (
                      <span
                        className="h-[6px] w-[6px] shrink-0 rounded-full bg-white"
                        title="Active profile"
                      />
                    )}
                    <span>{p.name}</span>
                    <span className="ov-mono text-[10px] opacity-60">{p.resolvedCount}</span>
                  </span>
                </OvChip>
              );
            })
          )}
          {mode === "new" && (
            <input
              ref={draftInput}
              value={draft}
              autoFocus
              maxLength={60}
              placeholder="Profile name"
              onChange={(e) => setDraft(e.target.value)}
              onBlur={() => void commitDraft()}
              onKeyDown={onDraftKey}
              className={INPUT}
            />
          )}
          {profilesLoaded && profiles.length === 0 && mode !== "new" && (
            <span className="text-[12px] text-white/40">No profiles yet.</span>
          )}
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <OvButton
            disabled={locked || !profile}
            onClick={() => void applySelected()}
          >
            {applying ? "Applying…" : "Apply"}
          </OvButton>
          <span className="ml-auto flex flex-wrap items-center gap-2">
            {confirmDelete && profile ? (
              <>
                <span className="text-[12px] text-white/70">Delete {profile.name}?</span>
                <OvButton tone="danger" disabled={locked} onClick={() => void deleteSelected()}>
                  Yes
                </OvButton>
                <OvButton onClick={() => setConfirmDelete(false)}>No</OvButton>
              </>
            ) : (
              <>
                <OvButton
                  tone="dashed"
                  disabled={locked || mode !== null}
                  onClick={() => {
                    setDraft("");
                    setMode("new");
                  }}
                >
                  New
                </OvButton>
                <OvButton
                  disabled={locked || !profile || mode !== null}
                  onClick={() => {
                    if (!profile) return;
                    setDraft(profile.name);
                    setMode("rename");
                  }}
                >
                  Rename
                </OvButton>
                <OvButton
                  disabled={locked || !profile}
                  onClick={() => {
                    if (profile) void duplicate(gameId, profile.id);
                  }}
                >
                  Duplicate
                </OvButton>
                <OvButton
                  tone="danger"
                  disabled={locked || !profile || profiles.length <= 1}
                  onClick={() => setConfirmDelete(true)}
                >
                  Delete
                </OvButton>
              </>
            )}
          </span>
        </div>

        {profile && missing > 0 && (
          <p className="text-[12px] text-white/45">
            {missing} mod{missing === 1 ? " in" : "s in"} this profile
            {missing === 1 ? " is" : " are"} no longer installed.
          </p>
        )}
      </div>

      {pending > 0 && (
        <div className="flex items-start gap-3 rounded-[10px] border border-[#f59e0b]/30 bg-[#f59e0b]/[0.08] px-4 py-3 text-[12.5px] leading-relaxed text-[#fcd34d]">
          <span className="min-w-0 flex-1">
            {pending} change{pending === 1 ? "" : "s"} saved. Press{" "}
            <span className="ov-mono rounded-[5px] bg-white/[0.12] px-[6px] py-[2px] text-white/85">
              {reloadKey}
            </span>{" "}
            in the game to load {pending === 1 ? "it" : "them"}. {pending === 1 ? "It" : "They"}{" "}
            also load{pending === 1 ? "s" : ""} on {pending === 1 ? "its" : "their"} own the next
            time the game starts.
          </span>
          <OvButton onClick={() => clearPending(gameId)}>Done</OvButton>
        </div>
      )}

      {progress && (
        <div className="rounded-[10px] border border-white/[0.08] bg-white/[0.04] px-4 py-3">
          <div className="mb-2 flex items-center justify-between text-[12px] text-white/60">
            <span className="truncate">{progress.message}</span>
            <span className="ov-mono ml-3 shrink-0">{Math.round(progressPercent)}%</span>
          </div>
          <div className="flex">
            <OvBar percent={progressPercent} />
          </div>
        </div>
      )}

      {loaded && mods.length > 0 && (
        <label className="flex items-center gap-[9px] rounded-[9px] border border-white/[0.12] bg-black/[0.28] px-3 py-[8px]">
          <Search size={14} className="shrink-0 text-white/45" />
          <input
            ref={filter}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape" && search) {
                e.preventDefault();
                setSearch("");
              }
            }}
            placeholder={`Filter ${installed.length} mod${installed.length === 1 ? "" : "s"}`}
            className="w-full bg-transparent text-[12.5px] text-white outline-none placeholder:text-white/40"
          />
          <span className="ov-mono ml-auto shrink-0 rounded-[5px] bg-white/[0.08] px-[6px] py-[2px] text-[10.5px] text-white/55">
            /
          </span>
        </label>
      )}

      <Section
        title="Mods in Profile"
        count={loaded && profile ? members.length : null}
        tone={profileTone}
        sectionRef={dropRef}
      >
        {!loaded || !profilesLoaded ? (
          <p className="text-[12.5px] text-white/45">Loading…</p>
        ) : !profile ? (
          <Empty>Pick a profile to see the mods in it.</Empty>
        ) : members.length === 0 ? (
          <Empty>Add installed mods from the menu on each row, or drag them here.</Empty>
        ) : shownMembers.length === 0 ? (
          <Empty>Nothing matches that.</Empty>
        ) : (
          <OvList>
            {shownMembers.map((mod) => (
              <ProfileRow
                key={mod.modId}
                mod={mod}
                dragging={drag?.modId === mod.modId}
                dragHandlers={dragHandlersFor(mod, "profile")}
                busy={locked}
                toggle={toggleFor(mod)}
                onRemove={() => void setMembership(mod, false)}
              />
            ))}
          </OvList>
        )}
      </Section>

      <Section
        title="Mods Installed"
        count={loaded ? installed.length : null}
        tone={installedTone}
        sectionRef={installedRef}
        action={
          <OvButton
            disabled={rescanning}
            onClick={() => void rescan()}
          >
            {rescanning ? "Refreshing…" : "Refresh"}
          </OvButton>
        }
      >
        {!loaded ? (
          <p className="text-[12.5px] text-white/45">Loading…</p>
        ) : mods.length === 0 ? (
          <Empty>No mods are installed for this game. The Mods gallery tab can add some.</Empty>
        ) : visible.length === 0 ? (
          <Empty>Nothing matches that.</Empty>
        ) : (
          <OvList>
            {visible.map((mod) => (
              <InstalledRow
                key={mod.modId}
                mod={mod}
                canDrag={!!profile && !inProfile.has(mod.modId)}
                dragging={drag?.modId === mod.modId}
                dragHandlers={dragHandlersFor(mod, "installed")}
                busy={locked}
                toggle={toggleFor(mod)}
                profileName={profile?.name ?? null}
                inProfile={inProfile.has(mod.modId)}
                onAddToProfile={() => void setMembership(mod, true)}
                onRemoveFromProfile={() => void setMembership(mod, false)}
                menu={rowMenu}
                onMenu={setRowMenu}
                onUninstall={() => void uninstall(mod)}
              />
            ))}
          </OvList>
        )}
      </Section>

      {drag && draggedMod && (
        <DragGhost mod={draggedMod} x={drag.x} y={drag.y} ghostRef={ghostRef} />
      )}
    </div>
  );
}
