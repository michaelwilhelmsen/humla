import { useEffect, useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { Check, Copy } from "lucide-react";
import { Btn } from "../../pages/settings/components/Btn";
import { Row } from "../../pages/settings/components/Section";
import { Select } from "../ui/Select";
import {
  ipc,
  onCopilotCliDownload,
  onCopilotSignIn,
  type CopilotDeviceCode,
  type CopilotSignInEvent,
} from "../../lib/ipc";
import { useCopilotModels, type CopilotHook } from "./useCopilot";

type Download =
  | { kind: "idle" }
  | { kind: "running"; fraction: number | null }
  | { kind: "failed"; message: string };

export function signInFailureMessage(e: CopilotSignInEvent): string {
  switch (e.status) {
    case "denied":
      return "GitHub sign-in was declined. Sign in again to use Copilot.";
    case "expired":
      return "The code expired before it was entered. Sign in again for a new one.";
    default:
      return e.message || "Couldn't sign in to GitHub. Try again.";
  }
}

/** GitHub Copilot's setup in Settings → Summary and Settings → Chat: the CLI
 *  Humla drives, the GitHub sign-in, and the model the tab uses. */
export function CopilotCard({
  copilot,
  model,
  onModelChange,
  modelDescription,
}: {
  copilot: CopilotHook;
  model: string;
  onModelChange: (v: string) => void;
  modelDescription: string;
}) {
  const { status, error, refresh, setLogin, ready } = copilot;
  const [download, setDownload] = useState<Download>({ kind: "idle" });
  const [code, setCode] = useState<CopilotDeviceCode | null>(null);
  const [starting, setStarting] = useState(false);
  const [signInError, setSignInError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const models = useCopilotModels(ready, model);

  useEffect(() => {
    let cancelled = false;
    const unsubs: (() => void)[] = [];
    const claim = (u: () => void) => {
      if (cancelled) u();
      else unsubs.push(u);
    };
    onCopilotCliDownload((e) => {
      if (e.status === "progress") setDownload({ kind: "running", fraction: e.fraction });
      else if (e.status === "done") {
        setDownload({ kind: "idle" });
        refresh();
      } else {
        setDownload({ kind: "failed", message: e.message || "The download failed." });
      }
    }).then(claim);
    onCopilotSignIn((e) => {
      setCode(null);
      if (e.status === "signed_in") {
        setSignInError(null);
        setLogin(e.login);
      } else {
        setSignInError(signInFailureMessage(e));
      }
    }).then(claim);
    return () => {
      cancelled = true;
      unsubs.forEach((u) => u());
    };
  }, [refresh, setLogin]);

  async function startDownload() {
    setDownload({ kind: "running", fraction: null });
    try {
      await ipc.copilotCliDownload();
    } catch (e) {
      setDownload({ kind: "failed", message: String(e) });
    }
  }

  async function signIn() {
    setSignInError(null);
    setStarting(true);
    try {
      const c = await ipc.copilotSignInStart();
      setCode(c);
      void openExternal(c.verificationUri).catch(() => {});
    } catch (e) {
      setSignInError(String(e));
    } finally {
      setStarting(false);
    }
  }

  async function cancelSignIn() {
    setCode(null);
    await ipc.copilotSignInCancel().catch(() => {});
  }

  async function signOut() {
    try {
      await ipc.copilotSignOut();
      setLogin(null);
    } catch (e) {
      setSignInError(String(e));
    }
  }

  function copyCode() {
    if (!code) return;
    navigator.clipboard?.writeText(code.userCode).then(
      () => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      },
      () => {},
    );
  }

  if (!status) {
    return (
      <p className="text-xs text-[var(--color-text-muted)] py-3.5">
        {error ? `Couldn't check GitHub Copilot: ${error}` : "Checking GitHub Copilot…"}
      </p>
    );
  }

  const pct =
    download.kind === "running" && download.fraction !== null
      ? Math.round(download.fraction * 100)
      : null;

  // A CLI that won't start is most likely on a protocol the SDK can't speak; the
  // supported download is found ahead of it once it exists.
  const brokenCli = !!status.cliPath && !!status.cliError && !status.cliIsDownload;

  return (
    <>
      <div className="py-3.5">
        <div className="text-sm">Copilot CLI</div>
        {status.cliPath && !brokenCli ? (
          <p className="text-xs text-[var(--color-text-muted)] mt-0.5">
            {status.cliVersion ? `Installed · ${status.cliVersion}` : "Installed"}
          </p>
        ) : (
          <>
            <p className="text-xs text-[var(--color-text-muted)] mt-0.5 mb-2">
              {brokenCli
                ? `The Copilot CLI on this Mac (version ${status.cliVersion ?? "unknown"}) didn't start. Download the version Humla supports (${status.downloadVersion}, about 140 MB).`
                : `Humla reaches Copilot through GitHub's Copilot CLI, which isn't on this Mac yet (version ${status.downloadVersion}, about 140 MB).`}
            </p>
            {brokenCli && (
              <p className="text-xs text-[var(--color-danger)] mb-2 break-words">{status.cliError}</p>
            )}
            {download.kind === "running" ? (
              <>
                <p className="text-xs text-[var(--color-text-muted)]">
                  Downloading{pct !== null ? ` — ${pct}%` : "…"}
                </p>
                <div
                  role="progressbar"
                  aria-label="Copilot CLI download"
                  aria-valuenow={pct ?? undefined}
                  aria-valuemin={0}
                  aria-valuemax={100}
                  className="mt-2 h-1 w-48 rounded-full bg-[var(--color-pill-hover)] overflow-hidden"
                >
                  <div
                    className="h-full bg-[var(--color-accent)] transition-[width]"
                    style={{ width: `${pct ?? 0}%` }}
                  />
                </div>
              </>
            ) : (
              <>
                <Btn onClick={() => void startDownload()}>
                  {download.kind === "failed" ? "Try again" : "Download Copilot CLI"}
                </Btn>
                {download.kind === "failed" && (
                  <p className="text-xs text-[var(--color-danger)] mt-2 break-words">
                    {download.message}
                  </p>
                )}
              </>
            )}
          </>
        )}
      </div>

      <div className="py-3.5">
        <div className="text-sm">GitHub account</div>
        {status.login ? (
          <div className="flex items-center justify-between gap-4 mt-0.5">
            <p className="text-xs text-[var(--color-text-muted)]">
              Signed in as <span className="text-[var(--color-text)]">{status.login}</span>
            </p>
            <Btn onClick={() => void signOut()}>Sign out</Btn>
          </div>
        ) : code ? (
          <div className="mt-1 space-y-2">
            <p className="text-xs text-[var(--color-text-muted)]">
              Enter this code on GitHub to let Humla use your Copilot subscription:
            </p>
            <div className="flex items-center gap-2">
              <code
                aria-label="GitHub sign-in code"
                className="nd-code text-2xl font-semibold tracking-widest px-3 py-2 rounded-md bg-[var(--color-pill-hover)] select-all"
              >
                {code.userCode}
              </code>
              <button type="button" onClick={copyCode} className="nd-btn shrink-0" aria-label="Copy code">
                {copied ? (
                  <>
                    <Check size={13} strokeWidth={2.5} />
                    Copied
                  </>
                ) : (
                  <>
                    <Copy size={13} strokeWidth={2} />
                    Copy
                  </>
                )}
              </button>
            </div>
            <p className="text-xs text-[var(--color-text-muted)]">
              Waiting for GitHub…{" "}
              <button
                type="button"
                onClick={() => void openExternal(code.verificationUri).catch(() => {})}
                className="underline hover:text-[var(--color-text)]"
              >
                Open {code.verificationUri.replace(/^https?:\/\//, "")}
              </button>
            </p>
            <Btn onClick={() => void cancelSignIn()}>Cancel</Btn>
          </div>
        ) : (
          <>
            <p className="text-xs text-[var(--color-text-muted)] mt-0.5 mb-2">
              Summaries and chat run on your GitHub Copilot subscription.
            </p>
            <Btn onClick={() => void signIn()} disabled={starting}>
              {starting ? "Starting…" : "Sign in with GitHub"}
            </Btn>
          </>
        )}
        {signInError && !code && (
          <p className="text-xs text-[var(--color-danger)] mt-2 break-words">{signInError}</p>
        )}
      </div>

      {ready && (
        <Row
          label="Model"
          description={modelDescription}
          control={<Select value={model || "auto"} onChange={onModelChange} options={models} />}
        />
      )}
    </>
  );
}
