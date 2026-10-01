// ------------ Mods Panel ------------
// The Mods tab of Settings. Holds the switch that turns mod support on, with the warning that mods can get an
// account banned.

import { AlertTriangle } from "lucide-react";
import { useModsStore } from "../../store/modsStore";
import { setConfigValue } from "../../lib/ipc";
import { MOD_RISK_WARNING, requestEnableMods } from "../../lib/mods";
import { SettingCard } from "../ui/SettingCard";
import { Toggle } from "../ui/Toggle";

export function ModsPanel() {
  const master = useModsStore((s) => s.masterEnabled);
  const setMasterEnabled = useModsStore((s) => s.setMasterEnabled);

  const apply = (on: boolean) =>
    void setConfigValue("behavior.modsEnabled", on).then(() => setMasterEnabled(on));

  const toggleMaster = (on: boolean) => {
    if (!on) {
      apply(false);
      return;
    }
    requestEnableMods(() => apply(true));
  };

  return (
    <SettingCard
      icon={<AlertTriangle size={18} />}
      title="Enable mod support"
      description={MOD_RISK_WARNING}
      control={<Toggle checked={master} ariaLabel="Enable mod support" onChange={toggleMaster} />}
    />
  );
}
