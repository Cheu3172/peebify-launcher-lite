import { describe, expect, it } from "vitest";
import { SETTINGS_DEFAULTS as TABLE_DEFAULTS } from "./settings";
import { SETTINGS_DEFAULTS } from "./settingsDefaults";

describe("settings defaults", () => {
  it("match the defaults in the settings table", () => {
    expect(SETTINGS_DEFAULTS).toEqual(TABLE_DEFAULTS);
  });
});
