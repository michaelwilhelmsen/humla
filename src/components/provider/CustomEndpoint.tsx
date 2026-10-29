import { useState } from "react";
import { Pencil } from "lucide-react";
import { Btn } from "../../pages/settings/components/Btn";
import { Row } from "../../pages/settings/components/Section";
import { Combobox } from "../ui/Combobox";
import { selectTriggerClass } from "../ui/Select";
import { ipc, type CustomProbe } from "../../lib/ipc";
import { useProviderKey } from "./useProviderKey";

// The user's own OpenAI-compatible endpoint (#173): one base URL and an
// optional key, shared by summaries, chat and transcription. Each surface
// shows this card beside its own model field, so an edit here is an edit for
// all three.

const fieldClass =
  "text-sm px-3 py-1.5 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors";

type Report = { listing: CustomProbe; completion: CustomProbe } | { error: string };

const PROBE_COLOR: Record<CustomProbe["outcome"], string> = {
  ok: "text-[var(--color-success)]",
  failed: "text-[var(--color-danger)]",
  skipped: "text-[var(--color-text-muted)]",
};

function ProbeLine({ label, probe }: { label: string; probe: CustomProbe }) {
  return (
    <p className={`text-xs mt-1 break-words ${PROBE_COLOR[probe.outcome]}`}>
      <span className="font-medium">{label}:</span> {probe.message}
    </p>
  );
}

export function CustomEndpointCard({
  baseUrl,
  onBaseUrlChange,
  testModel,
}: {
  baseUrl: string;
  onBaseUrlChange: (url: string) => void;
  /** The model of the surface showing the card, which the Test's completion
   *  uses. Left out where the surface asks for no completions (transcription),
   *  and then the Test reports the listing alone. */
  testModel?: string;
}) {
  const key = useProviderKey("custom");
  const [testing, setTesting] = useState(false);
  const [report, setReport] = useState<Report | null>(null);

  async function test() {
    setTesting(true);
    setReport(null);
    try {
      setReport(await ipc.customEndpointTest(testModel));
    } catch (e) {
      setReport({ error: String(e) });
    } finally {
      setTesting(false);
    }
  }

  return (
    <div className="py-3.5 flex flex-col gap-3">
      <div className="flex items-center justify-between gap-6">
        <div className="min-w-0">
          <div className="text-sm">Base URL</div>
          <p className="text-xs text-[var(--color-text-muted)] mt-0.5">
            Used exactly as typed: Humla adds <code>/chat/completions</code>, <code>/models</code>{" "}
            or <code>/audio/transcriptions</code> to it. Shared by summaries, chat and transcription.
          </p>
        </div>
        <input
          type="url"
          value={baseUrl}
          onChange={(e) => onBaseUrlChange(e.target.value)}
          placeholder="https://openrouter.ai/api/v1"
          aria-label="Custom endpoint base URL"
          spellCheck={false}
          className={`w-64 shrink-0 ${fieldClass}`}
        />
      </div>

      <div>
        <div className="text-sm">API key</div>
        <p className="text-xs text-[var(--color-text-muted)] mt-0.5 mb-2">
          Optional, and kept in the Keychain. Sent as a bearer token; an endpoint that takes no
          key gets no Authorization header.
        </p>
        <div className="flex gap-2">
          <input
            type="password"
            value={key.draft}
            onChange={(e) => key.setDraft(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && key.save()}
            placeholder={key.hasKey ? "•••••••• stored" : "Optional"}
            aria-label="Custom endpoint API key"
            className={`flex-1 min-w-0 ${fieldClass}`}
          />
          <Btn onClick={key.save} disabled={!key.draft.trim()}>
            Save
          </Btn>
          {key.hasKey && (
            <Btn onClick={key.remove} aria-label="Remove key">
              Remove
            </Btn>
          )}
          <Btn onClick={test} disabled={!baseUrl.trim() || testing}>
            {testing ? "Testing…" : "Test"}
          </Btn>
        </div>
        {key.result?.ok === false && (
          <p className="text-xs text-[var(--color-danger)] mt-2 break-words">{key.result.message}</p>
        )}
        {report && "error" in report && (
          <p className="text-xs text-[var(--color-danger)] mt-2 break-words">{report.error}</p>
        )}
        {report && "listing" in report && (
          <div className="mt-1">
            <ProbeLine label="Model list" probe={report.listing} />
            {testModel !== undefined && <ProbeLine label="Completion" probe={report.completion} />}
          </div>
        )}
      </div>
    </div>
  );
}

// A gateway can list hundreds of models; only this many are offered at once,
// and typing narrows them.
const MAX_LISTED = 100;

/** A model on the custom endpoint: any typed id, with what the endpoint lists
 *  offered while typing. The listing is fetched when editing starts, so it
 *  always reflects the URL and key as they are now. */
export function CustomModelField({
  value,
  onChange,
  description,
}: {
  value: string;
  onChange: (model: string) => void;
  description: string;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const [listed, setListed] = useState<string[]>([]);

  function start() {
    setDraft(value);
    setListed([]);
    setEditing(true);
    ipc
      .customListModels()
      .then(setListed)
      .catch(() => setListed([]));
  }

  function commit(next: string) {
    setEditing(false);
    const model = next.trim();
    if (model !== value) onChange(model);
  }

  // Untouched, the field shows the whole listing; once typed into, the matches.
  const query = draft.trim().toLowerCase();
  const matches = draft === value ? listed : listed.filter((m) => m.toLowerCase().includes(query));
  const options = editing ? matches.slice(0, MAX_LISTED).map((m) => ({ value: m })) : [];

  return (
    <Row
      label="Model"
      description={description}
      control={
        editing ? (
          // Escape ends the edit only; the Settings dialog closes on any Escape
          // that reaches the window.
          <div onKeyDown={(e) => e.key === "Escape" && e.stopPropagation()}>
            <Combobox
              autoFocus
              value={draft}
              onValueChange={setDraft}
              options={options}
              onCommit={commit}
              onCancel={() => setEditing(false)}
              aria-label="Custom endpoint model"
              listLabel="Models the endpoint lists"
              spellCheck={false}
              className={`w-64 ${fieldClass}`}
            />
          </div>
        ) : (
          <button type="button" onClick={start} className={selectTriggerClass}>
            <span className={`truncate ${value ? "" : "text-[var(--color-text-muted)]"}`}>
              {value || "Type a model…"}
            </span>
            <Pencil size={12} strokeWidth={1.8} className="shrink-0 text-[var(--color-text-muted)]" aria-hidden />
          </button>
        )
      }
    />
  );
}
