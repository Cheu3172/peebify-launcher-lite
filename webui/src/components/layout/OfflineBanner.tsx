// ------------ Offline Banner ------------
// The amber "You're offline" pill at the top of the window. Appears when the launcher loses internet, can be
// dismissed, and has a button to check the connection again.
import { useEffect, useState } from "react";
import { AnimatePresence, m } from "framer-motion";
import { Loader2, WifiOff, X } from "lucide-react";
import { useUiStore } from "../../store/uiStore";
import { networkRecheck } from "../../lib/ipc";

export function OfflineBanner() {
  const isOnline = useUiStore((s) => s.isOnline);
  const [dismissed, setDismissed] = useState(false);
  const [checking, setChecking] = useState(false);

  useEffect(() => {
    if (isOnline) setDismissed(false);
  }, [isOnline]);

  const shown = !isOnline && !dismissed;

  const recheck = async () => {
    setChecking(true);
    try {
      const online = await networkRecheck();
      if (online !== null) useUiStore.getState().setOnline(online);
    } finally {
      setChecking(false);
    }
  };

  return (
    <div className="pointer-events-none absolute left-[60px] right-[358px] top-[38px] z-(--z-banner) flex justify-center px-[12px]">
      <AnimatePresence>
        {shown && (
          <m.div
            initial={{ opacity: 0, y: -10 }}
            animate={{ opacity: 1, y: 0, transition: { duration: 0.2 } }}
            exit={{ opacity: 0, y: -10, transition: { duration: 0.15 } }}
            className="pointer-events-auto flex items-center gap-[9px] rounded-[16px] border border-amber-400/30 px-[14px] py-[7px] text-[12.5px] font-medium text-amber-100/90 shadow-lg"
            style={{
              background: "rgba(30,24,12,0.82)",
              backdropFilter: "blur(18px) saturate(160%)",
              WebkitBackdropFilter: "blur(18px) saturate(160%)",
            }}
          >
            <WifiOff size={14} className="shrink-0 text-amber-300" />
            <span className="min-w-0">You're offline. Downloads and news will resume automatically.</span>
            <button
              type="button"
              disabled={checking}
              onClick={() => void recheck()}
              className="ml-[2px] flex shrink-0 items-center gap-[5px] rounded-full border border-amber-300/30 px-[9px] py-[2px] text-[11.5px] font-semibold text-amber-100 transition hover:bg-white/10 disabled:cursor-not-allowed disabled:opacity-60"
            >
              {checking && <Loader2 size={11} className="animate-spin" />}
              {checking ? "Checking…" : "Retry"}
            </button>
            <button
              onClick={() => setDismissed(true)}
              aria-label="Dismiss offline notice"
              className="ml-[2px] grid h-[18px] w-[18px] shrink-0 place-items-center rounded-full text-amber-100/60 transition hover:bg-white/10 hover:text-amber-100"
            >
              <X size={12} />
            </button>
          </m.div>
        )}
      </AnimatePresence>
    </div>
  );
}
