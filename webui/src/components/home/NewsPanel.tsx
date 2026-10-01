// ------------ News Panel ------------
// The news card at the bottom left of Home. Shows the game's banner slideshow plus its latest notices and news,
// fetched from the game's own feed and refreshed every ten minutes. Clicking an item opens it in the browser.
import { useEffect, useMemo, useRef, useState } from "react";
import { AnimatePresence, m } from "framer-motion";
import { Newspaper } from "lucide-react";
import { fadeVariants } from "../../lib/motion";
import { useUiStore } from "../../store/uiStore";
import { useRendererSuspended } from "../../lib/useRendererSuspended";
import { getNewsData, type NewsData, type NewsEntry, type NewsSlide } from "../../lib/ipc";
import { openExternal } from "../../lib/tauri";
import { Hint } from "../ui/Tooltip";
import { CrossfadeMedia } from "../common/CrossfadeMedia";

const bannerSrc = (s: NewsSlide): string => s.url || "";

const entryKey = (i: NewsEntry): string => `${i.jumpUrl ?? ""}|${i.content ?? ""}`;

const DATE_AHEAD_MS = 2 * 86_400_000;
const NEWS_REFRESH_MS = 10 * 60_000;

type NewsDate = { year: number; month: number; day: number };

const parseNewsDate = (time: string | undefined, today: Date): NewsDate | null => {
  const match = /^(?:(\d{4})[-/.])?(\d{1,2})[-/.](\d{1,2})$/.exec((time ?? "").trim());
  if (!match) return null;
  const month = Number(match[2]);
  const day = Number(match[3]);
  if (month < 1 || month > 12 || day < 1 || day > 31) return null;
  let year = match[1] ? Number(match[1]) : today.getFullYear();
  if (!match[1]) {
    const t0 = new Date(today.getFullYear(), today.getMonth(), today.getDate()).getTime();
    const at = (y: number) => new Date(y, month - 1, day).getTime();
    if (at(year) > t0 + DATE_AHEAD_MS) year -= 1;
    else if (at(year + 1) <= t0 + DATE_AHEAD_MS) year += 1;
  }
  return { year, month, day };
};

const dateRank = (time: string | undefined, today: Date): number => {
  const d = parseNewsDate(time, today);
  return d ? d.year * 10000 + d.month * 100 + d.day : -1;
};

const fmtNewsDate = (time: string | undefined, today: Date): string => {
  const d = parseNewsDate(time, today);
  if (!d) return time ?? "";
  return new Date(d.year, d.month - 1, d.day).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    ...(d.year !== today.getFullYear() ? { year: "numeric" } : {}),
  });
};

const newsCache = new Map<string, NewsData>();

export function NewsPanel() {
  const activeGameId = useUiStore((s) => s.activeGameId);

  const [data, setData] = useState<NewsData>(() => newsCache.get(activeGameId) ?? {});
  const [loading, setLoading] = useState(!newsCache.has(activeGameId));
  const [slide, setSlide] = useState(0);
  const [hovered, setHovered] = useState(false);
  const [focused, setFocused] = useState(false);
  const [failed, setFailed] = useState(false);
  const [attempt, setAttempt] = useState(0);
  const suspended = useRendererSuspended();
  const lastFetch = useRef(0);
  const shownGame = useRef<string | null>(null);

  useEffect(() => {
    let alive = true;
    const cached = newsCache.get(activeGameId);
    if (shownGame.current !== activeGameId) {
      shownGame.current = activeGameId;
      setData(cached ?? {});
      setSlide(0);
    }
    setLoading(!cached);
    setFailed(false);
    lastFetch.current = Date.now();
    const fail = () => {
      if (!alive) return;
      setFailed(!cached);
      setLoading(false);
    };
    void getNewsData(activeGameId)
      .then((d) => {
        if (!d) return fail();
        newsCache.set(activeGameId, d);
        if (alive) {
          setData(d);
          setSlide((s) => (s < (d.slideshow?.length ?? 0) ? s : 0));
          setLoading(false);
        }
      })
      .catch(fail);
    return () => {
      alive = false;
    };
  }, [activeGameId, attempt]);

  useEffect(() => {
    if (suspended) return;
    const refreshIfStale = () => {
      if (Date.now() - lastFetch.current >= NEWS_REFRESH_MS) setAttempt((n) => n + 1);
    };
    refreshIfStale();
    const t = setInterval(refreshIfStale, NEWS_REFRESH_MS);
    return () => clearInterval(t);
  }, [suspended]);

  const slides = data.slideshow ?? [];
  const items = useMemo(() => {
    const notices = data.guidance?.notice?.contents ?? [];
    const seen = new Set(notices.map(entryKey));
    const news = (data.guidance?.news?.contents ?? []).filter((i) => !seen.has(entryKey(i)));
    const today = new Date();
    return [
      ...notices.map((item) => ({ item, notice: true })),
      ...news.map((item) => ({ item, notice: false })),
    ]
      .map((x, order) => ({
        ...x,
        order,
        rank: dateRank(x.item.time, today),
        date: fmtNewsDate(x.item.time, today),
      }))
      .sort((a, b) => b.rank - a.rank || a.order - b.order);
  }, [data]);

  useEffect(() => {
    if (hovered || focused || suspended || slides.length <= 1) return;
    const t = setInterval(() => setSlide((s) => (s + 1) % slides.length), 10000);
    return () => clearInterval(t);
  }, [hovered, focused, suspended, slides.length, activeGameId]);

  const banner = slides[slide] ?? slides[0];

  return (
    <div className="glass flex w-[414px] flex-col overflow-hidden rounded-[11px] shadow-2xl">
      {banner && (
        <div
          className="relative h-[162px] overflow-hidden bg-black/30"
          onMouseEnter={() => setHovered(true)}
          onMouseLeave={() => setHovered(false)}
          onFocus={() => setFocused(true)}
          onBlur={(e) => {
            if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setFocused(false);
          }}
        >
          <div
            role={banner.jumpUrl ? undefined : "img"}
            aria-hidden={banner.jumpUrl ? true : undefined}
            aria-label={banner.jumpUrl ? undefined : `Banner ${slides.indexOf(banner) + 1} of ${slides.length}`}
            className="absolute inset-0 h-full w-full"
          >
            <CrossfadeMedia src={bannerSrc(banner)} video={false} />
          </div>
          {banner.jumpUrl && (
            <button
              aria-label={`Open banner ${slides.indexOf(banner) + 1} of ${slides.length}`}
              onClick={() => banner.jumpUrl && void openExternal(banner.jumpUrl)}
              className="absolute inset-0 h-full w-full"
            />
          )}

          {slides.length > 1 && (
            <div className="absolute bottom-[4px] right-[6px] z-[2] flex">
              {slides.map((_, i) => (
                <button
                  key={i}
                  aria-label={`Show banner ${i + 1}`}
                  aria-current={i === slide || undefined}
                  onClick={() => setSlide(i)}
                  className="flex h-[12px] items-center px-[4px]"
                >
                  <span
                    className="h-[4px] rounded-[2px] transition-all"
                    style={{
                      width: i === slide ? 14 : 4,
                      background: i === slide ? "#fff" : "rgba(255,255,255,.5)",
                    }}
                  />
                </button>
              ))}
            </div>
          )}
        </div>
      )}

      <div className="flex items-center gap-[8px] border-b border-white/[0.07] px-[15px] py-[10px]">
        <Newspaper size={14} className="shrink-0 text-white/45" />
        <h3 className="text-[12.5px] font-semibold text-white/85">News &amp; Notices</h3>
      </div>

      <div className="px-[9px] pb-[10px] pt-[4px]">
        <div className="max-h-[132px] min-h-[106px] overflow-y-auto">
          <AnimatePresence mode="wait" initial={false}>
            <m.div key={`${activeGameId}:${loading}:${failed}`} variants={fadeVariants} initial="initial" animate="animate" exit="exit">
              {loading ? (
                <div role="status" className="animate-pulse px-[7px] py-[10px]">
                  <span className="sr-only">Loading news…</span>
                  {[0, 1, 2].map((i) => (
                    <div key={i} aria-hidden className="mb-[10px] h-[13px] rounded-[4px] bg-white/[0.07] last:mb-0" style={{ width: `${88 - i * 14}%` }} />
                  ))}
                </div>
              ) : items.length === 0 && failed ? (
                <div className="flex flex-col items-center gap-[8px] px-[7px] py-[12px]">
                  <p className="text-center text-[12px] text-white/40">Could not load news.</p>
                  <button
                    onClick={() => setAttempt((n) => n + 1)}
                    className="rounded-[6px] border border-white/[0.12] px-[10px] py-[3px] text-[11.5px] font-medium text-white/75 transition-colors hover:bg-white/[0.08]"
                  >
                    Retry
                  </button>
                </div>
              ) : items.length === 0 ? (
                <p className="px-[7px] py-[16px] text-center text-[12px] text-white/40">No news yet.</p>
              ) : (
                items.map(({ item, notice, date }, idx) => {
                  const key = `${entryKey(item)}|${idx}`;
                  const body = (
                    <>
                      <span className="flex min-w-0 items-center gap-[6px]">
                        {notice && (
                          <span className="shrink-0 rounded-[4px] bg-white/[0.08] px-[5px] py-[1px] text-[9.5px] font-semibold uppercase tracking-[0.04em] text-white/60">
                            Notice
                          </span>
                        )}
                        <span className="truncate text-[12.5px] font-medium leading-[1.3]">{item.content}</span>
                      </span>
                      <span className="shrink-0 text-[10.5px] text-white/[0.42]">{date}</span>
                    </>
                  );
                  const rowClass =
                    "flex h-[33px] w-full items-center justify-between gap-3 rounded-[7px] border-b border-white/[0.06] px-[7px] text-left last:border-b-0";
                  const url = item.jumpUrl;
                  return (
                    <Hint key={key} tip={item.content} className="block w-full">
                      {url ? (
                        <button
                          onClick={() => void openExternal(url)}
                          className={`${rowClass} transition-colors hover:bg-white/[0.05]`}
                        >
                          {body}
                        </button>
                      ) : (
                        <div className={rowClass}>{body}</div>
                      )}
                    </Hint>
                  );
                })
              )}
            </m.div>
          </AnimatePresence>
        </div>
      </div>
    </div>
  );
}
