// ------------ Error Boundary ------------
// Catches a crash while drawing the interface, writes it to the log and shows a plain "Something went wrong"
// screen with a Reload launcher button instead of a blank window.
import { Component, type ErrorInfo, type ReactNode } from "react";
import { log } from "../lib/log";

interface Props {
  children: ReactNode;
  fallback?: (reset: () => void) => ReactNode;
  scope?: string;
  context?: () => Record<string, unknown>;
}

interface State {
  error: Error | null;
}

function readContext(context: Props["context"]): Record<string, unknown> | string {
  if (!context) return "";
  try {
    return context();
  } catch (e) {
    return `(context unavailable: ${String(e)})`;
  }
}

const RETRY_WINDOW_MS = 10_000;
let lastRetryAt = 0;

// "Try again" first just re-draws the crashed part. If it crashes again straight after a retry, re-drawing won't
// help (a broken module, a stale reload), so the next press reloads the whole launcher instead.
export function retryOrReload(reset: () => void): void {
  if (Date.now() - lastRetryAt < RETRY_WINDOW_MS) {
    window.location.reload();
    return;
  }
  lastRetryAt = Date.now();
  reset();
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    const scope = this.props.scope ? ` (${this.props.scope})` : "";
    log.error(
      `React render crash${scope}:`,
      error,
      readContext(this.props.context),
      info.componentStack || "",
    );
    void log.flush();
  }

  reset = (): void => this.setState({ error: null });

  render(): ReactNode {
    if (!this.state.error) return this.props.children;
    if (this.props.fallback) return this.props.fallback(this.reset);
    return (
      <div className="flex h-screen w-screen flex-col items-center justify-center gap-4 bg-[#0d0d10] text-white">
        <div className="text-lg font-semibold">Something went wrong</div>
        <div className="max-w-[420px] text-center text-sm text-white/60">
          The launcher UI hit an unexpected error. It has been written to the log. Reloading
          usually fixes it.
        </div>
        <button
          className="rounded-lg bg-white/10 px-4 py-2 text-sm hover:bg-white/20"
          onClick={() => window.location.reload()}
        >
          Try again
        </button>
      </div>
    );
  }
}
