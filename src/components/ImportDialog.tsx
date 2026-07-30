import { useEffect, useState } from "react";
import { FileAudio } from "lucide-react";
import { Modal } from "../pages/settings/components/Modal";
import { Select } from "./ui/Select";
import { LANGUAGES, languageOptionLabel } from "../lib/languages";
import { ipc } from "../lib/ipc";

// Config step shown between picking an audio file and starting the import.
// Import runs the file through the transcription pipeline exactly once — the
// language and speaker-count choices can't be retro-applied to chunks that
// have already been transcribed. So we collect them up front instead of
// seeding from the global default (which produced wrong-language transcripts
// when the file's language didn't match the app default).

const LANGUAGE_OPTIONS = LANGUAGES.map((l) => ({
  value: l.value,
  label: languageOptionLabel(l),
}));

// Mirrors SpeakersPicker in Note.tsx: sentinel "0" = Auto (let the offline
// diarizer decide), 1–6 pins the cluster count.
const SPEAKER_OPTIONS = [
  { value: "0", label: "Auto" },
  { value: "1", label: "1 speaker" },
  { value: "2", label: "2 speakers" },
  { value: "3", label: "3 speakers" },
  { value: "4", label: "4 speakers" },
  { value: "5", label: "5 speakers" },
  { value: "6", label: "6 speakers" },
];

function basename(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

export function ImportDialog({
  path,
  onCancel,
  onConfirm,
}: {
  path: string;
  onCancel: () => void;
  onConfirm: (language: string, expectedSpeakers: number | null) => Promise<void>;
}) {
  // Preseed language from the global default as a convenience; the user is
  // free to override it — that's the whole point of this dialog.
  const [language, setLanguage] = useState<string>(LANGUAGES[0]?.value ?? "en");
  const [speakers, setSpeakers] = useState<string>("0");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    ipc
      .getSetting("language")
      .then((v) => {
        if (alive && v && LANGUAGES.some((l) => l.value === v)) setLanguage(v);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, []);

  async function confirm() {
    if (busy) return;
    setBusy(true);
    setError(null);
    const expected = parseInt(speakers, 10);
    try {
      await onConfirm(language, expected > 0 ? expected : null);
      // Success unmounts this dialog (caller navigates away); no cleanup needed.
    } catch (e) {
      setBusy(false);
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  return (
    <Modal open onClose={busy ? () => {} : onCancel} title="Import audio">
      <div className="flex flex-col gap-5">
        <div className="flex items-center gap-2 text-sm text-[var(--color-text-muted)]">
          <FileAudio size={16} strokeWidth={1.6} className="shrink-0" />
          <span className="truncate" title={path}>
            {basename(path)}
          </span>
        </div>

        <p className="text-sm text-[var(--color-text-muted)]">
          Pick the language spoken in this file. Transcription runs once and
          can’t be re-run per language afterward.
        </p>

        <div className="flex items-center justify-between gap-4">
          <label htmlFor="import-language" className="text-sm">Language</label>
          <Select id="import-language" value={language} onChange={setLanguage} options={LANGUAGE_OPTIONS} />
        </div>

        <div className="flex items-center justify-between gap-4">
          <label htmlFor="import-speakers" className="text-sm">Speakers</label>
          <Select id="import-speakers" value={speakers} onChange={setSpeakers} options={SPEAKER_OPTIONS} />
        </div>

        {error && (
          <p className="text-sm text-[var(--color-accent-text)]">{error}</p>
        )}

        <div className="flex justify-end gap-2 pt-1">
          <button
            type="button"
            onClick={onCancel}
            disabled={busy}
            className="text-sm px-3 py-1.5 rounded-md border border-[var(--color-line-visible)] hover:bg-[var(--color-pill-hover)] transition-colors disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={confirm}
            disabled={busy}
            className="text-sm px-3 py-1.5 rounded-md bg-[var(--color-text)] text-[var(--color-canvas)] hover:opacity-90 transition-opacity disabled:opacity-50"
          >
            {busy ? "Importing…" : "Import"}
          </button>
        </div>
      </div>
    </Modal>
  );
}
