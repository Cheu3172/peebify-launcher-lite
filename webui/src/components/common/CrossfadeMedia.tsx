// ------------ Crossfade Media ------------
// Shows a wallpaper or news banner (image or looping video) and fades smoothly to the next one when the source
// changes, so nothing flashes or pops. Used by the wallpaper layer, the news slideshow and the wallpaper picker
// preview.
import { useEffect, useRef, useState } from "react";
import { m } from "framer-motion";
import { log } from "../../lib/log";

const FADE_SECONDS = 0.6;

interface Layer {
  id: number;
  src: string;
  video: boolean;
  ready: boolean;
}

interface CrossfadeMediaProps {
  src: string;
  video: boolean;
  autoPlay?: boolean;
  playing?: boolean;
  onReady?: () => void;
  onError?: (src: string) => void;
}

export function CrossfadeMedia({
  src,
  video,
  autoPlay = true,
  playing = true,
  onReady,
  onError,
}: CrossfadeMediaProps) {
  const [layers, setLayers] = useState<Layer[]>([]);
  const nextIdRef = useRef(0);
  const pruneTimersRef = useRef<Set<number>>(new Set());
  const videoRefs = useRef<Map<number, HTMLVideoElement>>(new Map());
  const videoRefCallbacks = useRef<
    Map<number, (el: HTMLVideoElement | null) => (() => void) | undefined>
  >(new Map());

  const videoRefFor = (id: number) => {
    let cb = videoRefCallbacks.current.get(id);
    if (!cb) {
      cb = (el: HTMLVideoElement | null) => {
        if (!el) {
          videoRefs.current.delete(id);
          videoRefCallbacks.current.delete(id);
          return undefined;
        }
        videoRefs.current.set(id, el);
        return () => {
          videoRefs.current.delete(id);
          videoRefCallbacks.current.delete(id);
          el.pause();
          el.removeAttribute("src");
          el.load();
        };
      };
      videoRefCallbacks.current.set(id, cb);
    }
    return cb;
  };

  useEffect(() => {
    for (const el of videoRefs.current.values()) {
      if (playing && autoPlay) {
        void el.play().catch((e) => log.debug("[media] video resume was interrupted:", e));
      } else {
        el.pause();
      }
    }
  }, [playing, autoPlay]);

  useEffect(() => {
    setLayers((prev) => {
      const top = prev[prev.length - 1];
      if (top && top.src === src && top.video === video) return prev;
      nextIdRef.current += 1;
      return [
        ...prev.filter((layer) => layer.ready).slice(-1),
        { id: nextIdRef.current, src, video, ready: false },
      ];
    });
  }, [src, video]);

  useEffect(() => {
    const timers = pruneTimersRef.current;
    return () => {
      for (const t of timers) window.clearTimeout(t);
      timers.clear();
    };
  }, []);

  const markReady = (id: number) => {
    onReady?.();
    setLayers((prev) =>
      prev.map((layer) => (layer.id === id ? { ...layer, ready: true } : layer))
    );
    const holdMs = (FADE_SECONDS + 0.5) * 1000;
    const timer = window.setTimeout(() => {
      pruneTimersRef.current.delete(timer);
      setLayers((prev) => {
        const idx = prev.findIndex((layer) => layer.id === id);
        return idx > 0 ? prev.slice(idx) : prev;
      });
    }, holdMs);
    pruneTimersRef.current.add(timer);
  };

  const markFailed = (layer: Layer, code?: number) => {
    if (code === undefined) {
      log.warn("[media] failed to load", layer.src);
    } else {
      log.warn("[media] failed to load", layer.src, "code", code);
    }
    if (!onError) {
      markReady(layer.id);
      return;
    }
    onReady?.();
    setLayers((prev) => prev.filter((l) => l.id !== layer.id));
    onError(layer.src);
  };

  return (
    <>
      {layers.map((layer) => (
        <m.div
          key={layer.id}
          initial={{ opacity: 0 }}
          animate={{ opacity: layer.ready ? 1 : 0 }}
          transition={{
            duration: layer.id === 1 && layers.length === 1 ? 0 : FADE_SECONDS,
            ease: "easeInOut",
          }}
          className="absolute inset-0"
        >
          {layer.video ? (
            <video
              src={layer.src}
              preload="auto"
              autoPlay={autoPlay && playing}
              muted
              loop
              playsInline
              ref={videoRefFor(layer.id)}
              onLoadedData={() => markReady(layer.id)}
              onError={(e) => markFailed(layer, e.currentTarget.error?.code)}
              className="h-full w-full object-cover"
            />
          ) : (
            <img
              src={layer.src}
              alt=""
              referrerPolicy="no-referrer"
              onLoad={() => markReady(layer.id)}
              onError={() => markFailed(layer)}
              className="h-full w-full object-cover"
            />
          )}
        </m.div>
      ))}
    </>
  );
}
