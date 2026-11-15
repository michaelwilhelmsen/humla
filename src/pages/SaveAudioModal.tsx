import { useEffect, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { ipc, type AudioFormat, type Note, type TakeAudio } from "../lib/ipc";
import { useRecordingStore } from "../lib/store";
import { formatDurationWords, sessionTitle } from "../lib/sessions";
import { slug } from "./ExportModal";
import { Modal } from "./settings/components/Modal";
import { Segmented } from "./settings/components/Segmented";
import { Btn } from "./settings/components/Btn";

// ⋯ → Save audio… (#224). A note's audio lives under
// `recordings/<note id>/<take id>/playback.m4a`, which nobody can be expected to
// find, so this saves it wherever the user picks: every take joined in order
// into one file, since what they want is "the meeting", not a folder of takes.
//
// Its own sheet rather than a checkbox in Export: Export writes one document
// through one save panel, and audio is a second file of a different kind.
//
// It lists what it would save before it asks where, take by take, because a
// take's audio can be gone (retention off, "Delete stored audio…", a
// teammate's take that never downloaded) and a file quietly missing two of
// three takes reads as a bug. A list says which one is missing at a glance,
// where a sentence had to be read and counted.
const FORMATS: Record<AudioFormat, { label: string; filter: string }> = {
  m4a: { label: "M4A", filter: "M4A audio" },
  wav: { label: "WAV", filter: "WAV audio" },
};

function TakeRow({ label, value, missing, total }: {
  label: string;
  value: string;
  missing?: boolean;
  total?: boolean;
}) {
  return (
    <li
      className={
        "flex items-baseline justify-between gap-4 py-1.5 text-sm" +
        (total ? " font-medium border-t border-[var(--color-line)] mt-1 pt-2.5" : "")
      }
    >
      <span className={missing ? "text-[var(--color-text-muted)]" : undefined}>{label}</span>
      <span
        className={"tabular-nums " + (missing ? "text-[var(--color-text-muted)]" : "")}
        title={missing ? "Not stored on this Mac, so the file leaves it out" : undefined}
      >
        {value}
      </span>
    </li>
  );
}

export function SaveAudioModal({
  note,
  open,
  onClose,
  keepAudio,
}: {
  note: Note;
  open: boolean;
  onClose: () => void;
  /** The device's retention setting, for why a note has no audio. */
  keepAudio: boolean;
}) {
  const pushError = useRecordingStore((s) => s.pushError);
  const [takes, setTakes] = useState<TakeAudio[] | null>(null);
  const [format, setFormat] = useState<AudioFormat>("m4a");
  const [busy, setBusy] = useState(false);

  // Asked on every open: a take can finish or lose its audio in between.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setTakes(null);
    ipc.noteAudioExportInfo(note.id).then(
      (t) => !cancelled && setTakes(t),
      () => !cancelled && setTakes([]),
    );
    return () => {
      cancelled = true;
    };
  }, [open, note.id]);

  const saved = (takes ?? []).filter((t) => t.durationMs !== null);
  const hasAudio = saved.length > 0;
  const totalMs = saved.reduce((sum, t) => sum + (t.durationMs ?? 0), 0);

  async function onSave() {
    setBusy(true);
    try {
      const path = await save({
        defaultPath: `${slug(note.title)}.${format}`,
        filters: [{ name: FORMATS[format].filter, extensions: [format] }],
      });
      if (!path) return; // user cancelled the panel
      await ipc.exportNoteAudio(note.id, { path, format });
      onClose();
    } catch (e) {
      pushError({ noteId: note.id, message: String(e), kind: "export" });
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal open={open} onClose={onClose} title="Save audio" size="sm">
      {/* Nothing until the answer is in: the sheet moves focus to its first
          control on open, and a Close standing in for the row would keep it
          once Save… and the format appear. */}
      {takes && (
        <div className="flex flex-col gap-4">
          {takes.length === 0 ? (
            <p className="text-sm">Nothing has been recorded on this note.</p>
          ) : (
            <ul aria-label="Recordings" className="flex flex-col">
              {takes.map((t) => (
                <TakeRow
                  key={t.index}
                  label={sessionTitle(t.index)}
                  value={t.durationMs === null ? "Missing" : formatDurationWords(t.durationMs)}
                  missing={t.durationMs === null}
                />
              ))}
              {/* What the one file will run to — only worth a row once there
                  is more than one take to add up. */}
              {hasAudio && takes.length > 1 && (
                <TakeRow label="Total" value={formatDurationWords(totalMs)} total />
              )}
            </ul>
          )}

          {takes.length > 0 && !hasAudio && !keepAudio && (
            <p className="text-xs text-[var(--color-text-muted)]">
              Turn on Keep recorded audio in Settings → Recording to keep the audio of your next
              recording.
            </p>
          )}

          {hasAudio && (
            <div className="flex items-center justify-between gap-4">
              <span className="text-sm">Format</span>
              <Segmented<AudioFormat>
                label="Audio format"
                value={format}
                onChange={setFormat}
                options={(Object.keys(FORMATS) as AudioFormat[]).map((value) => ({
                  value,
                  label: FORMATS[value].label,
                }))}
              />
            </div>
          )}

          <div className="flex justify-end gap-2 pt-1">
            <Btn onClick={onClose}>{hasAudio ? "Cancel" : "Close"}</Btn>
            {hasAudio && (
              <Btn onClick={onSave} disabled={busy}>
                {busy ? "Saving…" : "Save…"}
              </Btn>
            )}
          </div>
        </div>
      )}
    </Modal>
  );
}
