// ------------ Settings Defaults ------------
// The default value of each launcher setting as a plain object, so stores can use it without pulling in the
// whole settings table and its icons. A test checks it matches the table.
export const SETTINGS_DEFAULTS: Record<string, string> = {
  startOnBoot: "false",
  startOnBootAction: "open",
  launchAction: "minimize",
  reopenAfterGameClose: "true",
  closeAction: "close",
  minimizeAction: "minimize",
  rememberWindowState: "true",
  osNotifications: "true",
  hideNewsPanel: "false",
  hideSocials: "false",
  hidePlaytime: "false",
  hideBottomRightButtons: "false",
  animatedWallpaper: "true",
  timeFormat: "system",
  showNsfwMods: "false",
  overlayEnabled: "false",
  overlayShotToast: "true",
  overlayShotFormat: "png",
  overlayShotQuality: "90",
  overlayRecRes: "native",
  overlayRecFps: "60",
  overlayRecCodec: "h264",
  overlayRecQuality: "balanced",
};
