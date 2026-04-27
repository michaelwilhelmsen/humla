import { useEffect, useState } from "react";
import { ipc, type PermissionKind, type PermissionStatus, type PermissionsStatus } from "../lib/ipc";

const LABEL: Record<PermissionKind, string> = {
  microphone: "Microphone",
  screen: "Screen Recording",
};

const HELP: Record<PermissionKind, string> = {
  microphone: "Required to record your voice.",
  screen: "Required to capture system audio (the other side of meetings).",
};

function statusLabel(s: PermissionStatus): { text: string; tone: "ok" | "bad" | "muted" } {
  switch (s) {
    case "granted": return { text: "Granted", tone: "ok" };
    case "denied": return { text: "Denied", tone: "bad" };
    case "restricted": return { text: "Restricted", tone: "bad" };
    case "not_determined": return { text: "Not requested", tone: "muted" };
    default: return { text: "Unknown", tone: "muted" };
  }
}

export function Permissions() {
  const [status, setStatus] = useState<PermissionsStatus | null>(null);
  const [busy, setBusy] = useState<PermissionKind | null>(null);

  async function refresh() {
    try {
      const s = await ipc.permissionsStatus();
      setStatus(s);
    } catch {
      // ignore — sidecar may not be present in some build configs
    }
  }

  useEffect(() => {
    refresh();
    const t = window.setInterval(refresh, 5000);
    const onFocus = () => refresh();
    window.addEventListener("focus", onFocus);
    return () => {
      window.clearInterval(t);
      window.removeEventListener("focus", onFocus);
    };
  }, []);

  async function request(kind: PermissionKind) {
    setBusy(kind);
    try {
      const s = await ipc.permissionsRequest(kind);
      setStatus(s);
      // Screen recording adds the app to the privacy list on request, but it
      // can't be granted programmatically — open System Settings so the user
      // can toggle the switch.
      if (kind === "screen" && s.screen !== "granted") {
        await ipc.permissionsOpenSettings("screen");
      }
    } catch {
      await refresh();
    } finally {
      setBusy(null);
    }
  }

  async function open(kind: PermissionKind) {
    await ipc.permissionsOpenSettings(kind);
  }

  if (!status) return null;

  return (
    <div className="flex flex-col gap-3">
      {(["microphone", "screen"] as PermissionKind[]).map((kind) => {
        const s = statusLabel(status[kind]);
        // Microphone can be prompted only when it's "not_determined". Screen
        // Recording always reports "denied" until CGRequestScreenCaptureAccess
        // has been called once — which is the call that adds the app to the
        // System Settings list. So allow Request whenever screen isn't granted.
        const showRequest =
          status[kind] === "not_determined" ||
          (kind === "screen" && status[kind] !== "granted");
        const showOpen = status[kind] !== "granted";
        return (
          <div
            key={kind}
            className="flex items-start justify-between gap-4 px-4 py-3 rounded-lg bg-[var(--color-surface)] border border-[var(--color-line)]"
          >
            <div className="min-w-0">
              <div className="flex items-center gap-2">
                <span className="text-sm font-medium">{LABEL[kind]}</span>
                <span
                  className="nd-chip"
                  style={{
                    color:
                      s.tone === "ok"
                        ? "var(--color-success)"
                        : s.tone === "bad"
                        ? "var(--color-accent)"
                        : "var(--color-text-muted)",
                    borderColor:
                      s.tone === "ok"
                        ? "var(--color-success)"
                        : s.tone === "bad"
                        ? "var(--color-accent)"
                        : "var(--color-line-visible)",
                  }}
                >
                  {s.text}
                </span>
              </div>
              <p className="text-xs text-[var(--color-text-muted)] mt-1">{HELP[kind]}</p>
            </div>
            <div className="flex gap-2 shrink-0">
              {showRequest && (
                <button
                  onClick={() => request(kind)}
                  disabled={busy !== null}
                  className="px-3 py-1.5 rounded-md text-sm border border-[var(--color-line-visible)] bg-[var(--color-surface)] hover:border-[var(--color-text)] disabled:opacity-50 transition-colors"
                >
                  {busy === kind ? "Requesting…" : "Request"}
                </button>
              )}
              {showOpen && (
                <button
                  onClick={() => open(kind)}
                  className="px-3 py-1.5 rounded-md text-sm border border-[var(--color-line-visible)] bg-[var(--color-surface)] hover:border-[var(--color-text)] transition-colors"
                >
                  Open System Settings
                </button>
              )}
            </div>
          </div>
        );
      })}
      <p className="text-xs text-[var(--color-text-muted)] mt-1">
        After enabling Screen Recording in System Settings, you must restart the app for the change to take effect.
      </p>
    </div>
  );
}
