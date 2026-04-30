import { useParams } from "react-router-dom";
import { useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { ipc, onSummaryThinkingDelta, onSummaryContentDelta, type Note as TNote } from "../lib/ipc";
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
  const [globalProvider, setGlobalProvider] = useState<string>("openai");
  // Live reasoning + content streamed from the local LLM. Cleared each time a
  // new summarize starts and again when the summary lands. Scoped by note id
  // so a delta from a different note's run doesn't leak into this view.
  const [thinkingStream, setThinkingStream] = useState<string>("");
  const [contentStream, setContentStream] = useState<string>("");
  const [thinkingExpanded, setThinkingExpanded] = useState<boolean>(true);
  const saveTimer = useRef<number | null>(null);

  useEffect(() => {
    ipc.getSetting("language").then((v) => v && setUiLang(v));
    ipc.getSetting("summary_provider").then((v) => v && setGlobalProvider(v));
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

  // Subscribe once per note id. Only append a delta if it belongs to this
  // note — defensive in case multiple summary calls are interleaved.
  useEffect(() => {
    if (!id) return;
    const unsubs: (() => void)[] = [];
    onSummaryThinkingDelta((e) => {
      if (e.noteId === id) setThinkingStream((s) => s + e.delta);
    }).then((u) => unsubs.push(u));
    onSummaryContentDelta((e) => {
      if (e.noteId === id) setContentStream((s) => s + e.delta);
    }).then((u) => unsubs.push(u));
    return () => unsubs.forEach((u) => u());
  }, [id]);

  // Reset the streams when a new summarize starts (phase transitions to
  // summarizing) and again when it ends — keeps stale text from sticking.
  useEffect(() => {
    if (isSummarizing) {
      setThinkingStream("");
      setContentStream("");
      setThinkingExpanded(true);
    }
  }, [isSummarizing]);

  // Once the saved summary lands, fold the reasoning panel away. Users can
  // still re-expand it from the header to inspect the trace.
  const summaryText = draft?.summary ?? "";
  useEffect(() => {
    if (summaryText.trim().length > 0) setThinkingExpanded(false);
  }, [summaryText]);

  // Auto-scroll the reasoning panel to the latest chunk so users see the
  // model thinking live without having to chase the scrollbar themselves.
  const reasoningRef = useRef<HTMLPreElement | null>(null);
  useEffect(() => {
    const el = reasoningRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [thinkingStream]);

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

  // Empty-string-as-null for summary_provider. "" clears the override and
  // lets the global setting kick in; "openai" / "local" sets it explicitly.
  function patchProvider(value: string) {
    if (!draft) return;
    const next = { ...draft, summary_provider: value };
    setDraft(next);
    if (saveTimer.current) window.clearTimeout(saveTimer.current);
    saveTimer.current = window.setTimeout(async () => {
      await ipc.updateNote(next.id, { summary_provider: value });
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

        <div className="flex flex-col gap-2 mb-10">
          <div className="flex items-center gap-3 flex-wrap">
            <span className="nd-chip">{dateChip}</span>
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
          <div className="flex items-center gap-3 flex-wrap">
            <PresetPicker
              value={draft.summary_preset || "meeting"}
              lang={uiLang}
              onChange={(v) => patch("summary_preset", v)}
            />
            <LanguagePicker
              value={draft.language || uiLang}
              onChange={(v) => patch("language", v)}
            />
            <SummaryProviderChip
              value={draft.summary_provider}
              globalDefault={globalProvider}
              onChange={patchProvider}
            />
          </div>
        </div>

        <NoteEditor
          key={draft.id}
          initialHTML={initialBody}
          onChange={(html) => patch("body", html)}
        />

        {showSummarySection && (
          <Card className="mt-8">
            <h2 className="nd-label mb-4">Summary</h2>
            {/* Live reasoning trace: shown while the model is thinking.
                Auto-scrolls; collapsible via the header. Once the final
                summary lands the panel stays available but starts
                collapsed so it doesn't dominate. */}
            {(thinkingStream.length > 0 || (isSummarizing && contentStream.length === 0)) && (
              <div className="mb-4 rounded-md border border-[var(--color-line)] bg-[var(--color-surface-raised)]">
                <button
                  type="button"
                  onClick={() => setThinkingExpanded((v) => !v)}
                  className="w-full flex items-center justify-between px-3 py-2 text-xs text-[var(--color-text-muted)]"
                >
                  <span>
                    {thinkingExpanded ? "▾" : "▸"} Reasoning
                    {thinkingStream.length > 0 && ` · ${thinkingStream.length.toLocaleString()} chars`}
                    {isSummarizing && thinkingStream.length === 0 && " · waiting for the model…"}
                  </span>
                </button>
                {thinkingExpanded && thinkingStream.length > 0 && (
                  <pre
                    ref={reasoningRef}
                    className="px-3 pb-3 text-xs leading-relaxed font-mono text-[var(--color-text-muted)] whitespace-pre-wrap break-words max-h-64 overflow-y-auto"
                  >
                    {thinkingStream}
                  </pre>
                )}
              </div>
            )}
            {hasSummary ? (
              <div className="prose-summary text-base leading-relaxed">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{draft.summary}</ReactMarkdown>
              </div>
            ) : contentStream.length > 0 ? (
              // Content is streaming in — render markdown live as it arrives.
              <div className="prose-summary text-base leading-relaxed">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{contentStream}</ReactMarkdown>
              </div>
            ) : (
              <SkeletonLines lines={5} />
            )}
          </Card>
        )}

        {showTranscriptSection && (
          <Card className="mt-4 focus-within:border-[var(--color-text)] transition-colors">
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
                <SpeakerLabels
                  transcript={draft.transcript}
                  onRename={(oldLabel, newLabel) =>
                    patch("transcript", renameSpeakerInTranscript(draft.transcript, oldLabel, newLabel))
                  }
                />
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

// Per-note summary provider override. The "auto" option (empty string) clears
// the override so the global setting kicks in. Display label tracks the
// effective provider so the user can tell at a glance whether *this* note
// will go to OpenAI or stay on-device.
function SummaryProviderChip({
  value,
  globalDefault,
  onChange,
}: {
  value: string;
  globalDefault: string;
  onChange: (v: string) => void;
}) {
  const effective = value.length > 0 ? value : globalDefault;
  const display = effective === "local" ? "Local" : "Cloud";
  const suffix = value.length > 0 ? "" : " · auto";
  return (
    <label className="nd-chip cursor-pointer pr-2">
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="bg-transparent appearance-none outline-none cursor-pointer uppercase tracking-[0.08em]"
        style={{ fontFamily: "var(--font-mono)" }}
      >
        <option value="">Summary: {display}{suffix}</option>
        <option value="openai">Summary: Cloud</option>
        <option value="local">Summary: Local</option>
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

// Stable colour palette for speaker pills. Pulled from the design tokens so
// dark mode adapts automatically. The order is intentional: blue first
// because --color-interactive is the most "neutral" decorative colour we
// have; red (--color-accent) last because the design language reserves it
// for "interrupt only" — a five-speaker meeting will still get red, but
// for the common 2–3 speaker case it stays out of the way.
const SPEAKER_COLORS = [
  "var(--color-interactive)",
  "var(--color-success)",
  "var(--color-warning)",
  "var(--color-accent)",
];

function speakerColorMap(labels: string[]): Map<string, string> {
  const map = new Map<string, string>();
  labels.forEach((label, i) => {
    map.set(label, SPEAKER_COLORS[i % SPEAKER_COLORS.length]);
  });
  return map;
}

// Parse the transcript for speaker turn prefixes — any line starting with
// `<label>: ` (label can be any non-colon text) is treated as a speaker
// turn. Returns labels in first-encounter order, deduplicated.
function extractSpeakerLabels(transcript: string): string[] {
  const seen = new Set<string>();
  const result: string[] = [];
  for (const rawLine of transcript.split("\n")) {
    const line = rawLine.trimStart();
    const match = line.match(/^([^:]{1,40}):\s/);
    if (match) {
      const label = match[1].trim();
      if (!seen.has(label)) {
        seen.add(label);
        result.push(label);
      }
    }
  }
  return result;
}

// Rewrite the transcript so every "<oldLabel>: " line start becomes
// "<newLabel>: ". Anchored to line starts via a multi-line regex; bare
// occurrences of the label inside text are left alone. Escapes regex
// metacharacters in oldLabel so renaming to/from values like "Speaker 1?"
// doesn't break.
function renameSpeakerInTranscript(transcript: string, oldLabel: string, newLabel: string): string {
  if (oldLabel === newLabel) return transcript;
  const escaped = oldLabel.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const re = new RegExp(`^(\\s*)${escaped}: `, "gm");
  return transcript.replace(re, `$1${newLabel}: `);
}

function SpeakerLabels({
  transcript,
  onRename,
}: {
  transcript: string;
  onRename: (oldLabel: string, newLabel: string) => void;
}) {
  const labels = useMemo(() => extractSpeakerLabels(transcript), [transcript]);
  const colors = useMemo(() => speakerColorMap(labels), [labels]);
  // Only render the strip when there are 2+ unique speakers — solo
  // monologues don't need management UI.
  if (labels.length < 2) return null;
  return (
    <div className="flex flex-wrap gap-2 mb-4">
      {labels.map((label) => (
        <SpeakerChip
          key={label}
          label={label}
          color={colors.get(label) ?? SPEAKER_COLORS[0]}
          onRename={(next) => onRename(label, next)}
        />
      ))}
    </div>
  );
}

function SpeakerChip({
  label,
  color,
  onRename,
}: {
  label: string;
  color: string;
  onRename: (next: string) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(label);
  const inputRef = useRef<HTMLInputElement | null>(null);

  // Snap the draft back to the canonical label whenever the underlying
  // label changes (e.g. polish replaced the transcript and our label was
  // re-derived).
  useEffect(() => {
    setDraft(label);
  }, [label]);

  useEffect(() => {
    if (editing) inputRef.current?.select();
  }, [editing]);

  function commit() {
    setEditing(false);
    const trimmed = draft.trim();
    if (!trimmed || trimmed === label) {
      setDraft(label);
      return;
    }
    onRename(trimmed);
  }

  if (editing) {
    // size= sets the visible character width; with monospace font this
    // makes the input width track the typed text. Floor at 3 so the
    // pill never collapses to nothing while the user is mid-edit.
    return (
      <input
        ref={inputRef}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        size={Math.max(draft.length, 3)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commit();
          } else if (e.key === "Escape") {
            e.preventDefault();
            setDraft(label);
            setEditing(false);
          }
        }}
        className="nd-speaker-pill cursor-text outline-none"
        style={{ background: color }}
      />
    );
  }

  return (
    <button
      type="button"
      onClick={() => setEditing(true)}
      title="Click to rename — applies to every turn from this speaker"
      className="nd-speaker-pill cursor-pointer hover:opacity-90"
      style={{ background: color }}
    >
      {label}
    </button>
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
  const [editing, setEditing] = useState(false);
  const taRef = useRef<HTMLTextAreaElement | null>(null);

  // Auto-size the textarea while in edit mode.
  useEffect(() => {
    if (!editing) return;
    const el = taRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
    // preventScroll so the browser doesn't yank the viewport when the
    // textarea takes focus — the user clicked here, they don't want to
    // be teleported. Cursor parks at end (could be smarter — map click
    // position to character index — but good enough for v1).
    el.focus({ preventScroll: true });
    const len = el.value.length;
    el.setSelectionRange(len, len);
  }, [editing, value]);

  // Force the styled-view path while a recording is in flight — we don't
  // want the user typing into a transcript that the backend is about to
  // replace via diarize/polish.
  const showEditor = editing && !disabled;

  if (showEditor) {
    return (
      <textarea
        ref={taRef}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={() => setEditing(false)}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            setEditing(false);
          }
        }}
        className="nd-bare w-full resize-none text-sm leading-relaxed text-[var(--color-text-muted)] focus:outline-none"
      />
    );
  }

  return (
    <TranscriptView
      transcript={value}
      onClick={() => {
        if (!disabled) setEditing(true);
      }}
      disabled={disabled}
    />
  );
}

// Styled transcript reader. Renders the transcript as a single
// pre-wrapped block so its rendered height matches the textarea exactly —
// no paragraph margins to add 8 px per line, which was causing the page
// to jump up when the card shrank into edit mode.
//
// Speaker labels at line starts ("Label: rest") get replaced with an
// inline coloured pill plus the rest; everything else flows as plain
// text with native \n line breaks preserved by white-space: pre-wrap.
// The whole view is click-to-edit unless `disabled` (recording in
// flight).
function TranscriptView({
  transcript,
  onClick,
  disabled,
}: {
  transcript: string;
  onClick: () => void;
  disabled: boolean;
}) {
  const labels = useMemo(() => extractSpeakerLabels(transcript), [transcript]);
  const colors = useMemo(() => speakerColorMap(labels), [labels]);
  const lines = transcript.split("\n");

  return (
    <div
      onClick={onClick}
      title={disabled ? "Editing is paused while recording" : "Click to edit"}
      className={
        "text-sm leading-relaxed text-[var(--color-text-muted)] whitespace-pre-wrap " +
        (disabled ? "cursor-default" : "cursor-text")
      }
    >
      {lines.map((line, i) => {
        const prefix = i > 0 ? "\n" : "";
        const m = line.match(/^(\s*)([^:]{1,40}):\s(.*)$/);
        if (m) {
          const [, lead, label, rest] = m;
          const color = colors.get(label.trim());
          if (color) {
            return (
              <span key={i}>
                {prefix}
                {lead}
                <span className="nd-speaker-pill mr-2" style={{ background: color }}>
                  {label}
                </span>
                {rest}
              </span>
            );
          }
        }
        return (
          <span key={i}>
            {prefix}
            {line}
          </span>
        );
      })}
    </div>
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
