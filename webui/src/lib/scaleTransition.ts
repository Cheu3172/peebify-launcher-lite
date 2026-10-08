// ------------ Scale Transition ------------
// Keeps the page from re-laying out while the Interface size animates: freezes the layout as a card, scales the card
// to the new size, then lets go once the window and the real zoom have been applied.
import { log } from "./log";
import { onEvent } from "./rpc";

// The page's half of the Interface size animation (the window half is animate_rezoom in window_manager.rs).
//
// "pin":   freeze the layout at its current size, as a fixed card in the window's top-left corner.
// "start": the window has already grown to the larger of the old and new size (it is transparent, so the spare
//          room is invisible). Scale the card to the new size on the page's own clock: no waiting on window
//          resize events, so it can't lag or clip, and the browser keeps the text sharp while it scales.
// "end":   the app has asked for the new window size and zoom. The zoom takes effect a moment later, and letting go
//          before then would draw the real layout at the old zoom (too small or too big). So the card is only
//          released in the frame the zoom actually changes: every frame checks the device pixel ratio, which
//          follows the zoom, and lets go before that frame is painted. "end" only arms a backstop timer.
//          The re-drawn layout is a touch sharper than the scaled image and can sit a fraction of a pixel apart, so
//          instead of popping in one frame it starts very slightly soft and comes into focus over a short moment.
// "fade-out" / "fade-in": a maximized window can't grow, so it dips to dark around the swap instead.

type Phase = "pin" | "start" | "end" | "fade-out" | "fade-in";
interface Payload {
  phase: Phase;
  ratio: number;
  ms: number;
}

const PINNED_CLASS = "ui-scale-pinned";
const EASING = "cubic-bezier(0.65, 0, 0.35, 1)";
const SAFETY_MS = 1500;
const END_FALLBACK_MS = 1000;
const FOCUS_BLUR_PX = 0.6;
const FOCUS_MS = 180;

let pinned = false;
let pinnedRatio = 1;
let watchFrame = 0;
let animation: Animation | null = null;
let safetyTimer = 0;
let veil: HTMLDivElement | null = null;

function watchZoom() {
  if (!pinned) return;
  if (Math.abs(window.devicePixelRatio - pinnedRatio) > 0.001) {
    unpin();
    focusIn();
    return;
  }
  watchFrame = requestAnimationFrame(watchZoom);
}

function pin() {
  window.clearTimeout(safetyTimer);
  if (pinned) return;
  pinned = true;
  pinnedRatio = window.devicePixelRatio;
  cancelAnimationFrame(watchFrame);
  watchFrame = requestAnimationFrame(watchZoom);
  const root = document.documentElement;
  root.style.setProperty("--ui-scale-pin-w", `${window.innerWidth}px`);
  root.style.setProperty("--ui-scale-pin-h", `${window.innerHeight}px`);
  root.classList.add(PINNED_CLASS);
}

function unpin() {
  window.clearTimeout(safetyTimer);
  cancelAnimationFrame(watchFrame);
  animation?.cancel();
  animation = null;
  if (!pinned) return;
  pinned = false;
  const root = document.documentElement;
  root.classList.remove(PINNED_CLASS);
  root.style.removeProperty("--ui-scale-pin-w");
  root.style.removeProperty("--ui-scale-pin-h");
}

function focusIn() {
  document.body.animate([{ filter: `blur(${FOCUS_BLUR_PX}px)` }, { filter: "blur(0px)" }], {
    duration: FOCUS_MS,
    easing: "ease-out",
  });
}

function start(ratio: number, ms: number) {
  if (!pinned) pin();
  animation?.cancel();
  animation = document.body.animate(
    [{ transform: "scale(1)" }, { transform: `scale(${ratio})` }],
    { duration: ms, easing: EASING, fill: "forwards" },
  );
  // If the final "end" never arrives, don't leave the page frozen.
  window.clearTimeout(safetyTimer);
  safetyTimer = window.setTimeout(unpin, ms + SAFETY_MS);
}

function fade(dark: boolean) {
  if (!veil) {
    veil = document.createElement("div");
    veil.setAttribute("aria-hidden", "true");
    Object.assign(veil.style, {
      position: "fixed",
      inset: "0",
      zIndex: "2147483647",
      pointerEvents: "none",
      background: "rgba(10, 10, 13, 0.9)",
      opacity: "0",
    } satisfies Partial<CSSStyleDeclaration>);
    document.body.appendChild(veil);
  }
  veil.style.transition = dark ? "opacity 100ms ease-in" : "opacity 180ms ease-out";
  veil.style.opacity = dark ? "1" : "0";
}

export function installScaleTransition(): void {
  void onEvent<Payload>("ui-scale-transition", ({ phase, ratio, ms }) => {
    if (phase === "pin") pin();
    else if (phase === "start") start(ratio, ms);
    else if (phase === "end") {
      window.clearTimeout(safetyTimer);
      safetyTimer = window.setTimeout(() => {
        log.warn("[scale] the zoom change was not seen; released the layout by timer");
        unpin();
      }, END_FALLBACK_MS);
    }
    else if (phase === "fade-out") fade(true);
    else if (phase === "fade-in") fade(false);
  });
}
