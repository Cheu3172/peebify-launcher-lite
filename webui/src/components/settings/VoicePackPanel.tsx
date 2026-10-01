// ------------ Voice Packs ------------
// Lets you pick which spoken languages to install for a game. Only appears for games that offer voice packs. A
// newly picked pack downloads on the next update, or right away if the game is installed.
import { useCallback, useEffect, useRef, useState } from "react";
import { Languages, ListChecks, RefreshCw, Volume2 } from "lucide-react";
import { getContentPacks, setContentPacks, type ContentPack } from "../../lib/ipc";
import { fmtBytes } from "../../lib/format";
import { SettingsExpander } from "../ui/SettingsExpander";
import { SettingCard } from "../ui/SettingCard";
import { Toggle } from "../ui/Toggle";
import { ActionButton } from "../ui/ActionButton";

const knownSupport = new Map<string, boolean>();

export function VoicePackPanel({ gameId }: { gameId: string }) {
  const [packs, setPacks] = useState<ContentPack[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const seq = useRef(0);

  const load = useCallback(() => {
    const id = ++seq.current;
    setLoading(true);
    setFailed(false);
    void getContentPacks(gameId).then((r) => {
      if (id !== seq.current) return;
      knownSupport.set(gameId, r.supported);
      setPacks(r.supported && !r.error ? r.packs : null);
      setFailed(r.error);
      setLoading(false);
    });
  }, [gameId]);

  useEffect(() => {
    const s = seq;
    load();
    return () => {
      s.current++;
    };
  }, [load]);

  if (loading) {
    if (!knownSupport.get(gameId)) return null;
    return (
      <SettingCard
        icon={<Languages size={18} />}
        title="Voice packs"
        description="Loading packs"
      />
    );
  }

  if (failed) {
    return (
      <SettingCard
        icon={<Languages size={18} />}
        title="Voice packs"
        description="Could not load the pack list. Check your connection and try again."
        control={
          <ActionButton onClick={load} icon={<RefreshCw size={13} />}>
            Retry
          </ActionButton>
        }
      />
    );
  }

  if (!packs || packs.length === 0) return null;

  const selectedCount = packs.filter((p) => p.selected).length;

  const apply = async (next: ContentPack[]) => {
    const prev = packs;
    setPacks(next);
    setBusy(true);
    try {
      const chosen = next.filter((p) => p.selected).map((p) => p.tag);
      const saved = await setContentPacks(gameId, chosen.length === next.length ? null : chosen);
      if (!saved) setPacks(prev);
    } finally {
      setBusy(false);
    }
  };

  const toggle = (tag: string, on: boolean) =>
    void apply(packs.map((p) => (p.tag === tag ? { ...p, selected: on } : p)));

  const total = packs.filter((p) => p.selected).reduce((sum, p) => sum + p.bytes, 0);

  return (
    <SettingsExpander
      icon={<Languages size={18} />}
      title="Voice packs"
      summary={
        selectedCount === 0
          ? "None selected, the game installs without spoken audio"
          : `${selectedCount} of ${packs.length} selected, ${fmtBytes(total)} on top of the base game`
      }
      description="Spoken audio installed alongside the game. A newly selected pack downloads on the next update, or right away with a repair. Turning a pack off stops future downloads but keeps files already on disk."
      control={
        <ActionButton
          onClick={() => void apply(packs.map((p) => ({ ...p, selected: true })))}
          disabled={busy || selectedCount === packs.length}
          icon={<ListChecks size={13} />}
        >
          Select all
        </ActionButton>
      }
    >
      {packs.map((pack) => (
        <SettingCard
          key={pack.tag}
          icon={<Volume2 size={18} />}
          title={pack.language ?? pack.tag}
          description={
            pack.language
              ? `${pack.tag} · ${fmtBytes(pack.bytes)} · ${pack.files} files`
              : `${fmtBytes(pack.bytes)} · ${pack.files} files`
          }
          control={
            <Toggle
              checked={pack.selected}
              disabled={busy}
              ariaLabel={pack.language ?? pack.tag}
              onChange={(v) => toggle(pack.tag, v)}
            />
          }
        />
      ))}
    </SettingsExpander>
  );
}
