// ------------ Game List ------------
// Every game the launcher knows about: its name, icon, wallpaper fallback, and which extras it supports
// (graphics API choice, voice packs, quality, FPS unlock and so on). Add a game here and the UI picks it up.
import type { Game, GameId } from "../types/game";

export const GAMES: Game[] = [
  {
    id: "wuwa",
    short: "WW",
    name: "Wuthering Waves",
    icon: "/icons/wuwa.webp",
    wallpaperStatic: "/icons/wallpaper_wuwa_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 12%, rgba(45,212,191,.45), rgba(45,212,191,0) 55%), linear-gradient(160deg,#0c2a2b 0%,#0a0e14 70%)",
    graphicsApiChoice: true,
    graphicsApiDefault: "dx12",
    resourceQualityChoice: true,
  },
  {
    id: "zzz",
    short: "ZZ",
    name: "Zenless Zone Zero",
    icon: "/icons/zzz.webp",
    wallpaperStatic: "/icons/wallpaper_zzz_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(255,212,0,.4), transparent 55%), linear-gradient(160deg,#241f08 0%,#0c0a08 70%)",
    graphicsApiChoice: true,
    voicePackChoice: true,
  },
  {
    id: "hsr",
    short: "HSR",
    name: "Honkai: Star Rail",
    icon: "/icons/honkaistarrail.webp",
    wallpaperStatic: "/icons/wallpaper_honkaistarrail_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(150,130,255,.45), transparent 55%), linear-gradient(160deg,#181433 0%,#0a0a14 70%)",
    voicePackChoice: true,
  },
  {
    id: "nte",
    short: "NtE",
    name: "Neverness to Everness",
    icon: "/icons/nte.webp",
    wallpaperStatic: "/icons/wallpaper_nte_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(255,93,158,.42), transparent 55%), linear-gradient(160deg,#2a1020 0%,#0d0810 70%)",
    contentPackChoice: true,
  },
  {
    id: "endfield",
    short: "AE",
    name: "Arknights: Endfield",
    icon: "/icons/endfield.webp",
    wallpaperStatic: "/icons/wallpaper_endfield_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(255,196,0,.38), transparent 55%), linear-gradient(160deg,#26200a 0%,#0d0c08 70%)",
    supportsQuickRepair: false,
  },
  {
    id: "genshin",
    short: "GI",
    name: "Genshin Impact",
    icon: "/icons/genshin.webp",
    wallpaperStatic: "/icons/wallpaper_genshin_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(120,180,255,.42), transparent 55%), linear-gradient(160deg,#101c33 0%,#0a0d14 70%)",
    voicePackChoice: true,
    fpsUnlock: true,
  },
  {
    id: "hi3",
    short: "HI3",
    name: "Honkai Impact 3rd",
    icon: "/icons/honkai3d.webp",
    wallpaperStatic: "/icons/wallpaper_hi3_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(192,132,252,.42), transparent 55%), linear-gradient(160deg,#1d1233 0%,#0b0912 70%)",
    voicePackChoice: true,
  },
  {
    id: "pgr",
    short: "PGR",
    name: "Punishing: Gray Raven",
    icon: "/icons/punishing.webp",
    wallpaperStatic: "/icons/wallpaper_pgr_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(255,107,87,.4), transparent 55%), linear-gradient(160deg,#2a1410 0%,#0f0a09 70%)",
    graphicsApiChoice: true,
  },
  {
    id: "gf2",
    short: "GF2",
    name: "Girls' Frontline 2: Exilium",
    icon: "/icons/gfl2.webp",
    wallpaperStatic: "/icons/wallpaper_gfl2_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(214,92,72,.36), transparent 55%), linear-gradient(160deg,#241312 0%,#0c0a0a 70%)",
  },
  {
    id: "gf1",
    short: "GF",
    name: "Girls' Frontline",
    icon: "/icons/gf1.webp",
    wallpaperStatic: "/icons/wallpaper_gf1_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(198,64,78,.34), transparent 55%), linear-gradient(160deg,#1f171a 0%,#0b0a0b 70%)",
    installHelpUrl: "https://store.steampowered.com/app/3887700/Girls_Frontline/",
    managed: false,
    betaNote:
      "Girls' Frontline is only distributed on PC through Steam. Install it there first, then point Peebify at the folder. Peebify launches it through Steam, so the overlay and playtime tracking keep working.",
  },
  {
    id: "re1999",
    short: "R1999",
    name: "Reverse: 1999",
    icon: "/icons/re1999.webp",
    wallpaperStatic: "/icons/wallpaper_re1999_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(201,162,39,.38), transparent 55%), linear-gradient(160deg,#241f14 0%,#0c0a09 70%)",
    supportsQuickRepair: false,
    betaNote:
      "Bluepoch ships Reverse: 1999 as whole archives with no per-file checksums, so Peebify can install and update the game but cannot check individual files. Repair clears leftover update folders; a damaged install has to be reinstalled.",
  },
  {
    id: "bd2",
    short: "BD2",
    name: "Brown Dust II",
    icon: "/icons/bd2.webp",
    wallpaperStatic: "/icons/wallpaper_bd2_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(133,121,201,.42), transparent 55%), linear-gradient(160deg,#1d1930 0%,#0b0a12 70%)",
    supportsQuickRepair: false,
    betaNote:
      "Neowiz ships Brown Dust II as one client package, so Peebify installs and updates it in a single download. Peebify checksums every file it unpacks, but repairing even one of them means fetching that whole package again.",
  },
  {
    id: "arknights",
    short: "AK",
    name: "Arknights",
    icon: "/icons/arknights.webp",
    wallpaperStatic: "/icons/wallpaper_arknights_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(59,130,246,.36), transparent 55%), linear-gradient(160deg,#111827 0%,#0a0b0e 70%)",
    graphicsApiChoice: true,
  },
  {
    id: "bluearchive",
    short: "BA",
    name: "Blue Archive",
    icon: "/icons/bluearchive.webp",
    wallpaperStatic: "/icons/wallpaper_bluearchive_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(56,189,248,.36), transparent 55%), linear-gradient(160deg,#10243a 0%,#080d14 70%)",
    installHelpUrl: "https://store.steampowered.com/app/3557620/Blue_Archive/",
    managed: false,
    betaNote:
      "Blue Archive is only distributed on PC through Steam. Install it there first, then point Peebify at the folder. Peebify launches it through Steam, so the overlay and playtime tracking keep working.",
  },
  {
    id: "dna",
    short: "DNA",
    name: "Duet Night Abyss",
    icon: "/icons/dna.webp",
    wallpaperStatic: "/icons/wallpaper_dna_noneanimated.webp",
    wallpaperFallback:
      "radial-gradient(120% 95% at 80% 10%, rgba(201,169,110,.34), transparent 55%), linear-gradient(160deg,#141a2e 0%,#090a12 70%)",
    graphicsApiChoice: true,
    graphicsApiDefault: "dx12",
    supportsQuickRepair: false,
    betaNote:
      "Pan Studio ships Duet Night Abyss as one archive, so Peebify installs and updates it in a single download and unpacks it with HDiffPatch. Peebify checks every file against Pan Studio's checksums, but repairing even one of them means fetching that whole archive again.",
  },
];

export const gameById = (id: GameId): Game =>
  GAMES.find((g) => g.id === id) ?? GAMES[0];

export function gameMeta(id: string | null): { name: string; icon: string | null } {
  if (!id) return { name: "Other", icon: null };
  const game = GAMES.find((g) => g.id === id);
  return game ? { name: game.name, icon: game.icon } : { name: id, icon: null };
}

export const isManaged = (id: GameId): boolean => gameById(id).managed !== false;
export const supportsQuickRepair = (id: GameId): boolean =>
  gameById(id).supportsQuickRepair !== false;
