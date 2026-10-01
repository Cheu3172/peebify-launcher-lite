// ------------ Formatting ------------
// Turns raw numbers into text people can read: file sizes, speeds, time left, durations, and
// relative times like "5m ago".
export function fmtBytes(n: number): string {
  if (n >= 999.5e9) return `${(n / 1e12).toFixed(1).replace(/\.0$/, "")} TB`;
  if (n >= 999.5e6) return `${(n / 1e9).toFixed(1).replace(/\.0$/, "")} GB`;
  if (n >= 999.5e3) return `${Math.round(n / 1e6)} MB`;
  if (n >= 999.5) return `${Math.round(n / 1e3)} KB`;
  return `${n} B`;
}

export function fmtSpeed(n: number): string {
  if (n <= 0) return "";
  if (n >= 9.95e6) return `${Math.round(n / 1e6)} MB/s`;
  if (n >= 999.5e3) return `${(n / 1e6).toFixed(1)} MB/s`;
  return `${Math.max(1, Math.round(n / 1e3))} KB/s`;
}

export function fmtEta(secs: number): string {
  if (!Number.isFinite(secs) || secs <= 0) return "";
  const whole = Math.floor(secs);
  const h = Math.floor(whole / 3600);
  const m = Math.floor((whole % 3600) / 60);
  const s = whole % 60;
  if (h > 0) return `${h}h ${m}m`;
  return m > 0 ? `${m}m ${s}s` : `${s}s`;
}

export function fmtHM(min: number): string {
  const m = Math.round(min);
  if (!(m > 0)) return "0m";
  if (m < 60) return `${m}m`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

export function fmtRelTime(ts: number, nowMs: number): string {
  if (!Number.isFinite(ts)) return "";
  const m = Math.floor((nowMs - ts) / 60000);
  if (m < 1) return "just now";
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.floor(h / 24);
  if (d < 30) return `${d}d ago`;
  if (d < 365) return `${Math.floor(d / 30)}mo ago`;
  return `${Math.floor(d / 365)}y ago`;
}

export type TimeFormat = "system" | "12" | "24";

function timeOpts(fmt: TimeFormat): Intl.DateTimeFormatOptions {
  if (fmt === "12") return { hour: "numeric", minute: "2-digit", hour12: true };
  if (fmt === "24") return { hour: "2-digit", minute: "2-digit", hourCycle: "h23" };
  return { hour: "numeric", minute: "2-digit" };
}

export function fmtClockTime(ts: number, fmt: TimeFormat): string {
  return new Date(ts).toLocaleTimeString(undefined, timeOpts(fmt));
}

export function fmtDateTime(ts: number, fmt: TimeFormat): string {
  return new Date(ts).toLocaleString(undefined, {
    year: "numeric",
    month: "numeric",
    day: "numeric",
    ...timeOpts(fmt),
  });
}

export function fmtTimeAgo(
  ts: number | null | undefined,
  nowMs: number,
  fmt: TimeFormat,
): string {
  if (!ts || Number.isNaN(ts)) return "Never";
  const mins = Math.round((nowMs - ts) / 60000);
  if (mins < 1) return "Just now";
  if (mins < 60) return `${mins}m ago`;
  return fmtDateTime(ts, fmt);
}

export function fmtRelativeDays(epochSeconds: number | null, nowMs: number): string {
  if (!epochSeconds) return "";
  const today = new Date(nowMs);
  today.setHours(0, 0, 0, 0);
  const then = new Date(epochSeconds * 1000);
  then.setHours(0, 0, 0, 0);
  const days = Math.round((today.getTime() - then.getTime()) / 86_400_000);
  if (days <= 0) return "today";
  if (days === 1) return "yesterday";
  if (days < 30) return `${days}d ago`;
  if (days < 365) return `${Math.floor(days / 30)}mo ago`;
  return `${Math.floor(days / 365)}y ago`;
}

export function fmtDuration(ms: number | null | undefined): string {
  if (!ms || ms <= 0) return "";
  const total = Math.round(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
  return `${h > 0 ? `${h}:` : ""}${mm}:${String(s).padStart(2, "0")}`;
}

export function fmtCompact(n: number): string {
  if (n >= 999_500) return `${(n / 1e6).toFixed(n >= 9_950_000 ? 0 : 1).replace(/\.0$/, "")}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(n >= 9950 ? 0 : 1).replace(/\.0$/, "")}k`;
  return String(n);
}
