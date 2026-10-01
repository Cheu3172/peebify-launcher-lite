import { describe, expect, it } from "vitest";
import type { QueueJob } from "./ipc";
import { remainingDownloadBytes, verifyFoundBrokenFiles } from "./download";

const job = (over: Partial<QueueJob>): QueueJob => ({
  id: "j",
  gameId: "g",
  kind: "install",
  phase: "downloading",
  total: 1000,
  downloaded: 400,
  percent: 40,
  speed: 0,
  etaSecs: 0,
  paused: false,
  error: null,
  ...over,
});

describe("verifyFoundBrokenFiles", () => {
  it("matches the backend's broken-files verify errors", () => {
    expect(verifyFoundBrokenFiles("1 file needs repair. Run a repair to replace it.")).toBe(true);
    expect(verifyFoundBrokenFiles("3 files need repair. Run a repair to replace them.")).toBe(true);
    expect(
      verifyFoundBrokenFiles(
        "2 of 900 Brown Dust II files do not match version 1.2.3. Repair replaces them.",
      ),
    ).toBe(true);
  });

  it("leaves other verify failures to Retry", () => {
    expect(verifyFoundBrokenFiles(null)).toBe(false);
    expect(verifyFoundBrokenFiles("Game path not set.")).toBe(false);
    expect(
      verifyFoundBrokenFiles(
        "3 files differ from version 1.2.0. An update is available, and it replaces these files.",
      ),
    ).toBe(false);
  });
});

describe("remainingDownloadBytes", () => {
  it("adds up what install and update jobs still have to download", () => {
    expect(
      remainingDownloadBytes([
        job({ kind: "install" }),
        job({ kind: "update", total: 500, downloaded: 100 }),
      ]),
    ).toBe(1000);
  });

  it("counts a paused download", () => {
    expect(remainingDownloadBytes([job({ paused: true })])).toBe(600);
  });

  it("ignores verify, repair and move bytes", () => {
    expect(
      remainingDownloadBytes([
        job({ kind: "verify" }),
        job({ kind: "repair" }),
        job({ kind: "move", phase: "copying" }),
        job({ kind: "move" }),
      ]),
    ).toBe(0);
  });

  it("ignores download jobs outside the downloading phase", () => {
    expect(
      remainingDownloadBytes([
        job({ phase: "validating" }),
        job({ phase: "extracting" }),
        job({ phase: "queued" }),
        job({ total: 0, downloaded: 0 }),
      ]),
    ).toBe(0);
  });

  it("never goes negative when downloaded runs past total", () => {
    expect(remainingDownloadBytes([job({ downloaded: 1200 }), job({})])).toBe(600);
  });
});
