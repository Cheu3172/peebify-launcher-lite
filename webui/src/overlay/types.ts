import type { ModsStatus } from "./ipc";
import type { GameId } from "../types/game";

export interface PanelProps {
  gameId: GameId;
  modsUsable: boolean;
  mods: ModsStatus | null;
}
