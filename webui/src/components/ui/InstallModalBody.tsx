// ------------ Install Dialog ------------
// The contents of the install dialog. Lets you choose the install folder, shows free space and download size,
// offers voice packs and resource quality where the game has them, and starts the download. Can also find a game
// that is already on this PC.
import { useCallback, useEffect, useRef, useState } from "react";
import {
  AlertTriangle,
  Download,
  FolderOpen,
  HardDrive,
  Loader2,
  FolderSearch,
  ScanSearch,
  ExternalLink,
} from "lucide-react";
import { gameById, isManaged } from "../../data/games";
import type { GameId } from "../../types/game";
import { useNotificationStore } from "../../store/notificationStore";
import { useGamesStore } from "../../store/gamesStore";
import { useCustomizationStore } from "../../store/customizationStore";
import { openExternal } from "../../lib/tauri";
import { fmtBytes } from "../../lib/format";
import {
  checkLinkedInstall,
  LINKED_UNCHECKED_TEXT,
  useResourceQuality,
} from "../../lib/gameActions";
import { SegmentedControl } from "./SegmentedControl";
import { Hint } from "./Tooltip";
import { Toggle } from "./Toggle";
import { MODAL_PRIMARY, MODAL_SECONDARY } from "./modalButtons";
import {
  getDefaultInstallPath,
  getDiskSpace,
  getInstallPreview,
  getContentPacks,
  setContentPacks,
  selectInstallDirectory,
  startDownload,
  locateGameInstall,
  detectDefaultGameInstall,
  getSteamInstall,
  DEFAULT_VOICE_PACK,
  RESOURCE_QUALITY_OPTIONS,
  VOICE_PACK_OPTIONS,
  type ContentPack,
  type InstallPreview,
  type ResourceQuality,
  type VoicePackLanguage,
} from "../../lib/ipc";

const INVALID_PATH_CHARS = /[<>:"/\\|?*]/g;

function folderNameFor(name: string): string {
  return name.replace(INVALID_PATH_CHARS, "").trim().replace(/[.\s]+$/, "");
}

function withGameFolder(picked: string, folder: string): string {
  if (!folder) return picked;
  const trimmed = picked.replace(/[\\/]+$/, "");
  const last = trimmed.split(/[\\/]/).pop() ?? "";
  if (last.toLowerCase() === folder.toLowerCase()) return trimmed;
  return `${trimmed}\\${folder}`;
}

function leafOf(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const cut = Math.max(trimmed.lastIndexOf("\\"), trimmed.lastIndexOf("/"));
  return cut >= 0 ? trimmed.slice(cut + 1) : trimmed;
}

function parentOf(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  return trimmed.slice(0, trimmed.length - leafOf(trimmed).length);
}

function fmtSize(bytes: number | null, approximate: boolean): string {
  if (bytes === null || bytes <= 0) return "—";
  return `${approximate ? "~" : ""}${fmtBytes(bytes)}`;
}

function previewReason(error: string | null): string {
  if (error && error.length <= 100 && /[.!]$/.test(error) && !/https?:|[{}]|HTTP|status|error/i.test(error))
    return error;
  return "Could not reach the update server.";
}

function InfoLine({
  label,
  value,
  pending,
}: {
  label: string;
  value: string | null;
  pending: boolean;
}) {
  return (
    <div className="flex items-center justify-between px-[12px] py-[9px]">
      <span className="text-[12.5px] text-white/50">{label}</span>
      {pending ? (
        <span className="h-[11px] w-[64px] animate-pulse rounded-full bg-white/10" />
      ) : (
        <span className="text-[12.5px] font-medium tabular-nums text-white/80">{value || "—"}</span>
      )}
    </div>
  );
}

export function InstallModalBody({ gameId, onClose }: { gameId: GameId; onClose: () => void }) {
  const game = gameById(gameId);
  const managed = isManaged(gameId);
  const push = useNotificationStore((s) => s.push);
  const [path, setPath] = useState("");
  const [maxRootLength, setMaxRootLength] = useState(0);
  const [folderName, setFolderName] = useState("");
  const [free, setFree] = useState(0);
  const [loading, setLoading] = useState(true);
  const [starting, setStarting] = useState(false);
  const [detecting, setDetecting] = useState(false);
  const [preview, setPreview] = useState<InstallPreview | null>(null);
  const [previewFailed, setPreviewFailed] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [packs, setPacks] = useState<ContentPack[] | null>(null);
  const [packsBusy, setPacksBusy] = useState(false);
  const [packsFailed, setPacksFailed] = useState(false);
  const voice = useCustomizationStore((s) => s.voicePack[gameId]) ?? DEFAULT_VOICE_PACK;
  const setVoicePack = useCustomizationStore((s) => s.setVoicePack);
  const resourceQuality = useResourceQuality(
    gameId,
    { installed: false, steamCopy: false, jobActive: false },
    { sizes: true },
  );
  const [qualityBusy, setQualityBusy] = useState(false);
  const previewRequest = useRef(0);
  const packsRequest = useRef(0);

  const rootLength = path.replace(/[\\/]+$/, "").length;
  const tooDeep = maxRootLength > 0 && rootLength > maxRootLength;

  useEffect(() => {
    if (!managed) return;
    let alive = true;
    void (async () => {
      const def = await getDefaultInstallPath(gameId);
      if (alive && def?.path) setPath(def.path);
      if (alive) setMaxRootLength(def?.maxRootLength ?? 0);
      if (alive) setFolderName(def?.folderName ?? "");
      const disk = await getDiskSpace(def?.path);
      if (alive && disk) setFree(disk.free);
      if (alive) setLoading(false);
    })();
    return () => {
      alive = false;
    };
  }, [gameId, managed]);

  const loadPreview = useCallback(async () => {
    const token = ++previewRequest.current;
    setPreview(null);
    setPreviewFailed(false);
    setPreviewError(null);
    const { preview: p, error } = await getInstallPreview(gameId);
    if (token !== previewRequest.current) return;
    if (p && !p.notSupported) setPreview(p);
    else {
      setPreviewError(error);
      setPreviewFailed(true);
    }
  }, [gameId]);

  const loadPacks = useCallback(async () => {
    if (!game.contentPackChoice) return;
    const token = ++packsRequest.current;
    setPacksFailed(false);
    const r = await getContentPacks(gameId);
    if (token !== packsRequest.current || !r.supported) return;
    if (r.error) setPacksFailed(true);
    else setPacks(r.packs);
  }, [gameId, game.contentPackChoice]);

  useEffect(() => {
    if (!managed) return;
    let alive = true;
    void loadPreview().then(() => {
      if (alive) void loadPacks();
    });
    return () => {
      alive = false;
      packsRequest.current += 1;
    };
  }, [managed, loadPreview, loadPacks]);

  const retryDetails = async () => {
    await loadPreview();
    if (!packs) await loadPacks();
  };

  const togglePack = async (tag: string, on: boolean) => {
    if (!packs || packsBusy) return;
    const prev = packs;
    const next = packs.map((p) => (p.tag === tag ? { ...p, selected: on } : p));
    setPacks(next);
    setPacksBusy(true);
    try {
      const chosen = next.filter((p) => p.selected).map((p) => p.tag);
      const saved = await setContentPacks(gameId, chosen.length === next.length ? null : chosen);
      if (!saved) setPacks(prev);
      else await loadPreview();
    } finally {
      setPacksBusy(false);
    }
  };

  const pickVoice = async (language: VoicePackLanguage) => {
    if (language === voice) return;
    await setVoicePack(gameId, language);
    await loadPreview();
  };

  const pickQuality = async (quality: ResourceQuality) => {
    if (quality === resourceQuality.quality || qualityBusy) return;
    setQualityBusy(true);
    try {
      await resourceQuality.setQuality(quality);
      await loadPreview();
    } finally {
      setQualityBusy(false);
    }
  };

  const pickFolder = async () => {
    const p = await selectInstallDirectory();
    if (!p) return;
    const target = withGameFolder(p, folderName || folderNameFor(game.name));
    setPath(target);
    const disk = await getDiskSpace(target);
    if (disk) setFree(disk.free);
  };

  const begin = () => {
    if (!path || starting || tooDeep) return;
    setStarting(true);
    void startDownload(path, gameId, () =>
      push({
        title: `Installing ${game.name}`,
        text: "Added to the download queue. Track it on the Downloads page.",
      }),
    );
    onClose();
  };

  const linkGame = async (title: string, text: string) => {
    void useCustomizationStore.getState().hydrate();
    const installed = useGamesStore.getState().installed;
    if (!installed.includes(gameId)) useGamesStore.getState().setInstalled([...installed, gameId]);
    const linked = managed && !(await getSteamInstall(gameId)).isSteamInstall;
    const checking = linked && checkLinkedInstall(gameId);
    push({
      type: "success",
      title,
      text: checking ? text : linked ? LINKED_UNCHECKED_TEXT : "Linked your existing installation.",
    });
    onClose();
  };

  const locate = async () => {
    const r = await locateGameInstall(gameId);
    if (r.ok) {
      await linkGame(
        `${game.name} located`,
        "Verifying your installation. Track it on the Downloads page.",
      );
    } else if (r.error) {
      push({ type: "warning", title: `Couldn't locate ${game.name}`, text: r.error });
    }
  };

  const detectDefault = async () => {
    if (detecting) return;
    setDetecting(true);
    try {
      const r = await detectDefaultGameInstall(gameId);
      if (r.ok) {
        await linkGame(
          `${game.name} found`,
          "Found it at Peebify's default install location. Verifying it now.",
        );
      } else if (r.error) {
        push({ type: "warning", title: `Couldn't find ${game.name}`, text: r.error });
      }
    } finally {
      setDetecting(false);
    }
  };

  const betaNote = (spacing: string) =>
    game.betaNote && (
      <div
        className={`${spacing} flex items-start gap-[9px] rounded-ui border border-(--color-warning,#fcd34d)/30 bg-(--color-warning,#fcd34d)/[0.08] px-[12px] py-[10px]`}
      >
        <AlertTriangle size={15} className="mt-[1px] shrink-0 text-(--color-warning,#fcd34d)" />
        <p className="text-[12.5px] leading-[1.5] text-white/85">{game.betaNote}</p>
      </div>
    );

  if (!managed) {
    const steamUrl = game.installHelpUrl;
    return (
      <div className="p-[22px]">
        <div className="mb-[6px] flex items-center gap-[10px]">
          <Download size={18} className="text-white/80" />
          <h2 className="text-[16px] font-semibold">Install {game.name}</h2>
        </div>
        <p className="mb-[18px] text-[13px] leading-[1.55] text-white/60">
          {game.name} is installed and updated through Steam. Install it there, then use{" "}
          <span className="text-white/80">Locate existing</span> to point Peebify at it.
        </p>
        {betaNote("mb-[18px]")}
        <div className="flex justify-end gap-[10px]">
          <button
            onClick={() => void locate()}
            className="flex items-center gap-[6px] rounded-ui border border-white/15 bg-white/[0.05] px-[14px] py-[8px] text-[13px] font-medium text-white transition duration-150 active:scale-[0.97] hover:bg-white/10"
          >
            <FolderSearch size={14} /> Locate existing
          </button>
          {steamUrl && (
            <button
              onClick={() => {
                void openExternal(steamUrl);
                onClose();
              }}
              className={MODAL_PRIMARY}
            >
              <ExternalLink size={14} /> Open in Steam
            </button>
          )}
        </div>
      </div>
    );
  }

  const installBytes = preview?.installBytes ?? preview?.downloadBytes ?? null;
  const requiredBytes = preview?.requiredBytes ?? installBytes;
  const shortOnSpace = requiredBytes !== null && free > 0 && free < requiredBytes;
  const includes = preview?.parts.map((p) => p.label).join(" · ") ?? "";

  return (
    <div className="p-[22px]">
      <div className="mb-[6px] flex items-center gap-[10px]">
        <Download size={18} className="text-white/80" />
        <h2 className="text-[16px] font-semibold">Install {game.name}</h2>
      </div>

      <div className="mb-[6px] mt-[16px] text-[11px] font-medium uppercase tracking-[0.6px] text-white/55">
        Install folder
      </div>
      <div className="mb-[12px] flex items-center gap-[10px]">
        <Hint tip={path || undefined} className="flex min-w-0 flex-1">
          <div className="flex min-w-0 flex-1 items-center rounded-ui border border-white/[0.08] bg-black/30 px-[12px] py-[9px] text-[12.5px]">
            {loading || !path ? (
              <span className="truncate text-white/80">
                {loading ? "Resolving default location…" : "No location selected"}
              </span>
            ) : (
              <>
                <span className="truncate text-white/45">{parentOf(path)}</span>
                <span className="shrink-0 text-white/90">{leafOf(path)}</span>
              </>
            )}
          </div>
        </Hint>
        <button
          onClick={() => void pickFolder()}
          className="flex shrink-0 items-center gap-[7px] rounded-ui border border-white/[0.08] bg-white/[0.04] px-[12px] py-[9px] text-[12.5px] font-medium transition duration-150 active:scale-[0.97] hover:bg-white/[0.08]"
        >
          <FolderOpen size={14} /> Browse
        </button>
      </div>

      {betaNote("mb-[12px]")}

      {game.resourceQualityChoice && (
        <>
          <div className="mb-[6px] text-[11px] font-medium uppercase tracking-[0.6px] text-white/55">
            Resource quality
          </div>
          <div role="radiogroup" aria-label="Resource quality" className="mb-[6px] grid grid-cols-3 gap-[8px]">
            {RESOURCE_QUALITY_OPTIONS.map(({ value, label }) => {
              const active = value === resourceQuality.quality;
              const size = resourceQuality.sizes?.[value];
              return (
                <button
                  key={value}
                  role="radio"
                  aria-checked={active}
                  disabled={qualityBusy}
                  onClick={() => void pickQuality(value)}
                  className={`rounded-ui border px-[12px] py-[9px] text-left transition duration-150 active:scale-[0.98] disabled:cursor-wait ${
                    active
                      ? "border-(--accent-a) bg-white/[0.09]"
                      : "border-white/[0.08] bg-white/[0.04] hover:bg-white/[0.07]"
                  }`}
                >
                  <div className={`text-[13px] font-semibold ${active ? "text-white" : "text-white/80"}`}>
                    {label}
                  </div>
                  {size || resourceQuality.loaded ? (
                    <div className="text-[11px] tabular-nums text-white/55">
                      {size ? fmtBytes(size.installBytes) : "—"}
                    </div>
                  ) : (
                    <div className="mt-[4px] h-[9px] w-[44px] animate-pulse rounded-full bg-white/10" />
                  )}
                </button>
              );
            })}
          </div>
          <p className="mb-[12px] text-[11.5px] leading-[1.5] text-white/55">
            Higher quality looks sharper but takes more disk space. You can switch later in settings.
          </p>
        </>
      )}

      {game.voicePackChoice && (
        <>
          <div className="mb-[6px] text-[11px] font-medium uppercase tracking-[0.6px] text-white/55">
            Voice pack
          </div>
          <div className="mb-[12px] flex items-center justify-between gap-[10px] rounded-ui border border-white/[0.08] bg-white/[0.04] px-[12px] py-[8px]">
            <span className="text-[12.5px] text-white/50">
              Audio language. You can change this later.
            </span>
            <SegmentedControl
              ariaLabel="Voice-over language"
              options={VOICE_PACK_OPTIONS}
              value={voice}
              onChange={(v) => void pickVoice(v as VoicePackLanguage)}
            />
          </div>
        </>
      )}

      {packs && packs.length > 0 && (
        <>
          <div className="mb-[6px] text-[11px] font-medium uppercase tracking-[0.6px] text-white/55">
            Voice packs
          </div>
          <div className="mb-[12px] overflow-hidden rounded-ui border border-white/[0.08] bg-white/[0.04] divide-y divide-white/[0.06]">
            <div className="px-[12px] py-[8px] text-[12.5px] text-white/50">
              Spoken audio installed with the game. You can change this later.
            </div>
            {packs.map((pack) => (
              <div key={pack.tag} className="flex items-center justify-between gap-[10px] px-[12px] py-[7px]">
                <div className="min-w-0">
                  <div className="truncate text-[12.5px] font-medium text-white/80">
                    {pack.language ?? pack.tag}
                  </div>
                  <div className="truncate text-[11px] tabular-nums text-white/55">
                    {pack.language ? `${pack.tag} · ${fmtBytes(pack.bytes)}` : fmtBytes(pack.bytes)}
                  </div>
                </div>
                <Toggle
                  checked={pack.selected}
                  disabled={packsBusy}
                  ariaLabel={pack.language ?? pack.tag}
                  onChange={(v) => void togglePack(pack.tag, v)}
                />
              </div>
            ))}
          </div>
        </>
      )}
      {!packs && packsFailed && (
        <>
          <div className="mb-[6px] text-[11px] font-medium uppercase tracking-[0.6px] text-white/55">
            Voice packs
          </div>
          <div className="mb-[12px] flex items-center justify-between gap-[10px] rounded-ui border border-white/[0.08] bg-white/[0.04] px-[12px] py-[8px]">
            <span className="text-[12.5px] text-white/50">Couldn't load the voice pack list.</span>
            <button
              onClick={() => void loadPacks()}
              className="shrink-0 text-[12.5px] font-medium text-white/60 underline-offset-2 transition-colors duration-150 hover:text-white hover:underline"
            >
              Retry
            </button>
          </div>
        </>
      )}

      <div className="mb-[10px] overflow-hidden rounded-ui border border-white/[0.08] bg-white/[0.04] divide-y divide-white/[0.06]">
        <InfoLine label="Version" value={preview?.version ?? null} pending={!preview && !previewFailed} />
        <InfoLine
          label="Download"
          value={preview ? fmtSize(preview.downloadBytes, preview.approximate) : null}
          pending={!preview && !previewFailed}
        />
        {(!preview || preview.installBytes !== null) && (
          <InfoLine
            label="Size on disk"
            value={preview ? fmtSize(preview.installBytes, preview.approximate) : null}
            pending={!preview && !previewFailed}
          />
        )}
        <div className="flex items-center justify-between px-[12px] py-[9px]">
          <span className="flex items-center gap-[7px] text-[12.5px] text-white/50">
            <HardDrive size={13} /> Free on drive
          </span>
          <span
            className={`text-[12.5px] font-medium tabular-nums ${shortOnSpace ? "text-(--color-warning,#fcd34d)" : "text-white/80"}`}
          >
            {free > 0 ? fmtBytes(free) : "—"}
          </span>
        </div>
      </div>

      {includes && (
        <div className="mb-[12px] text-[11.5px] leading-[1.5] text-white/55">Includes {includes}</div>
      )}
      {previewFailed && (
        <div className="mb-[12px] text-[11.5px] text-white/55">
          Size details unavailable. {previewReason(previewError)}{" "}
          <button
            onClick={() => void retryDetails()}
            className="font-medium text-white/60 underline-offset-2 transition-colors duration-150 hover:text-white hover:underline"
          >
            Retry
          </button>
        </div>
      )}
      {shortOnSpace && (
        <div className="mb-[12px] flex items-start gap-[9px] rounded-ui border border-(--color-warning,#fcd34d)/30 bg-(--color-warning,#fcd34d)/[0.08] px-[12px] py-[10px]">
          <AlertTriangle size={15} className="mt-[1px] shrink-0 text-(--color-warning,#fcd34d)" />
          <p className="text-[12.5px] leading-[1.5] text-white/85">
            Needs about {fmtBytes(requiredBytes)} free while installing. Only {fmtBytes(free)} is free on
            this drive. Pick another location or free up space first.
          </p>
        </div>
      )}
      {tooDeep && (
        <div className="mb-[12px] flex items-start gap-[9px] rounded-ui border border-(--color-danger,#f87171)/30 bg-(--color-danger,#f87171)/[0.08] px-[12px] py-[10px]">
          <AlertTriangle size={15} className="mt-[1px] shrink-0 text-(--color-danger-soft,#fca5a5)" />
          <p className="text-[12.5px] leading-[1.5] text-white/85">
            This folder is {rootLength - maxRootLength} character
            {rootLength - maxRootLength === 1 ? "" : "s"} too deep for {game.name}. Windows won't let
            the game reach its own files from here, so its updates would fail after installing. Pick
            a folder of at most {maxRootLength} characters. One directly off {"C:\\"} works.
          </p>
        </div>
      )}

      <div className="mb-[16px] flex gap-[10px]">
        <button
          onClick={() => void locate()}
          className="flex flex-1 items-center justify-center gap-[7px] rounded-ui border border-white/[0.08] bg-white/[0.03] py-[9px] text-[12.5px] font-medium text-white/70 transition duration-150 active:scale-[0.97] hover:bg-white/[0.07]"
        >
          <FolderSearch size={14} /> Locate existing install
        </button>
        <button
          disabled={detecting}
          onClick={() => void detectDefault()}
          className="flex flex-1 items-center justify-center gap-[7px] rounded-ui border border-white/[0.08] bg-white/[0.03] py-[9px] text-[12.5px] font-medium text-white/70 transition duration-150 active:scale-[0.97] hover:bg-white/[0.07] disabled:cursor-not-allowed disabled:opacity-50"
        >
          {detecting ? <Loader2 size={14} className="animate-spin" /> : <ScanSearch size={14} />} Check default
          location
        </button>
      </div>

      <div className="flex justify-end gap-[10px]">
        <button onClick={onClose} className={MODAL_SECONDARY}>
          Cancel
        </button>
        <button disabled={!path || loading || starting || tooDeep} onClick={begin} className={MODAL_PRIMARY}>
          {starting ? <Loader2 size={14} className="animate-spin" /> : <Download size={14} />} Install
        </button>
      </div>
    </div>
  );
}
