import { useEffect, useState } from "react";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { Plus, Trash2 } from "lucide-react";
import { ipc, type WebhookEndpointInput } from "../../../lib/ipc";
import { Btn } from "../components/Btn";
import { Row } from "../components/Section";
import { Toggle } from "../components/Toggle";
import { inputClass, type EditableKey } from "../types";
import type { PolicyRule } from "../../../lib/orgPolicy";
import { ManagedBadge } from "../ManagedRow";

// Send to webhook (#213): rows in the Integrations section, below MCP, because
// that is what it is: a way for the user's own tools to receive their notes.
//
// The switch is an ordinary setting written at once, like MCP's. It only
// decides whether a note's ⋯ menu offers the send. The endpoint is a secret (a
// URL often carries a token, its headers usually do), so it lives in the
// Keychain and has its own Save: the backend checks all of it together, and a
// half-typed URL must never be where a note goes.
//
// Nothing here reads the Keychain until the switch is on, so a user who never
// turns it on is never asked for Keychain access on its behalf.

const DOCS_URL = "https://humla.team/docs/integrations/webhooks#payload-schema-version-1";

/** One header row. A header loaded from the saved endpoint arrives with its
 *  name only (`stored`); left empty, its value stays what was saved. */
type HeaderRow = { name: string; value: string; stored: boolean };

/** What Save sends: blank rows dropped, and a stored header the user didn't
 *  retype sent without a value, which the backend reads as "keep it". */
function endpointInput(url: string, headers: HeaderRow[]): WebhookEndpointInput {
  return {
    url: url.trim(),
    headers: headers
      .filter((h) => h.name.trim() !== "" || h.value !== "")
      .map((h) => ({
        name: h.name.trim(),
        value: h.stored && h.value === "" ? null : h.value,
      })),
  };
}

export function WebhookRows({
  s,
  update,
  managed = null,
}: {
  s: Record<EditableKey, string>;
  update: (key: EditableKey, value: string) => void;
  /** Who keeps notes on this Mac (`local_only`): only a loopback URL is accepted. */
  managed?: PolicyRule | null;
}) {
  const enabled = s.webhook_enabled === "true";
  return (
    <>
      <Row
        label="Send notes to a webhook"
        description="Adds Send to webhook… to each note’s ⋯ menu. A note is sent only when you send it."
        control={
          <Toggle
            checked={enabled}
            onChange={(next) => update("webhook_enabled", next ? "true" : "false")}
            label="Enable sending to a webhook"
          />
        }
      />
      {enabled && <EndpointForm managed={managed} />}
    </>
  );
}

function EndpointForm({ managed }: { managed: PolicyRule | null }) {
  const [loaded, setLoaded] = useState(false);
  const [url, setUrl] = useState("");
  const [headers, setHeaders] = useState<HeaderRow[]>([]);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    let cancelled = false;
    ipc
      .webhookGetEndpoint()
      .then((endpoint) => {
        if (cancelled) return;
        setUrl(endpoint.url);
        setHeaders(endpoint.headers.map((name) => ({ name, value: "", stored: true })));
        setLoaded(true);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(String(e));
        setLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  function edit(next: () => void) {
    next();
    setSaved(false);
    setError(null);
  }

  function setHeader(index: number, patch: Partial<HeaderRow>) {
    edit(() => setHeaders((rows) => rows.map((h, i) => (i === index ? { ...h, ...patch } : h))));
  }

  async function save() {
    setSaving(true);
    setError(null);
    setSaved(false);
    try {
      await ipc.webhookSetEndpoint(endpointInput(url, headers));
      // A value just typed is now the stored one, and is never shown again.
      setHeaders((rows) =>
        rows
          .filter((h) => h.name.trim() !== "" || h.value !== "")
          .map((h) => ({ name: h.name.trim(), value: "", stored: true })),
      );
      setSaved(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  }

  if (!loaded) {
    return (
      <p className="py-3.5 text-sm text-[var(--color-text-muted)]" role="status">
        Loading webhook…
      </p>
    );
  }

  return (
    <>
      <div className="py-3.5">
        <div className="flex flex-wrap items-center gap-x-2 gap-y-1 mb-1.5">
          <span className="text-sm">Webhook URL</span>
          {managed && <ManagedBadge source={managed.source} reason={managed.reason} />}
        </div>
        <input
          aria-label="Webhook URL"
          type="url"
          autoComplete="off"
          spellCheck={false}
          className={inputClass}
          placeholder={managed ? "http://localhost:8787/webhook" : "https://example.com/webhook"}
          value={url}
          disabled={saving}
          onChange={(e) => edit(() => setUrl(e.target.value))}
        />
      </div>
      <Row label="Headers">
        <p className="text-xs text-[var(--color-text-muted)] mb-2">
          Saved values aren’t shown again. Leave one empty to keep it.
        </p>
        <div className="space-y-2">
          {headers.map((header, index) => (
            <div className="flex items-center gap-2" key={index}>
              <input
                aria-label={`Header ${index + 1} name`}
                placeholder="Authorization"
                autoComplete="off"
                spellCheck={false}
                className={`${inputClass} min-w-0 flex-1`}
                value={header.name}
                disabled={saving}
                onChange={(e) => setHeader(index, { name: e.target.value })}
              />
              <input
                aria-label={`Header ${index + 1} value`}
                placeholder={header.stored ? "Saved" : "Bearer your-token"}
                type="password"
                autoComplete="off"
                className={`${inputClass} min-w-0 flex-1`}
                value={header.value}
                disabled={saving}
                onChange={(e) => setHeader(index, { value: e.target.value })}
              />
              <button
                type="button"
                className="nd-btn-icon shrink-0"
                aria-label={`Remove header ${index + 1}`}
                disabled={saving}
                onClick={() => edit(() => setHeaders((rows) => rows.filter((_, i) => i !== index)))}
              >
                <Trash2 size={15} />
              </button>
            </div>
          ))}
          <button
            type="button"
            className="nd-btn"
            disabled={saving}
            onClick={() =>
              edit(() => setHeaders((rows) => [...rows, { name: "", value: "", stored: false }]))
            }
          >
            <Plus size={14} /> Add header
          </button>
        </div>
      </Row>
      <div className="py-3.5 space-y-3">
        <p className="text-xs text-[var(--color-text-muted)]">
          The URL and headers are kept in the macOS Keychain on this Mac and never synced. Audio
          is never sent.{" "}
          <button
            type="button"
            onClick={() => void openExternal(DOCS_URL)}
            className="underline hover:text-[var(--color-text)]"
          >
            What your receiver gets
          </button>
        </p>
        {error && (
          <p role="alert" className="text-sm text-[var(--color-danger)]">
            {error}
          </p>
        )}
        <div className="flex items-center gap-3">
          <Btn disabled={saving} onClick={() => void save()}>
            {saving ? "Saving…" : "Save webhook"}
          </Btn>
          {saved && (
            <span role="status" className="text-xs text-[var(--color-text-muted)]">
              Saved
            </span>
          )}
        </div>
      </div>
    </>
  );
}
