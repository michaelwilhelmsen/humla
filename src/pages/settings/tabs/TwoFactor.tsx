import { useCallback, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import {
  cloudApi,
  cloudError,
  joinNames,
  useCloudStore,
  type SecondFactor,
  type TwoFactorSetup,
} from "../../../lib/cloud";
import { TwoFactorCodeForm } from "../../../components/TwoFactorCodeForm";
import { Row, Section } from "../components/Section";
import { Btn } from "../components/Btn";
import { Modal } from "../components/Modal";
import { ValuePill } from "../components/ValuePill";

const inputCls =
  "w-full text-sm px-3 py-2 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors";

type FlowKind = "enable" | "replace" | "codes" | "disable";

const FLOW_TITLE: Record<FlowKind, string> = {
  enable: "Turn on two-factor authentication",
  replace: "Replace your authenticator app",
  codes: "New recovery codes",
  disable: "Turn off two-factor authentication",
};

const CONFIRM_LABEL: Record<Exclude<FlowKind, "enable">, string> = {
  replace: "Continue",
  codes: "Make new codes",
  disable: "Turn off",
};

/** "require" agrees with however many workspaces are named. */
function requires(names: string[]): string {
  return `${joinNames(names)} ${names.length === 1 ? "requires" : "require"}`;
}

// Settings → Account → Two-factor authentication. Shown only on a
// server that offers it.
export function TwoFactorSection() {
  const status = useCloudStore((s) => s.status);
  const [flow, setFlow] = useState<FlowKind | null>(null);
  const closeFlow = useCallback(() => setFlow(null), []);
  const tf = status.two_factor;
  if (!status.logged_in || !tf?.available) return null;

  const locked = (status.locked_workspaces ?? []).map((w) => w.name);
  const requiredBy = tf.required_by.map((w) => w.name);
  const left = tf.recovery_codes_left;

  return (
    <Section title="Two-factor authentication">
      {tf.enabled ? (
        <>
          <Row
            label="Authenticator app"
            control={<ValuePill color="var(--color-success)">On</ValuePill>}
          />
          <Row
            label="Recovery codes"
            description={
              left === 0
                ? "No recovery codes left. Make new ones in case you lose your phone."
                : `${left} left. Each works once.`
            }
            control={<Btn onClick={() => setFlow("codes")}>New codes…</Btn>}
          />
          <Row
            label="Replace authenticator"
            description="The old app stops working."
            control={<Btn onClick={() => setFlow("replace")}>Replace…</Btn>}
          />
          <Row
            label="Turn off"
            description={
              requiredBy.length > 0
                ? `${requires(requiredBy)} it, so it stays on while you're a member.`
                : "Sign in with your password alone."
            }
            control={
              <Btn onClick={() => setFlow("disable")} disabled={requiredBy.length > 0}>
                Turn off…
              </Btn>
            }
          />
        </>
      ) : (
        <>
          {locked.length > 0 && (
            <div className="py-3.5">
              <div className="rounded-md border border-[var(--color-warning)] bg-[var(--color-pill-hover)] px-3 py-2 text-xs">
                {requires(locked)} two-factor authentication. Turn it on to open{" "}
                {locked.length === 1 ? "it" : "them"}.
              </div>
            </div>
          )}
          <Row
            label="Authenticator app"
            control={<Btn onClick={() => setFlow("enable")}>Turn on…</Btn>}
          />
        </>
      )}
      {flow && <TwoFactorFlow kind={flow} onClose={closeFlow} />}
    </Section>
  );
}

function TwoFactorFlow({ kind, onClose }: { kind: FlowKind; onClose: () => void }) {
  const refresh = useCloudStore((s) => s.refresh);
  const [stage, setStage] = useState<"confirm" | "scan" | "codes">("confirm");
  const [password, setPassword] = useState("");
  const [setup, setSetup] = useState<TwoFactorSetup | null>(null);
  const [codes, setCodes] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function step(action: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (e) {
      const failure = cloudError(e);
      // A setup left too long is gone; starting it again needs the password.
      if (failure.reason === "setup_expired") setStage("confirm");
      setError(failure.message);
    } finally {
      setBusy(false);
    }
  }

  // Whatever stage it ends on, the account may have changed under the section.
  // Stable, because Modal moves focus into the dialog whenever its onClose changes.
  const close = useCallback(() => {
    void refresh();
    onClose();
  }, [refresh, onClose]);

  const confirm = (factor: SecondFactor | null) =>
    step(async () => {
      if (kind === "enable" || kind === "replace") {
        setSetup(await cloudApi.twoFactorSetup(password, factor));
        setStage("scan");
      } else if (kind === "codes" && factor) {
        setCodes(await cloudApi.twoFactorRecoveryCodes(password, factor));
        setStage("codes");
      } else if (kind === "disable" && factor) {
        await cloudApi.twoFactorDisable(password, factor);
        close();
      }
    });

  const link = (factor: SecondFactor) =>
    step(async () => {
      if (!("code" in factor)) return;
      setCodes(await cloudApi.twoFactorEnable(factor.code));
      setStage("codes");
    });

  return (
    <Modal open onClose={close} title={FLOW_TITLE[kind]} size="sm">
      <div className="flex flex-col gap-4">
        {stage === "confirm" && (
          <>
            <p className="text-sm text-[var(--color-text-muted)]">
              {kind === "enable"
                ? "Enter your password to start. You'll need an authenticator app on your phone, like 1Password, Google Authenticator, Apple Passwords or Authy."
                : "Enter your password, and a code from your authenticator app or one of your recovery codes."}
            </p>
            <input
              className={inputCls}
              type="password"
              aria-label="Password"
              autoComplete="current-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && kind === "enable" && password && confirm(null)}
            />
            {kind === "enable" ? (
              <>
                {error && <p className="text-xs text-[var(--color-danger)]">{error}</p>}
                <div>
                  <button
                    type="button"
                    onClick={() => confirm(null)}
                    disabled={busy || !password}
                    className="nd-btn nd-btn-primary"
                  >
                    {busy ? "Checking…" : "Continue"}
                  </button>
                </div>
              </>
            ) : (
              <TwoFactorCodeForm
                busy={busy}
                error={error}
                disabled={!password}
                autoFocus={false}
                submitLabel={CONFIRM_LABEL[kind]}
                onSubmit={(factor) => void confirm(factor)}
              />
            )}
          </>
        )}

        {stage === "scan" && setup && (
          <>
            <p className="text-sm text-[var(--color-text-muted)]">
              Scan this with your authenticator app, then enter the code it shows.
            </p>
            {setup.qr && <QrImage qr={setup.qr} />}
            <div className="flex flex-col gap-1">
              <span className="text-xs text-[var(--color-text-muted)]">
                Can't scan it? Enter this key instead:
              </span>
              <code className="nd-code text-sm select-all break-all">{groupKey(setup.secret)}</code>
            </div>
            <TwoFactorCodeForm
              busy={busy}
              error={error}
              allowRecovery={false}
              submitLabel={kind === "enable" ? "Turn on" : "Use this app"}
              onSubmit={(factor) => void link(factor)}
            />
          </>
        )}

        {stage === "codes" && <RecoveryCodes codes={codes} onDone={close} />}
      </div>
    </Modal>
  );
}

/** The secret in groups of four, as authenticator apps ask for it. */
function groupKey(secret: string): string {
  return (secret.match(/.{1,4}/g) ?? []).join(" ");
}

function QrImage({ qr }: { qr: { size: number; path: string } }) {
  const quiet = 4;
  const side = qr.size + quiet * 2;
  // Scanners need dark modules on a light ground, whatever the theme.
  return (
    <svg
      role="img"
      aria-label="QR code for your authenticator app"
      viewBox={`${-quiet} ${-quiet} ${side} ${side}`}
      shapeRendering="crispEdges"
      className="w-44 h-44 self-center rounded-md"
    >
      <rect x={-quiet} y={-quiet} width={side} height={side} fill="#ffffff" />
      <path d={qr.path} fill="#000000" />
    </svg>
  );
}

function RecoveryCodes({ codes, onDone }: { codes: string[]; onDone: () => void }) {
  const [note, setNote] = useState<string | null>(null);

  async function copy() {
    try {
      await navigator.clipboard.writeText(codes.join("\n"));
      setNote("Copied.");
    } catch {
      setNote("Couldn't copy. Select the codes and copy them yourself.");
    }
  }

  async function saveToFile() {
    try {
      const path = await save({
        defaultPath: "Humla recovery codes.txt",
        filters: [{ name: "Text", extensions: ["txt"] }],
      });
      if (!path) return;
      await cloudApi.saveRecoveryCodes(path, codes);
      setNote("Saved.");
    } catch (e) {
      setNote(cloudError(e).message);
    }
  }

  return (
    <>
      <p className="text-sm text-[var(--color-text-muted)]">
        Keep these somewhere safe. Each one signs you in once if you lose your phone, and this is
        the only time they're shown.
      </p>
      <ul className="grid grid-cols-2 gap-x-6 gap-y-1 font-code text-sm select-all">
        {codes.map((c) => (
          <li key={c}>{c}</li>
        ))}
      </ul>
      {note && <p className="text-xs text-[var(--color-text-muted)]">{note}</p>}
      <div className="flex flex-wrap items-center gap-2">
        <button type="button" onClick={onDone} className="nd-btn nd-btn-primary">
          Done
        </button>
        <Btn onClick={copy}>Copy</Btn>
        <Btn onClick={saveToFile}>Save…</Btn>
      </div>
    </>
  );
}
