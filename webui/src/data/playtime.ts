// ------------ Playtime Colors ------------
// The default chart color for each game, plus helpers to read a custom color someone picked in settings.
// Also holds the small shapes the playtime charts pass around.
import type { GameId } from "../types/game";

const GAME_COLORS: Record<GameId, string> = {
  wuwa: "#2dd4bf",
  zzz: "#fcd34d",
  hsr: "#9a8cff",
  nte: "#ff5d9e",
  endfield: "#ffa726",
  genshin: "#5bc0eb",
  hi3: "#c084fc",
  pgr: "#ff6b57",
  gf2: "#a3e635",
  gf1: "#e11d48",
  re1999: "#c9a227",
  bd2: "#94a3b8",
};

const HEX = /^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i;

export function isGameColor(value: string | undefined | null): value is string {
  return !!value && HEX.test(value.trim());
}

export function gameColor(
  id: GameId | string,
  overrides?: Record<string, string>,
): string {
  const custom = overrides?.[id];
  if (isGameColor(custom)) return custom.trim().toLowerCase();
  return GAME_COLORS[id as GameId] ?? "#888888";
}

export interface ShareSlice {
  id: GameId;
  name: string;
  minutes: number;
}

export interface GameTotal extends ShareSlice {
  sessions: number;
  longestSessionMinutes: number;
}
