import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

type Phase =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "up-to-date" }
  | { kind: "available"; update: Update }
  | { kind: "downloading"; downloaded: number; total: number | null }
  | { kind: "installing" }
  | { kind: "error"; message: string };

export function Updater() {
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });

  useEffect(() => {
    const unlisten = listen("menu://check-for-updates", () => {
      void runCheck(true);
    });

    void runCheck(false);

    async function runCheck(manual: boolean) {
      setPhase({ kind: "checking" });
      try {
        const update = await check();
        if (update) {
          setPhase({ kind: "available", update });
        } else if (manual) {
          setPhase({ kind: "up-to-date" });
          window.setTimeout(() => {
            setPhase((p) => (p.kind === "up-to-date" ? { kind: "idle" } : p));
          }, 2500);
        } else {
          setPhase({ kind: "idle" });
        }
      } catch (e) {
        if (manual) setPhase({ kind: "error", message: String(e) });
        else setPhase({ kind: "idle" });
      }
    }

    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  async function install(update: Update) {
    setPhase({ kind: "downloading", downloaded: 0, total: null });
    try {
      let total: number | null = null;
      let downloaded = 0;
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          total = event.data.contentLength ?? null;
          setPhase({ kind: "downloading", downloaded: 0, total });
        } else if (event.event === "Progress") {
          downloaded += event.data.chunkLength;
          setPhase({ kind: "downloading", downloaded, total });
        } else if (event.event === "Finished") {
          setPhase({ kind: "installing" });
        }
      });
      await relaunch();
    } catch (e) {
      setPhase({ kind: "error", message: String(e) });
    }
  }

  if (phase.kind === "idle") return null;

  return (
    <div className="no-drag fixed bottom-6 right-6 z-50 max-w-sm">
      <div className="px-4 py-3 rounded-lg bg-[var(--color-surface)] border border-[var(--color-line)] shadow-md text-sm">
        {phase.kind === "checking" && (
          <div className="text-[var(--color-text-muted)]">Checking for updates…</div>
        )}
        {phase.kind === "up-to-date" && (
          <div className="text-[var(--color-text)]">You're up to date.</div>
        )}
        {phase.kind === "available" && (
          <div>
            <div className="font-medium mb-1">Update available</div>
            <div className="text-[var(--color-text-muted)] mb-3">
              Humla {phase.update.version} is ready to install.
            </div>
            <div className="flex gap-2">
              <button
                onClick={() => install(phase.update)}
                className="px-3 py-1.5 rounded-md bg-[var(--color-text)] text-[var(--color-surface)] hover:opacity-90"
              >
                Install &amp; Restart
              </button>
              <button
                onClick={() => setPhase({ kind: "idle" })}
                className="px-3 py-1.5 rounded-md hover:bg-[var(--color-pill-hover)] text-[var(--color-text-muted)]"
              >
                Later
              </button>
            </div>
          </div>
        )}
        {phase.kind === "downloading" && (
          <div>
            <div className="font-medium mb-2">Downloading update…</div>
            <DownloadBar downloaded={phase.downloaded} total={phase.total} />
          </div>
        )}
        {phase.kind === "installing" && (
          <div className="text-[var(--color-text)]">Installing — restarting Humla…</div>
        )}
        {phase.kind === "error" && (
          <div>
            <div className="font-medium text-red-600 dark:text-red-400 mb-1">
              Update failed
            </div>
            <div className="text-[var(--color-text-muted)] mb-2 break-words">
              {phase.message}
            </div>
            <button
              onClick={() => setPhase({ kind: "idle" })}
              className="text-xs underline text-[var(--color-text-muted)] hover:text-[var(--color-text)]"
            >
              Dismiss
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

function DownloadBar({ downloaded, total }: { downloaded: number; total: number | null }) {
  const pct = total && total > 0 ? Math.min(100, (downloaded / total) * 100) : null;
  return (
    <div>
      <div className="h-1.5 rounded-full bg-[var(--color-line)] overflow-hidden">
        <div
          className="h-full bg-[var(--color-text)] transition-[width] duration-150"
          style={{ width: pct === null ? "30%" : `${pct}%` }}
        />
      </div>
      <div className="mt-1 text-xs text-[var(--color-text-muted)]">
        {pct === null
          ? `${formatBytes(downloaded)}…`
          : `${formatBytes(downloaded)} / ${formatBytes(total!)} (${pct.toFixed(0)}%)`}
      </div>
    </div>
  );
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
