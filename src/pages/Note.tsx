import { Link, useNavigate, useParams } from "react-router-dom";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  Calendar,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronUp,
  Circle,
  Cloud,
  Copy,
  FileText,
  Folder,
  Languages,
  MoreHorizontal,
  RefreshCw,
  Users,
  X,
} from "lucide-react";
import { ipc, onSummaryThinkingDelta, onSummaryContentDelta, type Note as TNote, type SummaryPrompt, type TimelineEntry } from "../lib/ipc";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useNotesStore, useRecordingStore } from "../lib/store";
import { RecordingBar } from "../components/RecordingBar";
import { SkeletonLines } from "../components/Skeleton";
import { NoteEditor } from "../components/Editor";
import { ContextMenu, ContextMenuItem } from "../components/ContextMenu";
import { SUMMARY_PRESETS, presetLabel } from "../lib/presets";
import { LANGUAGES, languageOptionLabel } from "../lib/languages";
import { useDeveloperMode } from "../lib/useDeveloperMode";

// Memoized Markdown renderer. ReactMarkdown's parse step is O(N) over
// the source string and we paint summaries that can hit 10K+ chars on
// long meetings; without memoization, every parent re-render (each
// body keystroke, each summary delta, each recording tick) re-parses
// the same string. Wrapping in memo + a stable `source` prop turns
// that into a single parse per actual content change.
const Markdown = memo(function Markdown({ source }: { source: string }) {
  return <ReactMarkdown remarkPlugins={[remarkGfm]}>{source}</ReactMarkdown>;
});

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
  const [transcriptExpanded, setTranscriptExpanded] = useState<boolean>(false);
  const [summaryExpanded, setSummaryExpanded] = useState<boolean>(false);
  const saveTimer = useRef<number | null>(null);
  const devMode = useDeveloperMode();
  // Playback bundle: the mixed WAV path (converted to a tauri:// asset
  // URL) and the per-turn timeline driving highlight rendering. Both
  // null/empty means this note pre-dates the playback feature or its
  // bundle hasn't been written yet — we fall back to the plain
  // TranscriptEditor in that case.
  const [playbackUrl, setPlaybackUrl] = useState<string | null>(null);
  const [timeline, setTimeline] = useState<TimelineEntry[]>([]);

  // Pending field changes accumulated across the debounce window. The
  // single saveTimer used to capture only one field's value per cycle —
  // editing title then body within 300 ms would clear the title's
  // setTimeout and never persist it. This object collects every field
  // touched since the last flush; when the timer fires it's sent as one
  // partial update and cleared.
  const pendingChanges = useRef<Partial<TNote>>({});
  // Mirror of the latest draft so the unmount flush can read it without
  // capturing a stale snapshot.
  const draftRef = useRef<TNote | null>(null);
  draftRef.current = draft;

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

  // On unmount, flush any pending edits that haven't fired their timer
  // yet. Fire-and-forget — the Tauri invoke promise survives the React
  // teardown. Without this, navigating away within the 300 ms window
  // loses the user's last edit.
  useEffect(() => {
    return () => {
      if (saveTimer.current) {
        window.clearTimeout(saveTimer.current);
        saveTimer.current = null;
      }
      const changes = pendingChanges.current;
      pendingChanges.current = {};
      const d = draftRef.current;
      if (!d) return;
      const keys = Object.keys(changes);
      if (keys.length === 0) return;
      void ipc.updateNote(d.id, changes as Parameters<typeof ipc.updateNote>[1]);
      upsert({ ...d, ...changes });
    };
  }, [upsert]);

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
  // Per-note summary state lives on its own channel (`summary_status`)
  // so summarising one note can't clobber another note's recording
  // state in the shared `recording_status` slot.
  const isSummarizing = useRecordingStore((s) => !!draft && !!s.summarizing[draft.id]);
  const isThisNoteActive = !!draft && recPhase.noteId === draft.id;
  const isRecording = isThisNoteActive && recPhase.phase === "recording";
  const isPaused = isThisNoteActive && recPhase.phase === "paused";
  const isStarting = isThisNoteActive && recPhase.phase === "starting";
  const isStopping = isThisNoteActive && recPhase.phase === "stopping";
  const isDiarizing = isThisNoteActive && recPhase.phase === "diarizing";

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
  // while a recording or diarization is in flight — otherwise our debounced
  // save round-trips through the store and clobbers in-progress edits.
  // Diarization replaces the transcript wholesale, so we want the editor to
  // reflect that update immediately.
  const allowTranscriptSync =
    isRecording || isPaused || isStarting || isStopping || isDiarizing;
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
  // phase changes. The post-stop diarize step writes the bundle, so
  // depending only on draft.id would leave the player hidden until
  // the user navigates away and back. Stable recording_phase
  // transitions: stopping → diarizing → idle — by the time we land
  // on idle, the bundle exists.
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

  // patch / patchProvider intentionally read from `draftRef.current`
  // rather than the `draft` closure so they can stay stable across
  // renders — keeps the React.memo on TranscriptEditor / TranscriptView
  // / TranscriptPlayer effective (otherwise a fresh function ref would
  // bust the memo on every parent render).
  const patch = useCallback(
    (field: "title" | "body" | "transcript" | "summary_preset" | "language", value: string) => {
      const cur = draftRef.current;
      if (!cur) return;
      const next = { ...cur, [field]: value };
      setDraft(next);
      pendingChanges.current = { ...pendingChanges.current, [field]: value };
      if (saveTimer.current) window.clearTimeout(saveTimer.current);
      saveTimer.current = window.setTimeout(async () => {
        saveTimer.current = null;
        const changes = pendingChanges.current;
        pendingChanges.current = {};
        if (Object.keys(changes).length === 0) return;
        await ipc.updateNote(next.id, changes as Parameters<typeof ipc.updateNote>[1]);
        const latest = draftRef.current ?? next;
        upsert({ ...latest, ...changes });
      }, 300);
    },
    [upsert],
  );

  // Empty-string-as-null for summary_provider. "" clears the override and
  // lets the global setting kick in; "openai" / "local" sets it explicitly.
  const patchProvider = useCallback(
    (value: string) => {
      const cur = draftRef.current;
      if (!cur) return;
      const next = { ...cur, summary_provider: value };
      setDraft(next);
      pendingChanges.current = { ...pendingChanges.current, summary_provider: value };
      if (saveTimer.current) window.clearTimeout(saveTimer.current);
      saveTimer.current = window.setTimeout(async () => {
        saveTimer.current = null;
        const changes = pendingChanges.current;
        pendingChanges.current = {};
        if (Object.keys(changes).length === 0) return;
        await ipc.updateNote(next.id, changes as Parameters<typeof ipc.updateNote>[1]);
        const latest = draftRef.current ?? next;
        upsert({ ...latest, ...changes });
      }, 300);
    },
    [upsert],
  );

  // Stable callbacks for the memoized transcript components. Without
  // these, fresh arrow refs every parent render would bust React.memo
  // and re-render the whole transcript on every keystroke elsewhere.
  const onTranscriptChange = useCallback((v: string) => patch("transcript", v), [patch]);

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
  // Live-feed alignment: while a recording is in flight, pin the
  // collapsed transcript card to its bottom so newly transcribed
  // chunks stay visible. After stop / on a saved note the user is
  // reading from the top, so flip back to top alignment.
  const transcriptLive = isRecording || isPaused || isStopping || isDiarizing;

  return (
    <div className="h-full flex flex-col">
      <div data-tauri-drag-region className="h-10 shrink-0" />
      {/* Two-layer scroll: the outer div is the full-width viewport that
          owns the scrollbar (so it sits flush with the right edge of
          the window). The inner div carries the centered max-w-3xl
          content column. Splitting them is the only way to get a
          window-edge scrollbar without making the content full-width. */}
      <div className="flex-1 overflow-y-auto">
        <div className="max-w-3xl mx-auto w-full px-12 pb-32">
        <NoteHeader noteId={draft.id} folderId={draft.folder_id} summary={draft.summary} body={draft.body} />
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
          className="nd-bare block text-5xl font-serif tracking-tight w-full mb-6 placeholder:text-[var(--color-text-muted)]/50 resize-none overflow-hidden focus:outline-none leading-tight"
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
            <CollapsibleHeader
              expanded={summaryExpanded}
              onToggle={() => setSummaryExpanded((v) => !v)}
              label="summary"
              actions={
                hasSummary ? (
                  <CopyButton
                    label="Summary"
                    // Prefer the saved summary; if a regen is mid-stream and
                    // the saved version is stale, copy reads the cached
                    // value the user can see (the streaming view replaces
                    // it visually but `draft.summary` is the source of
                    // truth until commit).
                    getText={() => draft.summary}
                  />
                ) : undefined
              }
            >
              <span>Summary</span>
              {isSummarizing && hasSummary && (
                <span className="text-xs text-[var(--color-text-muted)] flex items-center gap-1.5 normal-case tracking-normal">
                  <span
                    className="inline-block w-2 h-2 rounded-full bg-[var(--color-text-muted)] animate-pulse"
                    aria-hidden
                  />
                  Regenerating…
                </span>
              )}
            </CollapsibleHeader>
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
                    <Markdown source={thinkingStream} />
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
            <CollapsibleScroll expanded={summaryExpanded} bottomAligned={false}>
              {isSummarizing && contentStream.length > 0 ? (
                <div className="prose-summary text-base leading-relaxed">
                  <Markdown source={contentStream} />
                </div>
              ) : hasSummary ? (
                <div className="prose-summary text-base leading-relaxed">
                  <Markdown source={draft.summary} />
                </div>
              ) : contentStream.length > 0 ? (
                <div className="prose-summary text-base leading-relaxed">
                  <Markdown source={contentStream} />
                </div>
              ) : (
                <SkeletonLines lines={5} />
              )}
            </CollapsibleScroll>
          </Card>
        )}

        {showTranscriptSection && (
          <Card className="mt-4 focus-within:border-[var(--color-text)] transition-colors">
            <CollapsibleHeader
              expanded={transcriptExpanded}
              onToggle={() => setTranscriptExpanded((v) => !v)}
              label="transcript"
            >
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
            </CollapsibleHeader>
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
                    onChange={onTranscriptChange}
                    disabled={isRecording || isPaused || isStarting || isStopping || isDiarizing}
                    expanded={transcriptExpanded}
                    bottomAligned={transcriptLive}
                  />
                ) : (
                  <TranscriptEditor
                    value={draft.transcript}
                    onChange={onTranscriptChange}
                    disabled={isRecording || isPaused || isStarting || isStopping || isDiarizing}
                    expanded={transcriptExpanded}
                    bottomAligned={transcriptLive}
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

function stripHtml(html: string): string {
  if (!html) return "";
  const tmp = document.createElement("div");
  tmp.innerHTML = html;
  return (tmp.textContent || "").trim();
}

function NoteHeader({
  noteId,
  folderId,
  summary,
  body,
}: {
  noteId: string;
  folderId: string | null;
  summary: string;
  body: string;
}) {
  const navigate = useNavigate();
  const folders = useNotesStore((s) => s.folders);
  const removeLocal = useNotesStore((s) => s.removeLocal);
  const [menuPos, setMenuPos] = useState<{ x: number; y: number } | null>(null);
  const [copied, setCopied] = useState(false);

  const folder = folderId ? folders.find((f) => f.id === folderId) : null;
  const backTo = folder ? `/folder/${folder.id}` : "/";
  const backLabel = folder ? folder.name : "Home";
  const canCopy = !!(summary?.trim() || stripHtml(body));

  async function onCopy() {
    const text = summary?.trim() || stripHtml(body);
    if (!text) return;
    await navigator.clipboard.writeText(text);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1200);
  }

  async function onResummarize() {
    await ipc.summarizeNote(noteId);
  }

  async function onDelete() {
    setMenuPos(null);
    await ipc.deleteNote(noteId);
    removeLocal(noteId);
    navigate(backTo);
  }

  function openMenu(e: React.MouseEvent<HTMLButtonElement>) {
    const rect = e.currentTarget.getBoundingClientRect();
    // Anchor under the button, right-aligned so the menu doesn't run
    // off the edge on narrow viewports.
    setMenuPos({ x: rect.right - 160, y: rect.bottom + 4 });
  }

  return (
    <div className="flex items-center justify-between pt-2 mb-10">
      <Link
        to={backTo}
        className="inline-flex items-center gap-1.5 pl-1.5 pr-3 py-1 rounded-md text-sm text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-sidebar-active)] transition-colors"
      >
        <ChevronLeft size={14} strokeWidth={1.5} />
        <span className="truncate max-w-[200px]">{backLabel}</span>
      </Link>
      <div className="flex items-center gap-1">
        <IconAction onClick={onCopy} disabled={!canCopy} title={copied ? "Copied" : "Copy summary"}>
          {copied ? <Check size={16} strokeWidth={1.5} /> : <Copy size={16} strokeWidth={1.5} />}
        </IconAction>
        <IconAction onClick={onResummarize} title="Re-summarize">
          <RefreshCw size={16} strokeWidth={1.5} />
        </IconAction>
        <IconAction onClick={openMenu} title="More">
          <MoreHorizontal size={16} strokeWidth={1.5} />
        </IconAction>
      </div>
      {menuPos && (
        <ContextMenu x={menuPos.x} y={menuPos.y} onClose={() => setMenuPos(null)}>
          <ContextMenuItem onClick={onDelete} danger>
            Delete note
          </ContextMenuItem>
        </ContextMenu>
      )}
    </div>
  );
}

function IconAction({
  onClick,
  disabled,
  title,
  children,
}: {
  onClick: (e: React.MouseEvent<HTMLButtonElement>) => void;
  disabled?: boolean;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      title={title}
      aria-label={title}
      className="w-8 h-8 flex items-center justify-center rounded-md text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-sidebar-active)] disabled:opacity-30 disabled:cursor-not-allowed transition-colors"
    >
      {children}
    </button>
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

// Max height applied to a collapsed transcript or summary card so a
// long meeting doesn't push the rest of the page off screen. ~14rem
// is roughly 7 lines of body text on the default zoom — half of the
// previous 28rem cap, deliberately compact so a long meeting stays
// in a fixed footprint and the rest of the note (properties, body
// editor) stays visible. Click "expand" for the full read.
const COLLAPSED_MAX_HEIGHT = "14rem";

// Wraps a long content area in a scrollable region capped at
// `COLLAPSED_MAX_HEIGHT`. When `expanded` is true, no cap. When
// `bottomAligned` is true and content is shorter than the cap, content
// pins to the bottom (transcript live-feed UX); the effect also
// scroll-resets to the bottom whenever `bottomAlignKey` changes, so
// new chunks landing in the transcript stay visible.
//
// Intentionally not used in edit mode: the auto-resizing textarea
// inside `TranscriptEditor` already grows with its content, and a
// scroll cap on top of that would fight the user's typing.
function CollapsibleScroll({
  expanded,
  bottomAligned,
  bottomAlignKey,
  children,
}: {
  expanded: boolean;
  bottomAligned: boolean;
  bottomAlignKey?: string | number;
  children: React.ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (expanded || !bottomAligned) return;
    const el = ref.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [expanded, bottomAligned, bottomAlignKey]);
  if (expanded) return <>{children}</>;
  return (
    <div
      ref={ref}
      className="overflow-y-auto flex flex-col"
      style={{
        maxHeight: COLLAPSED_MAX_HEIGHT,
        justifyContent: bottomAligned ? "flex-end" : "flex-start",
      }}
    >
      {children}
    </div>
  );
}

// Card header that doubles as the expand/collapse trigger — the full
// width of the title bar is the click target so the hit area can't be
// missed. The chevron is styled as a light-gray rounded box on hover
// (matches the sidebar collapse button) so the affordance reads as a
// real button even though the click surface is the whole row.
function CollapsibleHeader({
  expanded,
  onToggle,
  label,
  children,
  actions,
}: {
  expanded: boolean;
  onToggle: () => void;
  label: string;
  children: React.ReactNode;
  // Optional trailing-actions slot rendered between the title content and
  // the chevron. Use this for buttons that act on the card's content
  // (e.g. copy summary). The slot is a `<div>`, not a button — pass real
  // <button>s as children. Nested-button avoidance is why this row uses
  // role=button on a <div> rather than a true <button> element.
  actions?: React.ReactNode;
}) {
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onToggle}
      onKeyDown={(e) => {
        // Only fire for keys hitting the header itself, not actions
        // nested inside (which handle their own Enter/Space natively).
        if (e.target !== e.currentTarget) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onToggle();
        }
      }}
      title={expanded ? `Collapse ${label}` : `Expand ${label}`}
      className="group nd-label mb-4 flex w-full items-center gap-3 cursor-pointer text-left"
    >
      {children}
      {actions && (
        <div className="peer ml-auto flex items-center">
          {actions}
        </div>
      )}
      <span
        className={
          (actions ? "" : "ml-auto ") +
          // peer-hover overrides suppress the row's group-hover styling
          // when the copy/actions slot is the actual hover target —
          // otherwise both buttons light up at once.
          "p-1.5 rounded-md text-[var(--color-text-muted)] transition-colors group-hover:bg-[var(--color-pill-hover)] group-hover:text-[var(--color-text)] peer-hover:!bg-transparent peer-hover:!text-[var(--color-text-muted)]"
        }
        aria-hidden
      >
        {expanded
          ? <ChevronUp size={16} strokeWidth={1.5} />
          : <ChevronDown size={16} strokeWidth={1.5} />}
      </span>
    </div>
  );
}

// Small copy-to-clipboard button rendered in the Summary card header.
// 1.5s "Copied" feedback via a Check icon swap. stopPropagation keeps
// the click from toggling the surrounding header row's collapse state.
function CopyButton({ getText, label }: { getText: () => string; label: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      onClick={async (e) => {
        e.stopPropagation();
        const text = getText();
        if (!text) return;
        try {
          await navigator.clipboard.writeText(text);
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1500);
        } catch (err) {
          console.warn("[note] clipboard write failed:", err);
        }
      }}
      title={copied ? `${label} copied` : `Copy ${label}`}
      aria-label={copied ? `${label} copied` : `Copy ${label}`}
      className="p-1.5 rounded-md text-[var(--color-text-muted)] hover:bg-[var(--color-pill-hover)] hover:text-[var(--color-text)] transition-colors"
    >
      {copied
        ? <Check size={16} strokeWidth={1.5} />
        : <Copy size={16} strokeWidth={1.5} />}
    </button>
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
  // label changes (e.g. diarize replaced the transcript and our label
  // was re-derived).
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

const TranscriptEditor = memo(function TranscriptEditor({
  value,
  onChange,
  disabled,
  expanded,
  bottomAligned,
}: {
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
  expanded: boolean;
  bottomAligned: boolean;
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
  // replace via diarize.
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

  // TranscriptView owns its own scroll container so the virtualizer can
  // measure visible items. Bypass CollapsibleScroll here — its
  // bottomAligned + maxHeight role is taken over by TranscriptView's
  // built-in scroller.
  return (
    <TranscriptView
      transcript={value}
      onClick={() => {
        if (!disabled) setEditing(true);
      }}
      disabled={disabled}
      expanded={expanded}
      bottomAligned={bottomAligned}
    />
  );
});

// Styled transcript reader. Each line is its own block so we can hang
// a coloured speaker dot in the left gutter (absolute-positioned at
// `left: -14px` from the line's edge, outside the text flow). The
// label prefix that the textarea shows as raw text ("Speaker 1: ") is
// rendered inside the line as transparent — keeps the wrap identical
// to the textarea so flipping into edit mode doesn't jolt the page
// height.
//
// The whole view is click-to-edit unless `disabled` (recording in
// flight). The dot's click bubbles up to enter edit mode too — its
// own purpose is purely visual / a hover affordance, since the rename
// UI lives in the chip strip above.
// Parse each transcript line once per transcript change. With long
// recordings (~3-5k lines), running the regex inside render on every
// parent re-render is a measurable bottleneck. Cache the parsed
// structure keyed by the transcript string instead.
type ParsedTranscriptLine =
  | { kind: "speaker"; lead: string; label: string; trimmedLabel: string; rest: string }
  | { kind: "plain"; text: string };

function parseTranscriptLines(transcript: string): ParsedTranscriptLine[] {
  return transcript.split("\n").map((line) => {
    const m = line.match(/^(\s*)([^:]{1,40}):\s(.*)$/);
    if (m) {
      const [, lead, label, rest] = m;
      return { kind: "speaker", lead, label, trimmedLabel: label.trim(), rest };
    }
    return { kind: "plain", text: line };
  });
}

const TranscriptView = memo(function TranscriptView({
  transcript,
  onClick,
  disabled,
  expanded,
  bottomAligned,
}: {
  transcript: string;
  onClick: () => void;
  disabled: boolean;
  expanded: boolean;
  bottomAligned: boolean;
}) {
  const labels = useMemo(() => extractSpeakerLabels(transcript), [transcript]);
  const colors = useMemo(() => speakerColorMap(labels), [labels]);
  const lines = useMemo(() => parseTranscriptLines(transcript), [transcript]);

  // Virtualize the line list. With long meetings the DOM grows to 3-5k
  // line nodes; even when memoized, the browser still spends layout +
  // paint time per scroll frame proportional to that node count. Render
  // only the visible window + a small buffer instead.
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const virtualizer = useVirtualizer({
    count: lines.length,
    getScrollElement: () => scrollRef.current,
    // Slightly higher than the rendered line-height so the first paint
    // is roughly correct; measureElement corrects after mount.
    estimateSize: () => 24,
    overscan: 12,
  });

  // Live recording: pin to the latest line so newly transcribed chunks
  // stay visible without manual scrolling. Equivalent to the old
  // `bottomAligned` flex-end trick, but expressed as a scrollToIndex.
  useEffect(() => {
    if (!bottomAligned || lines.length === 0) return;
    virtualizer.scrollToIndex(lines.length - 1, { align: "end" });
  }, [bottomAligned, lines.length, virtualizer]);

  return (
    <div
      ref={scrollRef}
      onClick={onClick}
      title={disabled ? "Editing is paused while recording" : "Click to edit"}
      className={
        "text-sm leading-relaxed text-[var(--color-text-muted)] overflow-y-auto " +
        (disabled ? "cursor-default" : "cursor-text")
      }
      style={{ maxHeight: expanded ? "70vh" : "14rem" }}
    >
      <div
        style={{
          height: virtualizer.getTotalSize(),
          position: "relative",
          width: "100%",
        }}
      >
        {virtualizer.getVirtualItems().map((vrow) => {
          const line = lines[vrow.index];
          let content: React.ReactNode;
          if (line.kind === "speaker") {
            const color = colors.get(line.trimmedLabel);
            if (color) {
              content = (
                <div className="relative whitespace-pre-wrap">
                  <span
                    className="nd-speaker-dot"
                    style={{ background: color }}
                    title={line.trimmedLabel}
                    aria-label={`Speaker: ${line.trimmedLabel}`}
                  />
                  <span aria-hidden className="opacity-0 select-none">
                    {line.lead}
                    {line.label}:{" "}
                  </span>
                  {line.rest || " "}
                </div>
              );
            } else {
              content = (
                <div className="whitespace-pre-wrap">
                  {`${line.lead}${line.label}: ${line.rest}` || " "}
                </div>
              );
            }
          } else {
            content = (
              <div className="whitespace-pre-wrap">
                {line.text || " "}
              </div>
            );
          }
          return (
            <div
              key={vrow.key}
              data-index={vrow.index}
              ref={virtualizer.measureElement}
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                transform: `translateY(${vrow.start}px)`,
              }}
            >
              {content}
            </div>
          );
        })}
      </div>
    </div>
  );
});

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
const TranscriptPlayer = memo(function TranscriptPlayer({
  noteId,
  timeline,
  setTimeline,
  playbackUrl,
  transcript,
  onChange,
  disabled,
  expanded,
  bottomAligned,
}: {
  noteId: string;
  timeline: TimelineEntry[];
  setTimeline: React.Dispatch<React.SetStateAction<TimelineEntry[]>>;
  playbackUrl: string;
  transcript: string;
  onChange: (v: string) => void;
  disabled: boolean;
  expanded: boolean;
  bottomAligned: boolean;
}) {
  const audioRef = useRef<HTMLAudioElement | null>(null);
  // The two derived states from playback position: which chunks
  // currently bracket currentTime (mic + sys can overlap, so this is a
  // set, not a single index) and which word inside each active chunk
  // is currently sounding. We deliberately do NOT store currentMs in
  // state: the rAF tick polls audio.currentTime and only calls
  // setState when one of these crosses a boundary. This bounds
  // re-render frequency to "transitions per second" (~5–10 Hz on
  // normal speech) instead of the rAF tick rate (60 Hz), so hundreds
  // of word DOM nodes don't get re-walked every frame.
  const [activeIdxs, setActiveIdxs] = useState<number[]>([]);
  const [activeWordByIdx, setActiveWordByIdx] = useState<Record<number, number>>({});
  // The chunk we follow with scrollIntoView. Picking the
  // most-recently-entered active chunk matches reading flow during
  // overlap: when a new line lights up, the viewport eases toward it
  // without losing the prior line's highlight.
  const [scrollAnchorIdx, setScrollAnchorIdx] = useState(-1);
  const [editing, setEditing] = useState(false);
  const taRef = useRef<HTMLTextAreaElement | null>(null);
  const containerRef = useRef<HTMLDivElement | null>(null);
  // Held in a ref so the rAF tick can read the latest timeline
  // without restarting. Updated on every timeline change.
  const timelineRef = useRef(timeline);
  timelineRef.current = timeline;

  const labels = useMemo(
    () => Array.from(new Set(timeline.map((t) => t.label).filter(Boolean))),
    [timeline],
  );
  const colors = useMemo(() => speakerColorMap(labels), [labels]);

  // Collapse consecutive same-speaker timeline entries into a single
  // rendered "turn". The DB transcript already merges them — the
  // playback view used to render one row per timeline entry, which
  // produced fragments like "Duer sjef? Er" / "sjefen?" / "Askep ..."
  // as three separate blue bullets even though they're one Speaker 1
  // paragraph in the saved transcript. Grouping at render time keeps
  // per-chunk audio anchors (each constituent chunk's words stay
  // distinct for karaoke highlight and click-to-seek) while showing
  // one bullet per speaker turn.
  //
  // `indices` references back into `timeline` so the per-chunk IPCs
  // (label cycle, delete) still operate on the underlying chunks.
  // `wordCountByChunk` lets the active-word highlight map an
  // (active chunk index, active word index in that chunk) pair to a
  // single position in the flattened `words` array.
  const groups = useMemo(() => {
    type Group = {
      label: string;
      indices: number[];
      startMs: number;
      endMs: number;
      text: string;
      words: Array<{ text: string; start_ms: number; end_ms: number }>;
      wordCountByChunk: number[];
    };
    const out: Group[] = [];
    for (let i = 0; i < timeline.length; i++) {
      const e = timeline[i];
      const ws = e.words ?? [];
      const label = e.label || "";
      const last = out[out.length - 1];
      if (last && last.label === label) {
        last.indices.push(i);
        last.endMs = Math.max(last.endMs, e.end_ms);
        last.text = last.text ? `${last.text} ${e.text}` : e.text;
        last.words.push(...ws);
        last.wordCountByChunk.push(ws.length);
      } else {
        out.push({
          label,
          indices: [i],
          startMs: e.start_ms,
          endMs: e.end_ms,
          text: e.text,
          words: [...ws],
          wordCountByChunk: [ws.length],
        });
      }
    }
    return out;
  }, [timeline]);

  const chunkToGroup = useMemo(() => {
    const m = new Map<number, number>();
    groups.forEach((g, gi) => g.indices.forEach((ci) => m.set(ci, gi)));
    return m;
  }, [groups]);

  // Virtualize the chunk rows. Each row also embeds N word <span>s; for
  // long meetings the total DOM cost is multiplicative (chunks × words)
  // and dominates scroll/paint frames. Virtualizing collapses it to the
  // visible window plus a small overscan buffer.
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const virtualizer = useVirtualizer({
    count: groups.length,
    getScrollElement: () => scrollRef.current,
    // Turns vary in height by word count; estimate is a typical
    // single-line turn, measureElement corrects after mount.
    estimateSize: () => 32,
    overscan: 8,
  });

  useEffect(() => {
    if (scrollAnchorIdx < 0) return;
    // The rAF tick still tracks anchor by chunk index (the underlying
    // playback unit). Translate to the visible group index so the
    // virtualizer scrolls to the right row.
    const groupIdx = chunkToGroup.get(scrollAnchorIdx);
    if (groupIdx === undefined) return;
    virtualizer.scrollToIndex(groupIdx, { align: "auto" });
  }, [scrollAnchorIdx, virtualizer, chunkToGroup]);

  // Live recording / live diarize: keep the latest turn visible.
  useEffect(() => {
    if (!bottomAligned || groups.length === 0) return;
    virtualizer.scrollToIndex(groups.length - 1, { align: "end" });
  }, [bottomAligned, groups.length, virtualizer]);

  // rAF-driven active-position tracker. Compute the active set fresh
  // each tick but only call setState when something actually changes,
  // so steady-state re-renders stay at "transitions per second" (~5–10
  // Hz) instead of the rAF tick rate (60 Hz). The previous version
  // tracked a single activeIdx and skipped past overlapping mic+sys
  // chunks: the picker greedily took whichever chunk had the latest
  // start_ms ≤ currentTime, so a mic interjection mid-sentence
  // abandoned the still-playing sys line for the rest of its words.
  // Now: any chunk whose [start_ms, end_ms] brackets currentTime is
  // "active", and overlapping chunks all stay lit while their audio
  // is still in the merged playback.
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    let raf = 0;
    let stopped = false;
    let lastIdxsKey = "";
    let lastWordsKey = "";
    let lastAnchor = -1;

    const computeAndSync = () => {
      const tl = timelineRef.current;
      const ms = audio.currentTime * 1000;
      const idxs: number[] = [];
      const wordsByIdx: Record<number, number> = {};
      let anchor = -1;
      let anchorStart = -1;
      // Timeline is sorted by start_ms but spans can overlap, so we
      // can't break out of the loop early on the first start_ms > ms
      // — a later chunk on the other source might already have ended.
      // O(n) per tick; n is one entry per ~5–15 s chunk, so even a
      // 2-hour recording is < 1500 entries. Cheap.
      for (let i = 0; i < tl.length; i++) {
        const e = tl[i];
        if (e.start_ms > ms) break;
        if (e.end_ms < ms) continue;
        idxs.push(i);
        // Closest start_ms ≤ ms wins the scroll anchor — visually
        // matches what the user just heard begin.
        if (e.start_ms > anchorStart) {
          anchorStart = e.start_ms;
          anchor = i;
        }
        const ws = e.words;
        if (ws && ws.length > 0) {
          let wi = -1;
          for (let j = 0; j < ws.length; j++) {
            if (ws[j].start_ms <= ms) wi = j;
            else break;
          }
          if (wi >= 0) wordsByIdx[i] = wi;
        }
      }
      const idxsKey = idxs.join(",");
      // Stable key: sort by chunk idx so the same {idx: word} map
      // serialises identically regardless of insertion order. Cheap
      // for the small handful of active chunks at any moment (1–3).
      const wordsKey = Object.keys(wordsByIdx)
        .map(Number)
        .sort((a, b) => a - b)
        .map((k) => `${k}:${wordsByIdx[k]}`)
        .join(",");
      if (idxsKey !== lastIdxsKey) {
        lastIdxsKey = idxsKey;
        setActiveIdxs(idxs);
      }
      if (wordsKey !== lastWordsKey) {
        lastWordsKey = wordsKey;
        setActiveWordByIdx(wordsByIdx);
      }
      if (anchor !== lastAnchor) {
        lastAnchor = anchor;
        setScrollAnchorIdx(anchor);
      }
    };

    const tick = () => {
      if (stopped) return;
      computeAndSync();
      raf = requestAnimationFrame(tick);
    };
    const start = () => {
      if (raf) return;
      raf = requestAnimationFrame(tick);
    };
    const stop = () => {
      cancelAnimationFrame(raf);
      raf = 0;
      computeAndSync();
    };
    audio.addEventListener("play", start);
    audio.addEventListener("playing", start);
    audio.addEventListener("pause", stop);
    audio.addEventListener("ended", stop);
    audio.addEventListener("seeked", computeAndSync);
    computeAndSync();
    if (!audio.paused) start();
    return () => {
      stopped = true;
      cancelAnimationFrame(raf);
      audio.removeEventListener("play", start);
      audio.removeEventListener("playing", start);
      audio.removeEventListener("pause", stop);
      audio.removeEventListener("ended", stop);
      audio.removeEventListener("seeked", computeAndSync);
    };
  }, []);

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

  // Drop a single chunk row. Used to remove off-topic content the
  // mic / sys captured (unrelated speech, mis-attributed leak, etc.)
  // without re-recording. Optimistic local update so the row
  // disappears instantly, then the IPC rebuilds note.transcript
  // from the surviving entries.
  async function deleteGroup(g: { indices: number[] }) {
    if (disabled) return;
    const set = new Set(g.indices);
    // Delete from highest chunk index to lowest so each IPC sees a
    // valid still-present index in the backend timeline (which the
    // first delete starts shifting). The optimistic frontend update
    // operates on the original index set in one shot.
    const sortedDesc = [...g.indices].sort((a, b) => b - a);
    setTimeline((tl) => tl.filter((_, i) => !set.has(i)));
    for (const ci of sortedDesc) {
      try {
        await ipc.noteTimelineDeleteChunk(noteId, ci);
      } catch (err) {
        console.error("noteTimelineDeleteChunk failed", err);
      }
    }
    useRecordingStore
      .getState()
      .pushFlash(g.indices.length === 1 ? "Line deleted" : "Turn deleted");
  }

  // Click a turn's speaker dot to cycle the whole turn to the next
  // known speaker. The set of known speakers is whatever currently
  // appears in the timeline, so after a re-diarize gives the user a
  // couple of base speakers, they can rename one in the chip strip
  // and then cycle whole turns onto it.
  async function cycleGroupLabel(g: { label: string; indices: number[] }) {
    if (disabled) return;
    const labels = Array.from(new Set(timeline.map((e) => e.label).filter(Boolean)));
    if (labels.length < 2) return;
    const at = labels.indexOf(g.label);
    const next = labels[(at + 1) % labels.length] ?? labels[0];
    if (next === g.label) return;
    const set = new Set(g.indices);
    setTimeline((tl) =>
      tl.map((e, i) => (set.has(i) ? { ...e, label: next } : e)),
    );
    for (const ci of g.indices) {
      try {
        await ipc.noteTimelineSetChunkLabel(noteId, ci, next);
      } catch (err) {
        console.error("noteTimelineSetChunkLabel failed", err);
      }
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
          // preload="auto" so the whole WAV streams in up-front and
          // every subsequent seek is in-memory. With "metadata" each
          // user click triggered a range-request through Tauri's
          // asset protocol; rapid clicking flooded I/O and on at
          // least one user's machine made the whole system lag.
          preload="auto"
          className="flex-1 h-8"
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
          ref={scrollRef}
          className="text-sm leading-relaxed text-[var(--color-text-muted)] overflow-y-auto"
          style={{ maxHeight: expanded ? "70vh" : "14rem" }}
        >
        <div
          ref={containerRef}
          style={{
            height: virtualizer.getTotalSize(),
            position: "relative",
            width: "100%",
          }}
        >
          {(() => {
            const activeChunkSet = new Set(activeIdxs);
            const labelCount = new Set(
              timeline.map((e) => e.label).filter(Boolean),
            ).size;
            return virtualizer.getVirtualItems().map((vrow) => {
            const gi = vrow.index;
            const g = groups[gi];
            // A turn is active when any of its constituent chunks is.
            // Mic + sys overlap with the same label is rare after the
            // bridge, but we still want both to count.
            const isActive = g.indices.some((ci) => activeChunkSet.has(ci));
            const color = g.label ? colors.get(g.label) : undefined;
            const cyclable = labelCount >= 2 && !!g.label;
            // Map the (chunk idx, word idx within chunk) pair the rAF
            // tick tracks into the flattened position in g.words. Each
            // chunk contributes wordCountByChunk[k] words; sum prior
            // contributions to find the offset of the active chunk
            // inside the group, then add its in-chunk active word
            // index. A single audio position can light up at most one
            // word per active chunk; with non-overlapping turns this is
            // exactly one word in the group.
            const activeFlatIdxs = new Set<number>();
            if (isActive) {
              let offset = 0;
              for (let k = 0; k < g.indices.length; k++) {
                const ci = g.indices[k];
                if (activeChunkSet.has(ci)) {
                  const w = activeWordByIdx[ci];
                  if (w !== undefined && w >= 0) {
                    activeFlatIdxs.add(offset + w);
                  }
                }
                offset += g.wordCountByChunk[k];
              }
            }
            return (
              <div
                key={vrow.key}
                data-index={gi}
                ref={virtualizer.measureElement}
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  width: "100%",
                  transform: `translateY(${vrow.start}px)`,
                }}
              >
              <div
                data-idx={gi}
                className={
                  "group flex items-start gap-1 px-2 py-1 rounded transition-colors " +
                  (isActive
                    ? "bg-[var(--color-pill-hover)] text-[var(--color-text)]"
                    : "hover:bg-[var(--color-pill-hover)]")
                }
              >
                {g.label && color && (
                  <div className="relative w-3 shrink-0 self-stretch">
                    <button
                      type="button"
                      onClick={(e) => {
                        e.stopPropagation();
                        void cycleGroupLabel(g);
                      }}
                      disabled={!cyclable || disabled}
                      title={
                        cyclable
                          ? `${g.label} — click to reassign`
                          : g.label
                      }
                      className="nd-speaker-dot"
                      style={{
                        background: color,
                        left: 0,
                        top: "calc(0.5lh - 5px)",
                      }}
                      aria-label={`Speaker: ${g.label}`}
                    />
                  </div>
                )}
                {g.words.length > 0 ? (
                  <div className="flex-1 nd-bare cursor-text leading-relaxed">
                    {g.words.map((w, wi) => {
                      const wordActive = activeFlatIdxs.has(wi);
                      return (
                        <span
                          key={wi}
                          onClick={(e) => {
                            e.stopPropagation();
                            seek(w.start_ms);
                          }}
                          className={
                            "nd-word " + (wordActive ? "nd-word-active" : "")
                          }
                        >
                          {w.text}
                        </span>
                      );
                    }).reduce<React.ReactNode[]>((acc, node, i) => {
                      // Flatten with explicit space text nodes so words
                      // render with consistent spacing regardless of
                      // browser text-rendering quirks. Skip the leading
                      // space before the first word.
                      if (i > 0) acc.push(" ");
                      acc.push(node);
                      return acc;
                    }, [])}
                  </div>
                ) : (
                  <button
                    type="button"
                    onClick={() => seek(g.startMs)}
                    title="Click to play from here"
                    className="text-left flex-1 nd-bare cursor-text"
                  >
                    {g.text}
                  </button>
                )}
                {!disabled && (
                  <button
                    type="button"
                    onClick={(e) => {
                      e.stopPropagation();
                      void deleteGroup(g);
                    }}
                    title={
                      g.indices.length === 1
                        ? "Delete this line"
                        : "Delete this turn"
                    }
                    aria-label={
                      g.indices.length === 1
                        ? "Delete this line"
                        : "Delete this turn"
                    }
                    className="nd-bare shrink-0 self-start opacity-0 group-hover:opacity-100 transition-opacity p-0.5 rounded text-[var(--color-text-muted)] hover:text-[var(--color-accent)] hover:bg-[var(--color-pill-hover)]"
                  >
                    <X size={14} strokeWidth={1.5} />
                  </button>
                )}
              </div>
              </div>
            );
          });
          })()}
        </div>
        </div>
      )}
    </div>
  );
});

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
// Re-polls on every recording-phase transition so the diarize step's
// diagnostic write becomes visible without a page refresh: depending
// only on `noteId` (which doesn't change) would leave the link hidden
// until the user navigated away and back.
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
