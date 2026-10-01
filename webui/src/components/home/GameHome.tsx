// ------------ Game Home ------------
// The Home screen for the selected game. Lays out the socials tray, news panel, start bar and bottom right
// buttons on top of the wallpaper, and hides whichever ones the user turned off in settings.
import type { ReactNode } from "react";
import { AnimatePresence, m } from "framer-motion";
import { useSettingsStore } from "../../store/settingsStore";
import { useReducedMotionSafe, pageVariantsSolid } from "../../lib/motion";
import { SocialsTray } from "./SocialsTray";
import { NewsPanel } from "./NewsPanel";
import { StartBar } from "./StartBar";
import { BottomRightActions } from "./BottomRightActions";

function Fade({ dur, children }: { dur: number; children: ReactNode }) {
  return (
    <m.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1, transition: { duration: dur } }}
      exit={{ opacity: 0, transition: { duration: dur } }}
    >
      {children}
    </m.div>
  );
}

export function GameHome() {
  const hideSocials = useSettingsStore((s) => s.values.hideSocials === "true");
  const hideNewsPanel = useSettingsStore((s) => s.values.hideNewsPanel === "true");
  const hidePlaytime = useSettingsStore((s) => s.values.hidePlaytime === "true");
  const hideBottomRight = useSettingsStore((s) => s.values.hideBottomRightButtons === "true");
  const reduce = useReducedMotionSafe();
  const dur = reduce ? 0 : 0.15;

  return (
    <m.div variants={pageVariantsSolid} className="absolute inset-0">
      <AnimatePresence initial={false}>
        {!hideSocials && <Fade key="socials" dur={dur}><SocialsTray /></Fade>}
        {!hideBottomRight && <Fade key="actions" dur={dur}><BottomRightActions /></Fade>}
      </AnimatePresence>

      <div className="absolute bottom-[22px] left-[18px] z-[6] flex flex-col items-start gap-[12px]">
        <AnimatePresence initial={false}>
          {!hideNewsPanel && (
            <Fade key="news" dur={dur}>
              <m.div layout>
                <NewsPanel />
              </m.div>
            </Fade>
          )}
        </AnimatePresence>
        <m.div layout>
          <StartBar hidePlaytime={hidePlaytime} />
        </m.div>
      </div>
    </m.div>
  );
}
