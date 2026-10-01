// ------------ Staged Progress ------------
// A progress bar split into the steps of a job (downloading, unpacking and so on), so you can see which step is
// running and how far through it is. Dims while the job is paused or waiting for the network.
import type { QueueJob } from "../../lib/ipc";
import { stagesFor, stageIndexFor, stagePercent } from "../../lib/stages";

export function StagedProgress({
  job,
  kind,
  className = "",
}: {
  job: QueueJob;
  kind: string;
  className?: string;
}) {
  const stages = stagesFor(kind);
  const active = stageIndexFor(job, stages);
  const percent = stagePercent(job, stages);
  const idle = job.paused || !!job.waitingNetwork;

  const fillFor = (index: number): number => {
    if (index < active) return 100;
    if (index > active) return 0;
    return percent;
  };

  return (
    <div className={className}>
      <div className="flex gap-[4px]">
        {stages.map((stage, index) => {
          const fill = fillFor(index);
          return (
            <div
              key={stage.id}
              style={{ flexGrow: stage.weight, flexBasis: 0 }}
              className="h-[4px] overflow-hidden rounded-full bg-black/[0.38] shadow-[inset_0_1px_1px_rgba(0,0,0,0.3)]"
            >
              <div
                className={`relative h-full overflow-hidden rounded-full transition-[width] duration-300 ease-linear ${
                  idle ? "bg-white/35" : "bg-white"
                }`}
                style={{ width: `${fill}%` }}
              >
                {index === active && !idle && fill > 0 && (
                  <span className="progress-shimmer" />
                )}
              </div>
            </div>
          );
        })}
      </div>
      <div className="mt-[9px] flex gap-[4px]">
        {stages.map((stage, index) => {
          const done = index < active;
          const current = index === active;
          return (
            <div
              key={stage.id}
              style={{ flexGrow: stage.weight, flexBasis: 0, minWidth: current ? "max-content" : 0 }}
              className="flex items-center gap-[6px]"
            >
              <span
                className={`h-[6px] w-[6px] shrink-0 rounded-full ${
                  current
                    ? idle
                      ? "bg-(--color-warning,#fcd34d)/95"
                      : "bg-white"
                    : done
                      ? "bg-white/45"
                      : "shadow-[inset_0_0_0_1px_rgba(255,255,255,0.25)]"
                }`}
              />
              <span
                className={`truncate text-[11.5px] tabular-nums ${
                  current ? "text-white" : done ? "text-white/45" : "text-white/[0.28]"
                }`}
              >
                {stage.label}
                {current &&
                  (job.paused
                    ? " · paused"
                    : job.waitingNetwork
                      ? " · waiting"
                      : ` · ${Math.round(percent)}%`)}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}
