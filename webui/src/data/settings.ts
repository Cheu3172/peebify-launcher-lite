// ------------ Settings Table ------------
// A table of every toggle, dropdown and slider on the Settings page, grouped into sections and tabs.
// The settings UI is drawn from this, and the default values are worked out from it too.
import type { LucideIcon } from "lucide-react";
import {
  AppWindow,
  Bell,
  Clock,
  EyeOff,
  FastForward,
  FileImage,
  FileVideo,
  Film,
  Frame,
  Gamepad2,
  MessageCircle,
  MessageSquare,
  Minimize2,
  Monitor,
  MousePointerClick,
  Newspaper,
  Package,
  Power,
  Ratio,
  ScrollText,
  Share2,
  SlidersHorizontal,
  Sparkles,
  Timer,
  Trash2,
  Undo2,
  UserRound,
  X,
} from "lucide-react";
export type SettingsCategory =
  | "general"
  | "appearance"
  | "games"
  | "mods"
  | "overlay"
  | "about";

interface BaseSetting {
  id: string;
  icon: LucideIcon;
  title: string;
  description: string;
  group?: string;
  parent?: string;
  disableWhen?: (values: Record<string, string>) => boolean;
}

interface SegmentedSetting extends BaseSetting {
  kind: "segmented";
  options: { value: string; label: string }[];
  default: string;
}

interface ToggleSetting extends BaseSetting {
  kind: "toggle";
  default: string;
  invert?: boolean;
}

interface SelectSetting extends BaseSetting {
  kind: "select";
  options: { value: string; label: string }[];
  default: string;
}

interface NumberSetting extends BaseSetting {
  kind: "number";
  min: number;
  max: number;
  unit: string;
  zeroLabel: string;
  default: string;
}

interface ActionSetting extends BaseSetting {
  kind: "action";
  button: string;
  buttonIcon?: "external";
  danger?: boolean;
}

interface InfoSetting extends BaseSetting {
  kind: "info";
  value: string;
}

export type Setting =
  | SegmentedSetting
  | ToggleSetting
  | SelectSetting
  | NumberSetting
  | ActionSetting
  | InfoSetting;

interface SettingsSection {
  category: SettingsCategory;
  title: string;
  subtitle: string;
  settings: Setting[];
}

const triState = (a: string, b: string, c: string) => [
  { value: "minimize", label: a },
  { value: "tray", label: b },
  { value: "close", label: c },
];

export const SETTINGS_SECTIONS: SettingsSection[] = [
  {
    category: "general",
    title: "General",
    subtitle: "Startup, window, and notification behavior.",
    settings: [
      {
        kind: "toggle",
        id: "startOnBoot",
        icon: Power,
        group: "Startup",
        title: "Start with Windows",
        description: "Open the launcher on system startup.",
        default: "false",
      },
      {
        kind: "segmented",
        id: "startOnBootAction",
        icon: AppWindow,
        parent: "startOnBoot",
        group: "Startup",
        title: "On boot, open",
        description: "How the launcher appears when it starts with Windows.",
        options: [
          { value: "open", label: "Normally" },
          { value: "minimized", label: "Minimized" },
          { value: "tray", label: "To tray" },
        ],
        default: "open",
      },
      {
        kind: "segmented",
        id: "launchAction",
        icon: Gamepad2,
        group: "When a game launches",
        title: "Launcher window",
        description: "What the launcher does when a game starts. Close hides it until the game exits, then quits, so playtime is still tracked.",
        options: triState("Minimize", "To tray", "Close"),
        default: "minimize",
      },
      {
        kind: "toggle",
        id: "reopenAfterGameClose",
        icon: Undo2,
        group: "When a game launches",
        title: "Re-open after the game closes",
        description: "Restore the launcher when the game exits.",
        default: "true",
        disableWhen: (v) => v.launchAction === "close",
      },
      {
        kind: "segmented",
        id: "closeAction",
        icon: X,
        group: "Window",
        title: "When you close the launcher",
        description: "Action for the close (X) button.",
        options: triState("Minimize", "To tray", "Close"),
        default: "close",
      },
      {
        kind: "segmented",
        id: "minimizeAction",
        icon: Minimize2,
        group: "Window",
        title: "When you minimize",
        description: "What the minimize button does.",
        options: triState("Minimize", "To tray", "Close"),
        default: "minimize",
      },
      {
        kind: "toggle",
        id: "rememberWindowState",
        icon: Frame,
        group: "Window",
        title: "Remember window size & position",
        description: "Restore the launcher's last window bounds on startup.",
        default: "true",
      },
      {
        kind: "toggle",
        id: "osNotifications",
        icon: Bell,
        group: "Notifications",
        title: "System notifications",
        description:
          "Show Windows notifications for finished downloads and launch failures while the launcher is in the background.",
        default: "true",
      },
    ],
  },
  {
    category: "appearance",
    title: "Appearance",
    subtitle: "Wallpapers, icons, game colors, and what the home screen shows.",
    settings: [
      {
        kind: "toggle",
        id: "hideNewsPanel",
        icon: Newspaper,
        invert: true,
        group: "Home screen",
        title: "Show news panel",
        description: "Per-game news, notices, and the banner slideshow.",
        default: "false",
      },
      {
        kind: "toggle",
        id: "hideSocials",
        icon: Share2,
        invert: true,
        group: "Home screen",
        title: "Show socials tray",
        description: "Social links on the home screen.",
        default: "false",
      },
      {
        kind: "toggle",
        id: "hidePlaytime",
        icon: Timer,
        invert: true,
        group: "Home screen",
        title: "Show playtime pill",
        description: "The playtime tracker by the Start button.",
        default: "false",
      },
      {
        kind: "toggle",
        id: "hideBottomRightButtons",
        icon: MousePointerClick,
        invert: true,
        group: "Home screen",
        title: "Show quick-action buttons",
        description: "The repair / settings / folder shortcuts in the bottom-right.",
        default: "false",
      },
      {
        kind: "toggle",
        id: "animatedWallpaper",
        icon: Film,
        group: "Wallpaper",
        title: "Animated wallpaper",
        description: "Use the looping video wallpaper instead of a static image.",
        default: "true",
      },
      {
        kind: "segmented",
        id: "timeFormat",
        icon: Clock,
        group: "Time",
        title: "Clock format",
        description:
          "How playtime and session times are written. System follows your Windows region.",
        options: [
          { value: "system", label: "System" },
          { value: "12", label: "12-hour" },
          { value: "24", label: "24-hour" },
        ],
        default: "system",
      },
    ],
  },
  {
    category: "games",
    title: "Games",
    subtitle: "Pick your preferences for the side-bar, as well as additional customization.",
    settings: [],
  },
  {
    category: "mods",
    title: "Mods",
    subtitle: "Set up mod support and the XXMI tools.",
    settings: [
      {
        kind: "toggle",
        id: "showNsfwMods",
        icon: EyeOff,
        group: "GameBanana",
        title: "Show NSFW mods",
        description: "Show mature-rated mods in the GameBanana browser.",
        default: "false",
      },
    ],
  },
  {
    category: "overlay",
    title: "Overlay",
    subtitle: "The in-game drawer, the on-screen readout, and captures.",
    settings: [
      {
        kind: "toggle",
        id: "overlayEnabled",
        icon: Monitor,
        group: "Overlay",
        title: "Use the game overlay",
        description:
          "Draws a panel over the running game so you can change mods, watch performance, and capture clips without alt-tabbing.",
        default: "false",
      },

      {
        kind: "toggle",
        id: "overlayShotToast",
        icon: MessageSquare,
        parent: "overlayEnabled",
        group: "Screenshots",
        title: "Show a message on screen",
        description:
          "Confirms over the game that a capture was saved, and says why when one fails.",
        default: "true",
      },
      {
        kind: "segmented",
        id: "overlayShotFormat",
        icon: FileImage,
        parent: "overlayEnabled",
        group: "Screenshots",
        title: "Format",
        description: "PNG is lossless and large. JPEG is small and lossy.",
        options: [
          { value: "png", label: "PNG" },
          { value: "jpeg", label: "JPEG" },
        ],
        default: "png",
      },
      {
        kind: "number",
        id: "overlayShotQuality",
        icon: SlidersHorizontal,
        parent: "overlayEnabled",
        group: "Screenshots",
        title: "JPEG quality",
        description: "Higher keeps more detail and makes a bigger file. Ignored for PNG.",
        min: 40,
        max: 100,
        unit: "%",
        zeroLabel: "",
        default: "90",
        disableWhen: (v) => v.overlayShotFormat !== "jpeg",
      },

      {
        kind: "select",
        id: "overlayRecRes",
        icon: Ratio,
        parent: "overlayEnabled",
        group: "Recording",
        title: "Resolution",
        description: "Recording below your game's resolution costs less to encode.",
        options: [
          { value: "native", label: "Same as the game" },
          { value: "1440", label: "1440p" },
          { value: "1080", label: "1080p" },
          { value: "720", label: "720p" },
        ],
        default: "native",
      },
      {
        kind: "segmented",
        id: "overlayRecFps",
        icon: FastForward,
        parent: "overlayEnabled",
        group: "Recording",
        title: "Frame rate",
        description: "Frames per second in the saved file, not in the game.",
        options: [
          { value: "30", label: "30" },
          { value: "60", label: "60" },
          { value: "120", label: "120" },
        ],
        default: "60",
      },
      {
        kind: "select",
        id: "overlayRecCodec",
        icon: FileVideo,
        parent: "overlayEnabled",
        group: "Recording",
        title: "Codec",
        description:
          "H.264 plays everywhere. AV1 makes the smallest files. If your graphics card cannot encode the one you pick, Peebify records H.264 instead.",
        options: [
          { value: "h264", label: "H.264" },
          { value: "hevc", label: "HEVC (H.265)" },
          { value: "av1", label: "AV1" },
        ],
        default: "h264",
      },
      {
        kind: "select",
        id: "overlayRecQuality",
        icon: Sparkles,
        parent: "overlayEnabled",
        group: "Recording",
        title: "Quality",
        description:
          "Peebify picks the bitrate from this, your resolution and your frame rate, and only spends it where the picture is actually moving.",
        options: [
          { value: "efficient", label: "Efficient" },
          { value: "balanced", label: "Balanced" },
          { value: "high", label: "High" },
        ],
        default: "balanced",
      },
    ],
  },
  {
    category: "about",
    title: "About",
    subtitle: "Troubleshooting, credits, and community.",
    settings: [
      {
        kind: "action",
        id: "openLogs",
        icon: ScrollText,
        group: "Data",
        title: "Open logs folder",
        description: "Open the folder with launcher logs for troubleshooting.",
        button: "Open now",
      },
      {
        kind: "info",
        id: "developer",
        icon: UserRound,
        group: "Credits",
        title: "Developer",
        description: "Designs, builds, and maintains Peebify.",
        value: "Acheuy",
      },
      {
        kind: "action",
        id: "xxmi",
        icon: Package,
        group: "Credits",
        title: "XXMI",
        description:
          "Peebify's mod support is built on the XXMI modding framework and its per-game model importers, by SpectrumQT and leotorrez.",
        button: "GitHub",
        buttonIcon: "external",
      },
      {
        kind: "action",
        id: "discord",
        icon: MessageCircle,
        group: "Community",
        title: "Peebify Discord",
        description: "Get help, report a bug, or just say hello.",
        button: "Join",
        buttonIcon: "external",
      },
      {
        kind: "action",
        id: "clearData",
        icon: Trash2,
        group: "Danger zone",
        title: "Clear launcher data",
        description:
          "Resets all launcher settings to defaults. Playtime and mod profiles are erased. Installed games stay installed.",
        button: "Clear data",
        danger: true,
      },
    ],
  },
];

export const SETTINGS_DEFAULTS: Record<string, string> = Object.fromEntries(
  SETTINGS_SECTIONS.flatMap((s) =>
    s.settings
      .filter(
        (x): x is SegmentedSetting | ToggleSetting | SelectSetting | NumberSetting =>
          x.kind !== "action" && x.kind !== "info",
      )
      .map((x) => [x.id, x.default]),
  ),
);

export const ALL_SETTINGS: Setting[] = SETTINGS_SECTIONS.flatMap((s) => s.settings);

export const PEEBIFY_DISCORD_URL = "https://discord.gg/5kfpJTv2Xc";
export const XXMI_URL = "https://github.com/SpectrumQT/XXMI-Launcher";
