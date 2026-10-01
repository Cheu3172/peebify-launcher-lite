// ------------ Capture Viewer ------------
// Full screen viewer for screenshots and clips inside the overlay, with next and previous, open in
// folder, and delete.
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ChevronLeft, ChevronRight, ExternalLink, FolderOpen, Trash2, X } from "lucide-react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { onEvent, rpc } from "./ipc";
import { useTauriEvent } from "../lib/useTauriEvent";
import { fmtBytes, fmtRelTime } from "../lib/format";
import { useNow } from "../lib/useRelativeTime";

export interface ViewerCapture {
  path: string;
  name: string;
  sizeBytes: number;
  takenAt: string;
  kind: "screenshot" | "clip";
  gameId?: string | null;
}

const onOverlayClosed = (cb: () => void) => onEvent("overlay-closed", () => cb());

export function CaptureViewer({
  captures,
  path,
  onPath,
  onClose,
  onDelete,
}: {
  captures: ViewerCapture[];
  path: string;
  onPath: (path: string) => void;
  onClose: () => void;
  onDelete: (path: string) => void;
}) {
  const now = useNow();
  const [dimensions, setDimensions] = useState<string>("");
  const [armed, setArmed] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const video = useRef<HTMLVideoElement>(null);
  const root = useRef<HTMLDivElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);

  const index = useMemo(() => {
    const found = captures.findIndex((c) => c.path === path);
    return found < 0 ? 0 : found;
  }, [captures, path]);
  const current = captures[index];
  const open = current !== undefined;

  const step = useCallback(
    (delta: number) => {
      if (captures.length === 0) return;
      const next = (index + delta + captures.length) % captures.length;
      setNotice(null);
      onPath(captures[next].path);
    },
    [captures, index, onPath],
  );

  useEffect(() => {
    setDimensions("");
    setArmed(false);
  }, [path]);

  useEffect(() => {
    if (!notice) return;
    const timer = window.setTimeout(() => setNotice(null), 5000);
    return () => window.clearTimeout(timer);
  }, [notice]);

  const act = (channel: string, fallback: string) => {
    setNotice(null);
    void rpc<{ success?: boolean; error?: string }>(channel, current?.path)
      .then((result) => {
        if (result?.success === false) setNotice(result.error ?? fallback);
      })
      .catch((e) => setNotice(e instanceof Error ? e.message : fallback));
  };

  useEffect(() => {
    if (!armed) return;
    const timer = window.setTimeout(() => setArmed(false), 3000);
    return () => window.clearTimeout(timer);
  }, [armed]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      const target = e.target as HTMLElement | null;
      if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
        e.stopPropagation();
        if (target?.tagName === "VIDEO") return;
        e.preventDefault();
        step(e.key === "ArrowLeft" ? -1 : 1);
        return;
      }
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose, step]);

  useEffect(() => {
    if (!open || !root.current) return;
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const inerted: HTMLElement[] = [];
    for (const child of Array.from(document.body.children)) {
      if (!(child instanceof HTMLElement) || child === root.current || child.inert) continue;
      child.inert = true;
      inerted.push(child);
    }
    closeButton.current?.focus();
    return () => {
      inerted.forEach((child) => {
        child.inert = false;
      });
      if (previous?.isConnected) previous.focus();
    };
  }, [open]);

  useTauriEvent(onOverlayClosed, onClose);

  useEffect(() => {
    const element = video.current;
    return () => {
      if (!element) return;
      element.pause();
      element.removeAttribute("src");
      element.load();
    };
  }, [path]);

  if (!current) return null;

  const source = convertFileSrc(current.path);

  return createPortal(
    <div
      ref={root}
      role="dialog"
      aria-modal="true"
      aria-label={current.name}
      className="fixed inset-0 z-[120] flex flex-col"
      style={{ background: "#07070b" }}
    >
      <header className="flex shrink-0 items-center gap-3 border-b border-white/[0.08] px-5 py-3">
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13.5px] font-medium">{current.name}</div>
          <div className="ov-mono mt-[2px] truncate text-[11px] text-white/45">
            {fmtRelTime(Date.parse(current.takenAt), now)} · {current.sizeBytes > 0 ? fmtBytes(current.sizeBytes) : ""}
            {dimensions ? ` · ${dimensions}` : ""} · {index + 1} of {captures.length}
          </div>
          {notice && (
            <div role="alert" className="mt-[2px] truncate text-[11px] text-[#fca5a5]">
              {notice}
            </div>
          )}
        </div>
        <ViewerButton onClick={() => act("overlay-open-capture", "That file could not be opened.")}>
          <ExternalLink size={14} />
          Open
        </ViewerButton>
        <ViewerButton
          onClick={() => act("overlay-reveal-capture", "That file could not be shown.")}
        >
          <FolderOpen size={14} />
          Show in folder
        </ViewerButton>
        <ViewerButton
          tone="danger"
          onClick={() => {
            if (!armed) {
              setArmed(true);
              return;
            }
            setArmed(false);
            onDelete(current.path);
          }}
        >
          <Trash2 size={14} />
          {armed ? "Delete?" : "Delete"}
        </ViewerButton>
        <button
          type="button"
          ref={closeButton}
          aria-label="Close"
          onClick={onClose}
          className="ml-1 grid h-[30px] w-[30px] shrink-0 place-items-center rounded-[8px] text-white/60 hover:bg-white/[0.08] hover:text-white"
        >
          <X size={18} />
        </button>
      </header>

      <div className="relative flex min-h-0 flex-1 items-center justify-center p-6">
        {captures.length > 1 && (
          <Arrow side="left" onClick={() => step(-1)}>
            <ChevronLeft size={22} />
          </Arrow>
        )}

        {current.kind === "clip" ? (
          <video
            ref={video}
            key={current.path}
            src={source}
            controls
            autoPlay
            onLoadedMetadata={(e) => {
              const el = e.currentTarget;
              setDimensions(`${el.videoWidth} x ${el.videoHeight}`);
            }}
            className="max-h-full max-w-full rounded-[10px]"
          />
        ) : (
          <img
            key={current.path}
            src={source}
            alt={current.name}
            onLoad={(e) =>
              setDimensions(`${e.currentTarget.naturalWidth} x ${e.currentTarget.naturalHeight}`)
            }
            className="max-h-full max-w-full rounded-[10px] object-contain"
          />
        )}

        {captures.length > 1 && (
          <Arrow side="right" onClick={() => step(1)}>
            <ChevronRight size={22} />
          </Arrow>
        )}
      </div>
    </div>,
    document.body,
  );
}

function Arrow({
  side,
  onClick,
  children,
}: {
  side: "left" | "right";
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={side === "left" ? "Previous" : "Next"}
      onClick={onClick}
      className={`absolute top-1/2 grid h-[42px] w-[42px] -translate-y-1/2 place-items-center rounded-full bg-white/[0.08] text-white/70 hover:bg-white/[0.16] hover:text-white ${
        side === "left" ? "left-5" : "right-5"
      }`}
    >
      {children}
    </button>
  );
}

function ViewerButton({
  onClick,
  tone = "normal",
  children,
}: {
  onClick: () => void;
  tone?: "normal" | "danger";
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={`flex shrink-0 items-center gap-[6px] rounded-[8px] border px-3 py-[7px] text-[12px] font-medium transition-colors duration-150 ${
        tone === "danger"
          ? "border-[#ef4444]/40 bg-[#ef4444]/[0.08] text-[#fca5a5] hover:bg-[#ef4444]/[0.16]"
          : "border-white/15 bg-white/[0.06] text-white hover:bg-white/[0.12]"
      }`}
    >
      {children}
    </button>
  );
}
