// ------------ Mod Profiles ------------
// The profiles list in the Mods Library. Create, rename, duplicate, delete and pick a profile, and apply one so
// only its mods are switched on in the game.
import { useEffect, useRef, useState } from "react";
import { Check, Copy, Layers, Pencil, Play, Plus, Trash2, X } from "lucide-react";
import type { GameId } from "../../types/game";
import { useModProfilesStore } from "../../store/modProfilesStore";
import { useModsStore } from "../../store/modsStore";
import { useModalStore } from "../../store/modalStore";
import { useNotificationStore } from "../../store/notificationStore";
import { ActionButton } from "../ui/ActionButton";
import { GroupBox } from "../ui/SettingsGroup";
import { TooltipPortal, useAnchoredTip } from "../ui/Tooltip";

const INPUT =
  "w-full rounded-[7px] border border-(--accent-b)/50 bg-white/[0.06] px-[10px] py-[7px] text-[12.5px] text-white outline-none placeholder:text-white/30";

function IconAction({
  children,
  label,
  onClick,
  disabled = false,
  danger = false,
}: {
  children: React.ReactNode;
  label: string;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
}) {
  const tip = useAnchoredTip<HTMLSpanElement>("bottom");
  return (
    <span ref={tip.anchorRef} {...tip.bind} className="inline-flex">
      <button
        type="button"
        aria-label={label}
        disabled={disabled}
        onClick={onClick}
        className={`inline-flex h-[30px] w-[30px] items-center justify-center rounded-ui border border-white/[0.08] bg-white/[0.04] transition duration-150 hover:bg-white/[0.1] disabled:cursor-not-allowed disabled:opacity-30 ${
          danger ? "text-[#fca5a5] hover:border-[#ef4444]/40" : "text-white/55 hover:text-white/85"
        }`}
      >
        {children}
      </button>
      {tip.shown && (
        <TooltipPortal x={tip.pos.x} y={tip.pos.y} placement="bottom">
          {label}
        </TooltipPortal>
      )}
    </span>
  );
}

export function ModsHubProfiles({
  gameId,
  canApply,
  selectedId,
  disabled,
  onSelect,
}: {
  gameId: GameId;
  canApply: boolean;
  selectedId: string | null;
  disabled: boolean;
  onSelect: (id: string) => void;
}) {
  const profiles = useModProfilesStore((s) => s.profiles);
  const activeId = useModProfilesStore((s) => s.activeId);
  const applying = useModProfilesStore((s) => s.applying);
  const loadedGameId = useModProfilesStore((s) => s.loadedGameId);
  const create = useModProfilesStore((s) => s.create);
  const rename = useModProfilesStore((s) => s.rename);
  const remove = useModProfilesStore((s) => s.remove);
  const duplicate = useModProfilesStore((s) => s.duplicate);
  const apply = useModProfilesStore((s) => s.apply);
  const refreshModsIfCurrent = useModsStore((s) => s.refreshIfCurrent);
  const openConfirm = useModalStore((s) => s.openConfirm);
  const push = useNotificationStore((s) => s.push);

  const [mode, setMode] = useState<"new" | "rename" | null>(null);
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setMode(null);
  }, [gameId]);

  useEffect(() => {
    if (mode) inputRef.current?.select();
  }, [mode]);

  const loaded = loadedGameId === gameId;
  const selected = profiles.find((p) => p.id === selectedId) ?? null;
  const locked = disabled || !!applying;
  const missing = selected ? selected.modCount - selected.resolvedCount : 0;

  const commit = async () => {
    const name = draft.trim();
    const which = mode;
    setMode(null);
    if (!name) return;
    if (which === "new") {
      const created = await create(gameId, name);
      if (created) onSelect(created.id);
    } else if (which === "rename" && selected && name !== selected.name) {
      await rename(gameId, selected.id, name);
    }
  };

  const onKey = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") void commit();
    if (e.key === "Escape") setMode(null);
  };

  const applySelected = async () => {
    if (!selected || applying) return;
    const result = await apply(gameId, selected.id);
    void refreshModsIfCurrent(gameId);
    if (!result) return;
    const changes = result.enabled.length + result.disabled.length;
    const parts: string[] = [];
    if (result.enabled.length) parts.push(`${result.enabled.length} on`);
    if (result.disabled.length) parts.push(`${result.disabled.length} off`);
    if (result.missing.length) parts.push(`${result.missing.length} no longer installed`);
    const offNote = "Mods load once they are turned on for this game.";
    const summary = canApply
      ? parts.join(", ") || "Nothing needed changing."
      : parts.length
        ? `${parts.join(", ")}. ${offNote}`
        : offNote;
    push({
      type: result.failed.length ? "warning" : "success",
      title: changes ? `Switched to ${selected.name}` : `${selected.name} was already applied`,
      text: result.failed.length
        ? result.failed.map((f) => `${f.folderName}: ${f.error}`).join("; ")
        : summary,
    });
  };

  const confirmDelete = () => {
    if (!selected) return;
    openConfirm({
      title: `Delete the "${selected.name}" profile?`,
      message:
        "This only forgets the list of mods. None of your mod files are deleted, and whatever is switched on right now stays on.",
      confirmLabel: "Delete profile",
      danger: true,
      onConfirm: () => {
        void remove(gameId, selected.id);
      },
    });
  };

  return (
    <GroupBox className="self-start">
      <div className="flex items-center justify-between px-4 py-3">
        <span className="flex items-center gap-2 text-[12.5px] font-medium text-white/55">
          <Layers size={14} />
          Profiles
        </span>
        <IconAction
          label="New profile"
          disabled={locked || mode !== null}
          onClick={() => {
            setDraft("");
            setMode("new");
          }}
        >
          <Plus size={14} />
        </IconAction>
      </div>

      <div className="flex flex-col gap-[2px] p-[5px]">
        {!loaded ? (
          <div className="h-[34px] animate-pulse rounded-[7px] bg-white/[0.04]" />
        ) : (
          profiles.map((p) => {
            const isActive = p.id === activeId;
            const isSelected = selected?.id === p.id;
            if (mode === "rename" && isSelected) {
              return (
                <input
                  key={p.id}
                  ref={inputRef}
                  value={draft}
                  autoFocus
                  maxLength={60}
                  onChange={(e) => setDraft(e.target.value)}
                  onBlur={() => void commit()}
                  onKeyDown={onKey}
                  className={INPUT}
                />
              );
            }
            return (
              <button
                key={p.id}
                type="button"
                aria-pressed={isSelected}
                onClick={() => onSelect(p.id)}
                onDoubleClick={() => {
                  if (locked) return;
                  onSelect(p.id);
                  setDraft(p.name);
                  setMode("rename");
                }}
                className={`flex w-full items-center gap-2 rounded-[7px] px-[10px] py-[8px] text-left transition-colors ${
                  isSelected
                    ? "bg-white/[0.10] text-white"
                    : "text-white/70 hover:bg-white/[0.06] hover:text-white"
                }`}
              >
                <span className="min-w-0 flex-1 truncate text-[12.5px] font-medium">{p.name}</span>
                {isActive && (
                  <span
                    role="img"
                    aria-label="Active profile"
                    title="Active profile"
                    className="shrink-0 text-(--accent-text)"
                  >
                    <Check size={13} strokeWidth={2.5} aria-hidden />
                  </span>
                )}
                <span
                  title={`${p.resolvedCount} installed mod${p.resolvedCount === 1 ? "" : "s"}`}
                  className="shrink-0 text-[11px] tabular-nums text-white/35"
                >
                  {p.resolvedCount}
                  <span className="sr-only"> installed mod{p.resolvedCount === 1 ? "" : "s"}</span>
                </span>
              </button>
            );
          })
        )}
        {mode === "new" && (
          <input
            ref={inputRef}
            value={draft}
            autoFocus
            maxLength={60}
            placeholder="Profile name"
            onChange={(e) => setDraft(e.target.value)}
            onBlur={() => void commit()}
            onKeyDown={onKey}
            className={INPUT}
          />
        )}
        {loaded && profiles.length === 0 && mode !== "new" && (
          <p className="px-[10px] py-2 text-[12px] text-white/40">No profiles yet.</p>
        )}
      </div>

      {selected && missing > 0 && (
        <p className="flex items-center gap-1.5 px-4 py-3 text-[12px] text-white/45">
          <X size={12} className="shrink-0" />
          {missing} mod{missing === 1 ? " in" : "s in"} this profile
          {missing === 1 ? " is" : " are"} no longer installed.
        </p>
      )}

      <div className="flex flex-wrap items-center gap-[6px] px-3 py-3">
        <ActionButton
          variant="accent"
          icon={<Play size={14} />}
          disabled={locked || !selected}
          onClick={() => void applySelected()}
        >
          {applying ? "Applying…" : "Apply"}
        </ActionButton>
        <span className="ml-auto flex gap-[6px]">
          <IconAction
            label="Rename"
            disabled={locked || !selected || mode !== null}
            onClick={() => {
              if (!selected) return;
              setDraft(selected.name);
              setMode("rename");
            }}
          >
            <Pencil size={14} />
          </IconAction>
          <IconAction
            label="Duplicate"
            disabled={locked || !selected}
            onClick={() => {
              if (selected) void duplicate(gameId, selected.id);
            }}
          >
            <Copy size={14} />
          </IconAction>
          <IconAction
            label={profiles.length > 1 ? "Delete" : "You need at least one profile"}
            danger
            disabled={locked || !selected || profiles.length <= 1}
            onClick={confirmDelete}
          >
            <Trash2 size={14} />
          </IconAction>
        </span>
      </div>
    </GroupBox>
  );
}
