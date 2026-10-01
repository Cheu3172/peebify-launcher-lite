// ------------ Gallery Helpers ------------
// Small helpers for the gallery: picks the image or video to show on a tile, and groups captures under a heading
// for each day.
import type { MediaItem } from "../../lib/ipc";
import { localFileSrc } from "../../lib/customMedia";
import { fmtDayHeading } from "../../lib/playtimeStats";

export function tileMedia(item: MediaItem): { src: string | null; video: boolean } {
  if (item.thumbPath) return { src: localFileSrc(item.thumbPath), video: false };
  const src = localFileSrc(item.path);
  if (src && item.kind === "clip") return { src: `${src}#t=0.5`, video: true };
  return { src, video: false };
}

export interface DayGroup {
  key: string;
  label: string;
  items: MediaItem[];
}

export function groupByDay(items: MediaItem[]): DayGroup[] {
  const groups: DayGroup[] = [];
  let last: DayGroup | null = null;
  for (const item of items) {
    const d = new Date(item.takenAt);
    const key = `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
    if (last && last.key === key) {
      last.items.push(item);
      continue;
    }
    last = { key, label: fmtDayHeading(d), items: [item] };
    groups.push(last);
  }
  return groups;
}
