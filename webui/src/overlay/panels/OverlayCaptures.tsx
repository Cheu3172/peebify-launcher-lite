// ------------ Overlay Captures ------------
// The Captures tab of the overlay: screenshot and recording settings, the record state, and a browsable
// list of your screenshots and clips grouped by game.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AlertTriangle, X } from "lucide-react";
import { convertFileSrc } from "@tauri-apps/api/core";
import {
  chooseCaptureFolder,
  getOverlayConfig,
  onEvent,
  openCaptureFolder,
  resetCaptureFolder,
  rpc,
  type OverlayAudioTrack,
  type OverlayRecEstimate,
} from "../ipc";
import { useSettingsStore } from "../../store/settingsStore";
import { fmtBytes, fmtRelTime } from "../../lib/format";
import { useNow } from "../../lib/useRelativeTime";
import { spacedAccelerator } from "../../lib/hotkeys";
import { OvCard, OvChip, OvKey, OvLabel, OvMeter, OvSeg, OvToggle } from "../ui";
import { CaptureViewer, type ViewerCapture } from "../CaptureViewer";
import { GAMES } from "../../data/games";
import { useCustomizationStore } from "../../store/customizationStore";
import { iconFallback, iconSrcFor } from "../../lib/customMedia";

const UNGROUPED = "__ungrouped";
const PAGE = 60;
const PENDING_TIMEOUT_MS = 15_000;

type Capture = ViewerCapture & { thumbPath?: string | null };

type GameGroup = { id: string; name: string; known: boolean; items: Capture[] };

type Notice = { text: string; warning?: boolean };

function groupByGame(list: Capture[]): GameGroup[] {
  const byGame = new Map<string, Capture[]>();
  for (const capture of list) {
    const key = capture.gameId ?? UNGROUPED;
    const items = byGame.get(key);
    if (items) items.push(capture);
    else byGame.set(key, [capture]);
  }

  const ordered: GameGroup[] = [];
  for (const game of GAMES) {
    const items = byGame.get(game.id);
    if (items?.length) {
      ordered.push({ id: game.id, name: game.name, known: true, items });
    }
  }
  for (const [id, items] of byGame) {
    if (id !== UNGROUPED && !GAMES.some((game) => game.id === id)) {
      ordered.push({ id, name: id, known: false, items });
    }
  }
  const rest = byGame.get(UNGROUPED);
  if (rest?.length) ordered.push({ id: UNGROUPED, name: "Other", known: false, items: rest });
  return ordered;
}

export function OverlayCaptures() {
  const values = useSettingsStore((s) => s.values);
  const set = useSettingsStore((s) => s.set);
  const icons = useCustomizationStore((s) => s.gameIcons);
  const hydrateIcons = useCustomizationStore((s) => s.hydrate);
  const [captures, setCaptures] = useState<Capture[]>([]);
  const [supported, setSupported] = useState<boolean | null>(null);
  const [recordingSupported, setRecordingSupported] = useState(true);
  const [folder, setFolder] = useState("");
  const [folderIsDefault, setFolderIsDefault] = useState(true);
  const [tracks, setTracks] = useState<OverlayAudioTrack[]>([]);
  const [filter, setFilter] = useState<"All" | "Screenshots" | "Clips">("All");
  const [error, setError] = useState<Notice | null>(null);
  const [viewerError, setViewerError] = useState<string | null>(null);
  const [viewing, setViewing] = useState<string | null>(null);
  const viewingRef = useRef<string | null>(null);
  viewingRef.current = viewing;
  const [recording, setRecording] = useState(false);
  const [pending, setPending] = useState<"starting" | "stopping" | null>(null);
  const [since, setSince] = useState(0);
  const [shotHotkey, setShotHotkey] = useState("");
  const [recHotkey, setRecHotkey] = useState("");
  const [discordClients, setDiscordClients] = useState<{ id: string; label: string }[]>([]);
  const [estimate, setEstimate] = useState<OverlayRecEstimate | null>(null);
  const [shooting, setShooting] = useState(false);
  const [armed, setArmed] = useState<string | null>(null);
  const [limit, setLimit] = useState(PAGE);
  const [qualityDraft, setQualityDraft] = useState<number | null>(null);
  const viewedIndex = useRef(0);
  const drawerOpen = useRef(true);
  const now = useNow();

  useEffect(() => {
    if (!armed) return;
    const timer = window.setTimeout(() => setArmed(null), 3000);
    return () => window.clearTimeout(timer);
  }, [armed]);

  const load = useCallback(async () => {
    const list = await rpc<{ captures?: Capture[] }>("overlay-list-captures");
    setCaptures(list?.captures ?? []);
  }, []);

  const loadConfig = useCallback(async () => {
    const config = await getOverlayConfig();
    setFolder(config?.captureFolder ?? "");
    setFolderIsDefault(config?.captureFolderIsDefault ?? true);
    setTracks(config?.audioTracks ?? []);
    setEstimate(config?.recEstimate ?? null);
    const hotkeys = config?.hotkeys ?? [];
    setShotHotkey(hotkeys.find((h) => h.id === "overlayShotHotkey")?.accelerator ?? "");
    setRecHotkey(hotkeys.find((h) => h.id === "overlayRecHotkey")?.accelerator ?? "");
    const discord = await rpc<{ clients?: { id: string; label: string }[] }>(
      "discord-audio-clients",
    );
    setDiscordClients(discord?.clients ?? []);
  }, []);

  const loadRecording = useCallback(async () => {
    const status = await rpc<{ recording?: boolean; recordingSinceMs?: number }>(
      "get-overlay-status",
    );
    setRecording(status?.recording === true);
    setSince(status?.recordingSinceMs ?? 0);
  }, []);

  useEffect(() => {
    void rpc<{ supported?: boolean; recordingSupported?: boolean }>("overlay-capture-status").then(
      (s) => {
        setSupported(s?.supported === true);
        setRecordingSupported(s?.recordingSupported !== false);
      },
    );
    void load();
    void loadConfig();
    void loadRecording();
    void hydrateIcons();

    let disposed = false;
    const offs: Array<() => void> = [];
    const listen = (name: string, handler: (payload: unknown) => void) => {
      void onEvent(name, handler).then((off) => {
        if (disposed) off();
        else offs.push(off);
      });
    };
    const loadIfOpen = () => {
      if (drawerOpen.current && document.visibilityState !== "hidden") void load();
    };
    listen("overlay-capture", loadIfOpen);
    listen("overlay-capture-deleted", loadIfOpen);
    listen("overlay-closed", () => {
      drawerOpen.current = false;
    });
    listen("overlay-opened", () => {
      drawerOpen.current = true;
      void load();
      void loadConfig();
      void loadRecording();
      void hydrateIcons();
    });
    listen("overlay-record-starting", () => setPending("starting"));
    listen("overlay-record-started", (payload) => {
      const detail = payload as { audioWarnings?: string[] } | undefined;
      const warnings = detail?.audioWarnings ?? [];
      setError(warnings.length ? { text: warnings.join(" "), warning: true } : null);
      setPending(null);
      void loadRecording();
    });
    listen("overlay-record-stopped", (payload) => {
      const detail = payload as { reason?: string | null } | undefined;
      if (detail?.reason) setError({ text: detail.reason, warning: true });
      setPending(null);
      void loadRecording();
      loadIfOpen();
    });
    listen("overlay-record-failed", (payload) => {
      const detail = payload as { error?: string } | undefined;
      setError({ text: detail?.error ?? "Recording failed." });
      setPending(null);
      void loadRecording();
    });
    listen("overlay-record-cancelled", () => {
      setPending(null);
      void loadRecording();
    });
    return () => {
      disposed = true;
      offs.forEach((off) => off());
    };
  }, [load, loadConfig, loadRecording, hydrateIcons]);

  useEffect(() => {
    if (!pending) return;
    const timer = window.setTimeout(() => {
      setPending(null);
      void loadRecording();
    }, PENDING_TIMEOUT_MS);
    return () => window.clearTimeout(timer);
  }, [pending, loadRecording]);

  const closeViewer = useCallback(() => {
    setViewing(null);
    setViewerError(null);
  }, []);

  useEffect(() => {
    if (!viewerError) return;
    const timer = window.setTimeout(() => setViewerError(null), 5000);
    return () => window.clearTimeout(timer);
  }, [viewerError]);

  const write = (key: string, value: string) => {
    set(key, value);
  };

  const flag = (key: string, fallback: boolean) =>
    values[key] === undefined ? fallback : values[key] === "true";

  const selectedTracks = (values.overlayAudioTracks ?? "desktop,mic")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);

  const PER_APP = ["game", "discord"];

  const toggleTrack = (id: string) => {
    let next = selectedTracks.includes(id)
      ? selectedTracks.filter((x) => x !== id)
      : [...selectedTracks, id];
    if (!selectedTracks.includes(id)) {
      if (id === "desktop") next = next.filter((x) => !PER_APP.includes(x));
      if (PER_APP.includes(id)) next = next.filter((x) => x !== "desktop");
    }
    write("overlayAudioTracks", next.join(","));
  };

  const toggleDiscordClient = (id: string) => {
    const current = (values.overlayAudioDiscord ?? "")
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    const all = discordClients.map((c) => c.id);
    const effective = current.length ? current : all;
    const next = effective.includes(id)
      ? effective.filter((x) => x !== id)
      : [...effective, id];
    if (next.length === 0) return;
    write("overlayAudioDiscord", next.length === all.length ? "" : next.join(","));
  };

  const discordChosen = (id: string) => {
    const current = (values.overlayAudioDiscord ?? "")
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    return current.length === 0 || current.includes(id);
  };

  const remove = async (path: string) => {
    const result = await rpc<{ success?: boolean; error?: string }>(
      "overlay-delete-capture",
      path,
    );
    if (result?.success === false) {
      const text = result.error ?? "That file could not be deleted.";
      if (viewingRef.current) setViewerError(text);
      else setError({ text });
      return;
    }
    if (viewingRef.current === path) {
      const at = inViewerOrder.findIndex((c) => c.path === path);
      const next =
        (at >= 0 ? inViewerOrder[at + 1] ?? inViewerOrder[at - 1] : undefined) ??
        inViewerOrder.find((c) => c.path !== path);
      setViewing(next ? next.path : null);
    }
    void load();
  };

  const askRemove = (path: string) => {
    if (armed !== path) {
      setArmed(path);
      return;
    }
    setArmed(null);
    void remove(path);
  };

  const quality = Number.parseInt(values.overlayShotQuality ?? "90", 10) || 90;
  const res = values.overlayRecRes ?? "native";
  const fps = values.overlayRecFps ?? "60";
  const codec = values.overlayRecCodec ?? "h264";
  const recQuality = values.overlayRecQuality ?? "balanced";

  const takeScreenshot = async () => {
    if (shooting) return;
    setShooting(true);
    setError(null);
    try {
      const result = await rpc<{ success?: boolean; error?: string }>("overlay-screenshot");
      if (result?.success === false) setError({ text: result.error ?? "The screenshot failed." });
    } catch (e) {
      setError({ text: e instanceof Error ? e.message : String(e || "The screenshot failed.") });
    } finally {
      setShooting(false);
    }
  };

  const toggleRecording = async () => {
    if (pending) return;
    setPending(recording ? "stopping" : "starting");
    try {
      const result = await rpc<{ success?: boolean; error?: string }>("overlay-record");
      if (result?.success === false) {
        setPending(null);
        setError({ text: result.error ?? "Recording could not start." });
      }
    } catch (e) {
      setPending(null);
      setError({
        text: e instanceof Error ? e.message : String(e || "Recording could not start."),
      });
    }
  };

  const megabytesPerMinute = estimateMegabytesPerMinute(
    estimate,
    res,
    fps,
    codec,
    recQuality,
    selectedTracks.length > 0,
  );

  const shown = useMemo(
    () =>
      captures.filter((c) =>
        filter === "All" ? true : filter === "Clips" ? c.kind === "clip" : c.kind !== "clip",
      ),
    [captures, filter],
  );
  const stored = captures.reduce((total, c) => total + c.sizeBytes, 0);
  const paged = useMemo(() => shown.slice(0, limit), [shown, limit]);
  const totals = useMemo(() => {
    const counts = new Map<string, number>();
    for (const capture of shown) {
      const key = capture.gameId ?? UNGROUPED;
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    return counts;
  }, [shown]);

  const groups = useMemo(() => groupByGame(paged), [paged]);
  const inViewerOrder = useMemo(
    () => groupByGame(shown).flatMap((group) => group.items),
    [shown],
  );

  useEffect(() => {
    if (!viewing) return;
    const at = shown.findIndex((c) => c.path === viewing);
    if (at >= limit) setLimit(Math.ceil((at + 1) / PAGE) * PAGE);
  }, [viewing, shown, limit]);

  useEffect(() => {
    if (!viewing) return;
    const at = inViewerOrder.findIndex((c) => c.path === viewing);
    if (at >= 0) {
      viewedIndex.current = at;
      return;
    }
    const fallback = inViewerOrder[Math.min(viewedIndex.current, inViewerOrder.length - 1)];
    setViewing(fallback ? fallback.path : null);
  }, [viewing, inViewerOrder]);

  if (supported === false) {
    return (
      <p className="max-w-[70ch] text-[13px] leading-relaxed text-white/55">
        This version of Windows cannot capture a game window, so screenshots are not available.
        Windows 10 version 1903 or later is needed.
      </p>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-stretch gap-3">
        <OvCard className="flex min-w-0 flex-1 flex-col gap-3 px-[18px] py-4">
          <div className="flex items-center justify-between">
            <div className="text-[13px] font-semibold">Screenshots</div>
            {shotHotkey && <OvKey>{spacedAccelerator(shotHotkey)}</OvKey>}
          </div>

          <button
            type="button"
            onClick={() => void takeScreenshot()}
            disabled={shooting}
            className="flex items-center justify-center gap-[9px] rounded-[9px] border border-white/[0.16] bg-white/[0.08] px-4 py-[11px] text-[13px] font-semibold text-white transition-colors duration-150 hover:bg-white/[0.13] disabled:opacity-60"
          >
            {shooting ? "Taking screenshot…" : "Take screenshot"}
          </button>

          <div>
            <OvLabel>Format</OvLabel>
            <OvSeg
              className="mt-[7px]"
              options={[
                { value: "png", label: "PNG" },
                { value: "jpeg", label: "JPEG" },
              ]}
              value={values.overlayShotFormat ?? "png"}
              onChange={(v) => write("overlayShotFormat", v)}
              ariaLabel="Screenshot format"
            />
          </div>

          <div>
            <div className="flex items-center justify-between">
              <OvLabel>Quality</OvLabel>
              <span className="ov-mono text-[11px] text-white/60">{qualityDraft ?? quality}</span>
            </div>
            <div className="mt-[9px]">
              <OvMeter
                min={40}
                max={100}
                step={1}
                value={quality}
                disabled={(values.overlayShotFormat ?? "png") === "png"}
                onChange={(v) => write("overlayShotQuality", String(v))}
                onDraft={setQualityDraft}
                ariaLabel="Screenshot quality"
              />
            </div>
            {(values.overlayShotFormat ?? "png") === "png" && (
              <p className="mt-[6px] text-[11px] text-white/55">
                PNG is lossless, so quality only applies to JPEG.
              </p>
            )}
          </div>

          <div className="flex items-center justify-between gap-3 py-2">
            <span className="text-[12px] text-white/75">Show a message on screen</span>
            <OvToggle
              checked={flag("overlayShotToast", true)}
              ariaLabel="Show a message on screen"
              onChange={(v) => write("overlayShotToast", String(v))}
            />
          </div>

        </OvCard>

        <OvCard className="flex min-w-0 flex-[1.35] flex-col gap-3 px-[18px] py-4">
          <div className="flex items-center justify-between">
            <div className="text-[13px] font-semibold">Video recording</div>
            {recHotkey && <OvKey>{spacedAccelerator(recHotkey)}</OvKey>}
          </div>

          <button
            type="button"
            onClick={() => void toggleRecording()}
            disabled={pending !== null || (!recordingSupported && !recording)}
            className={`flex items-center justify-center gap-[9px] rounded-[9px] border px-4 py-[11px] text-[13px] font-semibold transition-colors duration-150 disabled:opacity-60 ${
              recording
                ? "border-[#ef4444]/40 bg-[#ef4444]/[0.14] text-[#fca5a5] hover:bg-[#ef4444]/20"
                : "border-white/[0.16] bg-white/[0.08] text-white hover:bg-white/[0.13]"
            }`}
          >
            <span
              aria-hidden="true"
              className={`h-[9px] w-[9px] rounded-full ${
                recording ? "animate-pulse bg-[#ef4444]" : "bg-white/60"
              }`}
            />
            {pending === "starting"
              ? "Starting…"
              : pending === "stopping"
                ? "Saving…"
                : recording
                  ? (
                      <>
                        Stop recording · <RecordingElapsed since={since} />
                      </>
                    )
                  : "Start recording"}
          </button>

          {!recordingSupported && (
            <p className="text-[11px] leading-relaxed text-white/55">
              Recording needs Windows Media Foundation. Install the Media Feature Pack from
              Optional features.
            </p>
          )}

          <div className="flex flex-col gap-3">
            <Setting label="Resolution">
              <OvSeg
                options={[
                  { value: "720", label: "720p" },
                  { value: "1080", label: "1080p" },
                  { value: "1440", label: "1440p" },
                  { value: "native", label: "Native" },
                ]}
                value={res}
                onChange={(v) => write("overlayRecRes", v)}
                ariaLabel="Recording resolution"
              />
            </Setting>
            <Setting label="Frame rate">
              <OvSeg
                options={[
                  { value: "30", label: "30" },
                  { value: "60", label: "60" },
                  { value: "120", label: "120" },
                ]}
                value={fps}
                onChange={(v) => write("overlayRecFps", v)}
                ariaLabel="Recording frame rate"
              />
            </Setting>
            <Setting label="Quality">
              <OvSeg
                options={[
                  { value: "efficient", label: "Efficient" },
                  { value: "balanced", label: "Balanced" },
                  { value: "high", label: "High" },
                ]}
                value={recQuality}
                onChange={(v) => write("overlayRecQuality", v)}
                ariaLabel="Recording quality"
              />
            </Setting>
            <Setting label="Encoder">
              <OvSeg
                options={[
                  { value: "h264", label: "H.264" },
                  { value: "hevc", label: "HEVC" },
                  { value: "av1", label: "AV1" },
                ]}
                value={codec}
                onChange={(v) => write("overlayRecCodec", v)}
                ariaLabel="Encoder"
              />
            </Setting>
            {megabytesPerMinute !== null && (
              <p className="text-[11px] leading-[1.45] text-white/55">
                About {megabytesPerMinute} MB per minute at these settings.
              </p>
            )}
          </div>

          <div>
            <OvLabel>Audio tracks</OvLabel>
            <div className="mt-[7px] flex flex-wrap gap-[7px]">
              {tracks.map((track) => (
                <OvChip
                  key={track.id}
                  active={selectedTracks.includes(track.id)}
                  disabled={!track.available}
                  title={track.available ? undefined : "Windows 11 is needed for this track"}
                  onClick={() => toggleTrack(track.id)}
                >
                  {track.label}
                </OvChip>
              ))}
            </div>
            <p className="mt-[6px] text-[11px] leading-[1.45] text-white/55">
              {selectedTracks.includes("desktop")
                ? "Desktop records everything you can hear, the game and Discord included."
                : selectedTracks.length === 0
                  ? "Nothing is selected, so clips will be silent."
                  : "Each source is captured on its own and mixed into one track."}
            </p>

            {selectedTracks.includes("discord") && (
              <div className="mt-[10px] rounded-[9px] border border-white/[0.08] bg-white/[0.03] px-3 py-[10px]">
                <div className="text-[11.5px] text-white/60">
                  {discordClients.length === 0
                    ? "No Discord client is running, so nothing will be captured from it."
                    : discordClients.length === 1
                      ? `Capturing ${discordClients[0].label}.`
                      : "More than one Discord is running. Choose which to capture."}
                </div>
                {discordClients.length > 1 && (
                  <div className="mt-[8px] flex flex-wrap gap-[7px]">
                    {discordClients.map((client) => (
                      <OvChip
                        key={client.id}
                        active={discordChosen(client.id)}
                        onClick={() => toggleDiscordClient(client.id)}
                      >
                        {client.label}
                      </OvChip>
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>

          <p className="text-[11px] leading-[1.5] text-white/55">
            Clips are saved as MP4 beside your screenshots. Resolution, frame rate and encoder
            apply to the next recording, not the one in progress.
          </p>
        </OvCard>
      </div>

      <div className="flex items-center justify-between gap-[14px] rounded-[10px] border border-white/[0.08] bg-white/[0.04] px-4 py-[13px]">
        <div className="min-w-0">
          <div className="text-[12.5px] font-medium">Captures folder</div>
          <div className="ov-mono mt-[3px] truncate text-[11px] text-white/50">{folder}</div>
        </div>
        <div className="flex shrink-0 gap-2">
          <button
            type="button"
            onClick={() => void openCaptureFolder()}
            className="rounded-[8px] border border-white/15 bg-white/[0.06] px-[13px] py-[7px] text-[12px] font-medium hover:bg-white/10"
          >
            Open
          </button>
          <button
            type="button"
            onClick={() =>
              void chooseCaptureFolder().then(() => {
                void loadConfig();
                void load();
              })
            }
            className="rounded-[8px] border border-white/15 bg-white/[0.06] px-[13px] py-[7px] text-[12px] font-medium hover:bg-white/10"
          >
            Change…
          </button>
          {!folderIsDefault && (
            <button
              type="button"
              onClick={() =>
                void resetCaptureFolder().then(() => {
                  void loadConfig();
                  void load();
                })
              }
              className="rounded-[8px] border border-white/15 bg-white/[0.06] px-[13px] py-[7px] text-[12px] font-medium hover:bg-white/10"
            >
              Use default
            </button>
          )}
        </div>
      </div>

      {error && (
        <div
          role={error.warning ? "status" : "alert"}
          className={`flex items-start gap-3 rounded-[10px] border px-4 py-3 text-[12.5px] leading-relaxed ${
            error.warning
              ? "border-[#f59e0b]/30 bg-[#f59e0b]/[0.08] text-[#fcd34d]"
              : "border-[#ef4444]/30 bg-[#ef4444]/[0.08] text-[#fca5a5]"
          }`}
        >
          {error.warning && <AlertTriangle size={16} className="mt-[2px] shrink-0" />}
          <span className="min-w-0 flex-1">{error.text}</span>
          <button
            type="button"
            aria-label="Dismiss"
            onClick={() => setError(null)}
            className="-my-1 -mr-2 grid h-[26px] w-[26px] shrink-0 place-items-center rounded-[6px] opacity-70 hover:bg-white/[0.08] hover:opacity-100"
          >
            <X size={14} />
          </button>
        </div>
      )}

      <div>
        <div className="mb-[10px] flex items-center gap-[7px]">
          {(["All", "Screenshots", "Clips"] as const).map((label) => (
            <button
              key={label}
              type="button"
              aria-pressed={filter === label}
              onClick={() => setFilter(label)}
              className={`rounded-[8px] border px-[11px] py-[6px] text-[12px] font-medium transition-colors duration-150 ${
                filter === label
                  ? "border-white/[0.14] bg-white/[0.09] text-white"
                  : "border-white/[0.08] text-white/55 hover:text-white"
              }`}
            >
              {label}
            </button>
          ))}
          {stored > 0 && (
            <span className="ml-auto text-[11.5px] text-white/55">
              {fmtBytes(stored)} stored
            </span>
          )}
        </div>

        {shown.length === 0 ? (
          <p className="text-[13px] text-white/55">Nothing captured yet.</p>
        ) : (
          groups.map((group) => (
            <div key={group.id} className="mt-[18px] first:mt-0">
              <div className="mb-[9px] flex items-center gap-[7px]">
                {group.known && (
                  <img
                    src={iconSrcFor(group.id, icons)}
                    onError={iconFallback(group.id)}
                    alt=""
                    className="h-[16px] w-[16px] rounded-[3px] object-cover"
                  />
                )}
                <span className="text-[12px] font-medium text-white/70">{group.name}</span>
                <span className="text-[11px] text-white/55">
                  {totals.get(group.id) ?? group.items.length}
                </span>
              </div>
              <div className="grid gap-[14px] [grid-template-columns:repeat(auto-fill,minmax(190px,1fr))]">
                {group.items.map((capture) => (
                  <div
                    key={capture.path}
                    className="overflow-hidden rounded-[10px] border border-white/[0.08] bg-white/[0.04]"
                  >
                    <button
                      type="button"
                      onClick={() => setViewing(capture.path)}
                      className="relative block w-full cursor-zoom-in"
                    >
                      <CapturePreview capture={capture} />
                      {capture.kind === "clip" && (
                        <span className="absolute bottom-[6px] right-[6px] rounded-[5px] bg-black/70 px-[6px] py-[2px] text-[10px] font-medium text-white/85">
                          Clip
                        </span>
                      )}
                    </button>
                    <div className="px-[10px] pb-[10px] pt-[9px]">
                      <div className="text-[11px] text-white/55">
                        {fmtRelTime(Date.parse(capture.takenAt), now)}
                      </div>
                      <div className="mt-[7px] flex gap-[5px]">
                        <button
                          type="button"
                          onClick={() => setViewing(capture.path)}
                          className="flex-1 rounded-[6px] bg-white/[0.07] py-[5px] text-[10.5px] text-white/75 hover:bg-white/[0.12]"
                        >
                          View
                        </button>
                        <button
                          type="button"
                          onClick={() => askRemove(capture.path)}
                          className="flex-1 rounded-[6px] bg-[#ef4444]/10 py-[5px] text-[10.5px] text-[#fca5a5] hover:bg-[#ef4444]/20"
                        >
                          {armed === capture.path ? "Delete?" : "Delete"}
                        </button>
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          ))
        )}
        {shown.length > paged.length && (
          <button
            type="button"
            onClick={() => setLimit((current) => current + PAGE)}
            className="mt-[16px] w-full rounded-[8px] border border-white/[0.08] py-[8px] text-[12px] font-medium text-white/55 transition-colors duration-150 hover:bg-white/[0.06] hover:text-white"
          >
            Show more ({shown.length - paged.length} left)
          </button>
        )}
      </div>

      {viewing && (
        <CaptureViewer
          captures={inViewerOrder}
          path={viewing}
          onPath={(path) => {
            setViewerError(null);
            setViewing(path);
          }}
          onClose={closeViewer}
          onDelete={(path) => void remove(path)}
        />
      )}

      {viewing &&
        viewerError &&
        createPortal(
          <div
            role="alert"
            className="fixed bottom-6 left-1/2 z-[130] flex max-w-[min(560px,90vw)] -translate-x-1/2 items-start gap-3 rounded-[10px] border border-[#ef4444]/40 bg-[#1a0d10]/95 px-4 py-3 text-[12.5px] leading-relaxed text-[#fca5a5] shadow-[0_10px_30px_-10px_rgba(0,0,0,0.8)]"
          >
            <span className="min-w-0 flex-1">{viewerError}</span>
            <button
              type="button"
              aria-label="Dismiss"
              onClick={() => setViewerError(null)}
              className="-my-1 -mr-2 grid h-[26px] w-[26px] shrink-0 place-items-center rounded-[6px] opacity-70 hover:bg-white/[0.08] hover:opacity-100"
            >
              <X size={14} />
            </button>
          </div>,
          document.body,
        )}
    </div>
  );
}

function CapturePreview({ capture }: { capture: Capture }) {
  if (capture.thumbPath) {
    return (
      <img
        src={convertFileSrc(capture.thumbPath)}
        alt={capture.name}
        loading="lazy"
        decoding="async"
        className="aspect-[16/9] w-full bg-black object-cover"
      />
    );
  }
  if (capture.kind === "clip") return <LazyClip path={capture.path} />;
  return (
    <img
      src={convertFileSrc(capture.path)}
      alt={capture.name}
      loading="lazy"
      decoding="async"
      className="aspect-[16/9] w-full object-cover"
    />
  );
}

function LazyClip({ path }: { path: string }) {
  const box = useRef<HTMLDivElement>(null);
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
      { rootMargin: "120px" },
    );
    observer.observe(node);
    return () => observer.disconnect();
  }, [visible]);

  return (
    <div ref={box} className="aspect-[16/9] w-full bg-black">
      {visible && (
        <video
          src={`${convertFileSrc(path)}#t=0.5`}
          muted
          playsInline
          preload="metadata"
          className="h-full w-full object-cover"
        />
      )}
    </div>
  );
}

function RecordingElapsed({ since }: { since: number }) {
  const [tick, setTick] = useState(0);

  useEffect(() => {
    if (!since) {
      setTick(0);
      return;
    }
    const update = () => setTick(Math.max(0, Math.round((Date.now() - since) / 1000)));
    update();
    const timer = window.setInterval(update, 1000);
    return () => window.clearInterval(timer);
  }, [since]);

  return <>{`${Math.floor(tick / 60)}:${String(tick % 60).padStart(2, "0")}`}</>;
}

function Setting({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-[7px]">
      <OvLabel>{label}</OvLabel>
      {children}
    </div>
  );
}

function estimateMegabytesPerMinute(
  estimate: OverlayRecEstimate | null,
  res: string,
  fps: string,
  codec: string,
  quality: string,
  withAudio: boolean,
): number | null {
  if (!estimate || estimate.sourceWidth <= 0 || estimate.sourceHeight <= 0) return null;
  const wanted = res === "native" ? 0 : Number.parseInt(res, 10) || 0;
  const height =
    wanted === 0 || wanted >= estimate.sourceHeight ? estimate.sourceHeight : wanted;
  const width = Math.floor((height * estimate.sourceWidth) / estimate.sourceHeight);
  const frames = Math.min(240, Math.max(24, Number.parseInt(fps, 10) || 60));
  const perPixel =
    (estimate.bitsPerPixel[quality] ?? estimate.bitsPerPixel.balanced ?? 0) *
    (codec === "hevc" || codec === "av1" ? estimate.modernCodecFactor : 1);
  const video = Math.min(
    estimate.maxBps,
    Math.max(estimate.minBps, Math.floor((width & ~1) * (height & ~1) * frames * perPixel)),
  );
  const bitsPerSecond = video + (withAudio ? estimate.audioBps : 0);
  return Math.max(1, Math.round((bitsPerSecond * 60) / 8 / 1_000_000));
}
