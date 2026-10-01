// ------------ Window Focus ------------
// A hook that says whether the launcher window currently has focus.
import { useEffect, useState } from "react";
import { isTauri } from "./tauri";

export function useWindowFocused(): boolean {
  const [focused, setFocused] = useState(() => typeof document === "undefined" || document.hasFocus());

  useEffect(() => {
    if (!isTauri()) return;
    let un: (() => void) | undefined;
    let cancelled = false;
    void import("@tauri-apps/api/window")
      .then(({ getCurrentWindow }) =>
        getCurrentWindow().onFocusChanged(({ payload }) => setFocused(payload)),
      )
      .then((f) => {
        if (cancelled) f();
        else un = f;
      });
    return () => {
      cancelled = true;
      un?.();
    };
  }, []);

  return focused;
}
