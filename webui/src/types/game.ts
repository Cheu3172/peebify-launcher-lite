// ------------ Game Types ------------
// The list of supported game ids and the shape of a game entry, plus the graphics API and quality choices.
export type GameId =
  | "wuwa"
  | "zzz"
  | "hsr"
  | "nte"
  | "endfield"
  | "genshin"
  | "hi3"
  | "pgr"
  | "gf2"
  | "gf1"
  | "re1999"
  | "bd2";

export type GraphicsApi = "dx11" | "dx12";

export type ResourceQuality = "sd" | "hd" | "uhd";

export interface Game {
  id: GameId;
  short: string;
  name: string;
  icon: string;
  wallpaperStatic: string;
  wallpaperFallback: string;
  installHelpUrl?: string;
  managed?: boolean;
  betaNote?: string;
  graphicsApiChoice?: boolean;
  graphicsApiDefault?: GraphicsApi;
  resourceQualityChoice?: boolean;
  voicePackChoice?: boolean;
  contentPackChoice?: boolean;
  supportsQuickRepair?: boolean;
  fpsUnlock?: boolean;
}
