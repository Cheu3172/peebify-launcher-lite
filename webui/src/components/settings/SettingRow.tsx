// ------------ Setting Row ------------
// Draws one setting from the settings list as the right kind of control (switch, dropdown, number, button or
// plain info), and runs the buttons such as Open logs and Clear launcher data.
import { useSyncExternalStore, type ReactNode } from "react";
import { ExternalLink, Loader2 } from "lucide-react";
import { ALL_SETTINGS, PEEBIFY_DISCORD_URL, XXMI_URL, type Setting } from "../../data/settings";
import { useSettingsStore } from "../../store/settingsStore";
import { useModsStore } from "../../store/modsStore";
import { useModalStore } from "../../store/modalStore";
import { useNotificationStore } from "../../store/notificationStore";
import { openLogsFolder, wipeLauncherData } from "../../lib/ipc";
import { openExternal } from "../../lib/tauri";
import { SettingCard } from "../ui/SettingCard";
import { SegmentedControl } from "../ui/SegmentedControl";
import { Toggle } from "../ui/Toggle";
import { Select } from "../ui/Select";
import { NumberField } from "../ui/NumberField";
import { ActionButton } from "../ui/ActionButton";

let wiping = false;
const wipeListeners = new Set<() => void>();
const setWiping = (on: boolean) => {
  wiping = on;
  wipeListeners.forEach((l) => l());
};
const subscribeWiping = (l: () => void) => {
  wipeListeners.add(l);
  return () => void wipeListeners.delete(l);
};

async function clearLauncherData() {
  if (wiping) return;
  setWiping(true);
  useNotificationStore.getState().toast({
    title: "Clearing launcher data",
    text: "Peebify will restart when it's done.",
  });
  try {
    await wipeLauncherData();
  } finally {
    setWiping(false);
  }
}

function onAction(id: string) {
  const { openConfirm } = useModalStore.getState();
  if (id === "discord") void openExternal(PEEBIFY_DISCORD_URL);
  else if (id === "xxmi") void openExternal(XXMI_URL);
  else if (id === "openLogs") void openLogsFolder();
  else if (id === "clearData") {
    if (wiping) return;
    openConfirm({
      title: "Clear launcher data?",
      message:
        "This resets all launcher settings to their defaults and erases playtime and mod profiles. Unfinished downloads are discarded. Installed games stay installed. This can't be undone.",
      confirmLabel: "Clear data",
      danger: true,
      onConfirm: () => void clearLauncherData(),
    });
  }
}

function settingValue(s: Setting, values: Record<string, string>): string {
  if (s.kind === "info") return s.value;
  return values[s.id] ?? (s.kind === "action" ? "" : s.default);
}

export function settingOn(s: Setting, values: Record<string, string>): boolean {
  if (s.kind !== "toggle") return false;
  const v = settingValue(s, values);
  return s.invert ? v !== "true" : v === "true";
}

export function valueLabel(s: Setting, values: Record<string, string>): string {
  const v = settingValue(s, values);
  switch (s.kind) {
    case "toggle":
      return settingOn(s, values) ? "On" : "Off";
    case "segmented":
    case "select":
      return s.options.find((o) => o.value === v)?.label ?? v;
    case "number":
      if ((v === "0" || !/^\d+$/.test(v)) && s.zeroLabel) return s.zeroLabel;
      return s.unit === "%" ? `${v}%` : `${v} ${s.unit}`;
    case "info":
      return v;
    case "action":
      return "";
  }
}

export function SettingRow({ setting: s }: { setting: Setting }) {
  const values = useSettingsStore((st) => st.values);
  const set = useSettingsStore((st) => st.set);
  const modsEnabled = useModsStore((st) => st.masterEnabled);
  const clearing = useSyncExternalStore(subscribeWiping, () => wiping);

  const parentDef = s.parent ? ALL_SETTINGS.find((x) => x.id === s.parent) : undefined;
  const parentOff = !!parentDef && parentDef.kind === "toggle" && values[s.parent!] !== "true";
  const disabled =
    parentOff || !!s.disableWhen?.(values) || (s.id === "showNsfwMods" && !modsEnabled);

  let control: ReactNode;
  switch (s.kind) {
    case "toggle":
      control = (
        <Toggle
          checked={settingOn(s, values)}
          disabled={disabled}
          ariaLabel={s.title}
          onChange={(v) => set(s.id, s.invert ? (v ? "false" : "true") : v ? "true" : "false")}
        />
      );
      break;
    case "segmented":
      control = (
        <SegmentedControl
          ariaLabel={s.title}
          options={s.options}
          value={values[s.id] ?? s.default}
          disabled={disabled}
          onChange={(v) => set(s.id, v)}
        />
      );
      break;
    case "select":
      control = (
        <Select
          ariaLabel={s.title}
          options={s.options}
          value={values[s.id] ?? s.default}
          disabled={disabled}
          onChange={(v) => set(s.id, v)}
        />
      );
      break;
    case "number":
      control = (
        <NumberField
          value={values[s.id] ?? s.default}
          min={s.min}
          max={s.max}
          unit={s.unit}
          zeroLabel={s.zeroLabel}
          disabled={disabled}
          ariaLabel={s.title}
          onChange={(v) => set(s.id, v)}
        />
      );
      break;
    case "action": {
      const busy = s.id === "clearData" && clearing;
      control = (
        <ActionButton
          variant={s.danger ? "danger" : "neutral"}
          icon={
            busy ? (
              <Loader2 size={15} className="animate-spin" />
            ) : s.buttonIcon === "external" ? (
              <ExternalLink size={15} />
            ) : undefined
          }
          disabled={busy}
          onClick={() => onAction(s.id)}
        >
          {busy ? "Clearing…" : s.button}
        </ActionButton>
      );
      break;
    }
    case "info":
      control = <span className="text-[14px] font-medium">{s.value}</span>;
      break;
  }

  return (
    <SettingCard
      icon={<s.icon size={18} />}
      title={s.title}
      description={s.description}
      disabled={disabled}
      danger={s.kind === "action" && !!s.danger}
      control={control}
    />
  );
}
