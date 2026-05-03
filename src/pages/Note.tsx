import { useParams } from "react-router-dom";
import { useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import {
  Calendar,
  Circle,
  Cloud,
  FileText,
  Folder,
  Languages,
  Users,
} from "lucide-react";
import { ipc, onSummaryThinkingDelta, onSummaryContentDelta, type Note as TNote, type SummaryPrompt, type TimelineEntry } from "../lib/ipc";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useNotesStore, useRecordingStore } from "../lib/store";
import { RecordingBar } from "../components/RecordingBar";
import { SkeletonLines } from "../components/Skeleton";
import { NoteEditor } from "../components/Editor";
import { SUMMARY_PRESETS, presetLabel } from "../lib/presets";
import { LANGUAGES, languageOptionLabel } from "../lib/languages";
import { useDeveloperMode } from "../lib/useDeveloperMode";

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
  const devMode = useDeveloperMode();
  // Playback bundle: the mixed WAV path (converted to a tauri:// asset
  // URL) and the per-turn timeline driving highlight rendering. Both
  // null/empty means this note pre-dates the playback feature or its
  // bundle hasn't been written yet — we fall back to the plain
  // TranscriptEditor in that case.
  const [playbackUrl, setPlaybackUrl] = useState<string | null>(null);
  const [timeline, setTimeline] = useState<TimelineEntry[]>([]);

  useEffect(() => {
    let cancelled = false;
    ipc.getSetting("language").then((v) => {
      if (!cancelled && v) setUiLang(v);
    });
    ipc.getSetting("summary_provider").then((v) => {
      if (!cancelled && v) setGlobalProvider(v);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  // Drop any pending debounced save when the component unmounts.
  // Without this, navigating away within the 300ms patch() window leaks
  // a setTimeout that fires after unmount with the stale `next` snapshot
  // and writes it back via ipc.updateNote + upsert(next), clobbering
  // edits the user made on the next visit to the same note.
  useEffect(() => {
    return () => {
      if (saveTimer.current) window.clearTimeout(saveTimer.current);
    };
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

  const titleRef = useRef<HTMLTextAreaElement | null>(null);
  // Resolved summary provider for *this* note: per-note override beats global.
  // Used to gate the live-reasoning panel — cloud OpenAI never streams
  // thinking content, so showing the dropdown there would be a permanent
  // "waiting for the model…" placeholder that never becomes anything.
  const effectiveProvider =
    draft?.summary_provider && draft.summary_provider.length > 0
      ? draft.summary_provider
      : globalProvider || "openai";
  const isLocalProvider = effectiveProvider === "local";

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
  //
  // The cancelled flag is load-bearing under React StrictMode (dev-only):
  // effects run mount → cleanup → mount again to surface lifecycle bugs.
  // Tauri's listen() is async, so a naive `.then((u) => unsubs.push(u))`
  // races: the first cleanup runs while the Promise is still pending, so
  // unsubs is empty and the listener leaks. The second mount adds a *new*
  // listener; both stay alive and every event fires twice — which is what
  // produced the "ThinkingThinking ProcessProcess" doubling in the
  // reasoning panel. The flag-and-immediately-unsub pattern below cleans
  // up listeners that finish registering after their effect was torn down.
  useEffect(() => {
    if (!id) return;
    let cancelled = false;
    const unsubs: (() => void)[] = [];
    const claim = (u: () => void) => {
      if (cancelled) u();
      else unsubs.push(u);
    };
    onSummaryThinkingDelta((e) => {
      if (e.noteId === id) setThinkingStream((s) => s + e.delta);
    }).then(claim);
    onSummaryContentDelta((e) => {
      if (e.noteId === id) setContentStream((s) => s + e.delta);
    }).then(claim);
    return () => {
      cancelled = true;
      unsubs.forEach((u) => u());
    };
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
  const reasoningRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = reasoningRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [thinkingStream]);

  // Auto-grow the title textarea so long titles wrap onto a second line
  // instead of horizontally clipping at the right edge of the page.
  useEffect(() => {
    const el = titleRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }, [draft?.title]);

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

  // Re-fetch the playback bundle whenever the note id or recording
  // phase changes. The post-stop chain (diarize → polish) writes the
  // bundle mid-flight, so depending only on draft.id would leave the
  // player hidden until the user navigates away and back. Stable
  // recording_phase transitions: stopping → diarizing → polishing →
  // idle — by the time we land on idle, the bundle exists.
  useEffect(() => {
    if (!draft) return;
    let cancelled = false;
    (async () => {
      const [path, tl] = await Promise.all([
        ipc.notePlaybackPath(draft.id).catch(() => null),
        ipc.noteTimeline(draft.id).catch((): TimelineEntry[] => []),
      ]);
      if (cancelled) return;
      setPlaybackUrl(path ? convertFileSrc(path) : null);
      setTimeline(tl);
    })();
    return () => {
      cancelled = true;
    };
  }, [draft?.id, recPhase.phase]);

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
        <textarea
          ref={titleRef}
          value={draft.title}
          onChange={(e) => patch("title", e.target.value)}
          onKeyDown={(e) => {
            // Block Enter so the title behaves like a single-line conceptual
            // field — text still wraps when wider than the column, but the
            // user can't accidentally introduce a literal newline.
            if (e.key === "Enter") {
              e.preventDefault();
              (e.currentTarget as HTMLTextAreaElement).blur();
            }
          }}
          placeholder="New note"
          rows={1}
          className="nd-bare block text-5xl font-light tracking-[-0.02em] w-full mb-6 placeholder:text-[var(--color-text-muted)]/50 resize-none overflow-hidden focus:outline-none leading-tight"
        />

        <div className="nd-prop-table mb-10">
          <PropertyRow icon={<Calendar size={14} />} label="created">
            <span>{dateChip}</span>
          </PropertyRow>
          {(isRecording || isStarting || isPaused) && (
            <PropertyRow
              icon={<Circle size={14} fill={isPaused ? "transparent" : "currentColor"} />}
              label="status"
              accent={isRecording || isStarting ? "var(--color-accent)" : undefined}
            >
              <span style={{ color: isRecording || isStarting ? "var(--color-accent)" : undefined }}>
                {isStarting ? "Starting" : isPaused ? "Paused" : "Recording"}
              </span>
            </PropertyRow>
          )}
          <PropertyRow icon={<Folder size={14} />} label="folder">
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
          </PropertyRow>
          <PropertyRow icon={<FileText size={14} />} label="preset">
            <PresetPicker
              value={draft.summary_preset || "meeting"}
              onChange={(v) => patch("summary_preset", v)}
            />
          </PropertyRow>
          <PropertyRow icon={<Languages size={14} />} label="language">
            <LanguagePicker
              value={draft.language || uiLang}
              onChange={(v) => patch("language", v)}
            />
          </PropertyRow>
          <PropertyRow icon={<Users size={14} />} label="speakers">
            <SpeakersPicker
              value={draft.expected_speakers}
              onChange={async (n) => {
                if (!draft) return;
                const next = { ...draft, expected_speakers: n };
                setDraft(next);
                await ipc.updateNote(draft.id, { expected_speakers: n });
                upsert(next);
              }}
            />
          </PropertyRow>
          <PropertyRow icon={<Cloud size={14} />} label="summary">
            <SummaryProviderChip
              value={draft.summary_provider}
              globalDefault={globalProvider}
              onChange={patchProvider}
            />
          </PropertyRow>
        </div>

        <NoteEditor
          key={draft.id}
          initialHTML={initialBody}
          onChange={(html) => patch("body", html)}
        />

        {showSummarySection && (
          <Card className="mt-8">
            <div className="flex items-baseline gap-3 mb-4">
              <h2 className="nd-label">Summary</h2>
              {isSummarizing && hasSummary && (
                <span className="text-xs text-[var(--color-text-muted)] flex items-center gap-1.5">
                  <span
                    className="inline-block w-2 h-2 rounded-full bg-[var(--color-text-muted)] animate-pulse"
                    aria-hidden
                  />
                  Regenerating…
                </span>
              )}
            </div>
            {/* Live reasoning trace: shown only on local LLM providers
                while the model is thinking. Cloud OpenAI doesn't stream
                a thinking trace through this path (reasoning models keep
                their chain-of-thought server-side), so showing the panel
                there is just a permanent "waiting for the model…"
                placeholder. Rendered ChatGPT-style — small muted text
                directly under the header, no inner border or panel, with
                full markdown formatting since Qwen-class models emit
                their thoughts as markdown. Once the final summary lands
                the trace starts collapsed so it doesn't dominate. */}
            {isLocalProvider && (thinkingStream.length > 0 || (isSummarizing && contentStream.length === 0)) && (
              <div className="mb-6">
                <button
                  type="button"
                  onClick={() => setThinkingExpanded((v) => !v)}
                  className="flex items-center gap-1.5 text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
                >
                  <span>
                    Reasoning
                    {thinkingStream.length > 0 && ` · ${thinkingStream.length.toLocaleString()} chars`}
                    {isSummarizing && thinkingStream.length === 0 && " · waiting for the model…"}
                  </span>
                  <span aria-hidden className="inline-block w-3 text-center">{thinkingExpanded ? "▾" : "▸"}</span>
                </button>
                {thinkingExpanded && thinkingStream.length > 0 && (
                  <div
                    ref={reasoningRef}
                    className="prose-reasoning mt-2 max-h-64 overflow-y-auto"
                  >
                    <ReactMarkdown remarkPlugins={[remarkGfm]}>{thinkingStream}</ReactMarkdown>
                  </div>
                )}
              </div>
            )}
            {/* Render priority: streaming first while summarizing (so the
                user sees the model working when re-running on a note that
                already has a saved summary), then the saved summary, then
                the streaming as a first-time fallback, then a skeleton.
                Without the isSummarizing guard, hasSummary would always
                win and the streaming would be invisible behind the cached
                summary — minutes of "nothing happening" on local LLMs. */}
            {isSummarizing && contentStream.length > 0 ? (
              <div className="prose-summary text-base leading-relaxed">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{contentStream}</ReactMarkdown>
              </div>
            ) : hasSummary ? (
              <div className="prose-summary text-base leading-relaxed">
                <ReactMarkdown remarkPlugins={[remarkGfm]}>{draft.summary}</ReactMarkdown>
              </div>
            ) : contentStream.length > 0 ? (
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
                  onRename={(oldLabel, newLabel) => {
                    patch(
                      "transcript",
                      renameSpeakerInTranscript(draft.transcript, oldLabel, newLabel),
                    );
                    // Mirror the rename into the timeline so the
                    // playback view's chunk highlights pick up the new
                    // label without a re-diarize. Local state update
                    // gives an instant repaint; the backend rewrite
                    // persists the change so it survives reload.
                    setTimeline((tl) =>
                      tl.map((e) =>
                        e.label === oldLabel ? { ...e, label: newLabel } : e,
                      ),
                    );
                    ipc
                      .noteTimelineRename(draft.id, oldLabel, newLabel)
                      .catch((err) =>
                        console.error("noteTimelineRename failed", err),
                      );
                  }}
                />
                {devMode && <DiagnosticsLinks noteId={draft.id} />}
                {playbackUrl && timeline.length > 0 ? (
                  <TranscriptPlayer
                    noteId={draft.id}
                    timeline={timeline}
                    setTimeline={setTimeline}
                    playbackUrl={playbackUrl}
                    transcript={draft.transcript}
                    onChange={(v) => patch("transcript", v)}
                    disabled={isRecording || isPaused || isStarting || isStopping || isDiarizing || isPolishing}
                  />
                ) : (
                  <TranscriptEditor
                    value={draft.transcript}
                    onChange={(v) => patch("transcript", v)}
                    disabled={isRecording || isPaused || isStarting || isStopping || isDiarizing || isPolishing}
                  />
                )}
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

/// One row in the Note's properties panel: icon + label on the left,
/// child editor on the right. Hover/focus styling and the dropdown caret
/// reveal are handled by the surrounding `.nd-prop-table` CSS.
function PropertyRow({
  icon,
  label,
  children,
  accent,
}: {
  icon: React.ReactNode;
  label: string;
  children: React.ReactNode;
  accent?: string;
}) {
  return (
    <div className="nd-prop-row">
      <div className="nd-prop-label" style={accent ? { color: accent } : undefined}>
        {icon}
        <span>{label}</span>
      </div>
      <div className="nd-prop-value">{children}</div>
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
        className="bg-transparent outline-none w-40"
      />
    );
  }

  return (
    <>
      <select
        value={value ?? "__root__"}
        onChange={(e) => handleChange(e.target.value)}
      >
        <option value="__root__">No folder</option>
        {folders.map((f) => (
          <option key={f.id} value={f.id}>
            {f.name}
          </option>
        ))}
        <option value="__new__">+ New folder…</option>
      </select>
      <span aria-hidden className="nd-prop-caret">▾</span>
    </>
  );
}

function PresetPicker({
  value,
  onChange,
}: {
  value: string;
  onChange: (v: string) => void;
}) {
  const [userPrompts, setUserPrompts] = useState<SummaryPrompt[]>([]);

  useEffect(() => {
    ipc.summaryPromptsList().then(setUserPrompts).catch(() => setUserPrompts([]));
  }, []);

  // If the note's saved value points at a deleted user prompt, surface a
  // "(missing)" entry so the user notices and can re-pick. Without this
  // the dropdown would silently render the first option without changing
  // the underlying value.
  const valueIsMissingUserPrompt =
    value.startsWith("custom:") &&
    !userPrompts.some((p) => `custom:${p.id}` === value);

  return (
    <>
      <select value={value} onChange={(e) => onChange(e.target.value)}>
        {SUMMARY_PRESETS.map((p) => (
          <option key={p.value} value={p.value}>
            {presetLabel(p)}
          </option>
        ))}
        {userPrompts.length > 0 && <option disabled>──────────</option>}
        {userPrompts.map((p) => (
          <option key={p.id} value={`custom:${p.id}`}>
            {p.name}
          </option>
        ))}
        {valueIsMissingUserPrompt && (
          <option value={value}>(deleted prompt)</option>
        )}
        {value === "custom" && <option value="custom">Custom (legacy)</option>}
      </select>
      <span aria-hidden className="nd-prop-caret">▾</span>
    </>
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
    <>
      <select value={value} onChange={(e) => onChange(e.target.value)}>
        {LANGUAGES.map((l) => (
          <option key={l.value} value={l.value}>
            {languageOptionLabel(l)}
          </option>
        ))}
      </select>
      <span aria-hidden className="nd-prop-caret">▾</span>
    </>
  );
}

// Per-note speaker count hint. Sentinel value 0 = "Auto" (let the offline
// diarizer decide via VBx — default for fresh notes). Any positive integer
// pins the cluster count, which is the most reliable fix for dominant-
// speaker conversations where auto-detect collapses to 1 cluster. We expose
// 1–6 as concrete options; rare edge cases above that get truncated to 6.
//
// In remote-call mode the count is *total* including the user — the backend
// subtracts 1 for the `You:` label before passing to the diarizer.
const SPEAKER_OPTIONS: { value: number; label: string }[] = [
  { value: 0, label: "Auto" },
  { value: 1, label: "1" },
  { value: 2, label: "2" },
  { value: 3, label: "3" },
  { value: 4, label: "4" },
  { value: 5, label: "5" },
  { value: 6, label: "6" },
];

function SpeakersPicker({
  value,
  onChange,
}: {
  value: number | null;
  onChange: (n: number | null) => void;
}) {
  // Internal sentinel: 0 stands in for `null` (auto) since <select> values
  // must be strings. Convert at the boundary.
  const selected = value ?? 0;
  return (
    <>
      <select
        value={selected}
        onChange={(e) => {
          const n = parseInt(e.target.value, 10);
          onChange(n > 0 ? n : null);
        }}
        title="Pin the expected speaker count to help diarization on dominant-speaker recordings. 'Auto' lets the model decide."
      >
        {SPEAKER_OPTIONS.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      <span aria-hidden className="nd-prop-caret">▾</span>
    </>
  );
}

// Per-note summary provider override. The "auto" option (empty string) clears
// the override so the global Settings setting kicks in. When the per-note
// override differs from the global default, the chip border switches to the
// warning colour so the user can spot that "this note is set to something
// other than what Settings says" — which was previously a silent footgun.
function SummaryProviderChip({
  value,
  globalDefault,
  onChange,
}: {
  value: string;
  globalDefault: string;
  onChange: (v: string) => void;
}) {
  const globalLabel = globalDefault === "local" ? "Local" : "Cloud";
  const effective = value.length > 0 ? value : globalDefault;
  const display = effective === "local" ? "Local" : "Cloud";
  // True when the user has explicitly picked something for this note that
  // disagrees with the global Settings — typically a leftover from earlier
  // testing. Surface the override state with a subtle warning-colored dot
  // next to the value so the user can spot that "this note is set to
  // something other than what Settings says" — previously a silent footgun.
  const isOverride = value.length > 0 && value !== globalDefault;
  return (
    <>
      {isOverride && (
        <span
          aria-hidden
          className="inline-block w-1.5 h-1.5 rounded-full"
          style={{ background: "var(--color-warning)" }}
        />
      )}
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        title={
          isOverride
            ? `Per-note override active — Settings is set to ${globalLabel}. Pick "From Settings" to defer.`
            : "Choose where this note's summary runs."
        }
      >
        <option value="">{display} · From Settings</option>
        <option value="openai">Cloud (override)</option>
        <option value="local">Local (override)</option>
      </select>
      <span aria-hidden className="nd-prop-caret">▾</span>
    </>
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

  // Auto-size the textarea on every keystroke. MUST be its own effect with
  // only `value` in deps — folding focus/setSelectionRange into the same
  // effect resets the cursor to the end every time the user types one
  // character (because the effect re-runs on each value change).
  useEffect(() => {
    if (!editing) return;
    const el = taRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }, [editing, value]);

  // Focus + park cursor at end ONLY on the editing-mode transition. No
  // `value` dependency — re-running this on each keystroke is what caused
  // the cursor to jump to the end after every typed character.
  // preventScroll keeps the viewport from yanking when the textarea takes
  // focus.
  useEffect(() => {
    if (!editing) return;
    const el = taRef.current;
    if (!el) return;
    el.focus({ preventScroll: true });
    const len = el.value.length;
    el.setSelectionRange(len, len);
  }, [editing]);

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

// Playback view. Renders the audio player and the timeline-driven
// transcript with active-turn highlighting. Each turn is its own button:
// click to seek + auto-play. The audio element is the source of truth
// for currentTime; we read it via timeupdate and pick the active turn
// by binary scan (timeline is small enough that linear is also fine).
//
// Edit mode: textarea on the raw note.transcript text, same convention
// as TranscriptEditor — the timeline isn't kept in sync with edits, so
// after a manual edit the highlights might mismatch slightly until the
// next recording or re-diarize regenerates the bundle. Acceptable
// trade-off for v1; the alternative (chunk-level edit UI) is a much
// bigger refactor.
function TranscriptPlayer({
  noteId,
  timeline,
  setTimeline,
  playbackUrl,
  transcript,
  onChange,
  disabled,
}: {
  noteId: string;
  timeline: TimelineEntry[];
  setTimeline: React.Dispatch<React.SetStateAction<TimelineEntry[]>>;
  playbackUrl: string;
  transcript: string;
  onChange: (v: string) => void;
  disabled: boolean;
}) {
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const [currentMs, setCurrentMs] = useState(0);
  const [editing, setEditing] = useState(false);
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);

  const labels = useMemo(
    () => Array.from(new Set(timeline.map((t) => t.label).filter(Boolean))),
    [timeline],
  );
  const colors = useMemo(() => speakerColorMap(labels), [labels]);

  // The active turn is the latest one whose start_ms is ≤ currentMs.
  // Timeline is sorted ascending by start_ms by construction (backend
  // sorts before serialising), so a linear scan is fine — meetings
  // rarely have more than a few hundred turns.
  const activeIdx = useMemo(() => {
    let idx = -1;
    for (let i = 0; i < timeline.length; i++) {
      if (timeline[i].start_ms <= currentMs) idx = i;
      else break;
    }
    return idx;
  }, [timeline, currentMs]);

  useEffect(() => {
    if (activeIdx < 0 || !containerRef.current) return;
    const el = containerRef.current.querySelector(`[data-idx="${activeIdx}"]`);
    if (el && "scrollIntoView" in el) {
      (el as HTMLElement).scrollIntoView({ block: "nearest", behavior: "smooth" });
    }
  }, [activeIdx]);

  useEffect(() => {
    if (!editing) return;
    const el = taRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }, [editing, transcript]);

  useEffect(() => {
    if (!editing) return;
    const el = taRef.current;
    if (!el) return;
    el.focus({ preventScroll: true });
    const len = el.value.length;
    el.setSelectionRange(len, len);
  }, [editing]);

  function seek(ms: number) {
    const a = audioRef.current;
    if (!a) return;
    a.currentTime = ms / 1000;
    a.play().catch(() => {});
  }

  // Click a chunk pill to cycle to the next known speaker. The set
  // of known speakers is whatever currently appears in the timeline,
  // so the user can reach any label they've already named without
  // typing — and after a re-diarize gives them a couple of base
  // speakers, they can rename one in the chip strip and then cycle
  // chunks onto it.
  async function cycleChunkLabel(idx: number) {
    if (disabled) return;
    const labels = Array.from(new Set(timeline.map((e) => e.label).filter(Boolean)));
    if (labels.length < 2) return;
    const current = timeline[idx].label;
    const at = labels.indexOf(current);
    const next = labels[(at + 1) % labels.length] ?? labels[0];
    if (next === current) return;
    setTimeline((tl) =>
      tl.map((e, i) => (i === idx ? { ...e, label: next } : e)),
    );
    try {
      await ipc.noteTimelineSetChunkLabel(noteId, idx, next);
    } catch (err) {
      console.error("noteTimelineSetChunkLabel failed", err);
    }
  }

  const showEditor = editing && !disabled;

  return (
    <div>
      <div className="flex items-center gap-2 mb-3">
        <audio
          ref={audioRef}
          src={playbackUrl}
          controls
          preload="metadata"
          className="flex-1 h-8"
          onTimeUpdate={(e) =>
            setCurrentMs(Math.floor(e.currentTarget.currentTime * 1000))
          }
        />
        {!showEditor && !disabled && (
          <button
            type="button"
            onClick={() => setEditing(true)}
            className="nd-bare text-xs text-[var(--color-text-muted)] underline hover:text-[var(--color-text)] shrink-0"
          >
            Edit
          </button>
        )}
      </div>
      {showEditor ? (
        <textarea
          ref={taRef}
          value={transcript}
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
      ) : (
        <div
          ref={containerRef}
          className="text-sm leading-relaxed text-[var(--color-text-muted)] flex flex-col gap-1"
        >
          {timeline.map((entry, i) => {
            const isActive = i === activeIdx;
            const color = entry.label ? colors.get(entry.label) : undefined;
            const labelCount = new Set(
              timeline.map((e) => e.label).filter(Boolean),
            ).size;
            const cyclable = labelCount >= 2 && !!entry.label;
            return (
              <div
                key={i}
                data-idx={i}
                className={
                  "flex items-start gap-1 px-2 py-1 rounded transition-colors " +
                  (isActive
                    ? "bg-[var(--color-pill-hover)] text-[var(--color-text)]"
                    : "hover:bg-[var(--color-pill-hover)]")
                }
              >
                {entry.label && color && (
                  <button
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation();
                      void cycleChunkLabel(i);
                    }}
                    disabled={!cyclable || disabled}
                    title={
                      cyclable
                        ? "Click to assign this chunk to the next speaker"
                        : entry.label
                    }
                    className={
                      "nd-speaker-pill shrink-0 " +
                      (cyclable ? "cursor-pointer hover:opacity-80" : "cursor-default")
                    }
                    style={{ background: color }}
                  >
                    {entry.label}
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => seek(entry.start_ms)}
                  title="Click to play from here"
                  className="text-left flex-1 nd-bare cursor-text"
                >
                  {entry.text}
                </button>
              </div>
            );
          })}
        </div>
      )}
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

// Small "Diagnostics / Audio" link row under the speaker chips. Each
// link is hidden until the corresponding files actually exist for this
// note, so we don't dangle dead links — diagnostics only after a
// diarize run has dumped its JSON, audio only when keep_audio was on
// at recording time. Clicks open the folder in Finder via Tauri's
// shell plugin (works on both files and directories on macOS).
//
// Re-polls on every recording-phase transition so the post-stop
// chain's diagnostic write becomes visible without a page refresh:
// the diarize/retranscribe/polish phases all write files mid-flight,
// and depending only on `noteId` (which doesn't change) would leave
// the link hidden until the user navigated away and back.
function DiagnosticsLinks({ noteId }: { noteId: string }) {
  const [diagFiles, setDiagFiles] = useState<string[]>([]);
  const [audioFiles, setAudioFiles] = useState<string[]>([]);
  const [rediarizing, setRediarizing] = useState(false);
  const [rediarizeError, setRediarizeError] = useState<string | null>(null);
  const phase = useRecordingStore((s) => s.status.phase);

  useEffect(() => {
    let cancelled = false;
    ipc.noteDiagnosticsFiles(noteId)
      .then((f) => {
        if (!cancelled) setDiagFiles(f);
      })
      .catch(() => {});
    ipc.noteAudioFiles(noteId)
      .then((f) => {
        if (!cancelled) setAudioFiles(f);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [noteId, phase]);

  const hasDiag = diagFiles.length > 0;
  const hasAudio = audioFiles.length > 0;
  // Re-diarize is only meaningful when both saved audio AND original
  // chunk timings exist. Hide it otherwise — the backend would reject
  // with an explanation, but pre-emptively gating keeps the affordance
  // honest.
  const canRediarize = hasAudio && hasDiag;
  if (!hasDiag && !hasAudio) return null;

  async function openDiag() {
    const dir = await ipc.noteDiagnosticsDir(noteId);
    await ipc.openInFinder(dir);
  }
  async function openAudio() {
    const dir = await ipc.noteAudioDir(noteId);
    await ipc.openInFinder(dir);
  }
  async function rediarize() {
    setRediarizing(true);
    setRediarizeError(null);
    try {
      await ipc.rediarizeNote(noteId);
      // Refresh file lists — a new diagnostic JSON gets written.
      const next = await ipc.noteDiagnosticsFiles(noteId).catch(() => diagFiles);
      setDiagFiles(next);
    } catch (e) {
      setRediarizeError(String(e));
    } finally {
      setRediarizing(false);
    }
  }

  return (
    <div className="flex flex-col gap-1 mb-3">
      <div className="flex items-center gap-3 text-xs text-[var(--color-text-muted)]">
        {hasDiag && (
          <button
            type="button"
            onClick={openDiag}
            className="underline hover:text-[var(--color-text)]"
            title="Open diagnostics folder in Finder"
          >
            Diagnostics ({diagFiles.length})
          </button>
        )}
        {hasAudio && (
          <button
            type="button"
            onClick={openAudio}
            className="underline hover:text-[var(--color-text)]"
            title="Open retained audio folder in Finder"
          >
            Audio ({audioFiles.length})
          </button>
        )}
        {canRediarize && (
          <button
            type="button"
            onClick={rediarize}
            disabled={rediarizing}
            className="underline hover:text-[var(--color-text)] disabled:opacity-50"
            title="Re-run diarization on the saved audio with the current settings"
          >
            {rediarizing ? "Re-diarizing…" : "Re-diarize"}
          </button>
        )}
      </div>
      {rediarizeError && (
        <p className="text-xs text-red-600 dark:text-red-400 break-all">
          {rediarizeError}
        </p>
      )}
    </div>
  );
}
