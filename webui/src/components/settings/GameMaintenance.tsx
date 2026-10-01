// ------------ Game Maintenance ------------
// The per-game section of Settings, Games. Covers auto-update and update checks, repair and verify, opening
// folders, the install location, and moving or removing the game.
import { useEffect, useState, type ReactNode } from "react";
import {
  Wrench,
  FolderOpen,
  Images,
  FolderInput,
  Trash2,
  Cpu,
  Layers,
  RefreshCw,
  TerminalSquare,
  Gamepad2,
  Languages,
  Rocket,
  Power,
  CalendarClock,
} from "lucide-react";
import type { GameId } from "../../types/game";
import { supportsQuickRepair } from "../../data/games";
import {
  JOB_ACTIVE_BLOCKER,
  qualityLabel,
  useGameActions,
  useModsForceDx11,
  useResourceQuality,
} from "../../lib/gameActions";
import { fmtBytes } from "../../lib/format";
import { useCustomizationStore } from "../../store/customizationStore";
import { useNotificationStore } from "../../store/notificationStore";
import { useGamesStore } from "../../store/gamesStore";
import { iconFallback, iconSrcFor } from "../../lib/customMedia";
import { verifyNeedsRepairFirst } from "../../lib/download";
import { FpsUnlockPanel } from "./FpsUnlockPanel";
import { VoicePackPanel } from "./VoicePackPanel";
import { SettingsGroup, GroupLabel } from "../ui/SettingsGroup";
import { SettingsExpander } from "../ui/SettingsExpander";
import {
  repairGame,
  verifyGameIntegrity,
  openGameFolder,
  openScreenshotFolder,
  getGameConfig,
  setGameUpdatePref,
  setCustomLauncher,
  getInstallPathHealth,
  setConfigValue,
  defaultGraphicsApi,
  RESOURCE_QUALITY_OPTIONS,
  downloadEnqueue,
  getAppliedVoicePacks,
  DEFAULT_VOICE_PACK,
  VOICE_PACK_OPTIONS,
  type GameUpdatePrefs,
  type GraphicsApi,
  type ResourceQuality,
  type VoicePackLanguage,
} from "../../lib/ipc";
import { SettingCard } from "../ui/SettingCard";
import { ActionButton } from "../ui/ActionButton";
import { Toggle } from "../ui/Toggle";
import { SegmentedControl } from "../ui/SegmentedControl";
import { TooltipPortal, useAnchoredTip } from "../ui/Tooltip";

function ButtonHint({ tip, children }: { tip: string; children: ReactNode }) {
  const hint = useAnchoredTip<HTMLSpanElement>("bottom");
  return (
    <span ref={hint.anchorRef} {...hint.bind} className="inline-flex">
      {children}
      {hint.shown && (
        <TooltipPortal x={hint.pos.x} y={hint.pos.y} placement="bottom">
          {tip}
        </TooltipPortal>
      )}
    </span>
  );
}

const SCHEDULE_OPTS = [
  { value: "off", label: "Never" },
  { value: "daily", label: "Daily" },
  { value: "weekly", label: "Weekly" },
];

const GRAPHICS_API_OPTS = [
  { value: "dx11", label: "DX11" },
  { value: "dx12", label: "DX12" },
];

function qualitySizes(sizes: ReturnType<typeof useResourceQuality>["sizes"]): string {
  if (!sizes) return "";
  const parts = RESOURCE_QUALITY_OPTIONS.flatMap(({ value }) => {
    const size = sizes[value];
    return size ? [`${qualityLabel(value)} ${fmtBytes(size.installBytes)}`] : [];
  });
  return parts.length ? ` ${parts.join(" · ")}.` : "";
}

const CUSTOM_LAUNCHER_OVERRIDES =
  "Not used while another program starts the game. Add these arguments in that program instead.";

const NO_PREFS: GameUpdatePrefs = {
  autoUpdate: false,
  autoUpdateOnStartup: false,
  autoUpdateSchedule: "off",
};

type PathHealth = Awaited<ReturnType<typeof getInstallPathHealth>> & { path?: string };

export function GameMaintenance({ gameId: activeId }: { gameId: GameId }) {
  const {
    game,
    managed,
    installed,
    partial,
    running,
    jobActive,
    steam,
    setSteam,
    steamCopy,
    uninstallBlocker,
    quickRepair,
    confirmFullRepair,
    locate,
    confirmMove,
    confirmUninstall,
  } = useGameActions(activeId);
  const modsForceDx11 = useModsForceDx11(activeId);
  const api = useCustomizationStore((s) => s.graphicsApi[activeId]) ?? defaultGraphicsApi(activeId);
  const setGraphicsApi = useCustomizationStore((s) => s.setGraphicsApi);
  const resourceQuality = useResourceQuality(
    activeId,
    { installed, steamCopy, jobActive },
    { sizes: true },
  );
  const voicePack = useCustomizationStore((s) => s.voicePack[activeId]) ?? DEFAULT_VOICE_PACK;
  const setVoicePack = useCustomizationStore((s) => s.setVoicePack);
  const gameIcons = useCustomizationStore((s) => s.gameIcons);
  const push = useNotificationStore((s) => s.push);
  const toast = useNotificationStore((s) => s.toast);

  const maintainable = installed && managed;
  const moveBlocker = jobActive
    ? JOB_ACTIVE_BLOCKER
    : running && installed && !steamCopy
      ? `Close ${game.name} first.`
      : null;
  const quickRepairable = supportsQuickRepair(activeId);

  const [health, setHealth] = useState<PathHealth | null>(null);
  const [healthTick, setHealthTick] = useState(0);
  useEffect(() => {
    let alive = true;
    setHealth(null);
    if (installed) {
      void getInstallPathHealth(activeId).then((h) => {
        if (alive) setHealth(h);
      });
    }
    return () => {
      alive = false;
    };
  }, [activeId, installed, jobActive, healthTick]);
  const pathProblem =
    health && (health.missing || health.tooDeep || health.notWritable) ? health.message : "";
  const installPath = installed ? (health?.path ?? "") : "";

  const [loadedPrefs, setLoadedPrefs] = useState<{ gameId: GameId; prefs: GameUpdatePrefs } | null>(
    null,
  );
  const prefsReady = loadedPrefs?.gameId === activeId;
  const prefs = loadedPrefs?.prefs ?? NO_PREFS;
  const [launchArgs, setLaunchArgs] = useState("");
  const [savedArgs, setSavedArgs] = useState("");
  const [customLauncher, setCustomLauncherPath] = useState("");
  useEffect(() => {
    let alive = true;
    void getGameConfig(activeId).then((cfg) => {
      if (!alive) return;
      setLoadedPrefs({ gameId: activeId, prefs: cfg.prefs });
      setLaunchArgs(cfg.launchArgs);
      setSavedArgs(cfg.launchArgs);
      setCustomLauncherPath(cfg.customLauncher);
    });
    return () => {
      alive = false;
    };
  }, [activeId]);

  const updatePref = (key: keyof GameUpdatePrefs, value: boolean | string) => {
    if (!prefsReady) return;
    setLoadedPrefs((p) =>
      p && p.gameId === activeId ? { ...p, prefs: { ...p.prefs, [key]: value } } : p,
    );
    void setGameUpdatePref(activeId, key, value);
  };

  const chooseCustomLauncher = (pick: boolean) => {
    void setCustomLauncher(activeId, pick).then((path) => {
      if (path !== undefined) setCustomLauncherPath(path);
    });
  };

  const [appliedVoices, setAppliedVoices] = useState<string[] | null>(null);
  useEffect(() => {
    let alive = true;
    setAppliedVoices(null);
    if (installed && managed && game.voicePackChoice && !jobActive) {
      void getAppliedVoicePacks(activeId).then((languages) => {
        if (alive) setAppliedVoices(languages);
      });
    }
    return () => {
      alive = false;
    };
  }, [activeId, installed, managed, game.voicePackChoice, jobActive]);

  const voicePending =
    !steamCopy && !!appliedVoices && appliedVoices.length > 0 && !appliedVoices.includes(voicePack);

  const applyVoicePack = () => {
    const label = VOICE_PACK_OPTIONS.find((o) => o.value === voicePack)?.label ?? voicePack;
    void downloadEnqueue(activeId, () =>
      push({
        title: `Applying the ${label} voice pack`,
        text: "Downloading it now and removing the old one. Track it on Downloads.",
      }),
    );
  };

  const setLaunchViaSteam = (on: boolean) => {
    if (steam) setSteam({ ...steam, launchViaSteam: on });
    void setConfigValue(`games.${activeId}.launchViaSteam`, on);
  };

  const saveLaunchArgs = () => {
    const trimmed = launchArgs.trim();
    if (trimmed === savedArgs) return;
    setSavedArgs(trimmed);
    void setConfigValue(`games.${activeId}.launchArgs`, trimmed);
  };

  const verifyIntegrity = async () => {
    const result = await verifyGameIntegrity(activeId, () =>
      toast({
        title: `Checking ${game.name}`,
        text: "Validating installed files. Track it on Downloads.",
      }),
    );
    if (!result) return;
    const broken = result.brokenFiles;
    if (broken === 0) {
      push({
        type: "success",
        title: `${game.name} is intact`,
        text: result.message ?? "Every installed file matches the game manifest.",
      });
      return;
    }
    const summary =
      result.message ??
      `${broken} file${broken === 1 ? " needs" : "s need"} repair. A full repair re-downloads ${broken === 1 ? "it" : "them"}.`;
    if (steamCopy) {
      push({
        type: "warning",
        title: `${game.name} has broken files`,
        text: `${summary} Repair this Steam copy with Verify integrity of game files in Steam.`,
      });
      return;
    }
    const updateReplaces =
      result.updatePending ?? useGamesStore.getState().updates.includes(activeId);
    if (updateReplaces && !verifyNeedsRepairFirst(result.message)) {
      push({
        type: "info",
        title: `${game.name} has an update`,
        text:
          (result.updatePending && result.message) ||
          `${broken} file${broken === 1 ? " differs" : "s differ"} from the latest version. The update replaces ${broken === 1 ? "it" : "them"}.`,
        action: {
          label: "Update",
          run: () =>
            void downloadEnqueue(activeId, () =>
              push({ title: `Updating ${game.name}`, text: "Track it on Downloads." }),
            ),
        },
      });
      return;
    }
    push({
      type: "warning",
      title: `${game.name} has broken files`,
      text: summary,
      action: {
        label: "Full repair",
        run: () =>
          void repairGame(activeId, false).then((queued) => {
            if (!queued) return;
            push({
              title: `Repairing ${game.name}`,
              text: "Replacing the broken files. Track it on Downloads.",
            });
          }),
      },
    });
  };

  const scheduleLabel = SCHEDULE_OPTS.find((o) => o.value === prefs.autoUpdateSchedule)?.label;
  const autoUpdateSummary = !installed
    ? "Install the game first"
    : !managed
      ? "Handled by the game's own launcher"
      : !loadedPrefs
        ? "\u00a0"
        : !prefs.autoUpdate
          ? "Off"
          : [
              prefs.autoUpdateOnStartup ? "Checks on startup" : null,
              prefs.autoUpdateSchedule !== "off" ? `${scheduleLabel} check` : null,
              "Checks after you play",
            ]
              .filter(Boolean)
              .join(" · ");

  return (
    <div className="flex flex-col gap-7">
      <div>
        <GroupLabel label="Selected game" />
        <div className="flex items-center gap-[10px] rounded-ui border border-white/[0.08] bg-white/[0.03] py-[6px] pl-[6px] pr-4">
          <img
            src={iconSrcFor(activeId, gameIcons)}
            onError={iconFallback(activeId)}
            alt=""
            className="h-[42px] w-[42px] shrink-0 rounded-[10px] object-cover"
          />
          <div className="min-w-0">
            <div className="truncate text-[15px] font-semibold text-white">{game.name}</div>
            <div className="mt-px text-[12px] text-white/55">
              {managed
                ? "Managed by Peebify"
                : "Managed by its own launcher, which handles repair and updates"}
            </div>
          </div>
        </div>
      </div>

      <SettingsGroup label="Maintenance">
        <SettingsExpander
          icon={<RefreshCw size={18} />}
          title={`Auto-update ${game.name}`}
          summary={autoUpdateSummary}
          description="Checks for updates and downloads them when the game is closed."
          disabled={!maintainable}
          control={
            <Toggle
              checked={prefs.autoUpdate && maintainable}
              disabled={!maintainable || !prefsReady}
              ariaLabel="Auto-update this game"
              onChange={(v) => updatePref("autoUpdate", v)}
            />
          }
        >
          <SettingCard
            icon={<Power size={18} />}
            title="Check on startup"
            description="Look for an update for this game when the launcher opens."
            disabled={!prefs.autoUpdate || !prefsReady}
            control={
              <Toggle
                checked={prefs.autoUpdateOnStartup}
                disabled={!prefs.autoUpdate || !prefsReady}
                ariaLabel="Check on startup"
                onChange={(v) => updatePref("autoUpdateOnStartup", v)}
              />
            }
          />
          <SettingCard
            icon={<CalendarClock size={18} />}
            title="Scheduled check"
            description="How often to look for an update while the launcher is open."
            disabled={!prefs.autoUpdate || !prefsReady}
            control={
              <SegmentedControl
                ariaLabel="Scheduled check"
                options={SCHEDULE_OPTS}
                value={prefs.autoUpdateSchedule}
                onChange={(v) => updatePref("autoUpdateSchedule", v)}
              />
            }
          />
        </SettingsExpander>

        <SettingCard
          icon={<Wrench size={18} />}
          title="Repair and verify"
          description={
            maintainable && jobActive
              ? JOB_ACTIVE_BLOCKER
              : maintainable && steamCopy
                ? "This is the Steam copy, so repair it with Verify integrity of game files in Steam."
                : "Replace broken or missing game files, or check them without changing anything."
          }
          disabled={!maintainable}
          control={
            <div className="flex gap-2">
              {quickRepairable && (
                <ButtonHint tip="Checks file sizes against the local manifest">
                  <ActionButton onClick={quickRepair} disabled={!maintainable || steamCopy || jobActive}>
                    Quick
                  </ActionButton>
                </ButtonHint>
              )}
              <ButtonHint tip="Hashes every file and re-downloads what is broken">
                <ActionButton
                  onClick={confirmFullRepair}
                  disabled={!maintainable || steamCopy || jobActive}
                >
                  Full
                </ActionButton>
              </ButtonHint>
              <ButtonHint tip="Reports problems without changing any files">
                <ActionButton
                  onClick={() => void verifyIntegrity()}
                  disabled={!maintainable || jobActive}
                >
                  Verify
                </ActionButton>
              </ButtonHint>
            </div>
          }
        />

        <SettingCard
          icon={<FolderOpen size={18} />}
          title="Open folders"
          description="Jump to the game's install folder or the captures Peebify saved for it."
          control={
            <div className="flex gap-2">
              <ActionButton
                onClick={() => void openGameFolder(activeId)}
                disabled={!installed}
                icon={<FolderOpen size={15} />}
              >
                Game
              </ActionButton>
              <ActionButton
                onClick={() => void openScreenshotFolder(activeId)}
                icon={<Images size={15} />}
              >
                Screenshots
              </ActionButton>
            </div>
          }
        />

        <SettingCard
          danger={!!pathProblem && !moveBlocker}
          icon={<FolderInput size={18} />}
          title="Install location"
          description={
            moveBlocker ||
            pathProblem ||
            (installed && steamCopy
              ? "Steam manages this copy. Move it from Steam under Properties, Installed Files, then use Locate to point Peebify at the new folder."
              : "Move the game to another folder, or point Peebify at a copy you installed or moved by hand.")
          }
          detail={
            installPath && (
              <p
                className="mt-1 max-w-[56ch] truncate font-mono text-[11.5px] text-white/55"
                title={installPath}
              >
                {installPath}
              </p>
            )
          }
          control={
            <div className="flex gap-2">
              <ActionButton
                onClick={confirmMove}
                disabled={!installed || steamCopy || !!moveBlocker || !!health?.missing}
              >
                Move…
              </ActionButton>
              <ActionButton
                onClick={() => void locate().then(() => setHealthTick((t) => t + 1))}
                disabled={jobActive}
              >
                Locate…
              </ActionButton>
            </div>
          }
        />

        {game.voicePackChoice && (
          <SettingCard
            icon={<Languages size={18} />}
            title="Voice-over language"
            description={
              voicePending
                ? "Not applied yet. Apply now downloads the new voice pack and removes the old one."
                : "Applies on the next update."
            }
            disabled={!installed}
            control={
              <div className="flex items-center gap-2">
                {voicePending && (
                  <ActionButton onClick={applyVoicePack} disabled={jobActive || running}>
                    Apply now
                  </ActionButton>
                )}
                <SegmentedControl
                  ariaLabel="Voice-over language"
                  options={VOICE_PACK_OPTIONS}
                  value={voicePack}
                  onChange={(v) => setVoicePack(activeId, v as VoicePackLanguage)}
                />
              </div>
            }
          />
        )}

        <VoicePackPanel gameId={activeId} />
      </SettingsGroup>

      <SettingsGroup label="Launch options">
        {game.graphicsApiChoice && (
          <SettingCard
            icon={<Cpu size={18} />}
            title="DirectX"
            description={
              customLauncher
                ? CUSTOM_LAUNCHER_OVERRIDES
                : modsForceDx11
                  ? `Mods are on for ${game.name} and only work with DirectX 11, so it always launches with that. Turn mods off to pick again.`
                  : "Force a specific Graphics API (DX11/DX12) onto a game."
            }
            disabled={!installed || modsForceDx11 || !!customLauncher}
            control={
              <SegmentedControl
                ariaLabel="DirectX version"
                options={GRAPHICS_API_OPTS}
                value={modsForceDx11 ? "dx11" : api}
                onChange={(v) => setGraphicsApi(activeId, v as GraphicsApi)}
              />
            }
          />
        )}
        {game.resourceQualityChoice && (
          <SettingCard
            icon={<Layers size={18} />}
            title="Resource quality"
            description={
              customLauncher
                ? CUSTOM_LAUNCHER_OVERRIDES
                : resourceQuality.pending
                  ? `${resourceQuality.installedLabel} is installed. Apply now downloads the ${qualityLabel(resourceQuality.quality)} files and removes the ${resourceQuality.installedLabel} ones.`
                  : `Higher quality looks sharper but takes more disk space${
                      installed ? "" : ". This is the quality that gets installed"
                    }.${qualitySizes(resourceQuality.sizes)}`
            }
            disabled={!!customLauncher}
            control={
              <div className="flex items-center gap-2">
                {resourceQuality.pending && (
                  <ActionButton onClick={resourceQuality.apply} disabled={jobActive || running}>
                    Apply now
                  </ActionButton>
                )}
                <SegmentedControl
                  ariaLabel="Resource quality"
                  options={RESOURCE_QUALITY_OPTIONS}
                  value={resourceQuality.quality}
                  onChange={(v) => resourceQuality.setQuality(v as ResourceQuality)}
                />
              </div>
            }
          />
        )}
        {steam?.isSteamInstall && (
          <SettingCard
            icon={<Gamepad2 size={18} />}
            title="Launch through Steam"
            description={
              steam.steamFound
                ? `This is the Steam copy of ${game.name}. Starting it through Steam keeps the overlay, controller settings and Steam playtime working.`
                : `This is the Steam copy of ${game.name}, but Steam itself couldn't be found on this PC, so the game is started directly.`
            }
            disabled={!steam.steamFound}
            control={
              <Toggle
                checked={steam.launchViaSteam && steam.steamFound}
                disabled={!steam.steamFound}
                ariaLabel="Launch through Steam"
                onChange={setLaunchViaSteam}
              />
            }
          />
        )}
        {game.fpsUnlock && <FpsUnlockPanel gameId={activeId} gameName={game.name} />}
        <SettingCard
          icon={<TerminalSquare size={18} />}
          title="Custom launch arguments"
          description={
            customLauncher
              ? CUSTOM_LAUNCHER_OVERRIDES
              : "Extra command-line arguments added when the game starts. Wrap an argument in quotes if it contains spaces."
          }
          disabled={!!customLauncher}
          control={
            <input
              type="text"
              value={launchArgs}
              disabled={!!customLauncher}
              spellCheck={false}
              placeholder="e.g. -popupwindow"
              aria-label="Custom launch arguments"
              onChange={(e) => setLaunchArgs(e.target.value)}
              onBlur={saveLaunchArgs}
              onKeyDown={(e) => {
                if (e.key === "Enter") saveLaunchArgs();
              }}
              className="w-[220px] shrink-0 rounded-ui border border-white/[0.08] bg-black/30 px-[12px] py-[8px] text-[12.5px] text-white/85 placeholder:text-white/30 focus:border-(--accent-a)"
            />
          }
        />
        <SettingCard
          icon={<Rocket size={18} />}
          title="Start the game with another program"
          description={
            customLauncher
              ? `${customLauncher}. Peebify's launch arguments, DirectX and quality choices aren't passed to it.`
              : "This replaces Peebify Launcher's normal behavior so that an external program can launch the game."
          }
          control={
            <div className="flex items-center gap-2">
              <ActionButton onClick={() => chooseCustomLauncher(true)}>
                {customLauncher ? "Change…" : "Choose…"}
              </ActionButton>
              <ActionButton disabled={!customLauncher} onClick={() => chooseCustomLauncher(false)}>
                Clear
              </ActionButton>
            </div>
          }
        />
      </SettingsGroup>

      <SettingsGroup label="Danger zone" danger>
        <SettingCard
          icon={<Trash2 size={18} />}
          title={partial ? "Discard download" : "Uninstall game"}
          description={
            ((installed || partial) && uninstallBlocker) ||
            (partial
              ? "Delete the files an unfinished install left behind. Playtime and settings are kept."
              : steamCopy
                ? "Hands the uninstall to Steam and removes the game from Peebify. Playtime and settings are kept."
                : "Permanently delete the installed game files. Playtime and settings are kept.")
          }
          danger
          control={
            <ActionButton
              variant="danger"
              onClick={() => void confirmUninstall()}
              disabled={(!installed && !partial) || !!uninstallBlocker}
            >
              {partial ? "Discard" : "Uninstall"}
            </ActionButton>
          }
        />
      </SettingsGroup>
    </div>
  );
}
