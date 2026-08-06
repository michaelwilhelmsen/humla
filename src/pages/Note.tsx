import { Link, useNavigate, useOutletContext, useParams } from "react-router-dom";
import { Fragment, memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  Building2,
  Calendar,
  CircleCheck,
  Eye,
  History,
  Check,
  ChevronDown,
  ChevronLeft,
  Circle,
  Cloud,
  Copy,
  FileText,
  Folder,
  Languages,
  MessageCircle,
  MessageSquare,
  MoreHorizontal,
  PanelRight,
  RefreshCw,
  Sparkles,
  Users,
  X,
} from "lucide-react";
import { ipc, onSummaryThinkingDelta, onSummaryContentDelta, type Note as TNote, type NoteRevision, type NoteSession, type SummaryPrompt, type TimelineEntry } from "../lib/ipc";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useLiveSetting } from "../lib/settingsBus";
import { useDownloadStore, useNotesStore, useRecordingStore } from "../lib/store";
import { computeSetupStatus } from "../lib/setupStatus";
import { useOwnerName, useCloudStore } from "../lib/cloud";
import { extractSpeakerLabels, renameSpeakerInTranscript } from "../lib/speakers";
import { shouldAdoptRemoteBody } from "../lib/noteSync";
import { SpeakerLabels, speakerColorMap } from "../components/SpeakerLabels";
import { RecordingSessions } from "../components/RecordingSessions";
import { groupTimeline, resolveActivePill, formatSessionCaption } from "../lib/sessions";
import { RecordingBar } from "../components/RecordingBar";
import { ChatPanel, type ChatSessionControls } from "../components/ChatPanel";
import { SelectablePopover } from "../components/SelectablePopover";
import {
  Menu,
  MenuContent,
  MenuRadioGroup,
  MenuRadioItem,
  MenuSeparator,
  MenuTrigger,
} from "../components/ui/Menu";
import { ChatHistoryControls } from "../components/ChatHistoryControls";
import { targetKey, type ChatTarget } from "../lib/chatTarget";
import type { LayoutOutletContext } from "../components/Layout";
import { SkeletonLines } from "../components/Skeleton";
import { NoteEditor } from "../components/Editor";
import { ContextMenu, ContextMenuItem } from "../components/ContextMenu";
import { ExportModal } from "./ExportModal";
import { SUMMARY_PRESETS, presetLabel } from "../lib/presets";
import { LANGUAGES, languageOptionLabel } from "../lib/languages";
import { useDeveloperMode } from "../lib/useDeveloperMode";
import { useSpeakerSuggestions } from "../lib/useSpeakerSuggestions";
import {
  notesWithSpeaker,
  renameOutcomeMessage,
  renameSpeakerAcrossNotes,
} from "../lib/crossNoteRename";
import { cn } from "../lib/cn";

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

/** Kick off a summary, surfacing any failure as a toast.
 *
 * Both entry points — the app bar's Summarize and the Summary panel's
 * regenerate — must go through this. An uncaught rejection here is an
 * unhandled promise: the spinner just stops, leaving no summary and no reason
 * why. That's exactly how a VPN dropping the OpenAI connection presented.
 *
 * `onFailure` lets the caller that owns the live-stream state drop a partial
 * response; see `clearSummaryStream` in `Note`.
 */
async function runSummarize(noteId: string, onFailure?: () => void) {
  try {
    await ipc.summarizeNote(noteId);
  } catch (e) {
    onFailure?.();
    useRecordingStore.getState().pushError({ noteId, message: String(e), kind: "summary" });
  }
}

export function Note() {
  const { id } = useParams<{ id: string }>();
  const { sidebarCollapsed } = useOutletContext<LayoutOutletContext>();
  const upsert = useNotesStore((s) => s.upsertLocal);
  const refreshNotes = useNotesStore((s) => s.refresh);
  const folders = useNotesStore((s) => s.folders);
  const note = useNotesStore((s) => s.notes.find((n) => n.id === id));
  // Every note in the active workspace. `notes_list` includes each one's full
  // transcript, so the cross-note rename (#116 part 2) needs no extra query to
  // find or count the notes it will rewrite.
  const allNotes = useNotesStore((s) => s.notes);
  const replaceTranscript = useNotesStore((s) => s.replaceTranscript);
  const [draft, setDraft] = useState<TNote | null>(null);
  // Version history panel + a nonce so restoring re-seeds the body editor
  // (initialBody is memoised on the note id, which doesn't change on restore).
  const [historyOpen, setHistoryOpen] = useState(false);
  const [revisions, setRevisions] = useState<NoteRevision[]>([]);
  const [restoreNonce, setRestoreNonce] = useState(0);
  // "Created by" attribution — resolves to a name only when the note was
  // authored by a teammate in the active workspace (null when it's yours,
  // local, or unresolvable). Called unconditionally before the early return.
  const ownerName = useOwnerName(draft?.owner ?? null);
  // Name of the workspace this note is shared with (for the visibility row).
  // Reads are workspace-scoped, so a note with a workspace_id belongs to the
  // active one. Empty workspace_id = Private / local-only.
  const sharedWorkspace = useCloudStore((s) => s.status.current_workspace?.name);
  // When signed in, the note's workspace becomes editable (move it between
  // Personal and any workspace you belong to). Signed out → static row only.
  const cloudLoggedIn = useCloudStore((s) => s.status.logged_in);
  // Per-note sync state: true while this note has an unpushed change queued.
  const notePending = useCloudStore((s) => (draft ? s.pendingNoteIds.has(draft.id) : false));
  // Read-only when you're a viewer in THIS NOTE's workspace (derive the role
  // from the note's own workspace_id, not the active workspace — a deep-linked
  // or mid-switch note can belong to a different one).
  const myWorkspaces = useCloudStore((s) => s.status.workspaces);
  const billingEnabled = useCloudStore((s) => s.status.billing_enabled);
  // Our own user id — to distinguish our recording lock from a teammate's.
  const myUserId = useCloudStore((s) => s.status.user?.id);
  const myName = useCloudStore((s) => s.status.user?.name ?? null);
  const noteWs = draft?.workspace_id
    ? myWorkspaces.find((w) => w.id === draft.workspace_id)
    : undefined;
  const isViewer = noteWs?.role === "viewer";
  // On humla-cloud, a workspace with no active/trialing subscription is read-only
  // until it's paid — the server blocks writes, so mirror that in the UI rather
  // than letting edits look saved locally but silently fail to sync.
  const lockedByPlan =
    billingEnabled && !!noteWs && noteWs.plan_status !== "active" && noteWs.plan_status !== "trialing";
  const readOnly = !!draft?.workspace_id && (isViewer || lockedByPlan);
  // Mirror into a ref so the memoised patch callbacks can gate without changing
  // identity (which would bust the transcript-view memos).
  const readOnlyRef = useRef(readOnly);
  readOnlyRef.current = readOnly;
  const [uiLang, setUiLang] = useState<string>("no");
  const [globalProvider, setGlobalProvider] = useState<string>("openai");
  // Device-wide audio retention (#24). Off is the shipped default, so an
  // unresolved read means off — don't promise a player that isn't coming. Read
  // through the settings bus, not a plain fetch: Settings is a dialog over a
  // *pinned* router location, so this view never sees the trip to /settings and
  // back and can't use navigation as its cue to re-read.
  const keepAudio = useLiveSetting("keep_audio") === "true";
  // Live reasoning + content streamed from the local LLM. Cleared each time a
  // new summarize starts and again when the summary lands. Scoped by note id
  // so a delta from a different note's run doesn't leak into this view.
  const [thinkingStream, setThinkingStream] = useState<string>("");
  const [contentStream, setContentStream] = useState<string>("");
  const [thinkingExpanded, setThinkingExpanded] = useState<boolean>(true);
  const [panelOpen, setPanelOpen] = useState<boolean>(true);
  const [activeTab, setActiveTab] = useState<"summary" | "transcript" | "chat">("summary");
  // Chat session chrome for the panel header (issue #62). ChatPanel owns all
  // chat state and publishes this projection up; the header's +/history buttons
  // render purely from it. null = no chat chrome (provider not ready / no tab).
  const [chatControls, setChatControls] = useState<ChatSessionControls | null>(null);
  const handleChatControls = useCallback((c: ChatSessionControls | null) => setChatControls(c), []);
  // This pane is always note-anchored (#94); the library-wide surface is #95.
  // Memoised so the panel isn't handed a fresh object on every Note render, and
  // null until the note loads — deliberately NOT `noteId: ""`, which is the
  // sentinel #82's decision record forbids and #93's backend rejects outright.
  const chatTarget = useMemo<ChatTarget | null>(
    () => (draft ? { kind: "note", noteId: draft.id } : null),
    [draft?.id],
  );
  const chatTargetKey = chatTarget ? targetKey(chatTarget) : null;
  const [panelWidth, setPanelWidth] = useState<number>(() => {
    const saved = typeof localStorage !== "undefined" ? Number(localStorage.getItem("humla.panelWidth")) : NaN;
    return saved >= 320 && saved <= 720 ? saved : 440;
  });
  const [resizing, setResizing] = useState<boolean>(false);
  const saveTimer = useRef<number | null>(null);
  const devMode = useDeveloperMode();
  // Playback bundle: the mixed WAV path (converted to a tauri:// asset
  // URL) and the per-turn timeline driving highlight rendering. Both
  // null/empty means this note pre-dates the playback feature or its
  // bundle hasn't been written yet — we fall back to the plain
  // TranscriptEditor in that case.
  const [playbackUrl, setPlaybackUrl] = useState<string | null>(null);
  const [timeline, setTimeline] = useState<TimelineEntry[]>([]);
  // Recording sessions (#16): every take on this note, in order. Drives the
  // playback carousel + the session-switched player. Empty for notes with no
  // recordings.
  const [sessions, setSessions] = useState<NoteSession[]>([]);

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
  // A file import replaying through the pipeline. Treated like a live capture
  // for UI purposes (transcript streams in, Record/Summarize hidden) but has no
  // pause/stop controls — the sidecar replays once and finishes on its own.
  const isImporting = isThisNoteActive && recPhase.phase === "importing";
  const recActive = isStarting || isRecording || isPaused || isStopping || isDiarizing || isImporting;

  // When a recording or import starts on this note, surface the live
  // transcript: open the context panel and switch to its Transcript tab.
  useEffect(() => {
    if (isStarting || isRecording || isImporting) {
      setPanelOpen(true);
      setActiveTab("transcript");
    }
  }, [isStarting, isRecording, isImporting]);

  // Drag-to-resize for the context panel. A handle on the panel's left edge
  // adjusts its width (clamped 320–720); persisted to localStorage. The
  // width transition is suppressed mid-drag so it tracks the cursor.
  const panelWidthRef = useRef(panelWidth);
  panelWidthRef.current = panelWidth;
  const resizeStartRef = useRef<{ x: number; w: number } | null>(null);
  const beginResize = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    resizeStartRef.current = { x: e.clientX, w: panelWidthRef.current };
    setResizing(true);
  }, []);
  useEffect(() => {
    if (!resizing) return;
    const onMove = (e: MouseEvent) => {
      const s = resizeStartRef.current;
      if (!s) return;
      setPanelWidth(Math.min(720, Math.max(320, s.w + (s.x - e.clientX))));
    };
    const onUp = () => {
      setResizing(false);
      try {
        localStorage.setItem("humla.panelWidth", String(panelWidthRef.current));
      } catch {
        /* ignore */
      }
    };
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      document.body.style.userSelect = "";
      document.body.style.cursor = "";
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [resizing]);

  // Recording lock for shared notes: poll who is recording this note so we can
  // show "X is recording…" and disable Record. Skipped for Personal notes and
  // while WE are the active recorder (we hold the lock then, so the backend
  // would just report ourselves). Cleared on any error → never blocks recording.
  const [lockedBy, setLockedBy] = useState<{ holderId: string; holderName: string } | null>(null);
  const noteWsId = draft?.workspace_id;
  useEffect(() => {
    if (!id || !noteWsId || isThisNoteActive) {
      setLockedBy(null);
      return;
    }
    let cancelled = false;
    const poll = async () => {
      try {
        const s = await ipc.noteRecordingStatus(id);
        if (cancelled) return;
        setLockedBy(s && s.holderId !== myUserId ? s : null);
      } catch {
        if (!cancelled) setLockedBy(null);
      }
    };
    poll();
    const t = window.setInterval(poll, 10000);
    return () => {
      cancelled = true;
      window.clearInterval(t);
    };
  }, [id, noteWsId, isThisNoteActive, myUserId]);

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
  const fitTitle = useCallback(() => {
    const el = titleRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }, []);
  useEffect(fitTitle, [fitTitle, draft?.title]);

  // Wrapping depends on the column's width, not only on the text, and the column
  // widens when the context panel closes or is dragged. Watching React state
  // won't do: the effect would run at commit, before the 300ms max-width
  // transition has moved, and re-bake the pre-transition height — which is how a
  // title that no longer wraps kept its two-line height and left a gap above the
  // meta bar. Observe the textarea itself so we refit on every frame of the
  // transition and every drag tick. The height writes are idempotent, so this
  // settles after one extra callback rather than looping.
  useEffect(() => {
    const el = titleRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let lastWidth = -1;
    const ro = new ResizeObserver(() => {
      const w = el.clientWidth;
      if (w === lastWidth) return;
      lastWidth = w;
      fitTitle();
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [fitTitle, draft?.id]);

  // Always pull summary updates from the store. Pull transcript updates only
  // while a recording or diarization is in flight — otherwise our debounced
  // save round-trips through the store and clobbers in-progress edits.
  // Diarization replaces the transcript wholesale, so we want the editor to
  // reflect that update immediately.
  const allowTranscriptSync =
    isRecording || isPaused || isStarting || isStopping || isDiarizing || isImporting;
  useEffect(() => {
    if (!note || !draft || note.id !== draft.id) return;
    setDraft((d) => {
      if (!d) return d;
      const nextSummary = note.summary;
      const nextTranscript = allowTranscriptSync ? note.transcript : d.transcript;
      // A cloud pull can land the body *after* we read the row — see
      // shouldAdoptRemoteBody for why adopting is gated on an empty draft with
      // nothing queued for save.
      const nextBody = shouldAdoptRemoteBody(d.body, note.body, "body" in pendingChanges.current)
        ? note.body
        : d.body;
      if (d.summary === nextSummary && d.transcript === nextTranscript && d.body === nextBody) {
        return d;
      }
      return { ...d, summary: nextSummary, transcript: nextTranscript, body: nextBody };
    });
  }, [note?.transcript, note?.summary, note?.body, allowTranscriptSync]);

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
      let [path, tl, sess] = await Promise.all([
        ipc.notePlaybackPath(draft.id).catch(() => null),
        ipc.noteTimeline(draft.id).catch((): TimelineEntry[] => []),
        ipc.noteSessions(draft.id).catch((): NoteSession[] => []),
      ]);
      // No local audio but the note is shared → pull it from the workspace.
      // Prefer per-session sync (#16): rebuild sessions.json + fetch each take's
      // playback/timeline. Fall back to the legacy single-file notes.audio for
      // notes that predate sessions (or were uploaded by an old client).
      if (!path && draft.workspace_id) {
        let got = await ipc.downloadNoteSessions(draft.id).catch(() => false);
        if (!got) {
          got = await ipc.downloadNoteAudio(draft.id).catch(() => false);
        }
        if (got && !cancelled) {
          path = await ipc.notePlaybackPath(draft.id).catch(() => null);
          sess = await ipc.noteSessions(draft.id).catch(() => sess);
          tl = await ipc.noteTimeline(draft.id).catch(() => tl);
        }
      }
      if (cancelled) return;
      setPlaybackUrl(path ? convertFileSrc(path) : null);
      setTimeline(tl);
      setSessions(sess);
    })();
    return () => {
      cancelled = true;
    };
    // keepAudio is a dep so turning retention back on fetches a shared note's
    // audio right away (#24) rather than on the next open. The backend enforces
    // the rule; this only decides when to ask again.
  }, [draft?.id, draft?.workspace_id, recPhase.phase, keepAudio]);

  // patch / patchProvider intentionally read from `draftRef.current`
  // rather than the `draft` closure so they can stay stable across
  // renders — keeps the React.memo on TranscriptEditor / TranscriptView
  // / TranscriptPlayer effective (otherwise a fresh function ref would
  // bust the memo on every parent render).
  const patch = useCallback(
    (field: "title" | "body" | "transcript" | "summary_preset" | "language", value: string) => {
      const cur = draftRef.current;
      if (!cur) return;
      if (readOnlyRef.current) return; // viewer: no local edits (server rejects too)
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
      if (readOnlyRef.current) return; // viewer: read-only
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
    // Keyed on the body value, not just the id: a late-arriving cloud body has to
    // reach the editor. Cursor-safe because `patch` stores the editor's own HTML,
    // so on a keystroke this recomputes to exactly what the editor already holds
    // and NoteEditor's equality guard makes the re-sync a no-op.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draft?.id, draft?.body, restoreNonce]);

  const dateChip = useMemo(() => (draft ? formatDateChip(draft.created_at) : "Today"), [draft]);

  // Drop a partial streamed response. The Summary panel falls back to
  // `contentStream` once `isSummarizing` clears, so a run that died mid-stream
  // would otherwise leave a truncated summary on screen next to the error toast
  // while the DB holds nothing. Both entry points route their failure here,
  // since either one streams into this same panel.
  const clearSummaryStream = () => setContentStream("");

  // Suggestion source for the speaker-rename picker (#116), fetched once there is
  // a transcript to label. Must stay ABOVE the `!draft` return below: a hook
  // after an early return changes the hook count as soon as a note loads.
  const speakerSuggestions = useSpeakerSuggestions(
    !!draft && draft.transcript.trim().length > 0 && !readOnly,
  );

  // How many OTHER notes carry each of this note's speaker labels (#116 part 2),
  // which is what gates the "rename everywhere" choice and puts the number on
  // its button. Counted from transcript text rather than `notes.speakers`: that
  // column is only written by `reindex_note` and can be stale, so the button
  // could otherwise promise a number it doesn't deliver.
  // Every note, with the open one's live draft substituted for the store's copy.
  // `patch` debounces, so the store can lag the transcript on screen by up to a
  // few hundred ms — sweeping off the stale copy would revert edits the user can
  // still see, and count off a transcript that isn't the one being renamed.
  const notesForSweep = useMemo(
    () => (draft ? allNotes.map((n) => (n.id === draft.id ? draft : n)) : allNotes),
    [allNotes, draft],
  );

  const otherNotesWithLabel = useMemo(() => {
    if (!draft) return {};
    const others = notesForSweep.filter((n) => n.id !== draft.id);
    return Object.fromEntries(
      extractSpeakerLabels(draft.transcript).map((label) => [
        label,
        notesWithSpeaker(others, label).length,
      ]),
    );
  }, [notesForSweep, draft]);

  if (!draft) return null;

  // Who may move this note to another workspace: its creator, or a workspace
  // owner/admin (who can reorganize). A plain member can't move a teammate's
  // note out from under them (a move tombstones the shared copy for everyone).
  // `owner` is empty for local/unsynced notes you just made — those are yours.
  const noteWsRole = draft.workspace_id
    ? myWorkspaces.find((w) => w.id === draft.workspace_id)?.role
    : undefined;
  const canMoveWorkspace =
    !draft.owner ||
    draft.owner === myUserId ||
    noteWsRole === "owner" ||
    noteWsRole === "admin";

  const hasSummary = draft.summary.trim().length > 0;
  const hasTranscript = draft.transcript.trim().length > 0;
  // Live-feed alignment: while a recording is in flight, pin the
  // collapsed transcript card to its bottom so newly transcribed
  // chunks stay visible. After stop / on a saved note the user is
  // reading from the top, so flip back to top alignment.
  const transcriptLive = isRecording || isPaused || isStopping || isDiarizing || isImporting;

  const folder = draft.folder_id ? folders.find((f) => f.id === draft.folder_id) : null;
  const backTo = folder ? `/folder/${folder.id}` : "/";
  const backLabel = folder ? folder.name : "Home";
  const otherActiveRecording = recPhase.noteId !== null && recPhase.noteId !== draft.id;
  const authorName = ownerName ?? myName ?? null;
  const authorInitial = (authorName ?? "?").slice(0, 1).toUpperCase();
  const noteWsName = draft.workspace_id
    ? (noteWs?.name ?? sharedWorkspace ?? "Workspace")
    : "Personal";
  const wsInitial = noteWsName.slice(0, 1).toUpperCase();

  return (
    <div className="h-full flex min-h-0">
      {/* Body column — toolbar + scrollable writing area. The context
          panel is a sibling card to the right; closing it widens the body
          into a focus-writing mode. */}
      <div className="flex-1 min-w-0 flex flex-col relative">
        <NoteToolbar
          noteId={draft.id}
          backTo={backTo}
          backLabel={backLabel}
          readOnly={readOnly}
          recActive={recActive}
          canRecord={!otherActiveRecording && !lockedBy}
          panelOpen={panelOpen}
          onTogglePanel={() => setPanelOpen((v) => !v)}
          onSummarizeFailed={clearSummaryStream}
          sidebarCollapsed={sidebarCollapsed}
        />
        <div className="flex-1 overflow-y-auto">
          <div
            className={cn(
              "mx-auto w-full px-12 pt-3 pb-32 transition-[max-width] duration-300",
              panelOpen ? "max-w-[640px]" : "max-w-[760px]",
            )}
          >
        {readOnly && (
          <div className="mb-4 flex items-center gap-2 px-3 py-2 rounded-md border border-[var(--color-line)] bg-[var(--color-pill-hover)] text-xs text-[var(--color-text-muted)]">
            <Eye size={13} strokeWidth={1.5} className="shrink-0" />
            <span>
              {isViewer
                ? "View-only — you have viewer access to this workspace, so this note can’t be edited."
                : "Read-only — this workspace needs an active subscription. The owner can start it in Settings → Account → Billing."}
            </span>
          </div>
        )}
        {!readOnly && lockedBy && (
          <div className="mb-4 flex items-center gap-2 px-3 py-2 rounded-md border border-[var(--color-line)] bg-[var(--color-pill-hover)] text-xs text-[var(--color-text-muted)]">
            <Circle size={9} fill="currentColor" strokeWidth={0} className="shrink-0 rec-dot text-[var(--color-record)]" />
            <span>
              <strong className="font-medium text-[var(--color-text)]">{lockedBy.holderName}</strong> is recording this note. Only one person can record a shared note at a time — the transcript will sync here when they stop.
            </span>
          </div>
        )}
        <textarea
          ref={titleRef}
          value={draft.title}
          onChange={(e) => patch("title", e.target.value)}
          readOnly={readOnly}
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
          className="nd-bare block w-full mb-4 text-[30px] font-semibold leading-[1.18] tracking-[-0.022em] placeholder:text-[var(--color-text-muted)]/50 resize-none overflow-hidden focus:outline-none"
        />

        <div className="mb-8 pb-4 border-b border-[var(--color-line)]">
          <div className="flex flex-wrap items-center gap-1 -mx-2">
          {authorName && (
            <span className="nd-meta" style={{ color: "var(--color-text)" }}>
              <span
                className="grid place-items-center w-[18px] h-[18px] rounded-full text-[9.5px] font-semibold"
                style={{ background: "var(--color-accent)", color: "var(--color-on-accent)" }}
              >
                {authorInitial}
              </span>
              <span className="font-medium">{authorName}</span>
            </span>
          )}
          {cloudLoggedIn ? (
            <WorkspacePicker
              value={draft.workspace_id}
              disabled={readOnly || !canMoveWorkspace}
              badge={
                <span
                  className="grid place-items-center w-[17px] h-[17px] rounded-[5px] text-[9px] font-semibold"
                  style={{ background: "var(--color-surface-raised)", color: "var(--color-text)" }}
                >
                  {wsInitial}
                </span>
              }
              onChange={async (workspaceId) => {
                if (!draft || readOnly || !canMoveWorkspace || workspaceId === draft.workspace_id) return;
                const next = { ...draft, workspace_id: workspaceId };
                setDraft(next);
                await ipc.setNoteWorkspace(draft.id, workspaceId);
                // Moving out of the active workspace removes it from that
                // list — refetch so the sidebar reflects the move.
                await refreshNotes();
              }}
            />
          ) : (
            draft.workspace_id && (
              <span className="nd-meta">
                <span
                  className="grid place-items-center w-[17px] h-[17px] rounded-[5px] text-[9px] font-semibold"
                  style={{ background: "var(--color-surface-raised)", color: "var(--color-text)" }}
                >
                  {wsInitial}
                </span>
                <span>{sharedWorkspace ?? "your workspace"}</span>
              </span>
            )
          )}
          <span className="nd-meta">
            <Calendar size={14} strokeWidth={1.7} />
            {dateChip}
          </span>
          <FolderPicker
            value={draft.folder_id}
            onChange={async (folderId) => {
              if (!draft || readOnly) return;
              const next = { ...draft, folder_id: folderId };
              setDraft(next);
              await ipc.moveNote(draft.id, folderId);
              upsert(next);
            }}
          />
          <ClientPicker
            value={draft.client_id ?? null}
            onChange={async (clientId) => {
              if (!draft || readOnly) return;
              const next = { ...draft, client_id: clientId };
              setDraft(next);
              await ipc.setNoteClient(draft.id, clientId);
              upsert(next);
            }}
          />
          {recActive ? (
            <span className="nd-meta" style={{ color: "var(--color-record)", fontWeight: 500 }}>
              <span
                className={cn("w-[7px] h-[7px] rounded-full", !isPaused && "rec-dot")}
                style={{
                  background: isPaused ? "transparent" : "var(--color-record)",
                  border: isPaused ? "1.5px solid var(--color-record)" : undefined,
                }}
              />
              {isStarting
                ? "Starting"
                : isPaused
                ? "Paused"
                : isStopping
                ? "Stopping"
                : isDiarizing
                ? "Identifying speakers"
                : "Recording"}
            </span>
          ) : draft.workspace_id ? (
            <span className="nd-meta" style={{ color: "var(--color-success)" }}>
              <CircleCheck size={14} strokeWidth={1.7} style={{ opacity: 1 }} />
              {notePending ? "Syncing…" : "Synced"}
            </span>
          ) : null}
          </div>
        </div>

        <NoteEditor
          key={draft.id}
          initialHTML={initialBody}
          onChange={(html) => patch("body", html)}
          editable={!readOnly}
        />

        <div className="mt-6 border-t border-[var(--color-line)] pt-3">
          <button
            onClick={async () => {
              const next = !historyOpen;
              setHistoryOpen(next);
              if (next) setRevisions(await ipc.noteRevisions(draft.id).catch(() => []));
            }}
            className="flex items-center gap-1.5 text-xs text-[var(--color-text-muted)] hover:text-[var(--color-text)] transition-colors"
          >
            <History size={13} strokeWidth={1.5} />
            <span>Version history{revisions.length ? ` · ${revisions.length}` : ""}</span>
          </button>
          {historyOpen && (
            <div className="mt-3 flex flex-col gap-0.5">
              {revisions.length === 0 ? (
                <div className="text-xs text-[var(--color-text-muted)]">No earlier versions yet.</div>
              ) : (
                revisions.map((rev) => (
                  <div
                    key={rev.id}
                    className="flex items-center gap-3 text-xs px-2 py-1.5 rounded hover:bg-[var(--color-pill-hover)]"
                  >
                    <span className="text-[var(--color-text-muted)] tabular-nums min-w-32 shrink-0">
                      {new Date(rev.created_at).toLocaleString(undefined, {
                        month: "short",
                        day: "numeric",
                        hour: "2-digit",
                        minute: "2-digit",
                        hour12: false,
                      })}
                    </span>
                    <span className="flex-1 truncate text-[var(--color-text)]">
                      {rev.title.trim() || "Untitled"}
                    </span>
                    {!readOnly && (
                      <button
                        onClick={async () => {
                          const updated = await ipc.restoreNoteRevision(draft.id, rev.id);
                          setDraft(updated);
                          upsert(updated);
                          setRestoreNonce((n) => n + 1);
                          setRevisions(await ipc.noteRevisions(draft.id).catch(() => []));
                        }}
                        className="shrink-0 text-[var(--color-interactive)] hover:underline"
                      >
                        Restore
                      </button>
                    )}
                  </div>
                ))
              )}
            </div>
          )}
        </div>

          </div>
        </div>
        {!readOnly && <RecordingBar noteId={draft.id} />}
      </div>

      <aside
        style={{ width: panelOpen ? panelWidth : 0 }}
        className={cn(
          "shrink-0 flex flex-col overflow-hidden rounded-[var(--radius-card)] bg-[var(--color-surface)] shadow-[var(--shadow-card)] relative z-30",
          !resizing && "transition-[width,opacity] duration-300 ease-[cubic-bezier(0.4,0,0.2,1)]",
          panelOpen ? "opacity-100 ml-1.5" : "opacity-0 pointer-events-none",
        )}
        aria-hidden={!panelOpen}
      >
          {panelOpen && (
            <div
              onMouseDown={beginResize}
              title="Drag to resize"
              className="group absolute left-0 top-0 bottom-0 z-40 w-2 cursor-col-resize"
            >
              <div className="absolute left-0 top-0 bottom-0 w-px bg-[var(--color-line-visible)] opacity-0 group-hover:opacity-100 transition-opacity" />
            </div>
          )}
          {/* Tabs + close */}
          <div className="h-12 shrink-0 flex items-center gap-1 pl-2 pr-1.5 border-b border-[var(--color-line)]">
            <div className="flex-1 flex gap-1">
              <button
                type="button"
                onClick={() => setActiveTab("summary")}
                className={cn(
                  "inline-flex items-center gap-1.5 px-2.5 py-1.5 rounded-[var(--radius)] text-[13px] font-medium transition-colors",
                  activeTab === "summary"
                    ? "bg-[var(--color-accent-soft)] text-[var(--color-accent-text)]"
                    : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]",
                )}
              >
                <FileText size={14} strokeWidth={1.6} />
                Summary
              </button>
              <button
                type="button"
                onClick={() => setActiveTab("transcript")}
                className={cn(
                  "inline-flex items-center gap-1.5 px-2.5 py-1.5 rounded-[var(--radius)] text-[13px] font-medium transition-colors",
                  activeTab === "transcript"
                    ? "bg-[var(--color-accent-soft)] text-[var(--color-accent-text)]"
                    : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]",
                )}
              >
                <MessageSquare size={14} strokeWidth={1.6} />
                Transcript
              </button>
              <button
                type="button"
                onClick={() => setActiveTab("chat")}
                className={cn(
                  "inline-flex items-center gap-1.5 px-2.5 py-1.5 rounded-[var(--radius)] text-[13px] font-medium transition-colors",
                  activeTab === "chat"
                    ? "bg-[var(--color-accent-soft)] text-[var(--color-accent-text)]"
                    : "text-[var(--color-text-muted)] hover:text-[var(--color-text)]",
                )}
              >
                <MessageCircle size={14} strokeWidth={1.6} />
                Chat
              </button>
            </div>
            {activeTab === "chat" && chatControls?.targetKey === chatTargetKey && (
              <ChatHistoryControls controls={chatControls} />
            )}
            <button
              type="button"
              onClick={() => setPanelOpen(false)}
              title="Close panel"
              aria-label="Close panel"
              className="nd-btn-icon"
            >
              <X size={16} strokeWidth={1.6} />
            </button>
          </div>

          <div className="flex-1 min-h-0 flex flex-col">
            {activeTab === "summary" ? (
              <div className="flex-1 min-h-0 overflow-y-auto px-4 py-4">
                <div className="flex flex-wrap items-center gap-2 mb-4">
                  <PresetPicker
                    value={draft.summary_preset || "meeting"}
                    onChange={(v) => patch("summary_preset", v)}
                  />
                  <SummaryProviderChip
                    value={draft.summary_provider}
                    globalDefault={globalProvider}
                    onChange={patchProvider}
                  />
                  <div className="ml-auto flex items-center gap-0.5">
                    {hasSummary && <CopyButton label="Summary" getText={() => draft.summary} />}
                    {!readOnly && (
                      <button
                        type="button"
                        onClick={() => void runSummarize(draft.id, clearSummaryStream)}
                        title="Regenerate summary"
                        aria-label="Regenerate summary"
                        className="nd-btn-icon nd-btn-icon-sm"
                      >
                        <RefreshCw size={15} strokeWidth={1.6} />
                      </button>
                    )}
                  </div>
                </div>

                {/* Live reasoning trace (local LLM only). Cloud OpenAI keeps
                    its chain-of-thought server-side, so the panel there would
                    be a permanent "waiting…" placeholder. */}
                {isLocalProvider && (thinkingStream.length > 0 || (isSummarizing && contentStream.length === 0)) && (
                  <div className="mb-5">
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
                      <div ref={reasoningRef} className="prose-reasoning mt-2 max-h-56 overflow-y-auto">
                        <Markdown source={thinkingStream} />
                      </div>
                    )}
                  </div>
                )}

                {/* Render priority: streaming while summarizing, then the
                    saved summary, then streaming as a first-time fallback,
                    then a skeleton; finally the empty state. */}
                {isSummarizing && contentStream.length > 0 ? (
                  <div className="prose-summary text-sm leading-relaxed"><Markdown source={contentStream} /></div>
                ) : hasSummary ? (
                  <div className="prose-summary text-sm leading-relaxed"><Markdown source={draft.summary} /></div>
                ) : contentStream.length > 0 ? (
                  <div className="prose-summary text-sm leading-relaxed"><Markdown source={contentStream} /></div>
                ) : isSummarizing ? (
                  <SkeletonLines lines={5} />
                ) : (
                  <PanelEmpty
                    icon={<Sparkles size={22} strokeWidth={1.5} />}
                    text={
                      recActive
                        ? "The summary is generated after you stop the recording."
                        : "No summary yet. Use Summarize in the toolbar to generate one from your notes and transcript."
                    }
                  />
                )}
              </div>
            ) : activeTab === "chat" ? (
              chatTarget && <ChatPanel target={chatTarget} onControls={handleChatControls} />
            ) : (
              <div className="flex-1 min-h-0 flex flex-col px-4 py-4">
                <div className="flex flex-wrap items-center gap-2 mb-4 shrink-0">
                  <LanguagePicker
                    value={draft.language || uiLang}
                    onChange={(v) => patch("language", v)}
                  />
                  <SpeakersPicker
                    value={draft.expected_speakers}
                    onChange={async (n) => {
                      if (!draft || readOnly) return;
                      const next = { ...draft, expected_speakers: n };
                      setDraft(next);
                      await ipc.updateNote(draft.id, { expected_speakers: n });
                      upsert(next);
                    }}
                  />
                </div>

                {(isRecording || isPaused) && <ListeningHeader noteId={draft.id} />}

                {hasTranscript ? (
                  <>
                    <div className="shrink-0">
                      <SpeakerLabels
                        transcript={draft.transcript}
                        readOnly={readOnly}
                        suggestions={speakerSuggestions}
                        otherNotesWithLabel={otherNotesWithLabel}
                        onRenameEverywhere={(oldLabel, newLabel) => {
                          if (readOnlyRef.current) return;
                          // This note included, through the same per-note path a
                          // single rename takes — one rewrite implementation, and
                          // `notes_update` pings the sync observer correctly.
                          // Undo comes free: `db::update_note` snapshots a
                          // revision on every transcript change.
                          void renameSpeakerAcrossNotes({
                            notes: notesForSweep,
                            oldLabel,
                            newLabel,
                            onRewritten: (noteId, transcript) => {
                              replaceTranscript(noteId, transcript);
                              // The open note's own draft, so the pills and the
                              // transcript view flip with everything else.
                              if (noteId === draft.id) {
                                setDraft((d) => (d ? { ...d, transcript } : d));
                                setTimeline((tl) =>
                                  tl.map((e) =>
                                    e.label === oldLabel ? { ...e, label: newLabel } : e,
                                  ),
                                );
                              }
                            },
                          }).then((outcome) => {
                            useRecordingStore
                              .getState()
                              .pushFlash(renameOutcomeMessage(outcome));
                          });
                        }}
                        onRename={(oldLabel, newLabel) => {
                          if (readOnlyRef.current) return;
                          patch("transcript", renameSpeakerInTranscript(draft.transcript, oldLabel, newLabel));
                          setTimeline((tl) =>
                            tl.map((e) => (e.label === oldLabel ? { ...e, label: newLabel } : e)),
                          );
                          ipc
                            .noteTimelineRename(draft.id, oldLabel, newLabel)
                            .then(() => {
                              // Re-upload the rewritten timelines for shared
                              // notes (#16); a no-op for Personal notes.
                              void ipc.uploadNoteSessions(draft.id);
                            })
                            .catch((err) => console.error("noteTimelineRename failed", err));
                        }}
                      />
                      {!readOnly && (
                        <RediarizeAction noteId={draft.id} keepAudio={keepAudio} />
                      )}
                      {devMode && <DiagnosticsLinks noteId={draft.id} />}
                    </div>
                    {/* The styled reader is driven by the timeline, not by the
                        audio: with keep_audio off (#24) the WAV is absent but
                        timeline.jsonl is still written, so speaker pills,
                        session dividers and rename all still work — the player
                        row is what disappears. */}
                    {timeline.length > 0 ? (
                      <TranscriptPlayer
                        noteId={draft.id}
                        timeline={timeline}
                        setTimeline={setTimeline}
                        sessions={sessions}
                        fallbackPlaybackUrl={playbackUrl}
                        audioAvailable={
                          !!playbackUrl || sessions.some((s) => s.hasPlayback)
                        }
                        keepAudio={keepAudio}
                        transcript={draft.transcript}
                        onChange={onTranscriptChange}
                        disabled={readOnly || recActive}
                        fill
                        bottomAligned={transcriptLive}
                      />
                    ) : (
                      <TranscriptEditor
                        value={draft.transcript}
                        onChange={onTranscriptChange}
                        disabled={readOnly || recActive}
                        fill
                        bottomAligned={transcriptLive}
                      />
                    )}
                    {isRecording && <SkeletonLines lines={2} className="mt-3 shrink-0" />}
                  </>
                ) : recActive ? (
                  <SkeletonLines lines={4} />
                ) : (
                  <PanelEmpty
                    icon={<MessageSquare size={22} strokeWidth={1.5} />}
                    text="No transcript yet. Start a recording from the toolbar to capture and transcribe audio."
                  />
                )}

                {(isRecording || isPaused) && <LiveHint />}
              </div>
            )}
          </div>
        </aside>
    </div>
  );
}

// Live "listening" banner shown at the top of the Transcript tab while a
// recording is in flight. Mirrors the floating bar's status (mic/sys
// seconds, chunk count, elapsed) but in-context, so the panel reads as
// actively capturing even before the first words land. Self-subscribes to
// the recording store so the parent Note doesn't re-render on every
// diagnostic tick.
function ListeningHeader({ noteId }: { noteId: string }) {
  const status = useRecordingStore((s) => s.status);
  const phase = status.noteId === noteId ? status.phase : "idle";
  const diag = useRecordingStore((s) => s.diag);
  const showDiag = !!diag && diag.noteId === noteId;
  const paused = phase === "paused";

  const [elapsed, setElapsed] = useState(0);
  useEffect(() => {
    if (phase !== "recording" && phase !== "paused") {
      setElapsed(0);
      return;
    }
    if (phase === "paused") return; // hold the timer while paused
    const start = Date.now() - elapsed * 1000;
    const t = window.setInterval(() => setElapsed(Math.floor((Date.now() - start) / 1000)), 250);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [phase]);

  return (
    <div className="shrink-0 mb-4 flex items-center gap-3 px-3 py-2.5 rounded-[var(--radius)] border border-[var(--color-line)] bg-[var(--color-surface-2)]">
      <span className="inline-flex items-center gap-[7px] shrink-0 text-[12.5px] font-semibold text-[var(--color-record)] whitespace-nowrap">
        <span className={cn("inline-block w-2 h-2 rounded-full bg-[var(--color-record)]", !paused && "rec-dot")} />
        {paused ? "Paused" : "Recording"}
      </span>
      {showDiag && (
        <span className="min-w-0 truncate text-[12px] text-[var(--color-text-muted)] tabular-nums">
          mic {(diag.micFrames / 16000).toFixed(0)}s · sys {(diag.sysFrames / 16000).toFixed(0)}s · {diag.chunks} chunk{diag.chunks === 1 ? "" : "s"}
        </span>
      )}
      <span className="ml-auto shrink-0 text-[11px] text-[var(--color-text-disabled)] tabular-nums">{formatElapsed(elapsed)}</span>
    </div>
  );
}

// Footer note under the live transcript — sets the expectation that speaker
// labels only appear after the post-stop diarize pass (during recording the
// lines arrive plain, in capture order).
function LiveHint() {
  return (
    <div className="shrink-0 mt-3 flex items-center gap-1.5 text-[12px] text-[var(--color-text-disabled)]">
      <Users size={13} strokeWidth={1.6} />
      Speakers are identified when you stop.
    </div>
  );
}

function formatElapsed(s: number) {
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${m}:${r.toString().padStart(2, "0")}`;
}

// Note toolbar — back link, primary actions (Record / Summarize), the
// context-panel toggle, and an overflow menu. Replaces the old cryptic
// copy/refresh/more icon row. Record + Summarize appear only when idle
// (not readOnly); during a recording the floating bar takes over.
function NoteToolbar({
  noteId,
  backTo,
  backLabel,
  readOnly,
  recActive,
  canRecord,
  panelOpen,
  onTogglePanel,
  onSummarizeFailed,
  sidebarCollapsed,
}: {
  noteId: string;
  backTo: string;
  backLabel: string;
  readOnly: boolean;
  recActive: boolean;
  canRecord: boolean;
  panelOpen: boolean;
  onTogglePanel: () => void;
  // Summarize streams into the Summary panel, whose state lives in `Note` —
  // so a failure here has to reach up there to drop the partial response.
  onSummarizeFailed: () => void;
  sidebarCollapsed: boolean;
}) {
  const navigate = useNavigate();
  const removeLocal = useNotesStore((s) => s.removeLocal);
  const note = useNotesStore((s) => s.notes.find((n) => n.id === noteId));
  const [menuPos, setMenuPos] = useState<{ x: number; y: number } | null>(null);
  const [exportOpen, setExportOpen] = useState(false);

  async function record() {
    // Pre-flight: don't start a doomed recording when the pipeline isn't
    // functional (same shared predicate as the nag chip + recap). Covers a
    // missing mic AND a missing/unconfigured STT path. A failed Record click
    // is an ERROR, not a confirmation: it goes through the persistent error
    // toast (which carries the Open Settings affordance), never a
    // 2.5-second flash — that read as "the button did nothing".
    try {
      const setup = await computeSetupStatus();
      if (!setup.pipelineReady) {
        const downloading = useDownloadStore.getState().active;
        const message = !setup.micGranted
          ? "Can't record — microphone permission isn't granted yet. Grant it in Settings."
          : setup.stt.kind === "local"
            ? downloading?.modelId === setup.stt.model
              ? `Can't record yet — the transcription model (${setup.stt.model}) is still downloading. Try again when it finishes.`
              : `Can't record — the selected transcription model (${setup.stt.model}) isn't downloaded. Download it in Settings → Transcription.`
            : `Can't record — no API key stored for ${setup.stt.provider}. Add one in Settings → Transcription.`;
        // Sticky: this blocks the user's next action; auto-dismissing it
        // read as "the button did nothing". NOTE: mentioning "Settings" in
        // these messages is what makes the Toaster attach its Open Settings
        // shortcut — keep that word if you rewrite the copy.
        useRecordingStore.getState().pushError({ noteId, message, sticky: true });
        return;
      }
    } catch {
      // If the predicate can't be computed, fall through and attempt the
      // recording — the backend still surfaces real failures as errors.
    }
    try {
      await ipc.recordingStart(noteId);
    } catch (e) {
      useRecordingStore.getState().pushError({ noteId, message: String(e) });
    }
  }
  async function onDelete() {
    setMenuPos(null);
    await ipc.deleteNote(noteId);
    removeLocal(noteId);
    navigate(backTo);
  }
  function openMenu(e: React.MouseEvent<HTMLButtonElement>) {
    const rect = e.currentTarget.getBoundingClientRect();
    setMenuPos({ x: rect.right - 160, y: rect.bottom + 4 });
  }

  return (
    <div data-tauri-drag-region className={cn("relative z-30 h-12 shrink-0 flex items-center gap-2 pr-3", sidebarCollapsed ? "pl-[116px]" : "pl-3")}>
      <Link
        to={backTo}
        className="no-drag inline-flex items-center gap-1.5 pl-1.5 pr-2.5 py-1.5 rounded-[var(--radius)] text-[13px] text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-pill-hover)] transition-colors"
      >
        <ChevronLeft size={15} strokeWidth={1.6} />
        <span className="truncate max-w-[180px]">{backLabel}</span>
      </Link>
      <div className="flex-1" />
      {!readOnly && !recActive && (
        <>
          <button onClick={record} disabled={!canRecord} className="no-drag nd-btn" title="Record (⌘R)">
            <Circle size={10} fill="currentColor" strokeWidth={0} className="text-[var(--color-record)]" />
            <span>Record</span>
          </button>
          <button
            onClick={() => void runSummarize(noteId, onSummarizeFailed)}
            className="no-drag nd-btn nd-btn-primary"
            title="Summarize"
          >
            <Sparkles size={15} strokeWidth={1.6} />
            <span>Summarize</span>
          </button>
        </>
      )}
      <button
        onClick={onTogglePanel}
        className={cn("no-drag nd-btn-icon", panelOpen && "is-active")}
        title="Toggle context panel"
        aria-label="Toggle context panel"
        aria-pressed={panelOpen}
      >
        <PanelRight size={16} strokeWidth={1.7} />
      </button>
      {!readOnly && (
        <button onClick={openMenu} className="no-drag nd-btn-icon" title="More" aria-label="More">
          <MoreHorizontal size={16} strokeWidth={1.7} />
        </button>
      )}
      {menuPos && (
        <ContextMenu x={menuPos.x} y={menuPos.y} onClose={() => setMenuPos(null)}>
          {note && (
            <ContextMenuItem
              onClick={() => {
                setMenuPos(null);
                setExportOpen(true);
              }}
            >
              Export…
            </ContextMenuItem>
          )}
          <ContextMenuItem onClick={onDelete} danger>
            Delete note
          </ContextMenuItem>
        </ContextMenu>
      )}
      {note && (
        <ExportModal note={note} open={exportOpen} onClose={() => setExportOpen(false)} />
      )}
    </div>
  );
}

// Centered empty-state for a panel tab (no summary / no transcript yet).
function PanelEmpty({ icon, text }: { icon: React.ReactNode; text: string }) {
  return (
    <div className="flex flex-col items-center text-center gap-3 px-6 py-12 text-[var(--color-text-disabled)]">
      <span aria-hidden>{icon}</span>
      <p className="text-[13px] leading-relaxed max-w-[240px]">{text}</p>
    </div>
  );
}

// A chip-shaped picker for the Summary and Transcript panels: the trigger sizes
// to the SELECTED option (not the widest one, which is what a native <select>
// would do — WKWebView ignores `field-sizing: content`), and the choices come
// from the shared Menu (#114) rather than a system popup. Rows carry a
// checkmark on the active one, matching every other picker in the app.
//
// The trigger wears `nd-meta`, the same chip as the note meta bar's folder /
// client / workspace pickers — these were the app's only bordered picker chips
// (`.nd-ctl`, now deleted), which made two panels' worth of pickers read as a
// different kind of control from every other picker in the app. It takes the
// `is-filled` variant because this row is left-aligned with no metadata beside
// it: a transparent chip there reads as indented prose rather than a control.
export type CtlOption = {
  value: string;
  label: string;
  /** Draws a divider above this row (e.g. built-in presets vs. your own). */
  separatorBefore?: boolean;
};

function CtlSelect({
  icon,
  extra,
  label,
  value,
  onChange,
  title,
  options,
}: {
  icon: React.ReactNode;
  extra?: React.ReactNode;
  label: string;
  value: string;
  onChange: (v: string) => void;
  title?: string;
  options: CtlOption[];
}) {
  return (
    <Menu>
      <MenuTrigger className="nd-meta is-interactive is-filled" title={title} aria-label={title ?? label}>
        {icon}
        {extra}
        <span className="truncate" style={{ maxWidth: 160 }}>{label}</span>
        <ChevronDown size={12} strokeWidth={2} />
      </MenuTrigger>
      <MenuContent aria-label={title ?? label}>
        <MenuRadioGroup value={value} onValueChange={onChange}>
          {options.map((o) => (
            <Fragment key={o.value}>
              {o.separatorBefore && <MenuSeparator />}
              <MenuRadioItem value={o.value}>
                <span className="truncate">{o.label}</span>
              </MenuRadioItem>
            </Fragment>
          ))}
        </MenuRadioGroup>
      </MenuContent>
    </Menu>
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
  const label = (value ? folders.find((f) => f.id === value)?.name : null) ?? "No folder";

  // Same shape as the Client picker below — a none row, the list, and inline
  // creation — so it's the same primitive (#114). It owns the whole chip so the
  // trigger's hit area matches the old full-chip transparent <select> overlay.
  return (
    <SelectablePopover
      ariaLabel="Folder"
      trigger={
        <span className="nd-meta is-interactive">
          <Folder size={14} strokeWidth={1.6} />
          <span className="truncate" style={{ maxWidth: 150 }}>{label}</span>
        </span>
      }
      items={folders.map((f) => ({ id: f.id, label: f.name }))}
      activeId={value}
      onSelect={onChange}
      noneLabel="No folder"
      createLabel="New folder"
      createPlaceholder="Folder name"
      onCreate={async (name) => {
        const folder = await ipc.createFolder(name);
        upsertFolder(folder);
        onChange(folder.id);
      }}
    />
  );
}

// Per-Note Client picker (issue #43).
// SelectablePopover primitive: assign / reassign / unassign plus full inline
// create / rename / delete — all Client management lives here (there's no
// browse-by-Client surface). Mirrors FolderPicker's placement but is richer,
// which is why it carries create/rename/delete where FolderPicker doesn't.
function ClientPicker({
  value,
  onChange,
}: {
  value: string | null;
  onChange: (clientId: string | null) => void;
}) {
  const clients = useNotesStore((s) => s.clients);
  const upsertClient = useNotesStore((s) => s.upsertClient);
  const removeClient = useNotesStore((s) => s.removeClient);
  const current = value ? clients.find((c) => c.id === value) : null;

  return (
    <SelectablePopover
      ariaLabel="Client"
      trigger={
        <span className="nd-meta is-interactive">
          <Building2 size={14} strokeWidth={1.6} />
          <span className="truncate" style={{ maxWidth: 150 }}>
            {current?.name ?? "No client"}
          </span>
          <ChevronDown size={12} strokeWidth={2} />
        </span>
      }
      items={clients.map((c) => ({ id: c.id, label: c.name }))}
      activeId={value}
      onSelect={onChange}
      noneLabel="No client"
      createLabel="New client"
      createPlaceholder="Client name"
      onCreate={async (name) => {
        const client = await ipc.createClient(name);
        upsertClient(client);
        onChange(client.id);
      }}
      onRename={async (id, name) => {
        await ipc.renameClient(id, name);
        const existing = useNotesStore.getState().clients.find((c) => c.id === id);
        if (existing) upsertClient({ ...existing, name });
      }}
      onDelete={async (id) => {
        await ipc.deleteClient(id);
        removeClient(id);
        // If the deleted Client was this note's, reflect the un-tag locally
        // (the backend already un-tagged it during delete).
        if (value === id) onChange(null);
      }}
    />
  );
}

// Move a note between Personal (local-only) and any workspace you belong to.
// Only rendered when signed in (workspaces exist). "Personal" is the none row
// of the shared picker (#114); the chip's workspace-initial badge rides along
// inside the trigger so the whole chip stays clickable.
function WorkspacePicker({
  value,
  onChange,
  badge,
  disabled = false,
}: {
  value: string;
  onChange: (workspaceId: string) => void;
  badge: React.ReactNode;
  disabled?: boolean;
}) {
  const workspaces = useCloudStore((s) => s.status.workspaces);
  const label = value
    ? (workspaces.find((w) => w.id === value)?.name ?? "Workspace")
    : "Personal (this device)";

  // Locked: show the workspace as plain text rather than an editable dropdown.
  // Used when the current user isn't allowed to move this note (not its creator
  // and not a workspace admin) or the note is otherwise read-only.
  if (disabled) {
    return (
      <span
        className="nd-meta text-[var(--color-text-muted)]"
        title="Only the note’s creator or a workspace admin can move it to another workspace."
      >
        {badge}
        {value ? (workspaces.find((w) => w.id === value)?.name ?? "a workspace") : label}
      </span>
    );
  }
  return (
    <SelectablePopover
      ariaLabel="Workspace"
      trigger={
        <span className="nd-meta is-interactive">
          {badge}
          <span className="truncate" style={{ maxWidth: 150 }}>{label}</span>
        </span>
      }
      items={workspaces.map((w) => ({ id: w.id, label: w.name }))}
      activeId={value || null}
      onSelect={(id) => onChange(id ?? "")}
      noneLabel="Personal (this device)"
    />
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

  const builtin = SUMMARY_PRESETS.find((p) => p.value === value);
  const customPrompt = userPrompts.find((p) => `custom:${p.id}` === value);
  const label = builtin
    ? presetLabel(builtin)
    : customPrompt
    ? customPrompt.name
    : value === "custom"
    ? "Custom (legacy)"
    : value.startsWith("custom:")
    ? "(deleted prompt)"
    : value;
  return (
    <CtlSelect
      icon={<FileText size={14} strokeWidth={1.6} />}
      label={label}
      value={value}
      onChange={onChange}
      title="Summary preset"
      options={[
        ...SUMMARY_PRESETS.map((p) => ({ value: p.value, label: presetLabel(p) })),
        ...userPrompts.map((p, i) => ({
          value: `custom:${p.id}`,
          label: p.name,
          // Divider between the built-ins and your own prompts.
          separatorBefore: i === 0,
        })),
        ...(valueIsMissingUserPrompt ? [{ value, label: "(deleted prompt)" }] : []),
        ...(value === "custom" ? [{ value: "custom", label: "Custom (legacy)" }] : []),
      ]}
    />
  );
}

function LanguagePicker({
  value,
  onChange,
}: {
  value: string;
  onChange: (v: string) => void;
}) {
  const current = LANGUAGES.find((l) => l.value === value);
  return (
    <CtlSelect
      icon={<Languages size={14} strokeWidth={1.6} />}
      label={current ? languageOptionLabel(current) : value}
      value={value}
      onChange={onChange}
      title="Transcription language"
      options={LANGUAGES.map((l) => ({ value: l.value, label: languageOptionLabel(l) }))}
    />
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
  // Internal sentinel: 0 stands in for `null` (auto), since the picker's
  // values are strings. Convert at the boundary.
  const selected = value ?? 0;
  return (
    <CtlSelect
      icon={<Users size={14} strokeWidth={1.6} />}
      label={selected === 0 ? "Auto" : `${selected} speakers`}
      value={String(selected)}
      onChange={(v) => {
        const n = parseInt(v, 10);
        onChange(n > 0 ? n : null);
      }}
      title="Expected speakers — diarization hint. 'Auto' lets the model decide."
      options={SPEAKER_OPTIONS.map((o) => ({
        value: String(o.value),
        label: o.label === "Auto" ? "Auto" : `${o.label} speakers`,
      }))}
    />
  );
}

// Per-note summary provider — a simple Cloud / Local toggle. Defaults to the
// global Settings value when the note has no explicit choice (value === "").
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
  const label = effective === "local" ? "Local" : "Cloud";
  return (
    <CtlSelect
      icon={<Cloud size={14} strokeWidth={1.6} />}
      label={label}
      value={effective}
      onChange={onChange}
      title="Where this note's summary runs"
      options={[
        { value: "openai", label: "Cloud" },
        { value: "local", label: "Local" },
      ]}
    />
  );
}

// Small copy-to-clipboard button rendered in the Summary panel header.
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
// have; red (--color-speaker-4) last because the design language reserves it
// for "interrupt only" — a five-speaker meeting will still get red, but
// for the common 2–3 speaker case it stays out of the way.
// SPEAKER_COLORS / speakerColorMap / SpeakerLabels / SpeakerChip moved to
// ../components/SpeakerLabels (imported above) so the chip strip — now with
// the merge affordance (#23) — can be unit-tested in isolation.
// extractSpeakerLabels / renameSpeakerInTranscript live in ../lib/speakers.

const TranscriptEditor = memo(function TranscriptEditor({
  value,
  onChange,
  disabled,
  fill,
  bottomAligned,
}: {
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
  fill?: boolean;
  bottomAligned: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const taRef = useRef<HTMLTextAreaElement | null>(null);

  // Auto-size the textarea on every keystroke. MUST be its own effect with
  // only `value` in deps — folding focus/setSelectionRange into the same
  // effect resets the cursor to the end every time the user types one
  // character (because the effect re-runs on each value change).
  useEffect(() => {
    if (!editing || fill) return;
    const el = taRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }, [editing, value, fill]);

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
        className={cn(
          "nd-bare w-full resize-none text-sm leading-relaxed text-[var(--color-text-muted)] focus:outline-none",
          fill && "flex-1 min-h-0 overflow-y-auto",
        )}
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
      fill={fill}
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
  fill,
  bottomAligned,
}: {
  transcript: string;
  onClick: () => void;
  disabled: boolean;
  fill?: boolean;
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
        (fill ? "flex-1 min-h-0 " : "") +
        (disabled ? "cursor-default" : "cursor-text")
      }
      style={fill ? undefined : { maxHeight: "14rem" }}
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
                  {`${line.lead}${line.label}: ${line.rest}`}
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
// Exported for unit tests (session-switch seek behaviour, BUG A/B).
export const TranscriptPlayer = memo(function TranscriptPlayer({
  noteId,
  timeline,
  setTimeline,
  sessions,
  fallbackPlaybackUrl,
  audioAvailable,
  keepAudio,
  transcript,
  onChange,
  disabled,
  fill,
  bottomAligned,
}: {
  noteId: string;
  timeline: TimelineEntry[];
  setTimeline: React.Dispatch<React.SetStateAction<TimelineEntry[]>>;
  // Recording sessions (#16) in order. One <audio> element plays the
  // *active* session's playback.wav; the reader shows every session's text.
  sessions: NoteSession[];
  // Latest/legacy single-file playback URL, used when a session has no
  // resolvable per-session file yet (e.g. a downloaded workspace note).
  fallbackPlaybackUrl: string | null;
  // Whether anything is playable at all (#24). False for a note recorded (or
  // synced) while `keep_audio` was off: the timeline is on disk, the WAV isn't,
  // so the reader renders in full and the player is replaced by one line of
  // explanation. `keepAudio` is the current setting, and only picks which
  // explanation — pointing at a setting that is already on would be noise.
  audioAvailable: boolean;
  keepAudio: boolean;
  transcript: string;
  onChange: (v: string) => void;
  disabled: boolean;
  fill?: boolean;
  bottomAligned: boolean;
}) {
  const audioRef = useRef<HTMLAudioElement | null>(null);
  // Which session's audio is loaded in the player. Defaults to the first
  // session that has a playback file. Clicking a pill (or a word in another
  // session) switches it.
  const [activeSessionId, setActiveSessionId] = useState<string | null>(
    () => (sessions.find((s) => s.hasPlayback) ?? sessions[0])?.id ?? null,
  );
  const activeSessionIdRef = useRef(activeSessionId);
  activeSessionIdRef.current = activeSessionId;
  // Keep the active session valid as sessions load / change.
  useEffect(() => {
    if (sessions.length === 0) return;
    if (!sessions.some((s) => s.id === activeSessionId)) {
      setActiveSessionId((sessions.find((s) => s.hasPlayback) ?? sessions[0]).id);
    }
  }, [sessions, activeSessionId]);

  // A seek (and optional resume) to apply once the freshly-swapped <audio>
  // source has loaded — how "switch session then seek-and-keep-playing"
  // survives the src change.
  const pendingSeekRef = useRef<{ ms: number; play: boolean } | null>(null);

  // Consume any queued seek against the already-loaded <audio>. Used both when
  // the src is about to reload (via loadeddata) and when it WON'T reload (same
  // resolved url) — in the latter case no loadeddata fires, so the seek must be
  // applied inline or it strands and later misfires on the next genuine swap
  // (BUG A/B). Guarded on readyState so we don't seek an element that has no
  // media yet; a not-yet-ready element still has loadeddata coming.
  function applyPendingSeek() {
    const pending = pendingSeekRef.current;
    const audio = audioRef.current;
    if (!pending || !audio) return;
    if (audio.readyState < 1 /* HAVE_METADATA */) return;
    pendingSeekRef.current = null;
    audio.currentTime = pending.ms / 1000;
    if (pending.play) audio.play().catch(() => {});
  }

  // Resolve the active session's playback.wav → tauri asset URL. Falls back
  // to the single-file URL for notes without per-session files.
  const [activeUrl, setActiveUrl] = useState<string | null>(fallbackPlaybackUrl);
  // Mirror of activeUrl so the resolver can tell whether the <audio> src is
  // actually about to change. When two sessions resolve to the SAME url (multiple
  // takes sharing legacy notes.audio, a downloaded workspace note, or a legacy /
  // unmatched-session timeline), setting the same src fires no loadeddata.
  const activeUrlRef = useRef<string | null>(fallbackPlaybackUrl);
  useEffect(() => {
    let cancelled = false;
    (async () => {
      let nextUrl: string | null;
      if (!activeSessionId) {
        nextUrl = fallbackPlaybackUrl;
      } else {
        const p = await ipc
          .noteSessionPlaybackPath(noteId, activeSessionId)
          .catch(() => null);
        if (cancelled) return;
        nextUrl = p ? convertFileSrc(p) : fallbackPlaybackUrl;
      }
      if (cancelled) return;
      const sameUrl = nextUrl === activeUrlRef.current;
      activeUrlRef.current = nextUrl;
      setActiveUrl(nextUrl);
      // If the src won't change, loadeddata won't fire — apply any queued seek
      // now against the already-loaded element instead of stranding it.
      if (sameUrl) applyPendingSeek();
    })();
    return () => {
      cancelled = true;
    };
  }, [activeSessionId, noteId, fallbackPlaybackUrl]);
  const [isPlaying, setIsPlaying] = useState(false);
  // Topmost session divider currently in view — the idle active-pill anchor.
  const [topVisibleSessionId, setTopVisibleSessionId] = useState<string | null>(null);
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
  // single position in the flattened `words` array. Groups break at
  // session boundaries too, so each carries its session identity and a
  // divider anchor (#16).
  const groups = useMemo(() => groupTimeline(timeline), [timeline]);
  const sessionById = useMemo(() => {
    const m = new Map<string, NoteSession>();
    sessions.forEach((s) => m.set(s.id, s));
    return m;
  }, [sessions]);
  // The visually-active pill: playhead's session while playing, else the
  // topmost divider in view (idle scroll orientation).
  const activePillId = resolveActivePill({
    playing: isPlaying,
    playheadSessionId: activeSessionId,
    topVisibleSessionId,
    sessions,
  });

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
      const active = activeSessionIdRef.current;
      for (let i = 0; i < tl.length; i++) {
        const e = tl[i];
        // Karaoke/seek only track the session whose playback.wav is loaded.
        // Other sessions' text still renders (reader fix) — it just never
        // lights up, and its local times (which restart at 0) never match
        // this playhead. Skip before the sorted-break so an earlier
        // session's entries don't stop the scan prematurely.
        if (active !== null && e.sessionId !== active) continue;
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
      setIsPlaying(true);
      if (raf) return;
      raf = requestAnimationFrame(tick);
    };
    const stop = () => {
      setIsPlaying(false);
      cancelAnimationFrame(raf);
      raf = 0;
      computeAndSync();
    };
    // When the source swaps to a newly-selected session, apply any pending
    // seek (and resume) once the new audio is ready. This is how a pill
    // click / cross-session word click seeks-and-keeps-playing across the
    // src change.
    const onLoaded = () => {
      applyPendingSeek();
      computeAndSync();
    };
    audio.addEventListener("play", start);
    audio.addEventListener("playing", start);
    audio.addEventListener("pause", stop);
    audio.addEventListener("ended", stop);
    audio.addEventListener("seeked", computeAndSync);
    audio.addEventListener("loadeddata", onLoaded);
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
      audio.removeEventListener("loadeddata", onLoaded);
    };
  }, []);

  useEffect(() => {
    if (!editing || fill) return;
    const el = taRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }, [editing, transcript, fill]);

  useEffect(() => {
    if (!editing) return;
    const el = taRef.current;
    if (!el) return;
    el.focus({ preventScroll: true });
    const len = el.value.length;
    el.setSelectionRange(len, len);
  }, [editing]);

  // Seek to a millisecond offset within a given session. When it's the
  // already-loaded session, seek + play inline; otherwise swap the source to
  // that session and apply the seek once it loads. Clicking a word/line
  // always starts playback (matches the pre-session behaviour).
  function seekInSession(sessionId: string, ms: number) {
    if (sessionId === activeSessionIdRef.current) {
      const a = audioRef.current;
      if (!a) return;
      a.currentTime = ms / 1000;
      a.play().catch(() => {});
      return;
    }
    pendingSeekRef.current = { ms, play: true };
    setActiveSessionId(sessionId);
  }

  // Click a session pill: scroll that session's first turn to the top and
  // seek the player to its start, keeping the current play state
  // (seek-and-keep-playing). Read-only carousel — no delete in v1.
  function selectSession(sessionId: string) {
    const groupIdx = groups.findIndex((g) => g.sessionId === sessionId);
    if (groupIdx >= 0) virtualizer.scrollToIndex(groupIdx, { align: "start" });
    setTopVisibleSessionId(sessionId);
    if (sessionId === activeSessionIdRef.current) {
      const a = audioRef.current;
      if (a) a.currentTime = 0;
      return;
    }
    const wasPlaying = audioRef.current ? !audioRef.current.paused : false;
    pendingSeekRef.current = { ms: 0, play: wasPlaying };
    setActiveSessionId(sessionId);
  }

  // Idle active-pill tracking: on scroll, find the topmost rendered turn and
  // adopt its session so the lit pill follows the reader (#16).
  function handleScroll() {
    const el = scrollRef.current;
    if (!el) return;
    const st = el.scrollTop;
    const items = virtualizer.getVirtualItems();
    const vis = items.find((it) => it.start + it.size > st) ?? items[0];
    if (vis) {
      const sid = groups[vis.index]?.sessionId;
      if (sid) setTopVisibleSessionId(sid);
    }
  }

  // Drop a single chunk row. Used to remove off-topic content the
  // mic / sys captured (unrelated speech, mis-attributed leak, etc.)
  // without re-recording. Optimistic local update so the row
  // disappears instantly, then the IPC rebuilds note.transcript
  // from the surviving entries.
  async function deleteGroup(g: { indices: number[] }) {
    if (disabled) return;
    const set = new Set(g.indices);
    // Map each merged-timeline index to its (session, local chunk index) so
    // the edit routes to the right session file. A group is single-session,
    // so delete highest local index first — each backend delete shifts the
    // remaining indices in that file.
    const targets = g.indices
      .map((ci) => ({ sessionId: timeline[ci]?.sessionId, chunkIdx: timeline[ci]?.chunkIdx }))
      .filter((t): t is { sessionId: string; chunkIdx: number } => t.sessionId != null)
      .sort((a, b) => b.chunkIdx - a.chunkIdx);
    setTimeline((tl) => tl.filter((_, i) => !set.has(i)));
    for (const t of targets) {
      try {
        await ipc.noteTimelineDeleteChunk(noteId, t.sessionId, t.chunkIdx);
      } catch (err) {
        console.error("noteTimelineDeleteChunk failed", err);
      }
    }
    // Push the rewritten session timelines to the workspace (#16); Personal
    // notes short-circuit in the backend.
    void ipc.uploadNoteSessions(noteId);
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
    const targets = g.indices
      .map((ci) => ({ sessionId: timeline[ci]?.sessionId, chunkIdx: timeline[ci]?.chunkIdx }))
      .filter((t): t is { sessionId: string; chunkIdx: number } => t.sessionId != null);
    setTimeline((tl) =>
      tl.map((e, i) => (set.has(i) ? { ...e, label: next } : e)),
    );
    for (const t of targets) {
      try {
        await ipc.noteTimelineSetChunkLabel(noteId, t.sessionId, t.chunkIdx, next);
      } catch (err) {
        console.error("noteTimelineSetChunkLabel failed", err);
      }
    }
    // Sync the relabelled session timelines (#16); no-op for Personal notes.
    void ipc.uploadNoteSessions(noteId);
  }

  const showEditor = editing && !disabled;

  return (
    <div className={fill ? "flex flex-col min-h-0 flex-1" : undefined}>
      <div className={cn(fill && "shrink-0")}>
        <RecordingSessions
          sessions={sessions}
          activeId={activePillId}
          onSelect={selectSession}
        />
      </div>
      <div className={cn("flex items-center gap-2 mb-3", fill && "shrink-0")}>
        {audioAvailable ? (
          <audio
            ref={audioRef}
            src={activeUrl ?? undefined}
            controls
            // preload="auto" so the whole WAV streams in up-front and
            // every subsequent seek is in-memory. With "metadata" each
            // user click triggered a range-request through Tauri's
            // asset protocol; rapid clicking flooded I/O and on at
            // least one user's machine made the whole system lag.
            preload="auto"
            className="flex-1 h-8"
          />
        ) : (
          <p className="flex-1 text-xs text-[var(--color-text-muted)]">
            {keepAudio
              ? "No audio saved for this recording."
              : "Audio not stored on this device — Settings → Recording"}
          </p>
        )}
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
          className={cn(
            "nd-bare w-full resize-none text-sm leading-relaxed text-[var(--color-text-muted)] focus:outline-none",
            fill && "flex-1 min-h-0 overflow-y-auto",
          )}
        />
      ) : (
        <div
          ref={scrollRef}
          onScroll={handleScroll}
          className={cn(
            "text-sm leading-relaxed text-[var(--color-text-muted)] overflow-y-auto",
            fill && "flex-1 min-h-0",
          )}
          style={fill ? undefined : { maxHeight: "14rem" }}
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
              {sessions.length > 1 && g.firstInSession && (
                <SessionDivider
                  session={sessionById.get(g.sessionId)}
                  index={g.sessionIndex}
                />
              )}
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
                            seekInSession(g.sessionId, w.start_ms);
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
                    onClick={() => seekInSession(g.sessionId, g.startMs)}
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
                    className="nd-bare shrink-0 self-start opacity-0 group-hover:opacity-100 transition-opacity p-0.5 rounded text-[var(--color-text-muted)] hover:text-[var(--color-danger)] hover:bg-[var(--color-pill-hover)]"
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

// Session divider rendered in the styled reader at each take's first turn
// (#16). Manifest-derived and orientation-only — since it's anchored to the
// timeline (the same source the styled view renders from), it always sits at
// a real session boundary and never drifts, even when the user hand-edits the
// transcript textarea (that edits the DB text, not the timeline).
function SessionDivider({
  session,
  index,
}: {
  session: NoteSession | undefined;
  index: number;
}) {
  const caption = session ? formatSessionCaption(session) : "";
  return (
    <div className="flex items-center gap-2 px-2 pt-3 pb-1 select-none">
      <span className="nd-label shrink-0">Recording {index}</span>
      {caption && caption !== `Recording ${index}` && (
        <span className="text-[10px] text-[color:var(--color-text-muted)] tracking-[0.02em]">
          {caption}
        </span>
      )}
      <span className="h-px flex-1 bg-[color:var(--color-line-visible)]" aria-hidden />
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
// Re-polls on every recording-phase transition so the diarize step's
// diagnostic write becomes visible without a page refresh: depending
// only on `noteId` (which doesn't change) would leave the link hidden
// until the user navigated away and back.
function DiagnosticsLinks({ noteId }: { noteId: string }) {
  const [diagFiles, setDiagFiles] = useState<string[]>([]);
  const [audioFiles, setAudioFiles] = useState<string[]>([]);
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
  if (!hasDiag && !hasAudio) return null;

  async function openDiag() {
    const dir = await ipc.noteDiagnosticsDir(noteId);
    await ipc.openInFinder(dir);
  }
  async function openAudio() {
    const dir = await ipc.noteAudioDir(noteId);
    await ipc.openInFinder(dir);
  }

  return (
    <div className="flex items-center gap-3 text-xs text-[var(--color-text-muted)] mb-3">
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
    </div>
  );
}

// User-facing "Re-diarize" affordance — sits above the transcript player so
// it's adjacent to the speaker chip strip the user just looked at to realise
// the speaker count is wrong. Visible regardless of dev mode.
//
// With no retained audio there is nothing to re-cluster, so it renders greyed
// with a one-line reason instead of vanishing (#24): a silently absent control
// reads as a bug, and the reason is actionable — it names the setting that
// would keep the audio next time. Only shown at all once the note has a
// transcript, so a fresh empty note isn't decorated with a dead control.
//
// Doesn't pre-check chunks.json existence; the backend has a clear error
// message for notes recorded before chunks were persisted (audio is
// there, chunks aren't) so we surface that on click rather than hiding
// the button entirely.
function RediarizeAction({
  noteId,
  keepAudio,
}: {
  noteId: string;
  keepAudio: boolean;
}) {
  const [hasAudio, setHasAudio] = useState(false);
  const [multiSession, setMultiSession] = useState(false);
  const [rediarizing, setRediarizing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const phase = useRecordingStore((s) => s.status.phase);

  useEffect(() => {
    let cancelled = false;
    ipc.noteAudioFiles(noteId)
      .then((f) => {
        if (!cancelled) setHasAudio(f.length > 0);
      })
      .catch(() => {});
    // On a multi-session note the backend runs the cross-session unify
    // pass (#17) instead of the latest-take re-diarize, so the copy
    // should say what will actually happen.
    ipc.noteSessions(noteId)
      .then((s) => {
        if (!cancelled) setMultiSession(s.length > 1);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [noteId, phase]);

  // Greyed, not gone: the control stays visible with its reason beside it, so
  // "why can't I fix these speaker labels" has an answer in place (#24).
  //
  // The reason is deliberately terse — the panel already carries one line
  // where the player would be ("Audio not stored on this device — Settings →
  // Recording"), and stacking a second copy of that pointer above the
  // transcript turned the top of the panel into three lines of grey apology.
  // The full pointer lives in the tooltip.
  if (!hasAudio) {
    return (
      <p className="text-xs mb-3">
        <button
          type="button"
          disabled
          className="text-[var(--color-text-disabled)] cursor-not-allowed"
          title={
            keepAudio
              ? "Speaker re-detection needs the recording's audio, which isn't saved for this note."
              : "Speaker re-detection needs stored audio — turn on Keep recorded audio in Settings → Recording."
          }
        >
          Re-diarize speakers
        </button>
        <span className="text-[var(--color-text-muted)]">
          {" · needs stored audio"}
        </span>
      </p>
    );
  }

  async function rediarize() {
    setRediarizing(true);
    setError(null);
    try {
      await ipc.rediarizeNote(noteId);
      // Re-diarize rewrote the session timelines — push them to the
      // workspace (#16); a no-op for Personal notes.
      void ipc.uploadNoteSessions(noteId);
    } catch (e) {
      setError(String(e));
    } finally {
      setRediarizing(false);
    }
  }

  return (
    <div className="flex flex-col gap-1 mb-3">
      <button
        type="button"
        onClick={rediarize}
        disabled={rediarizing}
        className="self-start text-xs underline text-[var(--color-text-muted)] hover:text-[var(--color-text)] disabled:opacity-50"
        title={
          multiSession
            ? "Re-run speaker detection across all recordings on this note, so the same voice gets one label everywhere"
            : "Re-run speaker detection using the saved audio and the speaker count above"
        }
      >
        {rediarizing
          ? multiSession
            ? "Unifying speakers…"
            : "Re-diarizing…"
          : multiSession
            ? "Unify speakers across recordings"
            : "Re-diarize speakers"}
      </button>
      {error && (
        <p className="text-xs text-red-600 dark:text-red-400 break-all">
          {error}
        </p>
      )}
    </div>
  );
}
