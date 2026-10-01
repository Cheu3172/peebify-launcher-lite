// ------------ Mod Warning ------------
// The risk warning shown before mods are turned on, and the confirm dialog that uses it.
import { useModalStore } from "../store/modalStore";

export const MOD_RISK_WARNING =
  "Mods are made by other people, and Peebify loads them into the game while you play. This obviously breaks the rules of the game, and carries a risk of a ban even with the preventative measures we use.";

export function requestEnableMods(onConfirm: () => void): void {
  useModalStore.getState().openConfirm({
    title: "Turn on mod support?",
    message: MOD_RISK_WARNING,
    confirmLabel: "I understand, turn mods on",
    danger: true,
    onConfirm,
  });
}
