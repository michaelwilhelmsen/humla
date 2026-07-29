import { useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { save } from "@tauri-apps/plugin-dialog";
import { X } from "lucide-react";
import { ipc, type ExportFormat, type Note } from "../lib/ipc";
import { useRecordingStore } from "../lib/store";
import { useCloudStore } from "../lib/cloud";
import { Modal } from "./settings/components/Modal";
import { Segmented } from "./settings/components/Segmented";
import { Toggle } from "./settings/components/Toggle";
import { Btn } from "./settings/components/Btn";

// Export a note's summary / transcript / typed notes as ONE combined
// Markdown or plain-text file (issue #18). The kit modal supplies only the
// chrome; this component owns the body. Confirming opens the native save
// panel (dialog plugin) and hands the chosen path to `export_note`.

// Slugify a note title into a filename stem. Unicode-letter/number aware so
// non-ASCII titles still produce a readable name; empty → "note".
function slug(title: string): string {
  const s = title
    .toLowerCase()
    .trim()
    .replace(/[^\p{L}\p{N}]+/gu, "-")
    .replace(/^-+|-+$/g, "");
  return s || "note";
}

// Cheap "does the typed body have any text" check for the checkbox
// enable/disable state. The authoritative HTML→text conversion happens
// backend; this only needs to know whether the section is worth offering.
function hasText(html: string): boolean {
  return html.replace(/<[^>]*>/g, "").replace(/&nbsp;/g, " ").trim() !== "";
}

function Check({
  checked,
  onChange,
  disabled,
  label,
  hint,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  disabled?: boolean;
  label: string;
  hint?: string;
}) {
  return (
    <label
      className={
        "flex items-center gap-2.5 py-1.5 text-sm " +
        (disabled ? "opacity-40 cursor-not-allowed" : "cursor-pointer")
      }
    >
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        className="h-4 w-4 accent-[var(--color-accent)]"
      />
      <span>{label}</span>
      {hint && <span className="text-[var(--color-text-muted)] text-xs">{hint}</span>}
    </label>
  );
}

// Contextual team hint. Manually exporting a note to hand it to someone is the
// one moment where a team workspace is the literal answer to what the user is
// doing, so this is the only place the pitch appears outside Settings. Shown
// only on Personal (someone already in a workspace doesn't need telling), and
// dismissed for good on the first ×. Sits below the action row so it can never
// compete with Export, and says nothing about price — the trial lives next to
// the button that starts one.
const HINT_KEY = "humla.teamHint.exportDismissed";

function TeamHint({ onLeave }: { onLeave: () => void }) {
  const navigate = useNavigate();
  const inWorkspace = useCloudStore((s) => s.status.current_workspace !== null);
  const [dismissed, setDismissed] = useState(
    () => localStorage.getItem(HINT_KEY) === "1",
  );

  if (inWorkspace || dismissed) return null;

  return (
    <div className="flex items-start gap-2 pt-3 border-t border-[var(--color-line)]">
      <p className="flex-1 nd-meta">
        Exporting to share it? A team workspace syncs notes to teammates
        automatically.{" "}
        <button
          onClick={() => {
            onLeave();
            navigate("/settings?tab=account");
          }}
          className="underline text-[var(--color-accent-text)] hover:no-underline"
        >
          Learn more
        </button>
      </p>
      <button
        onClick={() => {
          localStorage.setItem(HINT_KEY, "1");
          setDismissed(true);
        }}
        title="Don't show this again"
        aria-label="Dismiss team workspace hint"
        className="shrink-0 text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
      >
        <X size={13} strokeWidth={1.5} />
      </button>
    </div>
  );
}

export function ExportModal({
  note,
  open,
  onClose,
}: {
  note: Note;
  open: boolean;
  onClose: () => void;
}) {
  const pushError = useRecordingStore((s) => s.pushError);

  const hasSummary = note.summary.trim() !== "";
  const hasTranscript = note.transcript.trim() !== "";
  const hasNotes = hasText(note.body);

  const [format, setFormat] = useState<ExportFormat>("markdown");
  // Defaults: Summary + Transcript checked (when they have content); Notes off.
  const [includeSummary, setIncludeSummary] = useState(hasSummary);
  const [includeTranscript, setIncludeTranscript] = useState(hasTranscript);
  const [includeNotes, setIncludeNotes] = useState(false);
  const [includeSpeakerLabels, setIncludeSpeakerLabels] = useState(true);
  const [busy, setBusy] = useState(false);

  const nothingSelected =
    !(includeSummary && hasSummary) &&
    !(includeTranscript && hasTranscript) &&
    !(includeNotes && hasNotes);

  const filters = useMemo(
    () =>
      format === "markdown"
        ? [{ name: "Markdown", extensions: ["md", "markdown"] }]
        : [{ name: "Plain text", extensions: ["txt"] }],
    [format],
  );

  async function onExport() {
    setBusy(true);
    try {
      const ext = format === "markdown" ? "md" : "txt";
      const path = await save({
        defaultPath: `${slug(note.title)}.${ext}`,
        filters,
      });
      if (!path) return; // user cancelled the panel
      await ipc.exportNote(note.id, {
        path,
        format,
        includeSummary: includeSummary && hasSummary,
        includeTranscript: includeTranscript && hasTranscript,
        includeNotes: includeNotes && hasNotes,
        includeSpeakerLabels,
      });
      onClose();
    } catch (e) {
      pushError({ noteId: note.id, message: `Export failed: ${String(e)}` });
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal open={open} onClose={onClose} title="Export note">
      <div className="flex flex-col gap-5">
        <div className="flex flex-col gap-1">
          <div className="nd-label">Include</div>
          <Check
            checked={includeSummary && hasSummary}
            disabled={!hasSummary}
            onChange={setIncludeSummary}
            label="Summary"
            hint={hasSummary ? undefined : "empty"}
          />
          <Check
            checked={includeTranscript && hasTranscript}
            disabled={!hasTranscript}
            onChange={setIncludeTranscript}
            label="Transcript"
            hint={hasTranscript ? undefined : "empty"}
          />
          <Check
            checked={includeNotes && hasNotes}
            disabled={!hasNotes}
            onChange={setIncludeNotes}
            label="Notes"
            hint={hasNotes ? undefined : "empty"}
          />
        </div>

        <div className="flex items-center justify-between gap-4">
          <span className="text-sm">Speaker labels in transcript</span>
          <Toggle
            checked={includeSpeakerLabels}
            onChange={setIncludeSpeakerLabels}
            disabled={!(includeTranscript && hasTranscript)}
            label="Include speaker labels in transcript"
          />
        </div>

        <div className="flex items-center justify-between gap-4">
          <span className="text-sm">Format</span>
          <Segmented<ExportFormat>
            label="Export format"
            value={format}
            onChange={setFormat}
            options={[
              { value: "markdown", label: "Markdown" },
              { value: "txt", label: "Plain text" },
            ]}
          />
        </div>

        <div className="flex justify-end gap-2 pt-1">
          <Btn onClick={onClose}>Cancel</Btn>
          <Btn onClick={onExport} disabled={nothingSelected || busy}>
            {busy ? "Exporting…" : "Export…"}
          </Btn>
        </div>

        <TeamHint onLeave={onClose} />
      </div>
    </Modal>
  );
}
