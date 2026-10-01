import { describe, expect, it } from "vitest";
import type { QueueJob } from "./ipc";
import { overallPercent, stageIndexFor, stagePercent, stagesFor } from "./stages";

const job = (kind: string, phase: string, percent = 0, total = 0): QueueJob => ({
  id: "j",
  gameId: "g",
  kind,
  phase,
  total,
  downloaded: 0,
  percent,
  speed: 0,
  etaSecs: 0,
  paused: false,
  error: null,
});

const indexOf = (kind: string, phase: string): number =>
  stageIndexFor(job(kind, phase), stagesFor(kind));

describe("stageIndexFor", () => {
  it("keeps a repair that is still fetching on the Scan stage", () => {
    expect(indexOf("repair", "downloading")).toBe(0);
    expect(indexOf("repair", "validating")).toBe(1);
    expect(indexOf("repair", "verifying")).toBe(1);
    expect(indexOf("repair", "repairing")).toBe(2);
  });

  it("maps install and move phases to their own stages", () => {
    expect(indexOf("install", "downloading")).toBe(0);
    expect(indexOf("install", "validating")).toBe(1);
    expect(indexOf("install", "extracting")).toBe(1);
    expect(indexOf("move", "downloading")).toBe(0);
    expect(indexOf("move", "moving")).toBe(0);
  });

  it("handles queued and done", () => {
    expect(indexOf("repair", "queued")).toBe(-1);
    expect(indexOf("repair", "done")).toBe(3);
  });

  it("keeps a pre-download check on the Download stage", () => {
    const stages = stagesFor("install");
    expect(stages.some((s) => s.id === "check")).toBe(false);
    expect(indexOf("install", "scanning")).toBe(0);
    expect(indexOf("install", "downloading")).toBe(0);
    expect(stageIndexFor(job("install", "downloading", 0, 1024), stages)).toBe(0);
    expect(indexOf("repair", "scanning")).toBe(1);
    expect(indexOf("verify", "scanning")).toBe(0);
  });
});

describe("overallPercent", () => {
  it("never moves a repair backwards from fetching to validating", () => {
    const fetching = overallPercent(job("repair", "downloading", 100), "repair");
    const validating = overallPercent(job("repair", "validating", 0), "repair");
    expect(fetching).toBeLessThanOrEqual(validating);
  });

  it("starts a repair's Replace stage where its file check ended", () => {
    const checked = overallPercent(job("repair", "verifying", 100), "repair");
    const replacing = overallPercent(job("repair", "repairing", 0), "repair");
    expect(replacing).toBeCloseTo(checked);
    expect(replacing).toBeLessThan(100);
  });

  it("adds no progress for a download's pre-download check", () => {
    const checking = overallPercent(job("update", "scanning", 0, 1024), "update");
    const checked = overallPercent(job("update", "scanning", 100, 1024), "update");
    const started = overallPercent(job("update", "downloading", 0, 1024), "update");
    expect(checking).toBe(0);
    expect(checked).toBe(0);
    expect(started).toBe(0);
    expect(stagePercent(job("update", "scanning", 100, 1024), stagesFor("update"))).toBe(0);
  });
});
