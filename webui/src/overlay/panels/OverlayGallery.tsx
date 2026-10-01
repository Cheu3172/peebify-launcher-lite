// ------------ Overlay Mods Gallery ------------
// The Mods gallery tab of the overlay. It reuses the launcher's GameBanana browser, or explains how to
// turn mods on if they are off for this game.
import { BrowseMods } from "../../components/mods/BrowseMods";
import type { PanelProps } from "../types";

export function OverlayGallery({ gameId, modsUsable, mods: status }: PanelProps) {
  if (!modsUsable) {
    if (!status) return null;
    return (
      <p className="max-w-[70ch] text-[13px] leading-relaxed text-white/45">
        Mods are turned off for this game. Turn them on in the launcher, then restart the game to
        browse mods here.
      </p>
    );
  }
  return <BrowseMods key={gameId} gameId={gameId} />;
}
