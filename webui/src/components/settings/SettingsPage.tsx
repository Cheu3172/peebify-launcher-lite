// ------------ Settings Page ------------
// The Settings tab. A row of tabs (General, Appearance, Games, Mods, Overlay, About) with the matching settings
// underneath.
import { useEffect, useMemo, useRef } from "react";
import { AnimatePresence, m } from "framer-motion";
import { fadeVariants, pageVariants } from "../../lib/motion";
import {
  SlidersHorizontal,
  Gamepad2,
  Palette,
  Puzzle,
  MonitorPlay,
  Info,
} from "lucide-react";
import {
  SETTINGS_SECTIONS,
  type Setting,
  type SettingsCategory,
} from "../../data/settings";
import { useSessionUiStore } from "../../store/sessionUiStore";
import { GameMaintenance } from "./GameMaintenance";
import { LibraryPanel } from "./LibraryPanel";
import { ModsPanel } from "./ModsPanel";
import { OverlayPanel } from "./OverlayPanel";
import { SettingsGroup } from "../ui/SettingsGroup";
import {
  SETTINGS_PANEL_ID,
  SettingsTabStrip,
  settingsTabId,
  type TabItem,
} from "./SettingsTabStrip";
import { SettingRow } from "./SettingRow";
import { CustomWallpaperCard, CustomIconCard, GameColorsCard } from "./AppearanceEditors";

const CATS: { id: SettingsCategory; label: string; icon: typeof SlidersHorizontal }[] = [
  { id: "general", label: "General", icon: SlidersHorizontal },
  { id: "appearance", label: "Appearance", icon: Palette },
  { id: "games", label: "Games", icon: Gamepad2 },
  { id: "mods", label: "Mods", icon: Puzzle },
  { id: "overlay", label: "Overlay", icon: MonitorPlay },
  { id: "about", label: "About", icon: Info },
];

const TAB_ITEMS: TabItem[] = CATS.map(({ id, label, icon: Icon }) => ({
  id,
  label,
  icon: <Icon size={16} />,
}));

export function SettingsPage() {
  const cat = useSessionUiStore((s) => s.settingsCat);
  const setCat = useSessionUiStore((s) => s.setSettingsCat);
  const settingsGameId = useSessionUiStore((s) => s.settingsGameId);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    scrollRef.current?.scrollTo({ top: 0 });
  }, [cat]);

  const section = SETTINGS_SECTIONS.find((s) => s.category === cat)!;

  const groups = useMemo(() => {
    const out: { name?: string; items: Setting[] }[] = [];
    for (const s of section.settings) {
      const last = out[out.length - 1];
      if (last && last.name === s.group) last.items.push(s);
      else out.push({ name: s.group, items: [s] });
    }
    return out;
  }, [section]);

  const renderGroups = () =>
    groups.map((g, gi) => (
      <SettingsGroup
        key={g.name ?? gi}
        label={g.name}
        danger={g.items.every((s) => s.kind === "action" && s.danger)}
      >
        {g.items.map((s) => (
          <SettingRow key={s.id} setting={s} />
        ))}
      </SettingsGroup>
    ));

  const body = () => {
    switch (cat) {
      case "overlay":
        return <OverlayPanel />;
      case "mods":
        return (
          <>
            <SettingsGroup label="Mod support">
              <ModsPanel />
            </SettingsGroup>
            {renderGroups()}
          </>
        );
      case "appearance":
        return (
          <>
            {renderGroups()}
            <div className="flex flex-col gap-3">
              <CustomWallpaperCard />
              <CustomIconCard />
              <GameColorsCard />
            </div>
          </>
        );
      default:
        return renderGroups();
    }
  };

  return (
    <m.div variants={pageVariants} className="absolute inset-0 flex flex-col">
      <div className="shrink-0 px-10 pt-6">
        <h1 className="mb-4 text-[24px] font-semibold leading-tight">Settings</h1>
        <SettingsTabStrip
          items={TAB_ITEMS}
          active={cat}
          onSelect={(id) => setCat(id as SettingsCategory)}
        />
      </div>

      <div
        ref={scrollRef}
        role="tabpanel"
        id={SETTINGS_PANEL_ID}
        aria-labelledby={settingsTabId(cat)}
        className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-10 pb-10 pt-6"
      >
        <AnimatePresence mode="wait" initial={false}>
          <m.div key={cat} variants={fadeVariants} initial="initial" animate="animate" exit="exit">
            <div className="mx-auto max-w-[1260px]">
              <div className="mb-6 border-b border-white/10 pb-4">
                <h2 className="text-[18px] font-semibold">{section.title}</h2>
                <p className="mt-1 text-[14px] text-white/55">{section.subtitle}</p>
              </div>

              {cat === "games" ? (
                <div className="flex items-start gap-7 pb-4">
                  <div className="w-[292px] shrink-0 max-[1100px]:w-[240px]">
                    <LibraryPanel />
                  </div>
                  <div className="min-w-0 flex-1">
                    <GameMaintenance gameId={settingsGameId} />
                  </div>
                </div>
              ) : (
                <div className="flex flex-col gap-7 pb-4">{body()}</div>
              )}
            </div>
          </m.div>
        </AnimatePresence>
      </div>
    </m.div>
  );
}
