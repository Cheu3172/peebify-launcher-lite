// ------------ Bottom Right Actions ------------
// The pair of buttons at the bottom right of the Home screen. Community tools opens the game's list of links
// (wiki, maps and so on), and Quick settings holds repair, move, uninstall, folder shortcuts, DirectX and
// resource quality for the selected game.
import { Fragment, useEffect, useState } from "react";
import {
  Users,
  Settings,
  ExternalLink,
  Wrench,
  FolderOpen,
  FolderInput,
  FolderSearch,
  Trash2,
  Images,
  Cpu,
  Layers,
  Zap,
  RefreshCw,
  Globe,
  Newspaper,
  Map as MapIcon,
  Hammer,
  LineChart,
  BookOpen,
  Share2,
} from "lucide-react";
import type { GameId } from "../../types/game";
import { ExpandUp } from "./ExpandUp";
import { Hint } from "../ui/Tooltip";
import { useUiStore } from "../../store/uiStore";
import { useCustomizationStore } from "../../store/customizationStore";
import { useGameLinksStore } from "../../store/remoteContentStore";
import { gameById, supportsQuickRepair } from "../../data/games";
import { openExternal } from "../../lib/tauri";
import {
  JOB_ACTIVE_BLOCKER,
  qualityLabel,
  useGameActions,
  useModsForceDx11,
  useResourceQuality,
} from "../../lib/gameActions";
import {
  openGameFolder,
  openScreenshotFolder,
  getCustomLauncher,
  defaultGraphicsApi,
  RESOURCE_QUALITY_OPTIONS,
  type GraphicsApi,
} from "../../lib/ipc";

const MENU_WIDTH = "w-[200px] min-[1000px]:w-[236px]";

export function BottomRightActions() {
  return (
    <div className="absolute bottom-[22px] right-4 z-[6] flex items-end gap-[10px]">
      <CommunityToolsMenu />
      <QuickSettingsMenu />
    </div>
  );
}

const TOOL_ICONS: { match: RegExp; icon: typeof Globe }[] = [
  { match: /official|homepage|site/i, icon: Globe },
  { match: /news|notice|blog/i, icon: Newspaper },
  { match: /track/i, icon: LineChart },
  { match: /map/i, icon: MapIcon },
  { match: /build|guide|tier/i, icon: Hammer },
  { match: /wiki|database/i, icon: BookOpen },
  { match: /network|hub|community/i, icon: Share2 },
];

function toolMeta(rawName: string, game: { name: string; short: string }) {
  let label = rawName.trim();
  for (const prefix of [game.name, game.short]) {
    if (label.toLowerCase().startsWith(prefix.toLowerCase())) {
      const stripped = label.slice(prefix.length).replace(/^[\s:·–—-]+/, "").trim();
      if (stripped) {
        label = stripped;
        break;
      }
    }
  }
  if (/^official$/i.test(label)) label = "Official Site";
  const icon = TOOL_ICONS.find((t) => t.match.test(rawName))?.icon ?? ExternalLink;
  return { label, icon };
}

function CommunityToolsMenu() {
  const activeId = useUiStore((s) => s.activeGameId);
  const game = gameById(activeId);
  const tools = useGameLinksStore((s) => s.links[activeId]?.communityTools) ?? [];
  return (
    <ExpandUp className={MENU_WIDTH} icon={Users} label="Community tools">
      {(close) => (
        <div className="py-[6px]">
          {tools.map((t) => {
            const { label, icon: Icon } = toolMeta(t.name, game);
            return (
              <Hint key={t.name} tip={t.name} placement="left" className="block w-full">
                <button
                  onClick={() => {
                    close();
                    void openExternal(t.url);
                  }}
                  className="group flex w-full items-center gap-[10px] px-[12px] py-[7px] text-left text-[13px] text-white/85 transition-colors hover:bg-white/[0.06]"
                >
                  <span className="grid h-[28px] w-[28px] shrink-0 place-items-center rounded-[8px] bg-white/[0.06] text-white/65 transition-colors group-hover:bg-white/[0.09] group-hover:text-white/90">
                    <Icon size={14} />
                  </span>
                  <span className="flex-1 truncate">{label}</span>
                  <ExternalLink
                    size={12}
                    className="shrink-0 opacity-0 transition-opacity group-hover:opacity-50"
                  />
                </button>
              </Hint>
            );
          })}
        </div>
      )}
    </ExpandUp>
  );
}

const GRAPHICS_APIS: { value: GraphicsApi; label: string }[] = [
  { value: "dx11", label: "DX11" },
  { value: "dx12", label: "DX12" },
];

function PillSwitch<T extends string>({
  options,
  value,
  disabled,
  onChange,
}: {
  options: { value: T; label: string }[];
  value: T;
  disabled?: boolean;
  onChange: (v: T) => void;
}) {
  return (
    <span
      className={`flex shrink-0 items-center gap-[2px] rounded-[7px] border border-white/[0.09] bg-black/30 p-[2px] ${
        disabled ? "opacity-45" : ""
      }`}
    >
      {options.map((o) => {
        const active = o.value === value;
        return (
          <button
            key={o.value}
            disabled={disabled}
            onClick={() => onChange(o.value)}
            aria-pressed={active}
            className={`rounded-[5px] px-[6px] py-[3px] text-[10px] font-semibold leading-none tracking-[0.01em] transition-colors ${
              active
                ? "bg-white/[0.14] text-white"
                : disabled
                  ? "text-white/30"
                  : "text-white/45 hover:text-white/80"
            }`}
          >
            {o.label}
          </button>
        );
      })}
    </span>
  );
}

function useCustomLauncherSet(gameId: string): boolean {
  const [set, setSet] = useState(false);
  useEffect(() => {
    let alive = true;
    void getCustomLauncher(gameId).then((path) => {
      if (alive) setSet(!!path);
    });
    return () => {
      alive = false;
    };
  }, [gameId]);
  return set;
}

const NOT_USED_WITH_CUSTOM_LAUNCHER = "Not used while another program starts the game.";

function DirectXRow({
  gameId,
  installed,
  forceDx11,
  customLauncher,
}: {
  gameId: GameId;
  installed: boolean;
  forceDx11: boolean;
  customLauncher: boolean;
}) {
  const api = useCustomizationStore((s) => s.graphicsApi[gameId]) ?? defaultGraphicsApi(gameId);
  const setGraphicsApi = useCustomizationStore((s) => s.setGraphicsApi);
  const locked = !installed || forceDx11 || customLauncher;

  return (
    <Hint
      tip={forceDx11 && !customLauncher ? "Mods need DirectX 11" : undefined}
      placement="left"
      className="block w-full"
    >
      <div
        className={`flex w-full items-center gap-[10px] px-[12px] py-[7px] text-[13px] ${
          locked ? "text-white/30" : "text-white/85"
        }`}
      >
        <span
          className={`grid h-[28px] w-[28px] shrink-0 place-items-center rounded-[8px] ${
            locked ? "bg-white/[0.04] text-white/25" : "bg-white/[0.06] text-white/65"
          }`}
        >
          <Cpu size={14} />
        </span>
        <span className="flex min-w-0 flex-1 flex-col">
          <span className="truncate">DirectX</span>
          {customLauncher && (
            <span className="whitespace-normal text-[11px] leading-snug text-white/35">
              {NOT_USED_WITH_CUSTOM_LAUNCHER}
            </span>
          )}
        </span>
        <PillSwitch
          options={GRAPHICS_APIS}
          value={forceDx11 ? "dx11" : api}
          disabled={locked}
          onChange={(v) => setGraphicsApi(gameId, v)}
        />
      </div>
    </Hint>
  );
}

function ResourceQualityRow({
  installed,
  jobActive,
  running,
  customLauncher,
  resourceQuality: { quality, setQuality, installedLabel, pending, apply },
}: {
  installed: boolean;
  jobActive: boolean;
  running: boolean;
  customLauncher: boolean;
  resourceQuality: ReturnType<typeof useResourceQuality>;
}) {
  const locked = customLauncher;
  const hint = customLauncher
    ? NOT_USED_WITH_CUSTOM_LAUNCHER
    : `${installedLabel} is installed. Apply to switch to ${qualityLabel(quality)}.`;

  return (
    <Hint
      tip={!customLauncher && !pending && !installed ? "The quality that gets installed." : undefined}
      placement="left"
      className="block w-full"
    >
      <div className={`w-full px-[12px] py-[7px] text-[13px] ${locked ? "text-white/30" : "text-white/85"}`}>
        <div className="flex items-center gap-[10px]">
          <span
            className={`grid h-[28px] w-[28px] shrink-0 place-items-center rounded-[8px] ${
              locked ? "bg-white/[0.04] text-white/25" : "bg-white/[0.06] text-white/65"
            }`}
          >
            <Layers size={14} />
          </span>
          <span className="min-w-0 flex-1 truncate">Quality</span>
          <PillSwitch
            options={RESOURCE_QUALITY_OPTIONS}
            value={quality}
            disabled={locked}
            onChange={setQuality}
          />
        </div>
        {(customLauncher || pending) && (
          <div className="mt-[6px] flex items-center gap-[8px] pl-[38px]">
            <span className="min-w-0 flex-1 text-[11px] leading-snug text-white/45">{hint}</span>
            {pending && !locked && (
              <button
                onClick={apply}
                disabled={jobActive || running}
                className="shrink-0 rounded-[6px] bg-white/[0.12] px-[9px] py-[5px] text-[11px] font-semibold leading-none text-white transition-colors hover:bg-white/[0.2] disabled:opacity-40"
              >
                Apply
              </button>
            )}
          </div>
        )}
      </div>
    </Hint>
  );
}

function QuickSettingsMenu() {
  const activeId = useUiStore((s) => s.activeGameId);
  const {
    game,
    managed,
    installed,
    partial,
    running,
    jobActive,
    steamCopy,
    uninstallBlocker,
    checkUpdates,
    quickRepair,
    confirmFullRepair,
    locate,
    confirmMove,
    confirmUninstall,
  } = useGameActions(activeId);
  const forceDx11 = useModsForceDx11(activeId);
  const customLauncher = useCustomLauncherSet(activeId);
  const resourceQuality = useResourceQuality(activeId, { installed, steamCopy, jobActive });

  type Item = {
    icon: typeof Wrench;
    label: string;
    hint?: string;
    danger?: boolean;
    disabled?: boolean;
    dividerBefore?: boolean;
    onClick: () => void;
  };

  const options: Item[] = [
    {
      icon: RefreshCw,
      label: "Check for updates",
      disabled: !installed || !managed,
      onClick: () => void checkUpdates(),
    },
    {
      icon: Zap,
      label: "Quick repair",
      hint:
        installed && managed && !steamCopy && !supportsQuickRepair(activeId)
          ? "Only a full repair works here"
          : installed && managed && !steamCopy && jobActive
            ? JOB_ACTIVE_BLOCKER
            : undefined,
      disabled:
        !installed || !managed || steamCopy || !supportsQuickRepair(activeId) || jobActive,
      onClick: quickRepair,
    },
    {
      icon: Wrench,
      label: "Full repair",
      hint: installed && managed && !steamCopy && jobActive ? JOB_ACTIVE_BLOCKER : undefined,
      disabled: !installed || !managed || steamCopy || jobActive,
      onClick: confirmFullRepair,
    },
    {
      icon: FolderOpen,
      label: "Game folder",
      dividerBefore: true,
      disabled: !installed,
      onClick: () => void openGameFolder(activeId),
    },
    {
      icon: Images,
      label: "Screenshots",
      onClick: () => void openScreenshotFolder(activeId),
    },
    {
      icon: FolderSearch,
      label: "Locate existing install",
      dividerBefore: true,
      onClick: () => void locate(),
    },
    {
      icon: FolderInput,
      label: "Move game",
      hint: installed && !steamCopy ? (uninstallBlocker ?? undefined) : undefined,
      disabled: !installed || steamCopy || running || jobActive,
      onClick: confirmMove,
    },
    {
      icon: Trash2,
      label: partial ? "Discard download" : "Uninstall game",
      hint: installed || partial ? (uninstallBlocker ?? undefined) : undefined,
      danger: true,
      disabled: (!installed && !partial) || !!uninstallBlocker,
      onClick: () => void confirmUninstall(),
    },
  ];

  return (
    <ExpandUp className={MENU_WIDTH} icon={Settings} label="Quick settings">
      {(close) => (
        <div className="max-h-[calc(100vh-124px)] overflow-y-auto overscroll-contain py-[6px]">
          {options.map(({ icon: Icon, label, hint, danger, disabled, dividerBefore, onClick }) => (
            <Fragment key={label}>
              {dividerBefore && <div className="mx-[12px] my-[5px] h-px bg-white/[0.07]" />}
              <button
                onClick={
                  disabled
                    ? undefined
                    : () => {
                        close();
                        onClick();
                      }
                }
                disabled={disabled}
                className={`group flex w-full items-center gap-[10px] px-[12px] py-[7px] text-left text-[13px] transition-colors ${
                  disabled
                    ? "cursor-not-allowed text-white/30"
                    : `${danger ? "text-red-300" : "text-white/85"} hover:bg-white/[0.06]`
                }`}
              >
                <span
                  className={`grid h-[28px] w-[28px] shrink-0 place-items-center rounded-[8px] transition-colors ${
                    disabled
                      ? "bg-white/[0.04] text-white/25"
                      : danger
                        ? "bg-red-500/[0.12] text-red-300/90"
                        : "bg-white/[0.06] text-white/65 group-hover:bg-white/[0.09] group-hover:text-white/90"
                  }`}
                >
                  <Icon size={14} />
                </span>
                <span className="flex min-w-0 flex-col">
                  <span className="truncate">{label}</span>
                  {hint && (
                    <span className="whitespace-normal text-[11px] leading-snug text-white/35">
                      {hint}
                    </span>
                  )}
                </span>
              </button>
            </Fragment>
          ))}
          {(game.graphicsApiChoice || game.resourceQualityChoice) && (
            <div className="mx-[12px] my-[5px] h-px bg-white/[0.07]" />
          )}
          {game.graphicsApiChoice && (
            <DirectXRow
              gameId={activeId}
              installed={installed}
              forceDx11={forceDx11}
              customLauncher={customLauncher}
            />
          )}
          {game.resourceQualityChoice && (
            <ResourceQualityRow
              installed={installed}
              jobActive={jobActive}
              running={running}
              customLauncher={customLauncher}
              resourceQuality={resourceQuality}
            />
          )}
        </div>
      )}
    </ExpandUp>
  );
}
