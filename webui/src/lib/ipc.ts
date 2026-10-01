// ------------ Backend Bridge ------------
// Every call from the UI into the Rust side lives here, wrapped in typed functions and event listeners so
// components never deal with raw channel names. It is split into sections: games, settings, mods, overlay,
// customization, playtime, the download queue, maintenance, installing, and captures.
import { log } from "./log";
import { jobFailureKey, useNotificationStore } from "../store/notificationStore";
import { useQueueStore } from "../store/queueStore";
import { GAMES } from "../data/games";
import { isGameColor } from "../data/playtime";
import type { GameId, GraphicsApi, ResourceQuality } from "../types/game";
import { onEvent, rpc, rpcAction, rpcRead, unwrap } from "./rpc";

export type { GraphicsApi, ResourceQuality };

export interface PendingInstall {
  path: string;
  versionType: string;
}

interface GameInfo {
  id: string;
  installed: boolean;
  pendingInstall: PendingInstall | null;
}

export type BuildType = "stable" | "beta";

export const CHANNEL_LABEL: Record<BuildType, string> = {
  stable: "Stable",
  beta: "Beta",
};

interface AppInfo {
  version: string;
  buildType: BuildType;
}

const asBuildType = (v: unknown): BuildType => (v === "beta" ? v : "stable");

export async function appInfo(): Promise<AppInfo> {
  const [versionData, buildData] = await Promise.all([
    rpcRead<{ version?: string }>("get-app-version"),
    rpc("get-build-info").catch(() => null),
  ]);
  return {
    version: String(versionData?.version || "0.0.0"),
    buildType: asBuildType((buildData as { buildType?: unknown } | null)?.buildType),
  };
}

// ------------ Games and Launching ------------
// Listing games, launching them, and the events for a game starting, stopping, failing to launch,
// or having an update available.
export async function listGames(): Promise<GameInfo[]> {
  const supported = await rpcRead<{ games?: unknown[] }>("get-supported-games");
  const games = Array.isArray(supported?.games) ? supported.games : [];
  const ids = games.map((raw) => String((raw as Record<string, unknown>).id));
  const cfg =
    (ids.length
      ? await getConfigValues(
          ids.flatMap((id) => [`games.${id}.gamePath`, `games.${id}.pendingInstall`]),
        )
      : undefined) || {};
  return ids.map((id) => {
    const installPath = (cfg[`games.${id}.gamePath`] as string | null | undefined) || null;
    const pending = cfg[`games.${id}.pendingInstall`] as
      | { path?: string; versionType?: string }
      | null
      | undefined;
    return {
      id,
      installed: !!installPath,
      pendingInstall: pending?.path
        ? { path: String(pending.path), versionType: String(pending.versionType || "default") }
        : null,
    };
  });
}

interface LaunchOutcome {
  accepted: boolean;
  viaSteam: boolean;
}

export async function launchGame(id: string): Promise<LaunchOutcome> {
  const res = await rpcAction<{ success?: boolean; viaSteam?: boolean }>(
    "Launch",
    "launch-game",
    id,
  );
  return { accepted: res?.success === true, viaSteam: res?.viaSteam === true };
}

export async function onGameStarted(cb: (id: string) => void): Promise<() => void> {
  return onEvent<unknown>("game-started", (p) => cb(idOf(p)));
}

export async function getRunningGameIds(ids: readonly string[]): Promise<string[] | null> {
  const all = await rpcRead<{ runningIds?: unknown }>("get-running-game");
  if (!Array.isArray(all?.runningIds)) return null;
  const running = all.runningIds.map(String);
  return ids.filter((id) => running.includes(id));
}

export async function onGameLaunchFailed(
  cb: (id: string, reason: string) => void,
): Promise<() => void> {
  return onEvent<{ gameId?: string; reason?: string }>("game-launch-failed", (p) =>
    cb(idOf(p), String(p?.reason || "")),
  );
}

export async function onModsLoadFailed(
  cb: (gameId: string, error: string, message: string) => void,
): Promise<() => void> {
  return onEvent<{ gameId?: string; error?: string; message?: string }>("mods-load-failed", (p) =>
    cb(String(p?.gameId || ""), String(p?.error || ""), String(p?.message || "")),
  );
}

export interface GameUpdateVersions {
  currentVersion: string | null;
  latestVersion: string | null;
}

type GameUpdateStatus =
  | ({ ok: true; available: boolean; steamManaged: boolean } & GameUpdateVersions)
  | { ok: false; error: string };

function updateVersions(r: { currentVersion?: unknown; latestVersion?: unknown }): GameUpdateVersions {
  return {
    currentVersion: typeof r.currentVersion === "string" ? r.currentVersion : null,
    latestVersion: typeof r.latestVersion === "string" ? r.latestVersion : null,
  };
}

export async function checkGameUpdate(gameId: string, force = false): Promise<GameUpdateStatus> {
  try {
    const r = await rpc<{
      success?: boolean;
      error?: string;
      updateAvailable?: boolean;
      steamManaged?: boolean;
      currentVersion?: unknown;
      latestVersion?: unknown;
    }>("check-for-updates", gameId, force);
    if (!r || r.success === false) {
      return { ok: false, error: r?.error || "The update server did not answer." };
    }
    return {
      ok: true,
      available: !!r.updateAvailable,
      steamManaged: !!r.steamManaged,
      ...updateVersions(r),
    };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

export async function updateViaSteam(gameId: string): Promise<boolean> {
  const r = await rpcAction<{ success?: boolean }>("Steam update", "steam-update", gameId);
  return r?.success === true;
}

export async function onGameUpdateAvailable(
  cb: (gameId: string, steamManaged: boolean, versions: GameUpdateVersions) => void,
): Promise<() => void> {
  return onEvent<{
    gameId?: string;
    steamManaged?: boolean;
    currentVersion?: unknown;
    latestVersion?: unknown;
  }>("update-available", (p) => {
    if (p?.gameId) cb(String(p.gameId), !!p.steamManaged, updateVersions(p));
  });
}

export async function onGameStopped(cb: (id: string) => void): Promise<() => void> {
  return onEvent<unknown>("game-stopped", (p) => cb(idOf(p)));
}

function idOf(p: unknown): string {
  return String((p as { gameId?: unknown } | null)?.gameId ?? "");
}

// ------------ Settings and Config ------------
// Reading and saving launcher settings and per-game config: update preferences, launch arguments,
// a custom launcher, the FPS unlock, and Steam installs.
export async function getSettings(): Promise<Record<string, string>> {
  const r = await rpcRead<{ settings?: Record<string, string> }>("get-settings");
  return r?.settings || {};
}

export interface ConfigNotice {
  type: "info" | "warning";
  title: string;
  text: string;
  saveBlocked?: boolean;
}

export async function getConfigNotice(): Promise<ConfigNotice | null> {
  const r = await rpcRead<{ configNotice?: Partial<ConfigNotice> | null }>("get-settings");
  const n = r?.configNotice;
  if (!n || typeof n.title !== "string" || typeof n.text !== "string") return null;
  return {
    type: n.type === "warning" ? "warning" : "info",
    title: n.title,
    text: n.text,
    saveBlocked: n.saveBlocked === true,
  };
}

export async function setSetting(key: string, value: string): Promise<boolean> {
  const res = await rpcAction<{ success?: boolean; warning?: string; warningTitle?: string }>(
    "Save settings",
    "set-setting",
    key,
    value,
  );
  if (res?.warning) {
    useNotificationStore.getState().push({
      type: "warning",
      title: res.warningTitle ?? "Setting saved",
      text: res.warning,
    });
  }
  return res?.success === true;
}

export async function setConfigValue(key: string, value: unknown): Promise<void> {
  await rpcAction("Save setting", "set-config", key, value);
}

async function getConfigValue(key: string): Promise<unknown> {
  const r = await rpcRead<{ value?: unknown }>("get-config", key);
  return r?.value;
}

async function getConfigValues(keys: string[]): Promise<Record<string, unknown> | undefined> {
  const r = await rpcRead<{ values?: Record<string, unknown> }>("get-config", keys);
  return r?.values;
}

export interface GameUpdatePrefs {
  autoUpdate: boolean;
  autoUpdateOnStartup: boolean;
  autoUpdateSchedule: "off" | "daily" | "weekly";
}

const UPDATE_PREF_KEYS = ["autoUpdate", "autoUpdateOnStartup", "autoUpdateSchedule"] as const;

function updatePrefsFrom(g: Record<string, unknown>): GameUpdatePrefs {
  return {
    autoUpdate: g.autoUpdate !== false,
    autoUpdateOnStartup: g.autoUpdateOnStartup !== false,
    autoUpdateSchedule:
      g.autoUpdateSchedule === "off" || g.autoUpdateSchedule === "weekly" ? g.autoUpdateSchedule : "daily",
  };
}

async function getGameFields(gameId: string, fields: readonly string[]): Promise<Record<string, unknown>> {
  const values = (await getConfigValues(fields.map((f) => `games.${gameId}.${f}`))) || {};
  return Object.fromEntries(fields.map((f) => [f, values[`games.${gameId}.${f}`]]));
}

export async function getGameUpdatePrefs(gameId: string): Promise<GameUpdatePrefs> {
  return updatePrefsFrom(await getGameFields(gameId, UPDATE_PREF_KEYS));
}

export interface GameConfig {
  prefs: GameUpdatePrefs;
  launchArgs: string;
  customLauncher: string;
  launchViaSteam: boolean;
}

export async function getGameConfig(gameId: string): Promise<GameConfig> {
  const g = await getGameFields(gameId, [
    ...UPDATE_PREF_KEYS,
    "launchArgs",
    "customLauncher",
    "launchViaSteam",
  ]);
  return {
    prefs: updatePrefsFrom(g),
    launchArgs: typeof g.launchArgs === "string" ? g.launchArgs : "",
    customLauncher: typeof g.customLauncher === "string" ? g.customLauncher : "",
    launchViaSteam: g.launchViaSteam !== false,
  };
}

export async function getGameLaunchArgs(gameId: string): Promise<string> {
  const raw = await getConfigValue(`games.${gameId}.launchArgs`);
  return typeof raw === "string" ? raw : "";
}

export async function getCustomLauncher(gameId: string): Promise<string> {
  const raw = await getConfigValue(`games.${gameId}.customLauncher`);
  return typeof raw === "string" ? raw : "";
}

export async function setCustomLauncher(
  gameId: string,
  pick: boolean,
): Promise<string | undefined> {
  const r = await rpcAction<{ success?: boolean; path?: string }>(
    "Change the custom launcher",
    "set-custom-launcher",
    gameId,
    pick ? "pick" : null,
  );
  return r?.success ? r.path : undefined;
}

export async function setGameUpdatePref(
  gameId: string,
  key: keyof GameUpdatePrefs,
  value: boolean | string,
): Promise<void> {
  await setConfigValue(`games.${gameId}.${key}`, value);
}

export type FpsUnlockState =
  | "idle"
  | "launching"
  | "attaching"
  | "locating"
  | "ready"
  | "failed";

export interface FpsUnlock {
  supported: boolean;
  enabled: boolean;
  targetFps: number;
  powerSave: boolean;
  backgroundFps: number;
  session: { state: FpsUnlockState; error: string | null };
  appliesNextLaunch: boolean;
}

const NO_FPS_UNLOCK: FpsUnlock = {
  supported: false,
  enabled: false,
  targetFps: 120,
  powerSave: false,
  backgroundFps: 30,
  session: { state: "idle", error: null },
  appliesNextLaunch: false,
};

export async function getFpsUnlock(gameId: string): Promise<FpsUnlock> {
  const r = await rpcRead<Partial<FpsUnlock>>("get-fps-unlock", gameId);
  if (!r) return NO_FPS_UNLOCK;
  return {
    supported: !!r.supported,
    enabled: !!r.enabled,
    targetFps: Number(r.targetFps) || NO_FPS_UNLOCK.targetFps,
    powerSave: !!r.powerSave,
    backgroundFps: Number(r.backgroundFps) || NO_FPS_UNLOCK.backgroundFps,
    session: {
      state: r.session?.state ?? "idle",
      error: r.session?.error ?? null,
    },
    appliesNextLaunch: !!r.appliesNextLaunch,
  };
}

export type FpsUnlockPatch = Partial<
  Pick<FpsUnlock, "enabled" | "targetFps" | "powerSave" | "backgroundFps">
>;

export async function setFpsUnlock(
  gameId: string,
  patch: FpsUnlockPatch,
): Promise<boolean | undefined> {
  const r = await rpcAction<{ success?: boolean; appliesNextLaunch?: boolean }>(
    "Save FPS unlocker setting",
    "set-fps-unlock",
    gameId,
    patch,
  );
  return r?.success ? !!r.appliesNextLaunch : undefined;
}

export async function onFpsUnlockStatus(
  cb: (status: { gameId: string; state: FpsUnlockState; error: string | null }) => void,
): Promise<() => void> {
  return onEvent<{ gameId?: string; state?: FpsUnlockState; error?: string | null }>(
    "fps-unlock-status",
    (p) => cb({ gameId: String(p?.gameId ?? ""), state: p?.state ?? "idle", error: p?.error ?? null }),
  );
}

export interface SteamInstall {
  isSteamInstall: boolean;
  appId: string | null;
  steamFound: boolean;
  launchViaSteam: boolean;
}

const NO_STEAM: SteamInstall = {
  isSteamInstall: false,
  appId: null,
  steamFound: false,
  launchViaSteam: true,
};

export async function getSteamInstall(
  gameId: string,
  launchViaSteam?: boolean,
): Promise<SteamInstall> {
  const [detected, viaSteam] = await Promise.all([
    rpcRead<{ isSteamInstall?: boolean; appId?: string | null; steamFound?: boolean }>(
      "get-steam-install",
      gameId,
    ),
    launchViaSteam ?? getConfigValue(`games.${gameId}.launchViaSteam`),
  ]);
  if (!detected) return NO_STEAM;
  return {
    isSteamInstall: !!detected.isSteamInstall,
    appId: detected.appId ?? null,
    steamFound: !!detected.steamFound,
    launchViaSteam: viaSteam !== false,
  };
}

interface ModSupportedGame {
  id: string;
  displayName: string;
  variant: string;
  experimental: boolean;
  forcesDx11: boolean;
  gameBananaId: number | null;
  installed: boolean;
  enabled: boolean;
}

interface ToolchainUpdate {
  package: string;
  installed: string;
  latest: string;
}

// ------------ Mods ------------
// Mod support for a game: status, installing the mod loader, importing and toggling mods, and mod
// profiles (named sets of mods you can swap between).
export interface ModsStatus {
  masterEnabled: boolean;
  gameEnabled: boolean;
  gameId: string;
  supportsMods: boolean;
  unsupportedReason: string | null;
  experimental: boolean;
  forcesDx11: boolean;
  variant: string | null;
  toolchainInstalled: boolean;
  toolchainPath: string;
  isDefault: boolean;
  versions: Record<string, string>;
  updates: ToolchainUpdate[];
  autoUpdate: boolean;
  supportedGames: ModSupportedGame[];
}

export interface LoaderSetting {
  id: string;
  label: string;
  description: string;
  kind: "bool" | "millis" | "key";
  value: string;
  accelerator?: string | null;
  min: number | null;
  max: number | null;
}

export async function getLoaderSettings(gameId: string): Promise<LoaderSetting[]> {
  const r = await rpcRead<{ settings?: LoaderSetting[] }>("mod-ini-settings", gameId);
  return r?.settings ?? [];
}

export async function setLoaderSetting(
  gameId: string,
  id: string,
  value: string,
): Promise<boolean> {
  const r = await rpcAction<{ success?: boolean }>(
    "Save mod loader setting",
    "set-mod-ini-setting",
    gameId,
    id,
    value,
  );
  return r?.success === true;
}

interface ModSource {
  kind: "archive" | "gamebanana" | "manual";
  gbModId?: number;
  gbFileId?: number;
  url?: string | null;
  version?: string | null;
  thumbnailUrl?: string | null;
}

export interface ModEntry {
  modId: string;
  folderName: string;
  name: string;
  enabled: boolean;
  sizeBytes: number | null;
  source: ModSource;
  version: string | null;
  installedAt: string | null;
  thumbnailUrl: string | null;
}

const NO_MODS: ModsStatus = {
  masterEnabled: false,
  gameEnabled: false,
  gameId: "",
  supportsMods: false,
  unsupportedReason: null,
  experimental: false,
  forcesDx11: false,
  variant: null,
  toolchainInstalled: false,
  toolchainPath: "",
  isDefault: true,
  versions: {},
  updates: [],
  autoUpdate: true,
  supportedGames: [],
};

export async function getModsStatus(gameId: string): Promise<ModsStatus> {
  const r = await rpcRead<ModsStatus>("mods-status", gameId);
  return r ? { ...NO_MODS, ...r } : NO_MODS;
}

export async function getModsMasterEnabled(): Promise<boolean> {
  return (await getConfigValue("behavior.modsEnabled")) === true;
}

export async function listMods(gameId: string): Promise<ModEntry[]> {
  const r = await rpcRead<{ mods?: ModEntry[] }>("list-mods", gameId);
  return r?.mods ?? [];
}

export async function installModToolchain(
  gameId: string,
): Promise<{ changed: boolean } | undefined> {
  const r = await rpcAction<{ success?: boolean; changed?: boolean }>(
    "Install mod tools",
    "install-mod-toolchain",
    gameId,
  );
  return r?.success ? { changed: r.changed === true } : undefined;
}

export async function uninstallModToolchain(
  gameId?: string,
): Promise<{ keptMods: string[]; sweptFiles: string[]; leftovers: string[] } | undefined> {
  const r = await rpcAction<{
    success?: boolean;
    keptMods?: string[];
    sweptFiles?: string[];
    leftovers?: string[];
  }>("Uninstall mod tools", "uninstall-mod-toolchain", gameId ?? null);
  return r?.success
    ? { keptMods: r.keptMods ?? [], sweptFiles: r.sweptFiles ?? [], leftovers: r.leftovers ?? [] }
    : undefined;
}

export async function setGameModsEnabled(
  gameId: string,
  enabled: boolean,
): Promise<{ sweptFiles: string[] } | undefined> {
  const r = await rpcAction<{ success?: boolean; sweptFiles?: string[] }>(
    "Save setting",
    "set-game-mods-enabled",
    gameId,
    enabled,
  );
  return r?.success ? { sweptFiles: r.sweptFiles ?? [] } : undefined;
}

export async function importModArchive(
  gameId: string,
  paths?: string[],
): Promise<{ installed: string[]; failed: string[] } | "cancelled" | undefined> {
  const r = await rpcAction<{
    success?: boolean;
    cancelled?: boolean;
    installed?: string[];
    failed?: string[];
  }>("Import mod", "import-mod-archive", gameId, paths ?? null);
  if (r?.cancelled) return "cancelled";
  if (!r?.success) return undefined;
  return { installed: r.installed ?? [], failed: r.failed ?? [] };
}

export async function setModEnabled(
  gameId: string,
  folderName: string,
  enabled: boolean,
): Promise<string | undefined> {
  const r = await rpcAction<{ success?: boolean; folderName?: string }>(
    enabled ? "Enable mod" : "Disable mod",
    "set-mod-enabled",
    gameId,
    folderName,
    enabled,
  );
  return r?.success === true ? (r.folderName ?? folderName) : undefined;
}

interface BulkToggleResult {
  changed: { folderName: string; previousFolderName: string; enabled: boolean }[];
  failed: { folderName: string; error: string }[];
}

export async function setModsEnabledBulk(
  gameId: string,
  folderNames: string[],
  enabled: boolean,
): Promise<BulkToggleResult | undefined> {
  const r = await rpcAction<{ success?: boolean } & BulkToggleResult>(
    enabled ? "Enable mods" : "Disable mods",
    "set-mods-enabled-bulk",
    gameId,
    folderNames,
    enabled,
  );
  return r?.success
    ? {
        changed: r.changed ?? [],
        failed: r.failed ?? [],
      }
    : undefined;
}

export interface ModProfile {
  id: string;
  name: string;
  modIds: string[];
  createdAt: string;
  modCount: number;
  resolvedCount: number;
}

interface ModProfileList {
  gameId: string;
  profiles: ModProfile[];
  activeId: string;
}

export async function listModProfiles(gameId: string): Promise<ModProfileList | undefined> {
  const r = await rpcRead<ModProfileList>("list-mod-profiles", gameId);
  return r ? { ...r, profiles: r.profiles ?? [] } : undefined;
}

export async function createModProfile(
  gameId: string,
  name: string,
): Promise<ModProfile | undefined> {
  const r = await rpcAction<{ success?: boolean; profile?: ModProfile }>(
    "Create profile",
    "create-mod-profile",
    gameId,
    name,
  );
  return r?.success ? r.profile : undefined;
}

export async function renameModProfile(
  gameId: string,
  profileId: string,
  name: string,
): Promise<boolean> {
  const r = await rpcAction<{ success?: boolean }>(
    "Rename profile",
    "rename-mod-profile",
    gameId,
    profileId,
    name,
  );
  return r?.success === true;
}

export async function deleteModProfile(gameId: string, profileId: string): Promise<boolean> {
  const r = await rpcAction<{ success?: boolean }>(
    "Delete profile",
    "delete-mod-profile",
    gameId,
    profileId,
  );
  return r?.success === true;
}

export async function duplicateModProfile(
  gameId: string,
  profileId: string,
): Promise<ModProfile | undefined> {
  const r = await rpcAction<{ success?: boolean; profile?: ModProfile }>(
    "Duplicate profile",
    "duplicate-mod-profile",
    gameId,
    profileId,
  );
  return r?.success ? r.profile : undefined;
}

export async function setModProfileMembers(
  gameId: string,
  profileId: string,
  add: string[],
  remove: string[],
): Promise<ModProfile | undefined> {
  const r = await rpcAction<{ success?: boolean; profile?: ModProfile }>(
    "Update profile",
    "set-mod-profile-members",
    gameId,
    profileId,
    add,
    remove,
  );
  return r?.success ? r.profile : undefined;
}

export interface ApplyProfileResult {
  activeId: string;
  enabled: { folderName: string }[];
  disabled: { folderName: string }[];
  failed: { folderName: string; error: string }[];
  missing: string[];
}

export async function applyModProfile(
  gameId: string,
  profileId: string,
): Promise<ApplyProfileResult | undefined> {
  const r = await rpcAction<{ success?: boolean } & ApplyProfileResult>(
    "Apply profile",
    "apply-mod-profile",
    gameId,
    profileId,
  );
  if (!r?.success) return undefined;
  return {
    activeId: String(r.activeId || profileId),
    enabled: r.enabled ?? [],
    disabled: r.disabled ?? [],
    failed: r.failed ?? [],
    missing: r.missing ?? [],
  };
}

export async function deleteMod(
  gameId: string,
  folderName: string,
  modId?: string,
): Promise<boolean> {
  const r = await rpcAction<{ success?: boolean }>(
    "Delete mod",
    "delete-mod",
    gameId,
    folderName,
    ...(modId ? [modId] : []),
  );
  return r?.success === true;
}

// ------------ Overlay and Mods Folder ------------
// Overlay shortcuts and capture folder settings, plus choosing the mods folder and mod install progress.
export interface OverlayHotkey {
  id: string;
  label: string;
  accelerator: string;
  default: string;
}

export interface OverlayOption {
  id: string;
  label: string;
}

export interface OverlayAudioTrack extends OverlayOption {
  available: boolean;
}

export interface OverlayConfig {
  hotkeys: OverlayHotkey[];
  audioTracks: OverlayAudioTrack[];
  selectedAudioTracks: string[];
  perAppAudio: boolean;
  captureFolder: string;
  captureFolderIsDefault: boolean;
  launchAction: string;
  recEstimate?: OverlayRecEstimate | null;
}

export interface OverlayRecEstimate {
  bitsPerPixel: Record<string, number>;
  modernCodecFactor: number;
  minBps: number;
  maxBps: number;
  audioBps: number;
  sourceWidth: number;
  sourceHeight: number;
}

export async function getOverlayConfig(): Promise<OverlayConfig | undefined> {
  return rpcRead<OverlayConfig>("get-overlay-config");
}

export async function setOverlayHotkey(
  id: string,
  accelerator: string,
): Promise<{ ok: boolean; warning: string | null }> {
  const r = await rpcAction<{ success?: boolean; warning?: string | null }>(
    "Change shortcut",
    "set-overlay-hotkey",
    id,
    accelerator,
  );
  return { ok: r?.success === true, warning: r?.warning ?? null };
}

export async function chooseCaptureFolder(): Promise<string | undefined> {
  const r = await rpcAction<{ success?: boolean; path?: string }>(
    "Change captures folder",
    "set-overlay-capture-folder",
    "pick",
  );
  return r?.success ? r.path : undefined;
}

export async function resetCaptureFolder(): Promise<string | undefined> {
  const r = await rpcAction<{ success?: boolean; path?: string }>(
    "Reset captures folder",
    "set-overlay-capture-folder",
    null,
  );
  return r?.success ? r.path : undefined;
}

export async function openCaptureFolder(): Promise<void> {
  await rpcAction("Open captures folder", "open-capture-folder");
}

export async function openModsFolder(gameId: string): Promise<void> {
  await rpcAction("Open mods folder", "open-mods-folder", gameId);
}

export async function chooseModsPath(): Promise<string | undefined> {
  const r = await rpcAction<{ success?: boolean; path?: string }>(
    "Change mods folder",
    "set-mods-path",
    "pick",
  );
  return r?.success ? r.path : undefined;
}

export async function resetModsPath(): Promise<string | undefined> {
  const r = await rpcAction<{ success?: boolean; path?: string }>(
    "Reset mods folder",
    "set-mods-path",
    null,
  );
  return r?.success ? r.path : undefined;
}

export interface ModProgress {
  gameId: string;
  message: string;
  percentage: number;
  done: boolean;
  failed: boolean;
}

export async function onModProgress(cb: (p: ModProgress) => void): Promise<() => void> {
  return onEvent<Partial<ModProgress>>("mods-progress", (p) =>
    cb({
      gameId: String(p?.gameId ?? ""),
      message: String(p?.message ?? ""),
      percentage: Number(p?.percentage ?? 0),
      done: p?.done === true,
      failed: p?.failed === true,
    }),
  );
}

// ------------ Customization and Library ------------
// Wallpapers, game icons, graphics API, quality and voice packs, the list of games shown in the sidebar,
// and the official wallpapers fetched from the server.
export interface MediaSlice {
  type: "default" | "custom";
  path: string | null;
}

export const defaultGraphicsApi = (gameId: string): GraphicsApi =>
  GAMES.find((g) => g.id === gameId)?.graphicsApiDefault ?? "dx11";

const RESOURCE_QUALITIES = ["sd", "hd", "uhd"] as const;
export const DEFAULT_RESOURCE_QUALITY: ResourceQuality = "hd";
export const RESOURCE_QUALITY_OPTIONS: { value: ResourceQuality; label: string }[] = [
  { value: "sd", label: "SD" },
  { value: "hd", label: "HD" },
  { value: "uhd", label: "UHD" },
];

export interface ResourceQualityInfo {
  installed: ResourceQuality[];
  sizes: Partial<Record<ResourceQuality, { downloadBytes: number; installBytes: number }>> | null;
}

const isQuality = (v: unknown): v is ResourceQuality =>
  typeof v === "string" && (RESOURCE_QUALITIES as readonly string[]).includes(v);

export async function getResourceQuality(
  gameId: string,
  withSizes = false,
): Promise<ResourceQualityInfo> {
  const r = await rpcRead<{ installed?: unknown; sizes?: unknown }>(
    "get-resource-quality",
    gameId,
    withSizes,
  );
  const installed = Array.isArray(r?.installed) ? r.installed.filter(isQuality) : [];
  const sizes: ResourceQualityInfo["sizes"] = {};
  if (r?.sizes && typeof r.sizes === "object") {
    for (const [k, v] of Object.entries(r.sizes as Record<string, unknown>)) {
      const s = v as { downloadBytes?: unknown; installBytes?: unknown };
      if (isQuality(k) && typeof s?.installBytes === "number" && typeof s.downloadBytes === "number") {
        sizes[k] = { downloadBytes: s.downloadBytes, installBytes: s.installBytes };
      }
    }
  }
  return { installed, sizes: Object.keys(sizes).length ? sizes : null };
}

const VOICE_PACK_LANGUAGES = ["en-us", "ja-jp", "ko-kr", "zh-cn"] as const;
export type VoicePackLanguage = (typeof VOICE_PACK_LANGUAGES)[number];
export const DEFAULT_VOICE_PACK: VoicePackLanguage = "en-us";
export const VOICE_PACK_OPTIONS: { value: VoicePackLanguage; label: string }[] = [
  { value: "en-us", label: "EN" },
  { value: "ja-jp", label: "JP" },
  { value: "ko-kr", label: "KR" },
  { value: "zh-cn", label: "CN" },
];

export async function getAppliedVoicePacks(gameId: string): Promise<string[] | null> {
  const r = await rpcRead<{ languages?: string[] | null }>("get-applied-voice-packs", gameId);
  return Array.isArray(r?.languages) ? r.languages : null;
}

interface Customization {
  wallpaper: Record<string, MediaSlice>;
  gameIcons: Record<string, MediaSlice>;
  graphicsApi: Record<string, GraphicsApi>;
  resourceQuality: Record<string, ResourceQuality>;
  voicePack: Record<string, VoicePackLanguage>;
  playtimeColor: Record<string, string>;
}

function normSlice(v: unknown): MediaSlice {
  const s = (v || {}) as { type?: string; path?: string | null };
  return { type: s.type === "custom" ? "custom" : "default", path: s.path ?? null };
}

export async function getCustomization(): Promise<Customization> {
  const empty: Customization = {
    wallpaper: {},
    gameIcons: {},
    graphicsApi: {},
    resourceQuality: {},
    voicePack: {},
    playtimeColor: {},
  };
  const ids = GAMES.map((g) => g.id);
  const cfg = await getConfigValues([
    "wallpaper",
    "gameIcons",
    ...ids.flatMap((id) => [
      `games.${id}.graphicsApi`,
      `games.${id}.resourceQuality`,
      `games.${id}.voicePackLanguage`,
      `games.${id}.playtimeColor`,
    ]),
  ]);
  if (!cfg) return empty;
  const asMap = (v: unknown) => (v && typeof v === "object" ? (v as Record<string, unknown>) : {});
  const wallpaper: Record<string, MediaSlice> = {};
  for (const [k, v] of Object.entries(asMap(cfg.wallpaper))) wallpaper[k] = normSlice(v);
  const gameIcons: Record<string, MediaSlice> = {};
  for (const [k, v] of Object.entries(asMap(cfg.gameIcons))) gameIcons[k] = normSlice(v);
  const graphicsApi: Record<string, GraphicsApi> = {};
  const resourceQuality: Record<string, ResourceQuality> = {};
  const voicePack: Record<string, VoicePackLanguage> = {};
  const playtimeColor: Record<string, string> = {};
  for (const k of ids) {
    const api = cfg[`games.${k}.graphicsApi`];
    if (api === "dx11" || api === "dx12") graphicsApi[k] = api;
    const quality = cfg[`games.${k}.resourceQuality`];
    if (typeof quality === "string" && (RESOURCE_QUALITIES as readonly string[]).includes(quality)) {
      resourceQuality[k] = quality as ResourceQuality;
    }
    const color = cfg[`games.${k}.playtimeColor`];
    if (typeof color === "string" && isGameColor(color)) playtimeColor[k] = color.trim().toLowerCase();
    const lang = cfg[`games.${k}.voicePackLanguage`];
    if (typeof lang === "string" && (VOICE_PACK_LANGUAGES as readonly string[]).includes(lang)) {
      voicePack[k] = lang as VoicePackLanguage;
    }
  }
  return {
    wallpaper,
    gameIcons,
    graphicsApi,
    resourceQuality,
    voicePack,
    playtimeColor,
  };
}

interface LibraryConfig {
  visible: GameId[];
  setupComplete: boolean;
}

let savedSetupComplete: boolean | undefined;

export async function getLibrary(): Promise<LibraryConfig> {
  const allIds = GAMES.map((g) => g.id);
  const library = (await getConfigValue("library")) as
    | { visible?: unknown; setupComplete?: unknown }
    | null
    | undefined;
  const raw = Array.isArray(library?.visible) ? library.visible : [];
  const visible = raw.filter((id): id is GameId => allIds.includes(id as GameId));
  const setupComplete = library?.setupComplete === true;
  savedSetupComplete = library ? setupComplete : undefined;
  return {
    visible: [...new Set(visible)],
    setupComplete,
  };
}

export async function saveLibrary(library: LibraryConfig): Promise<void> {
  await setConfigValue("library.visible", library.visible);
  if (library.setupComplete === savedSetupComplete) return;
  const r = await rpcAction<{ success?: boolean }>(
    "Save setting",
    "set-config",
    "library.setupComplete",
    library.setupComplete,
  );
  savedSetupComplete = r?.success === true ? library.setupComplete : undefined;
}

export async function selectWallpaperFile(): Promise<string | null> {
  const r = await rpcRead<{ path?: string }>("select-wallpaper-file");
  return r?.path || null;
}

export async function saveWallpaper(gameId: string, slice: MediaSlice): Promise<void> {
  await rpcAction("Save wallpaper", "save-wallpaper", { gameId, config: slice });
}

export async function selectGameIconFile(): Promise<string | null> {
  const r = await rpcRead<{ path?: string }>("select-game-icon-file");
  return r?.path || null;
}

export async function saveGameIcon(gameId: string, slice: MediaSlice): Promise<void> {
  await rpcAction("Save game icon", "save-game-icon", { gameId, config: slice });
}

export interface RemoteMedia {
  wallpaper: string | null;
  static: string | null;
}

export async function getWallpaperMedia(): Promise<Record<string, RemoteMedia>> {
  const r = await rpcRead<{
    media?: Record<string, { wallpaper?: string | null; static?: string | null }>;
  }>("get-wallpaper-media");
  const out: Record<string, RemoteMedia> = {};
  for (const [k, v] of Object.entries(r?.media || {})) {
    out[k] = { wallpaper: v?.wallpaper || null, static: v?.static || null };
  }
  return out;
}

export async function onWallpaperMediaUpdated(
  cb: (media: Record<string, RemoteMedia>) => void,
): Promise<() => void> {
  return onEvent<{ media?: Record<string, RemoteMedia> }>("wallpaper-media-updated", (p) =>
    cb((p?.media || {}) as Record<string, RemoteMedia>),
  );
}

// ------------ Playtime ------------
// Playtime totals, sessions and daily minutes, and removing a session.
interface PillStats {
  todayMinutes: number;
  weekMinutes: number;
  monthMinutes: number;
  allTimeMinutes: number;
  sessions: number;
  lastPlayed: string | null;
}

interface GameStat {
  id: string;
  minutes: number;
  sessions: number;
  longestSessionMinutes: number;
}

export interface SessionEntry {
  gameId: string;
  start: number;
  minutes: number;
  end?: number;
  live?: boolean;
}

export type DailyMap = Record<string, Record<string, number>>;

interface PlaytimeDashboard {
  games: GameStat[];
  daily: DailyMap;
  recentSessions: SessionEntry[];
  sessionsTruncated?: boolean;
}

export interface PlaytimeData {
  pill: Record<string, PillStats>;
  dashboard: PlaytimeDashboard;
}

export const EMPTY_PLAYTIME: PlaytimeData = {
  pill: {},
  dashboard: {
    games: [],
    daily: {},
    recentSessions: [],
  },
};

export async function getPlaytime(): Promise<PlaytimeData> {
  const r = await rpcRead<PlaytimeData>("get-playtime-data");
  return r?.dashboard ? { pill: r.pill || {}, dashboard: r.dashboard } : EMPTY_PLAYTIME;
}

export async function getOlderSessions(
  before: number,
): Promise<{ sessions: SessionEntry[]; hasMore: boolean } | null> {
  const r = unwrap<{ sessions?: SessionEntry[]; hasMore?: boolean }>(
    await rpcAction("Load earlier sessions", "get-playtime-sessions", before),
  );
  return r ? { sessions: r.sessions ?? [], hasMore: r.hasMore === true } : null;
}

export async function deletePlaytimeSession(gameId: string, start: number): Promise<boolean> {
  const r = await rpcAction<{ success?: boolean }>(
    "Remove session",
    "playtime-delete-session",
    gameId,
    start,
  );
  return r?.success === true;
}

// ------------ Download Queue ------------
// The download queue: its state, the events it sends, and pausing, resuming, prioritising or cancelling a
// job. Also where a job failure is turned into a notification.
export interface QueueJob {
  id: string;
  gameId: string;
  kind: string;
  baseKind?: string;
  phase: string;
  total: number;
  downloaded: number;
  percent: number;
  speed: number;
  etaSecs: number;
  paused: boolean;
  error: string | null;
  message?: string | null;
  warning?: boolean;
  parked?: boolean;
  waitingNetwork?: boolean;
}

interface QueueState {
  jobs: QueueJob[];
}

export async function downloadState(): Promise<QueueState> {
  const r = await rpcRead<{ jobs?: QueueJob[] }>("get-download-queue-state");
  return { jobs: Array.isArray(r?.jobs) ? r.jobs : [] };
}

export async function onQueueState(cb: (s: QueueState) => void): Promise<() => void> {
  return onEvent<{ jobs?: QueueJob[] }>("download-queue-state", (p) =>
    cb({ jobs: Array.isArray(p?.jobs) ? p.jobs : [] }),
  );
}

const ENDED_PHASES = ["done", "cancelled", "deferred", "error"];

const STARTS_DOWNLOAD = ["install", "update"];

export interface QueuedJobWatch {
  taken: () => boolean;
  stop: () => void;
}

export function watchQueuedJob(
  gameId: string,
  kinds: readonly string[],
  onQueued?: () => void,
): QueuedJobWatch {
  const ours = (jobs: QueueJob[]) =>
    jobs.some(
      (j) =>
        j.gameId === gameId &&
        kinds.includes(j.baseKind || j.kind) &&
        !ENDED_PHASES.includes(j.phase),
    );
  const already = ours(useQueueStore.getState().jobs);
  let taken = false;
  const stop = already
    ? () => {}
    : useQueueStore.subscribe((s) => {
        if (taken || !ours(s.jobs)) return;
        taken = true;
        onQueued?.();
      });
  return { taken: () => taken, stop };
}

export function reportJobFailure(
  label: string,
  gameId: string,
  detail: unknown,
  queued: boolean,
): void {
  log.warn(`[action] ${label} failed:`, detail ?? "(no detail)");
  if (queued) return;
  const text =
    detail instanceof Error
      ? detail.message
      : typeof detail === "string" && detail
        ? detail
        : "Something went wrong. Check the logs for details.";
  useNotificationStore.getState().push({
    type: "error",
    title: `${label} failed`,
    text,
    key: jobFailureKey(gameId),
  });
}

async function queuedAction<T>(
  label: string,
  channel: string,
  job: { gameId: string; kinds: readonly string[]; onQueued?: () => void },
  ...args: unknown[]
): Promise<T | undefined> {
  const watch = watchQueuedJob(job.gameId, job.kinds, job.onQueued);
  try {
    const res = await rpc<T>(channel, ...args);
    const r = res as { success?: boolean; error?: string; cancelled?: boolean } | undefined;
    if (r && typeof r === "object" && r.success === false && !r.cancelled) {
      reportJobFailure(label, job.gameId, r.error, watch.taken());
    }
    return res;
  } catch (e) {
    reportJobFailure(label, job.gameId, e, watch.taken());
    return undefined;
  } finally {
    watch.stop();
  }
}

export async function downloadEnqueue(gameId: string, onQueued?: () => void): Promise<void> {
  await queuedAction(
    "Download",
    "start-download",
    { gameId, kinds: STARTS_DOWNLOAD, onQueued },
    { gameId, versionType: "default" },
  );
}

export async function pauseJob(job: QueueJob): Promise<void> {
  if (job.kind === "repair") {
    await rpcAction("Pause", "pause-repair", job.gameId);
  } else {
    await rpcAction("Pause", "pause-download", job.id);
  }
}

export async function resumeJob(job: QueueJob): Promise<void> {
  if (job.kind === "repair") {
    await rpcAction("Resume", "resume-repair", job.gameId);
  } else {
    await rpcAction("Resume", "resume-download", job.id);
  }
}

export async function prioritizeDownload(id: string): Promise<void> {
  const res = await rpcAction<{ deferred?: string }>("Prioritize download", "prioritize-download", id);
  if (res?.deferred) {
    useNotificationStore.getState().push({ type: "info", title: "Up next", text: res.deferred });
  }
}

export async function downloadCancel(id: string, kind = "install"): Promise<void> {
  const channel =
    kind === "repair"
      ? "cancel-repair"
      : kind === "verify"
        ? "cancel-verify"
        : kind === "move"
          ? "cancel-move"
          : "cancel-download";
  await rpcAction("Cancel", channel, id);
}

// ------------ News and Maintenance ------------
// The news panel and game links, wiping launcher data, and game upkeep: repair, verify, move, uninstall.
export interface NewsEntry {
  content: string;
  time: string;
  jumpUrl: string;
}
export interface NewsSlide {
  url?: string;
  jumpUrl: string;
}
export interface NewsData {
  guidance?: {
    notice?: { contents?: NewsEntry[] };
    news?: { contents?: NewsEntry[] };
  };
  slideshow?: NewsSlide[];
}

export async function applyActiveGame(id: string): Promise<void> {
  await rpcAction("Switch game", "set-active-game", id);
}

export async function getNewsData(id: string): Promise<NewsData | undefined> {
  const outer = await rpcRead<{ data?: NewsData }>("get-news-data", id);
  return outer ? outer.data || {} : undefined;
}

export interface GameLinks {
  socials: { platform: string; url: string }[];
  communityTools: { name: string; url: string }[];
}

export async function getGameLinks(): Promise<Record<string, GameLinks>> {
  const r = await rpcRead<{ links?: Record<string, GameLinks> }>("get-game-links");
  return r?.links || {};
}

export async function openLogsFolder(): Promise<void> {
  await rpcAction("Open logs folder", "open-logs-folder");
}

export async function wipeLauncherData(): Promise<void> {
  let saved: [string, string][] = [];
  try {
    saved = Object.entries(localStorage).filter(([k]) => k.startsWith("peebify."));
    saved.forEach(([k]) => localStorage.removeItem(k));
  } catch (e) {
    log.warn("[action] Could not clear the interface's saved state:", e);
  }
  const r = await rpcAction<{ success?: boolean }>("Wipe launcher data", "wipe-launcher-data");
  if (r?.success === true) return;
  try {
    saved.forEach(([k, v]) => {
      if (localStorage.getItem(k) === null) localStorage.setItem(k, v);
    });
  } catch (e) {
    log.warn("[action] Could not restore the interface's saved state:", e);
  }
}

const repairModes: Record<string, "quick" | "full"> = {};

export const lastRepairMode = (gameId: string): "quick" | "full" | undefined => repairModes[gameId];

export async function repairGame(gameId: string, quick = false): Promise<boolean> {
  repairModes[gameId] = quick ? "quick" : "full";
  const r = await rpcAction<{ success?: boolean }>(
    "Repair",
    quick ? "start-quick-repair" : "start-repair",
    gameId,
  );
  return r?.success === true;
}

interface VerifyResult {
  brokenFiles: number;
  message?: string;
  updatePending?: boolean;
}

export async function verifyGameIntegrity(
  gameId: string,
  onQueued?: () => void,
): Promise<VerifyResult | undefined> {
  const watch = watchQueuedJob(gameId, ["verify"], onQueued);
  const r = await rpcAction<{
    success?: boolean;
    invalidFiles?: unknown[];
    message?: string;
    updatePending?: boolean;
  }>("Verify game files", "verify-game-integrity", gameId).finally(watch.stop);
  if (!r?.success) return undefined;
  return {
    brokenFiles: Array.isArray(r.invalidFiles) ? r.invalidFiles.length : 0,
    message: r.message || undefined,
    updatePending: typeof r.updatePending === "boolean" ? r.updatePending : undefined,
  };
}

export async function moveGame(gameId: string): Promise<void> {
  await queuedAction("Move game", "move-game-location", { gameId, kinds: ["move"] }, gameId);
}

export async function uninstallGame(gameId: string): Promise<boolean> {
  const r = await queuedAction<{ success?: boolean }>(
    "Uninstall",
    "uninstall-game",
    { gameId, kinds: ["uninstall"] },
    gameId,
  );
  return r?.success === true;
}

export async function openLeftoverFolder(gameId: string): Promise<void> {
  await rpcAction("Open folder", "open-leftover-folder", gameId);
}

interface DiskSpace {
  free: number;
  total: number;
  path?: string;
}
export async function getDiskSpace(path?: string): Promise<DiskSpace | null> {
  const r = await rpcRead<DiskSpace>("get-disk-space", path ?? null);
  return r ? { free: r.free || 0, total: r.total || 0, path: r.path } : null;
}

// ------------ Install Flow ------------
// Everything the install dialog needs: disk space, default paths, size preview, content packs, starting
// or resuming a download, and finding a game that is already installed.
export interface InstallPathHealth {
  path: string;
  tooDeep: boolean;
  notWritable: boolean;
  missing: boolean;
  message: string;
}
export async function getInstallPathHealth(gameId: string): Promise<InstallPathHealth> {
  const r = await rpcRead<{
    path?: string | null;
    tooDeep?: boolean;
    notWritable?: boolean;
    missing?: boolean;
    message?: string | null;
  }>("get-install-path-health", gameId);
  return {
    path: r?.path ?? "",
    tooDeep: !!r?.tooDeep,
    notWritable: !!r?.notWritable,
    missing: !!r?.missing,
    message: r?.message ?? "",
  };
}

interface DefaultInstallPath {
  path: string;
  maxRootLength?: number;
  folderName?: string;
}
export async function getDefaultInstallPath(gameId: string): Promise<DefaultInstallPath | null> {
  const r = await rpcRead<DefaultInstallPath>("get-default-install-path", { gameId });
  return r?.path
    ? {
        path: r.path,
        maxRootLength: r.maxRootLength ?? 0,
        folderName: r.folderName,
      }
    : null;
}

interface InstallPreviewPart {
  key: string;
  label: string;
  bytes: number;
  files: number;
}
export interface InstallPreview {
  gameId: string;
  version: string | null;
  downloadBytes: number | null;
  installBytes: number | null;
  requiredBytes: number | null;
  fileCount: number | null;
  approximate: boolean;
  parts: InstallPreviewPart[];
  notSupported: boolean;
}
interface InstallPreviewResult {
  preview: InstallPreview | null;
  error: string | null;
}
export async function getInstallPreview(gameId: string): Promise<InstallPreviewResult> {
  const r = (await rpc("get-install-preview", gameId).catch((e) => {
    log.warn("[read] get-install-preview failed:", e);
    return null;
  })) as (Partial<InstallPreview> & { success?: boolean; error?: string }) | null;
  if (!r || r.success === false) {
    if (r?.error) log.warn("[read] get-install-preview failed:", r.error);
    return { preview: null, error: r?.error ?? null };
  }
  return {
    preview: {
      gameId: r.gameId ?? gameId,
      version: r.version ?? null,
      downloadBytes: r.downloadBytes ?? null,
      installBytes: r.installBytes ?? null,
      requiredBytes: r.requiredBytes ?? null,
      fileCount: r.fileCount ?? null,
      approximate: !!r.approximate,
      parts: Array.isArray(r.parts) ? r.parts : [],
      notSupported: !!r.notSupported,
    },
    error: null,
  };
}

export interface ContentPack {
  tag: string;
  language: string | null;
  bytes: number;
  files: number;
  selected: boolean;
}
interface ContentPackList {
  supported: boolean;
  packs: ContentPack[];
  error: boolean;
}
export async function getContentPacks(gameId: string): Promise<ContentPackList> {
  const r = await rpcRead<{ packs?: ContentPack[]; supported?: boolean }>(
    "get-content-packs",
    gameId,
  );
  if (!r) return { supported: true, packs: [], error: true };
  if (!r.supported) return { supported: false, packs: [], error: false };
  return { supported: true, packs: Array.isArray(r.packs) ? r.packs : [], error: false };
}

export async function setContentPacks(gameId: string, tags: string[] | null): Promise<boolean> {
  const r = await rpcAction<{ success?: boolean }>(
    "Save setting",
    "set-config",
    `games.${gameId}.contentTags`,
    tags,
  );
  return !!r && r.success !== false;
}

export async function selectInstallDirectory(): Promise<string | null> {
  const r = await rpc<{ canceled?: boolean; path?: string }>("select-install-directory");
  return r && !r.canceled && r.path ? r.path : null;
}

export async function startDownload(
  installPath: string,
  gameId: string,
  onQueued?: () => void,
): Promise<void> {
  await queuedAction(
    "Install",
    "start-download",
    { gameId, kinds: STARTS_DOWNLOAD, onQueued },
    { installPath, versionType: "default", gameId },
  );
}

export async function resumeInstall(gameId: string, pending: PendingInstall): Promise<void> {
  await queuedAction(
    "Resume install",
    "start-download",
    { gameId, kinds: STARTS_DOWNLOAD },
    { installPath: pending.path, versionType: pending.versionType, gameId },
  );
}

interface LocateResult {
  ok: boolean;
  path?: string;
  error?: string;
  cancelled?: boolean;
}

export async function locateGameInstall(gameId: string): Promise<LocateResult> {
  const r = (await rpc("browse-game-path", gameId).catch((e) => {
    log.warn("[action] Locate game install failed:", e);
    return null;
  })) as {
    success?: boolean;
    path?: string;
    cancelled?: boolean;
    error?: string;
  } | null;
  if (r?.success && r.path) return { ok: true, path: r.path };
  return { ok: false, cancelled: !!r?.cancelled, error: r?.error };
}

export async function detectDefaultGameInstall(gameId: string): Promise<LocateResult> {
  const r = (await rpc("detect-default-game-path", gameId).catch((e) => {
    log.warn("[action] Detect default game install failed:", e);
    return null;
  })) as { success?: boolean; path?: string; error?: string } | null;
  if (r?.success && r.path) return { ok: true, path: r.path };
  return { ok: false, error: r?.error };
}

export async function openGameFolder(gameId: string): Promise<void> {
  await rpcAction("Open game folder", "open-game-folder", gameId);
}

export async function openScreenshotFolder(gameId: string): Promise<void> {
  await rpcAction("Open screenshots folder", "open-screenshot-folder", gameId);
}

// ------------ Network and Window Events ------------
// Online status, window maximize and restore, power saving, and the install complete and games
// detected events.
export async function getNetworkStatus(): Promise<boolean | null> {
  const r = await rpcRead<{ isOnline?: boolean }>("get-network-status");
  return typeof r?.isOnline === "boolean" ? r.isOnline : null;
}

export async function networkRecheck(): Promise<boolean | null> {
  const r = await rpcRead<{ isOnline?: boolean }>("network-recheck");
  return typeof r?.isOnline === "boolean" ? r.isOnline : null;
}

export async function onNetworkStatusChanged(
  cb: (online: boolean) => void,
): Promise<() => void> {
  return onEvent<{ isOnline?: boolean }>("network-status-changed", (p) =>
    cb(p?.isOnline !== false),
  );
}

export async function onInstallationComplete(
  cb: (gameId: string, version: string | null) => void,
): Promise<() => void> {
  return onEvent<{ gameId?: string; version?: string | null }>("installation-complete", (p) =>
    cb(String(p?.gameId || ""), p?.version ?? null),
  );
}

export async function onGamesDetected(
  cb: (gameIds: string[]) => void,
): Promise<() => void> {
  return onEvent<{ gameIds?: unknown }>("games-detected", (p) =>
    cb(Array.isArray(p?.gameIds) ? p.gameIds.map(String) : []),
  );
}

export async function onRendererPowerSave(cb: (suspended: boolean) => void): Promise<() => void> {
  return onEvent<boolean>("renderer-power-save", (p) => cb(p !== false));
}

export async function onWindowRestored(cb: () => void): Promise<() => void> {
  return onEvent<unknown>("window-restored", () => cb());
}

export async function onWindowMaximizedChanged(
  cb: (maximized: boolean) => void,
): Promise<() => void> {
  return onEvent<{ maximized?: boolean }>("window:maximized-changed", (p) => cb(!!p?.maximized));
}

export async function toggleMaximizeWindow(): Promise<boolean> {
  const r = await rpcRead<{ maximized?: boolean }>("toggle-maximize-window");
  return !!r?.maximized;
}

export async function getWindowMaximized(): Promise<boolean> {
  const r = await rpcRead<{ maximized?: boolean }>("get-window-state");
  return !!r?.maximized;
}

export async function getRendererPowerSave(): Promise<boolean> {
  const r = await rpcRead<{ powerSave?: boolean }>("get-window-state");
  return r?.powerSave === true;
}

const num = (v: unknown, fallback = 0): number =>
  typeof v === "number" && Number.isFinite(v) ? v : fallback;
const strOrNull = (v: unknown): string | null => (typeof v === "string" && v ? v : null);
const rec = (v: unknown): Record<string, unknown> | null =>
  v && typeof v === "object" && !Array.isArray(v) ? (v as Record<string, unknown>) : null;

// ------------ Captures ------------
// Listing, opening, revealing and deleting screenshots and clips, and the event that says the list changed.
type MediaKind = "screenshot" | "clip";

export interface MediaItem {
  path: string;
  kind: MediaKind;
  gameId: string | null;
  name: string;
  sizeBytes: number;
  takenAt: number;
  thumbPath: string | null;
}

interface CaptureList {
  ok: boolean;
  items: MediaItem[];
  folder: string;
  error?: string;
}

function asMediaItem(raw: unknown): MediaItem | null {
  const m = rec(raw);
  if (!m || typeof m.path !== "string") return null;
  const takenAt = typeof m.takenAt === "string" ? Date.parse(m.takenAt) : NaN;
  return {
    path: m.path,
    kind: m.kind === "clip" ? "clip" : "screenshot",
    gameId: strOrNull(m.gameId),
    name: typeof m.name === "string" ? m.name : "",
    sizeBytes: num(m.sizeBytes),
    takenAt: Number.isFinite(takenAt) ? takenAt : 0,
    thumbPath: strOrNull(m.thumbPath),
  };
}

export async function listCaptures(): Promise<CaptureList> {
  const empty: CaptureList = { ok: false, items: [], folder: "" };
  const raw = await rpc<{
    success: boolean;
    captures?: unknown[];
    folder?: string;
    error?: string;
  }>("overlay-list-captures");
  if (!raw?.success) return { ...empty, error: raw?.error || "Could not read your captures." };
  return {
    ok: true,
    items: Array.isArray(raw.captures)
      ? raw.captures.flatMap((c) => {
          const parsed = asMediaItem(c);
          return parsed ? [parsed] : [];
        })
      : [],
    folder: typeof raw.folder === "string" ? raw.folder : "",
  };
}

const isCaptureFolderKey = (key: unknown) =>
  key === "overlayCaptureFolder" || key === "behavior.overlayCaptureFolder";

export async function onCapturesChanged(cb: () => void): Promise<() => void> {
  const a = await onEvent<unknown>("overlay-capture", () => cb());
  const b = await onEvent<unknown>("overlay-record-stopped", () => cb());
  const c = await onEvent<unknown>("overlay-capture-deleted", () => cb());
  const d = await onEvent<{ key?: unknown } | null>("settings-changed", (payload) => {
    if (isCaptureFolderKey(payload?.key)) cb();
  });
  return () => {
    a();
    b();
    c();
    d();
  };
}

export async function deleteCapture(path: string): Promise<{ ok: boolean; error?: string }> {
  const raw = await rpc<{ success: boolean; error?: string }>("overlay-delete-capture", path);
  if (raw?.success) return { ok: true };
  return { ok: false, error: raw?.error || "Could not delete that capture." };
}

export async function openCapture(path: string): Promise<void> {
  await rpcAction("Open", "overlay-open-capture", path);
}

export async function revealCapture(path: string): Promise<void> {
  await rpcAction("Show in folder", "overlay-reveal-capture", path);
}