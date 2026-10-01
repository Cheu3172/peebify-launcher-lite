import { beforeEach, describe, expect, it, vi } from "vitest";

const calls: Array<{ channel: string; args: unknown[] }> = [];
let reply: (channel: string, args: unknown[]) => unknown = () => undefined;

vi.mock("./rpc", () => ({
  rpcRead: vi.fn(async (channel: string, ...args: unknown[]) => {
    calls.push({ channel, args });
    return reply(channel, args);
  }),
  rpc: vi.fn(async () => undefined),
  rpcAction: vi.fn(async () => undefined),
  onEvent: vi.fn(async () => () => {}),
  unwrap: vi.fn((r: unknown) => r),
}));

const { getGameConfig, getLibrary, getRunningGameIds } = await import("./ipc");

describe("config and running-game reads", () => {
  beforeEach(() => {
    calls.length = 0;
    reply = () => undefined;
  });

  it("lists running games with one call when the backend sends runningIds", async () => {
    reply = () => ({ success: true, running: true, gameId: "wuwa", runningIds: ["wuwa", "zzz"] });
    await expect(getRunningGameIds(["genshin", "wuwa", "zzz"])).resolves.toEqual(["wuwa", "zzz"]);
    expect(calls).toEqual([{ channel: "get-running-game", args: [] }]);
  });

  it("reports no answer when the reply has no runningIds", async () => {
    reply = () => ({ success: true, running: false });
    await expect(getRunningGameIds(["genshin", "zzz"])).resolves.toBeNull();
    expect(calls).toEqual([{ channel: "get-running-game", args: [] }]);
  });

  it("reads a game's settings in one targeted get-config call", async () => {
    reply = (_c, args) => {
      const keys = args[0] as string[];
      const values: Record<string, unknown> = Object.fromEntries(keys.map((k) => [k, null]));
      values["games.wuwa.autoUpdate"] = false;
      values["games.wuwa.autoUpdateSchedule"] = "weekly";
      values["games.wuwa.launchArgs"] = "-dx11";
      values["games.wuwa.launchViaSteam"] = false;
      return { success: true, values };
    };
    await expect(getGameConfig("wuwa")).resolves.toEqual({
      prefs: { autoUpdate: false, autoUpdateOnStartup: true, autoUpdateSchedule: "weekly" },
      launchArgs: "-dx11",
      customLauncher: "",
      launchViaSteam: false,
    });
    expect(calls).toHaveLength(1);
    expect(calls[0].channel).toBe("get-config");
    expect((calls[0].args[0] as string[]).every((k) => k.startsWith("games.wuwa."))).toBe(true);
  });

  it("reads only the library key for the library", async () => {
    reply = () => ({ success: true, value: { visible: ["wuwa", "wuwa", "nope"], setupComplete: true } });
    await expect(getLibrary()).resolves.toEqual({ visible: ["wuwa"], setupComplete: true });
    expect(calls).toEqual([{ channel: "get-config", args: ["library"] }]);
  });
});
