import { describe, expect, it } from "vitest";
import { wallpaperFor } from "./customMedia";

const remote = { wallpaper: "C:/cache/dna/loop.mp4", static: null };

describe("wallpaperFor", () => {
  it("plays the server's animated wallpaper", () => {
    expect(wallpaperFor("dna", undefined, remote, true)).toEqual({ src: "C:/cache/dna/loop.mp4", video: true });
  });

  it("falls back to the bundled still when animation is off and the server has no still", () => {
    expect(wallpaperFor("dna", undefined, remote, false)).toEqual({
      src: "/icons/wallpaper_dna_noneanimated.webp",
      video: false,
    });
  });

  it("keeps a custom wallpaper ahead of the server's animation", () => {
    const custom = { type: "custom" as const, path: "C:/pics/mine.png" };
    expect(wallpaperFor("dna", custom, remote, true)).toEqual({ src: "C:/pics/mine.png", video: false });
  });
});
