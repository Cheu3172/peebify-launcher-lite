// ------------ FPS Unlock ------------
// The frame rate unlock settings for games that cap at 60. Has the on/off switch, a target frame rate, a lower
// cap while tabbed out and a live status while the game starts. Only appears for games that support it.
import { useEffect, useState } from "react";
import { AnimatePresence, m } from "framer-motion";
import { Gauge, MonitorOff, Target } from "lucide-react";
import {
  getFpsUnlock,
  setFpsUnlock,
  onFpsUnlockStatus,
  onGameStopped,
  type FpsUnlock,
  type FpsUnlockPatch,
  type FpsUnlockState,
} from "../../lib/ipc";
import { useReducedMotionSafe } from "../../lib/motion";
import { useTauriEvent } from "../../lib/useTauriEvent";
import { SettingCard } from "../ui/SettingCard";
import { Toggle } from "../ui/Toggle";
import { SegmentedControl } from "../ui/SegmentedControl";
import { Slider } from "../ui/Slider";

const MIN_FPS = 30;
const MAX_FPS = 360;
const FPS_STEP = 10;
const FPS_TICKS = [String(MIN_FPS), String(MAX_FPS)];

const BACKGROUND_PRESETS = [
  { value: "off", label: "No cap" },
  { value: "10", label: "10" },
  { value: "30", label: "30" },
  { value: "60", label: "60" },
];

const STATUS: Partial<Record<FpsUnlockState, { label: string; tone: "accent" | "danger" }>> = {
  launching: { label: "Starting the game", tone: "accent" },
  attaching: { label: "Attaching", tone: "accent" },
  locating: { label: "Finding the setting", tone: "accent" },
  ready: { label: "Active", tone: "accent" },
  failed: { label: "Failed", tone: "danger" },
};

function StatusPill({ state }: { state: FpsUnlockState }) {
  const status = STATUS[state];
  if (!status) return null;
  const danger = status.tone === "danger";
  return (
    <span
      className="rounded-full px-[8px] py-[3px] text-[10px] font-medium uppercase tracking-[0.08em]"
      style={
        danger
          ? {
              background: "rgba(239,68,68,0.14)",
              color: "#fca5a5",
              border: "1px solid rgba(239,68,68,0.28)",
            }
          : {
              background: "rgba(var(--accent-b-rgb), 0.14)",
              color: "var(--accent-text)",
              border: "1px solid rgba(var(--accent-b-rgb), 0.28)",
            }
      }
    >
      {status.label}
    </span>
  );
}

export function FpsUnlockPanel({ gameId, gameName }: { gameId: string; gameName: string }) {
  const [loaded, setLoaded] = useState<{ gameId: string; config: FpsUnlock } | null>(null);
  const reduce = useReducedMotionSafe();
  const ready = loaded?.gameId === gameId;

  const setConfig = (change: (current: FpsUnlock) => FpsUnlock) =>
    setLoaded((current) =>
      current && current.gameId === gameId
        ? { gameId, config: change(current.config) }
        : current,
    );

  useEffect(() => {
    let alive = true;
    void getFpsUnlock(gameId).then((next) => {
      if (alive) setLoaded({ gameId, config: next });
    });
    return () => {
      alive = false;
    };
  }, [gameId]);

  useTauriEvent(onFpsUnlockStatus, (status) => {
    if (status.gameId !== gameId) return;
    const attached = status.state !== "idle" && status.state !== "failed";
    setConfig((current) => ({
      ...current,
      session: { state: status.state, error: status.error },
      appliesNextLaunch: attached ? false : current.appliesNextLaunch,
    }));
  });

  useTauriEvent(onGameStopped, (id) => {
    if (id !== gameId) return;
    setConfig((current) => ({ ...current, appliesNextLaunch: false }));
  });

  if (ready && !loaded.config.supported) return null;
  const config = loaded?.config.supported ? loaded.config : null;
  const enabled = !!config?.enabled;

  const update = (patch: FpsUnlockPatch) => {
    setConfig((current) => ({ ...current, ...patch }));
    void setFpsUnlock(gameId, patch).then((appliesNextLaunch) => {
      if (appliesNextLaunch === undefined) return;
      setConfig((current) => ({ ...current, appliesNextLaunch }));
    });
  };

  const setTargetFps = (value: number) => {
    const clamped = Math.min(Math.max(value, MIN_FPS), MAX_FPS);
    if (clamped !== config?.targetFps) update({ targetFps: clamped });
  };

  const failure =
    ready && config?.session.state === "failed" ? config.session.error : null;

  return (
    <>
      <SettingCard
        icon={<Gauge size={18} />}
        title="Unlock the frame rate"
        badge={ready && config ? <StatusPill state={config.session.state} /> : undefined}
        description={`${gameName} caps at 60. Unlocking it needs admin permissions on game start.`}
        control={
          <Toggle
            checked={enabled}
            disabled={!ready}
            ariaLabel="Unlock the frame rate"
            onChange={(on) => update({ enabled: on })}
          />
        }
      />

      <AnimatePresence initial={false}>
        {config && enabled && (
          <m.div
            key="options"
            initial={{ height: 0, opacity: 0 }}
            animate={{ height: "auto", opacity: 1 }}
            exit={{ height: 0, opacity: 0 }}
            transition={{ duration: reduce ? 0 : 0.26, ease: [0.4, 0, 0.2, 1] }}
            className="overflow-hidden"
          >
            <div className="divide-y divide-white/[0.06] bg-black/[0.14]">
              {failure && (
                <div className="px-5 py-3">
                  <p className="text-[12.5px] leading-snug text-[#fca5a5]">{failure}</p>
                </div>
              )}

              {ready && config.appliesNextLaunch && (
                <div className="px-5 py-3">
                  <p className="text-[12.5px] leading-snug text-white/55">
                    Takes effect the next time you start {gameName}.
                  </p>
                </div>
              )}

              <SettingCard
                icon={<Target size={18} />}
                title="Frame rate cap"
                description="Unlocks the 60 fps ceiling."
                disabled={!ready}
                control={
                  <Slider
                    min={MIN_FPS}
                    max={MAX_FPS}
                    step={FPS_STEP}
                    value={config.targetFps}
                    onChange={setTargetFps}
                    unit="fps"
                    ticks={FPS_TICKS}
                    ariaLabel="Frame rate cap"
                  />
                }
              />

              <SettingCard
                icon={<MonitorOff size={18} />}
                title="While tabbed out"
                description="Keeps fans quiet."
                disabled={!ready}
                control={
                  <SegmentedControl
                    ariaLabel="Frame rate while tabbed out"
                    options={BACKGROUND_PRESETS}
                    value={config.powerSave ? String(config.backgroundFps) : "off"}
                    onChange={(value) =>
                      value === "off"
                        ? update({ powerSave: false })
                        : update({ powerSave: true, backgroundFps: Number(value) })
                    }
                  />
                }
              />
            </div>
          </m.div>
        )}
      </AnimatePresence>
    </>
  );
}
