import { useParams } from "react-router-dom";
import { useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ipc, type Note as TNote } from "../lib/ipc";
import { useNotesStore, useRecordingStore } from "../lib/store";
import { RecordingBar } from "../components/RecordingBar";
import { SkeletonLines } from "../components/Skeleton";
import { NoteEditor } from "../components/Editor";
import { SUMMARY_PRESETS, presetLabelForLang } from "../lib/presets";

// Mirrors the dropdown in Settings. Kept inline for now; if a third place
// needs it we can extract to a shared module.
const LANGS: { value: string; label: string }[] = [
  { value: "auto", label: "Auto" },
  { value: "no", label: "Norsk" },
  { value: "en", label: "English" },
  { value: "sv", label: "Svenska" },
  { value: "da", label: "Dansk" },
];

function formatDateChip(ts: number) {
  const d = new Date(ts);
  const today = new Date(); today.setHours(0, 0, 0, 0);
  const start = new Date(d); start.setHours(0, 0, 0, 0);
  const diff = (today.getTime() - start.getTime()) / 86400000;
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  return d.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

export function Note() {
  const { id } = useParams<{ id: string }>();
  const upsert = useNotesStore((s) => s.upsertLocal);
  const note = useNotesStore((s) => s.notes.find((n) => n.id === id));
  const [draft, setDraft] = useState<TNote | null>(null);
  const [uiLang, setUiLang] = useState<string>("no");
  const saveTimer = useRef<number | null>(null);

  useEffect(() => {
    ipc.getSetting("language").then((v) => v && setUiLang(v));
  }, []);

  useEffect(() => {
    let cancelled = false;
    if (id) {
      ipc.getNote(id).then((n) => {
        if (!cancelled) {
          setDraft(n);
          upsert(n);
        }
      });
    }
    return () => { cancelled = true; };
  }, [id, upsert]);

  const recPhase = useRecordingStore((s) => s.status);
  const isThisNoteActive = !!draft && recPhase.noteId === draft.id;
  const isRecording = isThisNoteActive && recPhase.phase === "recording";
  const isPaused = isThisNoteActive && recPhase.phase === "paused";
  const isStarting = isThisNoteActive && recPhase.phase === "starting";
  const isStopping = isThisNoteActive && recPhase.phase === "stopping";
  const isDiarizing = isThisNoteActive && recPhase.phase === "diarizing";
  const isPolishing = isThisNoteActive && recPhase.phase === "polishing";
  const isSummarizing = isThisNoteActive && recPhase.phase === "summarizing";

  // Always pull summary updates from the store. Pull transcript updates only
  // while a recording, diarization, or polish is in flight — otherwise our
  // debounced save round-trips through the store and clobbers in-progress
  // edits. Diarization and polish both replace the transcript wholesale,
  // so we want the editor to reflect those updates immediately.
  const allowTranscriptSync =
    isRecording || isPaused || isStarting || isStopping || isDiarizing || isPolishing;
  useEffect(() => {
    if (!note || !draft || note.id !== draft.id) return;
    setDraft((d) => {
      if (!d) return d;
      const nextSummary = note.summary;
      const nextTranscript = allowTranscriptSync ? note.transcript : d.transcript;
      if (d.summary === nextSummary && d.transcript === nextTranscript) return d;
      return { ...d, summary: nextSummary, transcript: nextTranscript };
    });
  }, [note?.transcript, note?.summary, allowTranscriptSync]);

  function patch(field: "title" | "body" | "transcript" | "summary_preset" | "language", value: string) {
    if (!draft) return;
    const next = { ...draft, [field]: value };
    setDraft(next);
    if (saveTimer.current) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(async () => {
      await ipc.updateNote(next.id, { [field]: value });
      upsert(next);
    }, 300);
  }

  // Existing notes have plain-text bodies; wrap them in <p> tags so Tiptap
  // renders sensible paragraphs on first load. New bodies are stored as HTML.
  const initialBody = useMemo(() => {
    if (!draft) return "";
    const b = draft.body;
    if (!b) return "";
    if (b.trimStart().startsWith("<")) return b;
    return b
      .split(/\n{2,}/)
      .map((para) => `<p>${escapeHtml(para).replace(/\n/g, "<br />")}</p>`)
      .join("");
  }, [draft?.id]);

  const dateChip = useMemo(() => (draft ? formatDateChip(draft.created_at) : "Today"), [draft]);

  if (!draft) return null;

  const hasSummary = draft.summary.trim().length > 0;
  const hasTranscript = draft.transcript.trim().length > 0;
  const showTranscriptSection = hasTranscript || isRecording || isPaused || isStarting || isStopping;
  const showSummarySection = hasSummary || isSummarizing;

  return (
    <div className="h-full flex flex-col">
      <div data-tauri-drag-region className="h-10 shrink-0" />
      <div className="flex-1 overflow-y-auto px-12 pb-32 max-w-3xl mx-auto w-full">
        <input
          value={draft.title}
          onChange={(e) => patch("title", e.target.value)}
          placeholder="New note"
          className="text-5xl font-light tracking-[-0.02em] w-full mb-6 placeholder:text-[var(--color-text-muted)]/50"
        />

        <div className="flex items-center gap-3 mb-10">
          <span className="nd-chip">{dateChip}</span>
          <PresetPicker
            value={draft.summary_preset || "meeting"}
            lang={uiLang}
            onChange={(v) => patch("summary_preset", v)}
          />
          <LanguagePicker
            value={draft.language || uiLang}
            onChange={(v) => patch("language", v)}
          />
          <FolderPicker
            value={draft.folder_id}
            onChange={async (folderId) => {
              if (!draft) return;
              const next = { ...draft, folder_id: folderId };
              setDraft(next);
              await ipc.moveNote(draft.id, folderId);
              upsert(next);
            }}
          />
          {(isRecording || isStarting) && (
            <span className="nd-chip" style={{ color: "var(--color-accent)", borderColor: "var(--color-accent)" }}>
              <span className="rec-dot inline-block w-1.5 h-1.5 rounded-full" style={{ background: "var(--color-accent)" }} />
              {isStarting ? "Starting" : "Recording"}
            </span>
          )}
          {isPaused && (
            <span className="nd-chip">
              <span className="inline-block w-1.5 h-1.5 rounded-full bg-[var(--color-text-muted)]" />
              Paused
            </span>
          )}
        </div>

        <NoteEditor
          key={draft.id}
          initialHTML={initialBody}
          onChange={(html) => patch("body", html)}
        />

        {showSummarySection && (
          <Card className="mt-8">
            <h2 className="nd-label mb-4">Summary</h2>
            {hasSummary ? (
              <div className="prose-summary text-base leading-relaxed">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{draft.summary}</ReactMarkdown>
              </div>
            ) : (
              <SkeletonLines lines={5} />
            )}
          </Card>
        )}

        {showTranscriptSection && (
          <Card className="mt-4">
            <h2 className="nd-label mb-4 flex items-center gap-3">
              <span>Transcript</span>
              {isRecording && (
                <span className="inline-flex items-end gap-0.5 h-2.5">
                  {[0, 1, 2, 3].map((i) => (
                    <span
                      key={i}
                      className="rec-bar inline-block w-0.5 rounded-full h-full"
                      style={{ animationDelay: `${i * 130}ms`, background: "var(--color-accent)" }}
                    />
                  ))}
                </span>
              )}
            </h2>
            {hasTranscript ? (
              <>
                <TranscriptEditor
                  value={draft.transcript}
                  onChange={(v) => patch("transcript", v)}
                  disabled={isRecording || isPaused || isStarting || isStopping || isDiarizing || isPolishing}
                />
                {isRecording && <SkeletonLines lines={2} className="mt-3" />}
              </>
            ) : (
              <SkeletonLines lines={4} />
            )}
          </Card>
        )}
      </div>

      <RecordingBar noteId={draft.id} />
    </div>
  );
}

function FolderPicker({
  value,
  onChange,
}: {
  value: string | null;
  onChange: (folderId: string | null) => void;
}) {
  const folders = useNotesStore((s) => s.folders);
  const upsertFolder = useNotesStore((s) => s.upsertFolder);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");

  async function handleChange(raw: string) {
    if (raw === "__new__") {
      setCreating(true);
      setName("");
      return;
    }
    onChange(raw === "__root__" ? null : raw);
  }

  async function commit() {
    const trimmed = name.trim();
    if (!trimmed) {
      setCreating(false);
      setName("");
      return;
    }
    try {
      const folder = await ipc.createFolder(trimmed);
      upsertFolder(folder);
      onChange(folder.id);
    } finally {
      setCreating(false);
      setName("");
    }
  }

  if (creating) {
    return (
      <span className="nd-chip" style={{ borderColor: "var(--color-text-muted)" }}>
        <input
          autoFocus
          value={name}
          onChange={(e) => setName(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
            else if (e.key === "Escape") { setCreating(false); setName(""); }
          }}
          onBlur={commit}
          placeholder="Folder name"
          className="bg-transparent outline-none w-32 uppercase tracking-[0.08em] text-[11px]"
          style={{ fontFamily: "var(--font-mono)" }}
        />
      </span>
    );
  }

  return (
    <label className="nd-chip cursor-pointer pr-2">
      <select
        value={value ?? "__root__"}
        onChange={(e) => handleChange(e.target.value)}
        className="bg-transparent appearance-none outline-none cursor-pointer uppercase tracking-[0.08em]"
        style={{ fontFamily: "var(--font-mono)" }}
      >
        <option value="__root__">No folder</option>
        {folders.map((f) => (
          <option key={f.id} value={f.id}>
            {f.name}
          </option>
        ))}
        <option value="__new__">+ New folder…</option>
      </select>
      <span aria-hidden style={{ color: "var(--color-text-muted)" }}>▾</span>
    </label>
  );
}

function PresetPicker({
  value,
  lang,
  onChange,
}: {
  value: string;
  lang: string;
  onChange: (v: string) => void;
}) {
  return (
    <label className="nd-chip cursor-pointer pr-2">
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="bg-transparent appearance-none outline-none cursor-pointer uppercase tracking-[0.08em]"
        style={{ fontFamily: "var(--font-mono)" }}
      >
        {SUMMARY_PRESETS.map((p) => (
          <option key={p.value} value={p.value}>
            {presetLabelForLang(p, lang)}
          </option>
        ))}
        <option value="custom">{lang === "no" ? "Egendefinert" : "Custom"}</option>
      </select>
      <span aria-hidden style={{ color: "var(--color-text-muted)" }}>▾</span>
    </label>
  );
}

function LanguagePicker({
  value,
  onChange,
}: {
  value: string;
  onChange: (v: string) => void;
}) {
  return (
    <label className="nd-chip cursor-pointer pr-2">
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="bg-transparent appearance-none outline-none cursor-pointer uppercase tracking-[0.08em]"
        style={{ fontFamily: "var(--font-mono)" }}
      >
        {LANGS.map((l) => (
          <option key={l.value} value={l.value}>
            {l.label}
          </option>
        ))}
      </select>
      <span aria-hidden style={{ color: "var(--color-text-muted)" }}>▾</span>
    </label>
  );
}

function Card({ children, className = "" }: { children: React.ReactNode; className?: string }) {
  return (
    <section
      className={
        "rounded-xl bg-[var(--color-surface)] border border-[var(--color-line)] " +
        "px-6 py-5 " +
        className
      }
    >
      {children}
    </section>
  );
}

function TranscriptEditor({
  value,
  onChange,
  disabled,
}: {
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  const ref = useRef<HTMLTextAreaElement | null>(null);

  // Auto-size to content so the textarea grows like a div.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }, [value]);

  return (
    <textarea
      ref={ref}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      disabled={disabled}
      title={disabled ? "Editing is paused while recording" : undefined}
      className="nd-bare w-full resize-none text-sm leading-relaxed text-[var(--color-text-muted)] focus:outline-none disabled:cursor-default"
    />
  );
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}
