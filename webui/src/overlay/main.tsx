// ------------ Overlay Entry ------------
// Boots the in-game overlay window, with its own small crash screen so a bug can't leave you stuck
// behind a dead overlay.
import React, { useEffect } from "react";
import ReactDOM from "react-dom/client";
import OverlayApp from "./OverlayApp";
import { ErrorBoundary } from "../components/ErrorBoundary";
import { installGlobalErrorCapture } from "../lib/log";
import { rpc } from "../lib/rpc";
import { blockContextMenu } from "../lib/tauri";
import { OvButton } from "./ui";
import "../index.css";
import "./overlay.css";

installGlobalErrorCapture("overlay");
blockContextMenu();

const closeOverlay = () => void rpc("overlay-toggle", "close");

function OverlayCrash() {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      closeOverlay();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  return (
    <div className="ov-drawer flex h-full w-[420px] max-w-[92vw] flex-col justify-center gap-4 border-r border-white/[0.09] px-8 text-white">
      <div className="text-[15px] font-semibold">Something went wrong</div>
      <p className="text-[12.5px] leading-relaxed text-white/60">
        The overlay hit an error. It has been written to the log.
      </p>
      <div className="flex gap-2">
        <OvButton onClick={() => window.location.reload()}>Reload overlay</OvButton>
        <OvButton onClick={closeOverlay}>Close</OvButton>
      </div>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary scope="overlay" fallback={() => <OverlayCrash />}>
      <OverlayApp />
    </ErrorBoundary>
  </React.StrictMode>,
);
