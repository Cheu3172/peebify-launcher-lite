// ------------ Mods Page ------------
// The Mods tab. Shows the mod support switch, the game picker and setup for the mod tools (XXMI, the loader
// Peebify uses), then switches between the Library and Browse views.
import { useCallback, useEffect, useState } from "react";
import { AnimatePresence, m } from "framer-motion";
import {
  AlertTriangle,
  ArrowUpCircle,
  Download,
  Eye,
  FolderCog,
  FolderOpen,
  Lock,
  Package,
  Puzzle,
  RefreshCw,
  Settings2,
  Timer,
} from "lucide-react";
import { fadeVariants } from "../../lib/motion";
import { requestEnableMods } from "../../lib/mods";
import { gameById } from "../../data/games";
import type { GameId } from "../../types/game";
import { useUiStore } from "../../store/uiStore";
import { useSessionUiStore } from "../../store/sessionUiStore";
import { useModsStore } from "../../store/modsStore";
import { useNotificationStore } from "../../store/notificationStore";
import { useModalStore } from "../../store/modalStore";
import {
  chooseModsPath,
  getLoaderSettings,
  installModToolchain,
  openModsFolder,
  resetModsPath,
  setConfigValue,
  setGameModsEnabled,
  setLoaderSetting,
  uninstallModToolchain,
  type LoaderSetting,
} from "../../lib/ipc";
import { NumberField } from "../ui/NumberField";
import { GamePickerGrid } from "../settings/GamePickerGrid";
import { GlassPage } from "../ui/GlassPage";
import { GroupBox } from "../ui/SettingsGroup";
import { SettingCard } from "../ui/SettingCard";
import { SettingsExpander } from "../ui/SettingsExpander";
import { ActionButton } from "../ui/ActionButton";
import { SegmentedControl } from "../ui/SegmentedControl";
import { Toggle } from "../ui/Toggle";
import { ModsHub, type BrowseSeed } from "./ModsHub";
import { BrowseMods } from "./BrowseMods";

export function ModsPage() {
  const activeId = useUiStore((s) => s.activeGameId);
  const push = useNotificationStore((s) => s.push);
  const openConfirm = useModalStore((s) => s.openConfirm);
  const status = useModsStore((s) => s.status);
  const mods = useModsStore((s) => s.mods);
  const loadedGameId = useModsStore((s) => s.loadedGameId);
  const requestedGameId = useModsStore((s) => s.requestedGameId);
  const refresh = useModsStore((s) => s.refresh);
  const refreshIfCurrent = useModsStore((s) => s.refreshIfCurrent);
  const setProgress = useModsStore((s) => s.setProgress);
  const setMasterEnabled = useModsStore((s) => s.setMasterEnabled);

  const game = gameById(activeId);
  const tab = useSessionUiStore((s) => s.modsTab);
  const setTab = useSessionUiStore((s) => s.setModsTab);
  const [toolsBusyFor, setToolsBusyFor] = useState<ReadonlySet<string>>(() => new Set());
  const toolsBusy = toolsBusyFor.has(activeId);
  const markToolsBusy = (id: string, on: boolean) =>
    setToolsBusyFor((prev) => {
      const next = new Set(prev);
      if (on) next.add(id);
      else next.delete(id);
      return next;
    });
  const [setupOpen, setSetupOpen] = useState<boolean | null>(null);
  const [browseSeed, setBrowseSeed] = useState<BrowseSeed | null>(null);
  const clearBrowseSeed = useCallback(() => setBrowseSeed(null), []);
  const [loaderSettings, setLoaderSettings] = useState<LoaderSetting[]>([]);

  useEffect(() => {
    setSetupOpen(null);
    void refresh(activeId);
  }, [activeId, refresh]);

  const current = loadedGameId === activeId;

  useEffect(() => {
    const s = useModsStore.getState();
    if (s.loadedGameId !== null && s.loadedGameId !== activeId && s.requestedGameId !== activeId) {
      void refresh(activeId);
    }
  }, [loadedGameId, requestedGameId, activeId, refresh]);

  const toolsInstalled = status?.toolchainInstalled ?? false;

  useEffect(() => {
    let cancelled = false;
    if (!toolsInstalled) {
      setLoaderSettings([]);
      return;
    }
    void getLoaderSettings(activeId).then((rows) => {
      if (!cancelled) setLoaderSettings(rows);
    });
    return () => {
      cancelled = true;
    };
  }, [activeId, toolsInstalled]);

  const changeLoaderSetting = async (id: string, value: string) => {
    const gameId = activeId;
    const ok = await setLoaderSetting(gameId, id, value);
    if (!ok) return;
    const rows = await getLoaderSettings(gameId);
    if (useUiStore.getState().activeGameId === gameId) setLoaderSettings(rows);
  };

  const master = status?.masterEnabled ?? false;
  const supported = status?.supportsMods ?? false;
  const gameOn = status?.gameEnabled ?? false;
  const modCapableIds = (status?.supportedGames ?? []).map((g) => g.id as GameId);
  const toolchainUpdates = status?.updates ?? [];
  const autoUpdate = status?.autoUpdate ?? true;
  const updateSummary = toolchainUpdates
    .map((u) => `${u.package.toUpperCase()} ${u.installed} → ${u.latest}`)
    .join(", ");
  const hasGameBanana = !!status?.supportedGames.find((g) => g.id === activeId)?.gameBananaId;
  const showSetup = setupOpen ?? !toolsInstalled;
  const library = tab === "installed" || !hasGameBanana;

  const variantLabel = status?.variant?.toUpperCase() ?? "XXMI";
  const versionLabel =
    status?.variant && status.versions[status.variant]
      ? `${variantLabel} ${status.versions[status.variant]}`
      : variantLabel;
  const setupSummary = !toolsInstalled
    ? "The mod tools aren't installed yet"
    : `${versionLabel} installed, mods ${gameOn ? "on" : "off"} for ${game.name}`;

  const enableMaster = () =>
    requestEnableMods(() => {
      void setConfigValue("behavior.modsEnabled", true).then(() => {
        setMasterEnabled(true);
        void refreshIfCurrent(activeId);
      });
    });

  const toggleGame = async (on: boolean) => {
    const result = await setGameModsEnabled(activeId, on);
    if (!on && result?.sweptFiles.length) {
      push({
        title: "Cleaned up mod files",
        text: `Removed ${result.sweptFiles.length} leftover 3DMigoto file${
          result.sweptFiles.length === 1 ? "" : "s"
        } from the ${game.name} folder.`,
      });
    }
    void refreshIfCurrent(activeId);
  };

  const installTools = async () => {
    if (toolsBusy) return;
    markToolsBusy(activeId, true);
    const result = await installModToolchain(activeId);
    markToolsBusy(activeId, false);
    setProgress(null);
    if (result) {
      push(
        result.changed
          ? { type: "success", title: "Mod tools ready", text: `They're installed for ${game.name}.` }
          : { title: "Nothing to update", text: `The mod tools for ${game.name} are already up to date.` },
      );
    }
    void refreshIfCurrent(activeId);
  };

  const confirmUninstallTools = () =>
    openConfirm({
      title: `Uninstall the mod tools for ${game.name}?`,
      message: `This removes the ${variantLabel} loader and clears any 3DMigoto files left in the ${game.name} folder. Your mods and their settings stay installed, so they'll work again if you reinstall the tools.`,
      confirmLabel: "Uninstall tools",
      danger: true,
      onConfirm: () => {
        void (async () => {
          markToolsBusy(activeId, true);
          const result = await uninstallModToolchain(activeId);
          markToolsBusy(activeId, false);
          if (result?.leftovers.length) {
            push({
              type: "warning",
              title: "Some mod tool files were left behind",
              text: `Peebify couldn't remove ${result.leftovers.length} item(s), such as ${result.leftovers[0]}. Close anything using them and try again.`,
            });
          } else if (result) {
            push({
              type: "success",
              title: "Mod tools removed",
              text: result.keptMods.length
                ? `Your ${game.name} mods are still there for when you reinstall.`
                : "There were no mods to keep.",
            });
          }
          void refreshIfCurrent(activeId);
        })();
      },
    });

  const announceFolder = async (path: string | undefined) => {
    if (!path) return;
    await refreshIfCurrent(activeId);
    const found = useModsStore.getState().status?.toolchainInstalled ?? false;
    push({
      title: "Mod tools folder changed",
      text: found
        ? `Peebify now uses ${path}. The ${variantLabel} tools are already in it.`
        : `Peebify now uses ${path}. The ${variantLabel} tools aren't in it yet, so install them from Setup.`,
    });
  };

  const oldFolder = status?.toolchainPath ?? "";

  const confirmChangeFolder = () =>
    openConfirm({
      title: "Use a different mod tools folder?",
      message: `Nothing gets moved. Your mods and tools stay in ${oldFolder}, and Peebify only shows what is in the folder you pick. Pick this folder again to see them.`,
      confirmLabel: "Choose folder",
      onConfirm: () => void chooseModsPath().then(announceFolder),
    });

  const confirmResetFolder = () =>
    openConfirm({
      title: "Go back to the default folder?",
      message: `Nothing gets moved. Your mods and tools stay in ${oldFolder}, and Peebify goes back to its own folder. Pick this folder again to see them.`,
      confirmLabel: "Reset folder",
      onConfirm: () => void resetModsPath().then(announceFolder),
    });

  return (
    <GlassPage title="Mods" subtitle="Your mods, profiles and downloads for each game.">
      {!status ? null : !master ? (
        <div className="mx-auto mt-16 max-w-[460px] rounded-ui border border-dashed border-white/15 bg-white/[0.03] px-8 py-10 text-center">
          <span className="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-full bg-white/[0.06] text-white/60">
            <AlertTriangle size={22} />
          </span>
          <h2 className="text-[16px] font-semibold">Mod support is turned off</h2>
          <p className="mt-2 text-[13px] leading-[1.6] text-white/55">
            Mods are made by other people and using them can get your account banned, so Peebify
            keeps them off until you say otherwise. Turn them on to manage and browse mods here.
          </p>
          <div className="mt-5 flex justify-center">
            <ActionButton variant="accent" onClick={enableMaster}>
              Turn on mod support
            </ActionButton>
          </div>
        </div>
      ) : (
        <>
          <GamePickerGrid
            ids={modCapableIds.length ? modCapableIds : undefined}
            subtitleFor={(id) => {
              const g = status.supportedGames.find((s) => s.id === id);
              if (!g) return { text: "No mod support", tone: "off" };
              if (!g.installed) return { text: `${g.variant.toUpperCase()} · not installed`, tone: "off" };
              if (!g.enabled) return { text: `${g.variant.toUpperCase()} · ready`, tone: "off" };
              return { text: `${g.variant.toUpperCase()} · on`, tone: "on" };
            }}
          />

          {!supported ? (
            <GroupBox>
              <SettingCard
                icon={status.unsupportedReason ? <AlertTriangle size={18} /> : <Puzzle size={18} />}
                title={`Mods aren't available for ${game.name}`}
                description={
                  status.unsupportedReason ??
                  "No one has made a mod loader for this game yet, so there's nothing for Peebify to use."
                }
              />
            </GroupBox>
          ) : (
            <>
              <GroupBox>
                <SettingsExpander
                  icon={<Settings2 size={18} />}
                  title="Setup"
                  summary={setupSummary}
                  open={showSetup}
                  onOpenChange={setSetupOpen}
                >
                  <SettingCard
                    icon={<Puzzle size={18} />}
                    title={`Use mods in ${game.name}`}
                    description={
                      !toolsInstalled
                        ? "Install the mod tools below first, there's nothing to load your mods with until you do."
                        : status.experimental
                          ? "This game is experimental! The mod loader is less reliable here than on the others."
                          : "The mod loader starts just before the game, so Windows will ask for permission. Only the mods you switch on get loaded in."
                    }
                    disabled={!toolsInstalled}
                    control={
                      <Toggle
                        checked={gameOn}
                        disabled={!toolsInstalled || !current}
                        ariaLabel={`Use mods in ${game.name}`}
                        onChange={(v) => void toggleGame(v)}
                      />
                    }
                  />
                  {status.forcesDx11 && gameOn && game.graphicsApiChoice && (
                    <SettingCard
                      icon={<Lock size={18} />}
                      title="DirectX is locked to DX11"
                      description={`The mod tools only work with DX11, so ${game.name} always launches with DX11 while mods are on.`}
                    />
                  )}
                  <SettingCard
                    icon={toolchainUpdates.length ? <ArrowUpCircle size={18} /> : <Package size={18} />}
                    title="XXMI mod tools"
                    description={
                      !toolsInstalled
                        ? "Not installed. Peebify uses XXMI to load mods into the game, so nothing can load until it is."
                        : toolchainUpdates.length
                          ? autoUpdate
                            ? `${versionLabel} is installed. A newer version is out (${updateSummary}). Peebify installs it on its own while no game is running, or you can update now.`
                            : `${versionLabel} is installed. A newer version is out (${updateSummary}). Update now when you are ready.`
                          : `${versionLabel} is installed for ${game.name}. Peebify checks for newer versions in the background.`
                    }
                    control={
                      toolsInstalled ? (
                        <div className="flex items-center gap-2">
                          <ActionButton
                            variant={toolchainUpdates.length ? "accent" : undefined}
                            onClick={() => void installTools()}
                            disabled={toolsBusy || !current}
                            icon={toolsBusy ? undefined : <RefreshCw size={15} />}
                          >
                            {toolsBusy
                              ? "Working…"
                              : toolchainUpdates.length
                                ? "Update now"
                                : "Check for updates"}
                          </ActionButton>
                          <ActionButton
                            variant="danger"
                            onClick={confirmUninstallTools}
                            disabled={toolsBusy || !current}
                          >
                            Uninstall
                          </ActionButton>
                        </div>
                      ) : (
                        <ActionButton
                          variant="accent"
                          onClick={() => void installTools()}
                          disabled={toolsBusy || !current}
                          icon={toolsBusy ? undefined : <Download size={15} />}
                        >
                          {toolsBusy ? "Installing…" : "Install mod tools"}
                        </ActionButton>
                      )
                    }
                  />
                  <SettingCard
                    icon={<RefreshCw size={18} />}
                    title="Update the mod tools on their own"
                    description="Peebify installs new XXMI versions in the background while no game is running. Turn this off to keep the version you have until you update it yourself."
                    control={
                      <Toggle
                        checked={autoUpdate}
                        disabled={!current}
                        ariaLabel="Update the mod tools on their own"
                        onChange={(v) =>
                          void setConfigValue("behavior.modsAutoUpdate", v).then(() =>
                            refreshIfCurrent(activeId),
                          )
                        }
                      />
                    }
                  />
                  {loaderSettings
                    .filter((row) => row.kind === "bool" || row.kind === "millis")
                    .map((row) => (
                      <SettingCard
                        key={row.id}
                        icon={row.kind === "bool" ? <Eye size={18} /> : <Timer size={18} />}
                        title={row.label}
                        description={`${row.description} Takes effect the next time the game starts.`}
                        control={
                          row.kind === "bool" ? (
                            <Toggle
                              checked={row.value === "1"}
                              disabled={!current}
                              ariaLabel={row.label}
                              onChange={(v) => void changeLoaderSetting(row.id, v ? "1" : "0")}
                            />
                          ) : (
                            <NumberField
                              value={row.value}
                              min={row.min ?? 0}
                              max={row.max ?? 10000}
                              unit="ms"
                              zeroLabel="0"
                              disabled={!current}
                              ariaLabel={row.label}
                              onChange={(v) => void changeLoaderSetting(row.id, v)}
                            />
                          )
                        }
                      />
                    ))}
                  <SettingCard
                    icon={<FolderCog size={18} />}
                    title="Mod tools folder"
                    description={
                      status.isDefault
                        ? `${status.toolchainPath} (Peebify's own folder)`
                        : status.toolchainPath
                    }
                    control={
                      <div className="flex items-center gap-2">
                        <ActionButton
                          icon={<FolderOpen size={15} />}
                          disabled={!toolsInstalled || !current}
                          onClick={() => void openModsFolder(activeId)}
                        >
                          Open
                        </ActionButton>
                        <ActionButton onClick={confirmChangeFolder}>Change…</ActionButton>
                        <ActionButton
                          disabled={status.isDefault}
                          onClick={confirmResetFolder}
                        >
                          Reset
                        </ActionButton>
                      </div>
                    }
                  />
                </SettingsExpander>
              </GroupBox>

              <div className="mb-5 mt-6">
                <SegmentedControl
                  ariaLabel="Mods view"
                  options={[
                    { value: "installed", label: mods.length ? `Library (${mods.length})` : "Library" },
                    ...(hasGameBanana ? [{ value: "browse", label: "Browse" }] : []),
                  ]}
                  value={library ? "installed" : "browse"}
                  onChange={(v) => setTab(v as "installed" | "browse")}
                />
              </div>

              <AnimatePresence mode="wait" initial={false}>
                <m.div
                  key={library ? "library" : "browse"}
                  variants={fadeVariants}
                  initial="initial"
                  animate="animate"
                  exit="exit"
                >
                  {library ? (
                    <ModsHub
                      key={activeId}
                      gameId={activeId}
                      onBrowse={hasGameBanana ? () => setTab("browse") : undefined}
                      onOpenGameBanana={
                        hasGameBanana
                          ? (seed) => {
                              setBrowseSeed(seed);
                              setTab("browse");
                            }
                          : undefined
                      }
                    />
                  ) : (
                    <BrowseMods
                      key={activeId}
                      gameId={activeId}
                      initialMod={browseSeed}
                      onInitialModHandled={clearBrowseSeed}
                    />
                  )}
                </m.div>
              </AnimatePresence>
            </>
          )}
        </>
      )}
    </GlassPage>
  );
}
