// ------------ Overlay Panel ------------
// The Overlay tab of Settings. Turns the in-game overlay on, sets its shortcuts, and covers screenshot and
// recording options including which audio (even Discord voice) goes into clips.
import { useCallback, useEffect, useRef, useState } from "react";
import { AlertTriangle, Camera, FolderOpen, Info, Keyboard, Video, Volume2 } from "lucide-react";
import {
  chooseCaptureFolder,
  getOverlayConfig,
  openCaptureFolder,
  resetCaptureFolder,
  setOverlayHotkey,
  setSetting,
  type OverlayConfig,
} from "../../lib/ipc";
import { onEvent, rpc, rpcRead } from "../../lib/rpc";
import { SETTINGS_SECTIONS, type Setting } from "../../data/settings";
import { changedSettingKey, useSettingsStore } from "../../store/settingsStore";
import { useNotificationStore } from "../../store/notificationStore";
import { SettingsGroup } from "../ui/SettingsGroup";
import { SettingsExpander } from "../ui/SettingsExpander";
import { SettingCard } from "../ui/SettingCard";
import { ActionButton } from "../ui/ActionButton";
import { Toggle } from "../ui/Toggle";
import { SettingRow, settingOn, valueLabel } from "./SettingRow";
import { acceleratorOf, captureHint } from "../../lib/hotkeys";
import { GAMES } from "../../data/games";

interface LoaderKey {
  id: string;
  label: string;
  description: string;
  kind: string;
  accelerator: string | null;
  defaultAccelerator: string | null;
}

const PER_APP_TRACKS = ["game", "discord"];

const OVERLAY = SETTINGS_SECTIONS.find((s) => s.category === "overlay")!.settings;
const byId = (id: string): Setting => OVERLAY.find((s) => s.id === id)!;
const inGroup = (group: string, except: string[] = []): Setting[] =>
  OVERLAY.filter((s) => s.group === group && !except.includes(s.id));

const MASTER = byId("overlayEnabled");
const SHOT_ROWS = inGroup("Screenshots");
const REC_ROWS = inGroup("Recording");

function KeyCapture({
  accelerator,
  capturing,
  onStart,
  onEnd,
  onKey,
}: {
  accelerator: string | null;
  capturing: boolean;
  onStart: () => void;
  onEnd: () => void;
  onKey: (e: React.KeyboardEvent) => void;
}) {
  return (
    <button
      type="button"
      onClick={() => (capturing ? onEnd() : onStart())}
      onKeyDown={(e) => {
        if (capturing) onKey(e);
      }}
      onBlur={() => capturing && onEnd()}
      className={`min-w-[132px] rounded-ui border px-4 py-[9px] text-center text-[13px] font-medium tabular-nums transition duration-150 ${
        capturing
          ? "border-(--accent-b)/60 bg-(--accent-b)/12 text-(--accent-text)"
          : "border-white/15 bg-white/[0.05] text-white hover:border-white/25 hover:bg-white/[0.12]"
      }`}
    >
      {capturing ? "Press a shortcut…" : accelerator || "Not set"}
    </button>
  );
}

export function OverlayPanel() {
  const [config, setConfig] = useState<OverlayConfig | null>(null);
  const configRef = useRef<OverlayConfig | null>(null);
  const [failed, setFailed] = useState(false);
  const [capturing, setCapturing] = useState<string | null>(null);
  const hinted = useRef(false);
  const [loaderKeys, setLoaderKeys] = useState<LoaderKey[]>([]);
  const [loaderGameId, setLoaderGameId] = useState<string | null>(null);
  const [windowError, setWindowError] = useState<string | null>(null);
  const push = useNotificationStore((s) => s.push);
  const toast = useNotificationStore((s) => s.toast);
  const values = useSettingsStore((s) => s.values);
  const set = useSettingsStore((s) => s.set);

  const enabled = settingOn(MASTER, values);

  const load = useCallback(async () => {
    const loaded = await getOverlayConfig();
    configRef.current = loaded ?? null;
    setConfig(loaded ?? null);
    setFailed(!loaded);
    const ini = await rpc<{ gameId?: string; settings?: LoaderKey[] }>("mod-ini-settings");
    setLoaderGameId(ini?.gameId ?? null);
    setLoaderKeys((ini?.settings ?? []).filter((row) => row.kind === "key"));
  }, []);

  const loadStatus = useCallback(async () => {
    const status = await rpcRead<{ windowError?: string | null }>("get-overlay-status");
    setWindowError(status?.windowError || null);
  }, []);

  useEffect(() => {
    void load();
    void loadStatus();
  }, [load, loadStatus]);

  useEffect(() => {
    let disposed = false;
    const offs: Array<() => void> = [];
    const listen = (name: string, handler: (payload: unknown) => void) => {
      void onEvent(name, handler).then((off) => {
        if (disposed) off();
        else offs.push(off);
      });
    };
    listen("settings-changed", (payload) => {
      const key = changedSettingKey(payload);
      if (
        key === null ||
        key.startsWith("overlay") ||
        key.startsWith("behavior.overlay") ||
        key === "launchAction" ||
        key === "behavior.launchAction"
      ) {
        void load();
      }
    });
    listen("mod-ini-changed", () => void load());
    listen("overlay-status", (payload) => {
      const error = (payload as { windowError?: unknown } | null)?.windowError;
      if (typeof error === "string" && error) setWindowError(error);
      else void loadStatus();
    });
    return () => {
      disposed = true;
      offs.forEach((off) => off());
    };
  }, [load, loadStatus]);

  const startCapture = (id: string) => {
    hinted.current = false;
    setCapturing(id);
    void rpc("suspend-overlay-hotkeys", true);
  };

  const endCapture = useCallback(() => {
    setCapturing(null);
    void rpc("suspend-overlay-hotkeys", false);
  }, []);

  useEffect(() => () => void rpc("suspend-overlay-hotkeys", false), []);

  useEffect(() => {
    if (!capturing) return;
    const keepAlive = window.setInterval(() => void rpc("suspend-overlay-hotkeys", true), 8000);
    return () => window.clearInterval(keepAlive);
  }, [capturing]);

  const acceleratorOrHint = (e: React.KeyboardEvent): string | null => {
    const accelerator = acceleratorOf(e);
    const hint = accelerator ? null : captureHint(e);
    if (hint && !hinted.current) {
      hinted.current = true;
      toast({ title: "Pick a different shortcut", text: hint });
    }
    return accelerator;
  };

  const bind = async (id: string, e: React.KeyboardEvent) => {
    e.preventDefault();
    if (e.key === "Escape") {
      endCapture();
      return;
    }
    const accelerator = acceleratorOrHint(e);
    if (!accelerator) return;
    endCapture();
    await applyHotkey(id, accelerator);
  };

  const applyHotkey = async (id: string, accelerator: string) => {
    const result = await setOverlayHotkey(id, accelerator);
    if (result.warning) {
      push({ type: "warning", title: "Shortcut saved with a catch", text: result.warning });
    }
    if (result.ok) await load();
  };

  const loaderGameName = GAMES.find((g) => g.id === loaderGameId)?.name ?? "the active game";

  const bindLoader = async (id: string, e: React.KeyboardEvent) => {
    e.preventDefault();
    if (e.key === "Escape") {
      endCapture();
      return;
    }
    const accelerator = acceleratorOrHint(e);
    if (!accelerator) return;
    endCapture();
    await applyLoader(id, accelerator);
  };

  const applyLoader = async (id: string, accelerator: string) => {
    const result = await rpc<{ success?: boolean; error?: string }>(
      "set-mod-ini-setting",
      loaderGameId,
      id,
      accelerator,
    );
    if (result?.success === false) {
      push({
        type: "error",
        title: "That shortcut was not saved",
        text: result.error ?? "The mod loader would not take it.",
      });
      return;
    }
    await load();
  };

  const toggleCsv = async (key: string, field: "selectedAudioTracks", id: string) => {
    const latest = configRef.current;
    if (!latest) return;
    const current = latest[field];
    let next = current.includes(id) ? current.filter((x) => x !== id) : [...current, id];
    if (field === "selectedAudioTracks" && !current.includes(id)) {
      if (id === "desktop") next = next.filter((x) => !PER_APP_TRACKS.includes(x));
      if (PER_APP_TRACKS.includes(id)) next = next.filter((x) => x !== "desktop");
    }
    const updated = { ...latest, [field]: next };
    configRef.current = updated;
    setConfig(updated);
    if (!(await setSetting(key, next.join(",")))) await load();
  };

  const label = (id: string) => valueLabel(byId(id), values);
  const audioNames = config
    ? config.audioTracks
        .filter((t) => config.selectedAudioTracks.includes(t.id))
        .map((t) => t.label.replace(" only", ""))
    : [];

  const shotSummary = [
    values.overlayShotFormat === "jpeg"
      ? `JPEG ${label("overlayShotQuality")}`
      : label("overlayShotFormat"),
    settingOn(byId("overlayShotToast"), values) ? "Message on screen" : "No message on screen",
  ].join(" · ");
  const recSummary = [
    label("overlayRecRes"),
    `${label("overlayRecFps")} fps`,
    label("overlayRecCodec"),
    label("overlayRecQuality"),
    audioNames.length ? audioNames.join(", ") : "No audio",
  ].join(" · ");
  const shortcutSummary = config
    ? [...config.hotkeys.map((h) => h.accelerator), ...loaderKeys.map((k) => k.accelerator)]
        .filter(Boolean)
        .join(" · ")
    : "Not available";

  return (
    <div className="flex flex-col gap-7">
      <SettingsGroup label="Overlay">
        <SettingRow setting={MASTER} />
        {config?.launchAction === "close" && (
          <SettingCard
            danger
            icon={<AlertTriangle size={18} />}
            title="The launcher closes when a game starts"
            description="Peebify draws the overlay, so closing it on launch leaves nothing to draw with. Switch that to Minimize and the overlay will work."
            control={
              <ActionButton
                onClick={() => set("launchAction", "minimize")}
              >
                Minimize instead
              </ActionButton>
            }
          />
        )}
        {enabled && windowError && (
          <SettingCard
            danger
            icon={<AlertTriangle size={18} />}
            title="The overlay window could not be opened"
            description={`Restart the game to try again. Details: ${windowError}`}
          />
        )}
        {enabled && (
          <SettingCard
            icon={<Info size={18} />}
            title="Play in Borderless or Windowed"
            description="The overlay cannot draw over a game in exclusive fullscreen. Pick Borderless or Windowed in the game's display settings."
          />
        )}
      </SettingsGroup>

      <SettingsGroup label="Features" hint={enabled ? undefined : "Turn the overlay on to use these"}>
        <SettingsExpander
          icon={<Camera size={18} />}
          title="Screenshots"
          summary={shotSummary}
          description="How screenshots are saved and confirmed."
          disabled={!enabled}
        >
          {SHOT_ROWS.map((s) => (
            <SettingRow key={s.id} setting={s} />
          ))}
        </SettingsExpander>

        <SettingsExpander
          icon={<Video size={18} />}
          title="Recording"
          summary={recSummary}
          description="Video quality and which audio goes into the file."
          disabled={!enabled}
        >
          {REC_ROWS.map((s) => (
            <SettingRow key={s.id} setting={s} />
          ))}
          {config?.audioTracks.map((track) => {
            const on = config.selectedAudioTracks.includes(track.id);
            return (
              <SettingCard
                key={track.id}
                icon={<Volume2 size={18} />}
                title={`Record ${track.label.replace(" only", "").toLowerCase()}`}
                disabled={!track.available}
                description={
                  !track.available
                    ? "Windows 10 has no way to record one app's audio on its own, so this can't be offered here."
                    : track.id === "desktop"
                      ? "Everything you hear, including the game and Discord, mixed into the clip's audio."
                      : track.id === "mic"
                        ? "Your microphone, mixed into the clip's audio."
                        : `Just ${track.label.replace(" only", "")}, mixed into the clip's audio. Turning it on turns Desktop off, since Desktop already includes it.`
                }
                control={
                  <Toggle
                    checked={on}
                    disabled={!track.available}
                    ariaLabel={track.label}
                    onChange={() =>
                      void toggleCsv("overlayAudioTracks", "selectedAudioTracks", track.id)
                    }
                  />
                }
              />
            );
          })}
          {config && !config.perAppAudio && (
            <p className="px-5 py-3 text-[12px] text-white/55">Per-app tracks need Windows 11.</p>
          )}
        </SettingsExpander>
      </SettingsGroup>

      <SettingsGroup label="Shortcuts and files">
        {config && (
          <SettingsExpander
            icon={<Keyboard size={18} />}
            title="Keyboard shortcuts"
            summary={shortcutSummary}
            description="Click a shortcut, then press the keys you want. Escape cancels."
            disabled={!enabled}
          >
            {config.hotkeys.map((hk) => (
              <SettingCard
                key={hk.id}
                icon={<Keyboard size={18} />}
                title={hk.label}
                control={
                  <div className="flex gap-2">
                    {hk.accelerator !== hk.default && (
                      <ActionButton onClick={() => void applyHotkey(hk.id, hk.default)}>
                        Reset
                      </ActionButton>
                    )}
                    <KeyCapture
                      accelerator={hk.accelerator}
                      capturing={capturing === hk.id}
                      onStart={() => startCapture(hk.id)}
                      onEnd={endCapture}
                      onKey={(e) => void bind(hk.id, e)}
                    />
                  </div>
                }
              />
            ))}
            {loaderKeys.map((row) => (
              <SettingCard
                key={row.id}
                icon={<Keyboard size={18} />}
                title={row.label}
                description={`${row.description} Only for ${loaderGameName}. Applies the next time the game starts.`}
                control={
                  <div className="flex gap-2">
                    {row.defaultAccelerator && row.accelerator !== row.defaultAccelerator && (
                      <ActionButton
                        onClick={() => void applyLoader(row.id, row.defaultAccelerator ?? "")}
                      >
                        Reset
                      </ActionButton>
                    )}
                    <KeyCapture
                      accelerator={row.accelerator}
                      capturing={capturing === row.id}
                      onStart={() => startCapture(row.id)}
                      onEnd={endCapture}
                      onKey={(e) => void bindLoader(row.id, e)}
                    />
                  </div>
                }
              />
            ))}
          </SettingsExpander>
        )}
        {config && (
          <SettingCard
            icon={<FolderOpen size={18} />}
            title="Captures folder"
            description={config.captureFolder}
            control={
              <div className="flex gap-2">
                <ActionButton onClick={() => void openCaptureFolder()}>Open</ActionButton>
                {!config.captureFolderIsDefault && (
                  <ActionButton onClick={() => void resetCaptureFolder().then(load)}>
                    Reset
                  </ActionButton>
                )}
                <ActionButton
                  onClick={() =>
                    void chooseCaptureFolder().then((picked) => {
                      if (picked) {
                        void load();
                        push({ type: "success", title: "Captures folder changed", text: picked });
                      }
                    })
                  }
                >
                  Change…
                </ActionButton>
              </div>
            }
          />
        )}
        {failed && (
          <SettingCard
            icon={<Info size={18} />}
            title="Shortcuts and the captures folder could not be read"
            description="They come from the launcher backend, which did not answer. Reopening Settings will try again."
          />
        )}
      </SettingsGroup>
    </div>
  );
}
