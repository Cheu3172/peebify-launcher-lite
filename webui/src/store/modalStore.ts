// ------------ Modal Store ------------
// Which dialog is open: a confirm box or the install dialog for a game.
import { create } from "zustand";
import type { GameId } from "../types/game";

interface ConfirmOpts {
  title: string;
  message: string;
  confirmLabel?: string;
  cancelLabel?: string | null;
  danger?: boolean;
  onConfirm: () => void;
}

type ModalState = ({ kind: "confirm" } & ConfirmOpts) | { kind: "install"; gameId: GameId } | null;

interface ModalStore {
  modal: ModalState;
  openConfirm: (o: ConfirmOpts) => void;
  openInstall: (gameId: GameId) => void;
  close: () => void;
}

export const useModalStore = create<ModalStore>((set) => ({
  modal: null,
  openConfirm: (o) => set({ modal: { kind: "confirm", ...o } }),
  openInstall: (gameId) => set({ modal: { kind: "install", gameId } }),
  close: () => set({ modal: null }),
}));
