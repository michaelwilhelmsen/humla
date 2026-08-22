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
  Pencil,
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
import { billingCta, planIsLive } from "../lib/billing";
import { extractSpeakerLabels, renameSpeakerInTranscript } from "../lib/speakers";
import { htmlToText } from "../lib/noteList";
import { shouldAdoptRemoteBody, shouldAdoptRemoteTitle, shouldRequestTitleForBody } from "../lib/noteSync";
import { SpeakerLabels, speakerColorMap } from "../components/SpeakerLabels";
import { RecordingSessions } from "../components/RecordingSessions";
import {
  formatDuration,
  groupTimeline,
  needsSessionPull,
  resolveActivePill,
  formatSessionCaption,
  sessionTitle,
  type TimelineGroup,
} from "../lib/sessions";
import { RecordingBar } from "../components/RecordingBar";
import { NewWorkspaceModal } from "../components/NewWorkspaceModal";
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

/** Kick off a transcription run (#146), surfacing failures as a toast.
 *
 * `"pending"` finishes what a "Transcribe manually" capture left waiting;
 * `"all"` re-runs every take that still has its audio, replacing text the note
 * already had. Both go through here so neither can become an unhandled
 * rejection — the command refuses outright while the note is recording, and a
 * silent rejection there would look like a button that did nothing.
 *
 * Nothing local to track: the backend brackets the whole replay with
 * `transcribe_status`, and both buttons read that.
 */
async function runTranscribe(noteId: string, scope: "pending" | "all") {
  try {
    await ipc.transcribeNote(noteId, scope);
  } catch (e) {
    useRecordingStore.getState().pushError({ noteId, message: String(e) });
  }
  // No session-asset push here, deliberately: the run rewrites each take's
  // `timeline.jsonl`, and `transcribe_takes` pushes them itself. Doing it from
  // the view would leave every other caller of the command (the menu bar, a
  // future surface) silently skipping it.
}

/** Context-panel width bounds, and the body column's floor.
 *
 * `BODY_MIN` is measured, not chosen: the note toolbar's irreducible width is
 * the traffic-light gutter a collapsed sidebar spends (116px) plus a chevron,
 * three icon-only action buttons and two icon buttons — 391px in the warm
 * theme once every label has dropped (see `NoteToolbar`'s degradation steps,
 * and `scripts/measure-toolbar.js` for how to re-measure). 420 clears that with
 * ~30px spare for a future theme's wider controls.
 *
 * `PANEL_FLOOR` is below `PANEL_MIN` on purpose. On a window too narrow to give
 * both columns their nominal minimum, a panel a little under its comfortable
 * width is a better answer than a body column that cannot hold its own toolbar
 * — which is what the old flat 320–720 clamp produced, since it never looked at
 * the window at all. */
const PANEL_MIN = 320;
const PANEL_FLOOR = 260;
const PANEL_MAX = 720;
const BODY_MIN = 420;

/** How long the body has to sit still before #90's typed-note titler fires.
 *
 * Long enough that it reads as "the user stopped writing", not "the user paused
 * to think" — this spends a model call, and the note has the rest of its life to
 * get a title. */
const TITLE_BODY_SETTLE_MS = 10_000;

/** The note's title: an autosizing single-line box, or — while a title is being
 * written for it (#90) — a shimmer standing in for it.
 *
 * Standing IN for the box rather than dimming it is the point. The text there
 * is about to be replaced, so leaving it on screen faded shows the wrong answer
 * while the right one loads. The wrapper carries `nd-title` and an invisible
 * space so the line box is exactly the height the real title occupies and the
 * meta row beneath it can't jump.
 *
 * Exported for the `pnpm mock` harness (see TranscriptEditor): the two states
 * are a layout question, and jsdom can't answer one.
 */
export function NoteTitleBox({
  title,
  onChange,
  readOnly,
  writing,
}: {
  title: string;
  onChange: (value: string) => void;
  readOnly: boolean;
  writing: boolean;
}) {
  const ref = useRef<HTMLTextAreaElement | null>(null);

  // Auto-grow so long titles wrap onto a second line instead of horizontally
  // clipping at the right edge of the page.
  const fit = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }, []);
  useEffect(fit, [fit, title, writing]);

  // Wrapping depends on the column's width, not only on the text, and the column
  // widens when the context panel closes or is dragged. Watching React state
  // won't do: the effect would run at commit, before the 300ms max-width
  // transition has moved, and re-bake the pre-transition height — which is how a
  // title that no longer wraps kept its two-line height and left a gap above the
  // meta bar. Observe the textarea itself so we refit on every frame of the
  // transition and every drag tick. The height writes are idempotent, so this
  // settles after one extra callback rather than looping.
  //
  // `writing` is a dependency of both because the textarea UNMOUNTS behind the
  // shimmer — without re-running, these would measure, and watch, a node that is
  // no longer in the document once the real box comes back.
  useEffect(() => {
    const el = ref.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let lastWidth = -1;
    const ro = new ResizeObserver(() => {
      const w = el.clientWidth;
      if (w === lastWidth) return;
      lastWidth = w;
      fit();
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [fit, writing]);

  if (writing) {
    return (
      <div className="nd-title mb-4 relative" role="status" aria-label="Writing a title">
        <span className="invisible">&nbsp;</span>
        <span className="skeleton absolute left-0 top-[0.2em] bottom-[0.2em] w-[55%]" />
      </div>
    );
  }
  return (
    <textarea
      ref={ref}
      value={title}
      onChange={(e) => onChange(e.target.value)}
      readOnly={readOnly}
      onKeyDown={(e) => {
        // Block Enter so the title behaves like a single-line conceptual field —
        // text still wraps when wider than the column, but the user can't
        // accidentally introduce a literal newline.
        if (e.key === "Enter") {
          e.preventDefault();
          (e.currentTarget as HTMLTextAreaElement).blur();
        }
      }}
      placeholder="New note"
      rows={1}
      className="nd-bare nd-title block w-full mb-4 placeholder:text-[var(--color-text-muted)]/50 resize-none overflow-hidden focus:outline-none"
    />
  );
}

/** Regenerate a note's title on demand (#90), surfacing any failure as a toast.
 *
 * The automatic titler is silent — a user who never asked for a title must not
 * be told one failed. This one is not: the user pressed a button and is owed an
 * answer, including "the model gave back nothing usable, so your title is
 * unchanged".
 */
async function runGenerateTitle(noteId: string, onTitle: (title: string) => void) {
  try {
    const title = await ipc.noteGenerateTitle(noteId, true);
    if (title) {
      onTitle(title);
      return;
    }
    useRecordingStore.getState().pushError({
      noteId,
      kind: "title",
      message: "The model gave back nothing usable, so the title is unchanged.",
    });
  } catch (e) {
    useRecordingStore.getState().pushError({ noteId, kind: "title", message: String(e) });
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
  const lockedByPlan = billingEnabled && !!noteWs && !planIsLive(noteWs);
  // The owner can resolve a plan lock from right here, so the banner offers the
  // trial sheet instead of naming a Settings path to walk to.
  const canFixPlan = lockedByPlan && noteWs?.role === "owner";
  const readOnly = !!draft?.workspace_id && (isViewer || lockedByPlan);
  // Mirror into a ref so the memoised patch callbacks can gate without changing
  // identity (which would bust the transcript-view memos).
  const readOnlyRef = useRef(readOnly);
  readOnlyRef.current = readOnly;
  const [billingOpen, setBillingOpen] = useState(false);
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
    // `PANEL_FLOOR`, not `PANEL_MIN`: a width persisted from a cramped window
    // is legitimately under the nominal minimum, and rejecting it would reset
    // the panel to 440 on every reopen there.
    return saved >= PANEL_FLOOR && saved <= PANEL_MAX ? saved : 440;
  });
  // The width the two columns share, watched rather than derived: it is the
  // window minus the nav card, and the nav card collapses. Drives the clamp
  // that keeps the panel from squeezing the body column past what its toolbar
  // can survive — see PANEL_MIN / BODY_MIN.
  const columnsRef = useRef<HTMLDivElement | null>(null);
  const [columnsWidth, setColumnsWidth] = useState(0);
  useEffect(() => {
    const el = columnsRef.current;
    if (!el) return;
    setColumnsWidth(el.clientWidth);
    const ro = new ResizeObserver(([entry]) => setColumnsWidth(entry.contentRect.width));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  // How wide the panel may actually be right now. Before #146 the drag handler
  // clamped to a flat 320–720 with no regard for the window, so on a
  // minimum-size window the panel could be dragged over the whole view and
  // leave the body column at zero — taking its toolbar with it. The floor is
  // `PANEL_FLOOR`, not `PANEL_MIN`: on a window too narrow for both, a panel
  // slightly under its nominal minimum is a better answer than a body column
  // that can't hold its own toolbar.
  const maxPanelWidth =
    columnsWidth > 0
      ? Math.max(PANEL_FLOOR, Math.min(PANEL_MAX, columnsWidth - BODY_MIN))
      : PANEL_MAX;
  const effectivePanelWidth = Math.min(panelWidth, maxPanelWidth);
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
  // Whether the merged timelines account for every word of the transcript
  // (#169). Repair-on-open makes this true for the shapes it can explain; it
  // stays false for the ones it can't — a timeline present but short because
  // malformed lines were skipped, an asset that never downloaded — and the
  // reader then shows the whole transcript plainly rather than a turn list
  // that silently omits part of it. The comparison is the backend's; the
  // client never re-derives the grouping rule.
  const [timelineCoversTranscript, setTimelineCoversTranscript] = useState(true);
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
  // A title call is in flight for this note (#90) — from the post-stop chain or
  // from ⋯ → Regenerate title; the backend brackets both with the same event,
  // so there is nothing local to keep in step.
  const isTitling = useRecordingStore((s) => !!draft && !!s.titling[draft.id]);
  // A deferred transcription is replaying this note's retained audio (#146).
  // Its own channel, like summary and title: a live recording on another note
  // may be running the whole time, and this must not read as one.
  const isTranscribing = useRecordingStore(
    (s) => !!draft && !!s.transcribing[draft.id],
  );
  // Takes captured with "Transcribe manually" on that still hold their audio
  // (#146). The backend decides both halves — untranscribed AND audio still on
  // disk — so a take whose audio was swept away never offers an action that
  // can only fail.
  const pendingTranscription = sessions.some((sess) => sess.canTranscribe);
  // Any take whose raw streams are still on disk can be re-run — for a
  // recording that came back off the wrong language or the wrong model. Same
  // backend answer as `canTranscribe`, one condition looser.
  const canRetranscribe = sessions.some((sess) => sess.canRetranscribe);
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
  // adjusts its width (clamped to `maxPanelWidth`, which accounts for the
  // window); persisted to localStorage. The width transition is suppressed
  // mid-drag so it tracks the cursor.
  const panelWidthRef = useRef(panelWidth);
  panelWidthRef.current = panelWidth;
  const maxPanelWidthRef = useRef(maxPanelWidth);
  maxPanelWidthRef.current = maxPanelWidth;
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
      // The drag floor is `PANEL_MIN` wherever there's room for it — dragging
      // below the nominal minimum on a wide window was never allowed and
      // isn't now. `PANEL_FLOOR` is reachable only when the cap itself has
      // been pushed under `PANEL_MIN` by a narrow window.
      const cap = maxPanelWidthRef.current;
      const floor = Math.min(PANEL_MIN, cap);
      setPanelWidth(Math.min(cap, Math.max(floor, s.w + (s.x - e.clientX))));
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
      // The automatic titler (#90) is a backend write: it reaches the store via
      // `notes_changed`, and without adopting it here the new title shows up in
      // the sidebar while this note's title box still reads "Recording 19 Aug
      // 14:32". Guarded like the body — see shouldAdoptRemoteTitle.
      const nextTitle = shouldAdoptRemoteTitle(d.title, note.title, "title" in pendingChanges.current)
        ? note.title
        : d.title;
      if (
        d.summary === nextSummary &&
        d.transcript === nextTranscript &&
        d.body === nextBody &&
        d.title === nextTitle
      ) {
        return d;
      }
      return { ...d, summary: nextSummary, transcript: nextTranscript, body: nextBody, title: nextTitle };
    });
  }, [note?.transcript, note?.summary, note?.body, note?.title, allowTranscriptSync]);

  // #90's other half: a note that is typed and never recorded gets its title
  // here, at a body-settled checkpoint. The post-stop recording chain covers
  // everything that was recorded.
  //
  // The effect re-runs on every keystroke, so the timer restarts each time and
  // only a real pause in typing reaches the call — this IS the debounce. The
  // ref makes it one shot per note per view: a model that answers with junk
  // doesn't get asked again every time the user resumes typing, and the ⋯ menu
  // is the way to try again.
  const bodyTitledRef = useRef<string | null>(null);
  useEffect(() => {
    if (!draft) return;
    if (bodyTitledRef.current === draft.id) return;
    if (
      !shouldRequestTitleForBody({
        title: draft.title,
        bodyText: htmlToText(draft.body),
        hasTranscript: draft.transcript.trim() !== "",
        // ANY capture, not this note's. `recActive` is note-scoped, so it would
        // let a typed note B fire a model call while note A records — exactly
        // the contention for the GPU (and for a local Ollama model's slot) that
        // keeping this out of a recording is for.
        recording: recPhase.noteId !== null,
        readOnly,
      })
    ) {
      return;
    }
    const t = window.setTimeout(() => {
      bodyTitledRef.current = draft.id;
      // Silent, like the post-stop path: the user didn't ask for a title, so a
      // provider they haven't configured must not toast them. The backend
      // re-checks eligibility, and `notes_changed` carries the result back.
      void ipc.noteGenerateTitle(draft.id, false).catch(() => {});
    }, TITLE_BODY_SETTLE_MS);
    return () => window.clearTimeout(t);
  }, [draft?.id, draft?.title, draft?.body, draft?.transcript, recPhase.noteId, readOnly]);

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
      // Something's missing locally and the note is shared → pull it from the
      // workspace. Prefer per-session sync (#16): rebuild sessions.json + fetch
      // each take's playback/timeline. Fall back to the legacy single-file
      // notes.audio for notes that predate sessions (or were uploaded by an old
      // client).
      //
      // A MISSING TIMELINE is reason enough to ask, not just missing audio. The
      // gate used to be `!path` alone, which had a trap: the legacy fallback
      // writes a flat playback.wav and no timeline at all, so one visit through
      // it left `path` non-null forever and the per-session pull — the only
      // thing that fetches timelines — was never attempted again for that note.
      // A teammate's note stayed permanently without word timings, and with
      // them, without speaker labels.
      if (
        needsSessionPull({
          shared: !!draft.workspace_id,
          hasLocalPlayback: !!path,
          timelineEntries: tl.length,
        })
      ) {
        let got = await ipc.downloadNoteSessions(draft.id).catch(() => false);
        // Only worth the legacy round-trip if we still have no audio at all.
        if (!got && !path) {
          got = await ipc.downloadNoteAudio(draft.id).catch(() => false);
        }
        if (got && !cancelled) {
          path = await ipc.notePlaybackPath(draft.id).catch(() => null);
          sess = await ipc.noteSessions(draft.id).catch(() => sess);
          tl = await ipc.noteTimeline(draft.id).catch(() => tl);
        }
      }
      if (cancelled) return;

      // Repair before rendering (#169). Transcript text that no session
      // timeline accounts for is invisible in the styled reader, and the first
      // rebuild deletes it outright — cycling a speaker pill is one click and
      // reads as cosmetic. Synthesizing its session here puts the repair ahead
      // of every one of those paths, since all four are UI actions on an
      // already-open note.
      //
      // AFTER the session pull above, not before: a shared note whose
      // timelines are still arriving would otherwise get a synthesized session
      // for text the download was about to account for, and the words would
      // land twice.
      //
      // Viewers repair too. The synthesized session is derived from the note's
      // own transcript and written locally, so a read-only note is no less
      // entitled to it — what a viewer skips is the upload, which the server
      // would reject anyway. Skipping the call outright would leave a
      // teammate's note on the turn list with text missing from it, which is
      // the bug rather than a polite version of it.
      //
      // On failure the answer is "not covered", not "fine": a repair we
      // couldn't run is exactly when the turn list is most likely to be hiding
      // something, and the plain reader loses highlighting rather than words.
      const repair =
        tl.length === 0
          ? { repaired: false, coversTranscript: true }
          : await ipc
              .noteTimelineRepair(draft.id)
              .catch(() => ({ repaired: false, coversTranscript: false }));
      if (cancelled) return;
      if (repair.repaired) {
        tl = await ipc.noteTimeline(draft.id).catch(() => tl);
        sess = await ipc.noteSessions(draft.id).catch(() => sess);
        if (!readOnlyRef.current) void ipc.uploadNoteSessions(draft.id).catch(() => {});
        if (cancelled) return;
      }
      setTimelineCoversTranscript(repair.coversTranscript);

      setPlaybackUrl(path ? convertFileSrc(path) : null);
      setTimeline(tl);
      setSessions(sess);
      // The mirror image, for takes recorded HERE: re-attach any per-session
      // asset the server never received. The post-recording upload is a single
      // fire-and-forget call with no retry, so quitting the app (or a dropped
      // network) inside the minute after a recording stranded the timeline on
      // this device — the note synced, teammates got the text, and nobody ever
      // got the speaker labels. Repairing on open costs one lookup and re-sends
      // nothing the server already holds. Viewers skip it; the server would
      // reject the write anyway.
      if (draft.workspace_id && sess.length > 0 && !readOnlyRef.current) {
        void ipc.repairNoteSessions(draft.id).catch(() => {});
      }
    })();
    return () => {
      cancelled = true;
    };
    // keepAudio is a dep so turning retention back on fetches a shared note's
    // audio right away (#24) rather than on the next open. The backend enforces
    // the rule; this only decides when to ask again.
    //
    // isTranscribing is a dep for the same reason recPhase.phase is (#146): a
    // deferred transcription writes this note's timeline and flips its take to
    // transcribed, and it deliberately never touches the recording phase — so
    // without this the reader would keep rendering the pre-transcription
    // sessions until the user navigated away and back.
  }, [draft?.id, draft?.workspace_id, recPhase.phase, keepAudio, isTranscribing]);

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

  // ⋯ → Regenerate title (#90). The title box goes read-only and dims while the
  // model works — the menu closes on select, so that is the only place a busy
  // state can actually be seen, and it is where the user is already looking.
  //
  // Adopting the result bypasses shouldAdoptRemoteTitle deliberately: that guard
  // refuses to overwrite a user-owned title, and here the user asked for exactly
  // that. The backend has already persisted it, so this only catches the draft up
  // — and drops any queued rename, which the user has just superseded.
  const regenerateTitle = useCallback(() => {
    const cur = draftRef.current;
    if (!cur) return;
    void runGenerateTitle(cur.id, (title) => {
      delete pendingChanges.current.title;
      setDraft((d) => (d ? { ...d, title } : d));
    });
  }, []);

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

  // What Copy hands over (#166), which is not always `draft.transcript`. A
  // per-turn edit, a chunk delete or a label cycle (#170) rebuilds the string
  // in the backend and it arrives on `transcript_replaced`; the draft
  // deliberately refuses to adopt store transcript updates while idle (see
  // `allowTranscriptSync`) so a debounced save can't clobber typing. So read
  // the store — except while our own free-text edit is still queued, where the
  // draft is the newer of the two. Nothing here re-derives the transcript from
  // the timeline: the backend owns that projection (ADR-0004).
  const copyableTranscript = useCallback(
    () =>
      ("transcript" in pendingChanges.current
        ? draftRef.current?.transcript
        : note?.transcript) ??
      draftRef.current?.transcript ??
      "",
    [note?.transcript],
  );

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
    <div ref={columnsRef} className="h-full flex min-h-0">
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
          onRegenerateTitle={regenerateTitle}
          pendingTranscription={pendingTranscription}
          isTranscribing={isTranscribing}
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
            <span className="flex-1">
              {isViewer
                ? "View-only — you have viewer access to this workspace, so this note can’t be edited."
                : !canFixPlan
                  ? "Read-only — this workspace needs an active subscription. Ask the workspace owner to start it."
                  : noteWs?.plan_status === "past_due"
                    ? "Read-only — a payment for this workspace didn’t go through, so nothing syncs and nobody can edit."
                    : "Read-only until this workspace’s plan is live — nothing syncs and nobody can edit."}
            </span>
            {canFixPlan && (
              <button
                type="button"
                onClick={() => setBillingOpen(true)}
                className="shrink-0 font-medium text-[var(--color-accent-text)] hover:underline"
              >
                {/* Shared with the sheet's own CTA: a third hand-rolled cascade
                    on plan_status is how "past_due opens the Portal, not a
                    second Checkout" drifts out of true in one of them. */}
                {noteWs ? billingCta(noteWs).label : "Subscribe"}
              </button>
            )}
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
        {/* Deliberately outside the banner's own condition: a completed checkout
            makes this note writable, which unmounts the banner — and with it
            would go the sheet, one stage before it offers the first invite. */}
        {noteWs && (
          <NewWorkspaceModal
            open={billingOpen}
            onClose={() => setBillingOpen(false)}
            workspaceId={noteWs.id}
          />
        )}
        {/* A title is being written for this note. Standing in for the box
            rather than dimming it says the right thing — the text there is
            about to be replaced, so showing it fading is showing the wrong
            answer. The wrapper carries `nd-title` and an invisible space so the
            line box is exactly the height the real title occupies, and the bar
            can't move the meta row under it. */}
        <NoteTitleBox
          title={draft.title}
          onChange={(v) => patch("title", v)}
          readOnly={readOnly}
          writing={isTitling}
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
        style={{ width: panelOpen ? effectivePanelWidth : 0 }}
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
                  {/* Copy + Re-transcribe, mirroring the Summary group's
                      Copy + Regenerate pair — the two panels' actions should
                      not read as different kinds of control.

                      Copy takes the raw stored string, labels and all: view
                      mode drops the label text in favour of the coloured dot,
                      but a pasted transcript is only useful elsewhere if it
                      says who spoke.

                      Both need a transcript, so the wrapper carries that guard
                      and a note without one gets no empty flex box in the
                      picker row. Re-transcribe is gated on it with Copy, not
                      only on `canRetranscribe`: this control REPLACES a
                      transcript, so on a note whose take was never transcribed
                      it would sit beside the toolbar's Transcribe doing the
                      same work, under a tooltip that isn't true — and spend a
                      revision snapshot of nothing. */}
                  {hasTranscript && (
                    <div className="ml-auto flex items-center gap-0.5">
                      <CopyButton label="Transcript" getText={copyableTranscript} />
                      {!readOnly && canRetranscribe && (
                        <button
                          type="button"
                          onClick={() => void runTranscribe(draft.id, "all")}
                          disabled={isTranscribing || recActive}
                          title={
                            // Names what it replaces. The rewrite has no undo
                            // of its own, so the backend snapshots a note
                            // revision first — but a tooltip that said only
                            // "re-transcribe" would leave the user to discover
                            // that their corrected turns were gone.
                            isTranscribing
                              ? "Transcribing…"
                              : "Re-transcribe from the saved audio, replacing this transcript. Uses the language and speaker count above."
                          }
                          aria-label="Re-transcribe"
                          className="nd-btn-icon nd-btn-icon-sm"
                        >
                          <RefreshCw
                            size={15}
                            strokeWidth={1.6}
                            className={isTranscribing ? "animate-spin" : undefined}
                          />
                        </button>
                      )}
                    </div>
                  )}
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
                    {timeline.length > 0 && !timelineCoversTranscript ? (
                      // The turn list renders from the timeline alone, so any
                      // transcript text the timeline doesn't carry would simply
                      // not be drawn (#169). When repair-on-open couldn't
                      // account for the difference, show the whole transcript
                      // in the plain labelled reader: the same speaker dots,
                      // parsed out of the string, and nothing hidden. What is
                      // lost is playback highlighting and per-turn editing,
                      // both of which need the timeline this note is missing.
                      <div className={cn("flex flex-col", "min-h-0 flex-1")}>
                        <p className="nd-meta mb-2 shrink-0">
                          Part of this transcript has no recording timeline behind it, so
                          playback highlighting and per-turn editing are unavailable here.
                        </p>
                        <TranscriptView
                          transcript={draft.transcript}
                          onClick={() => {}}
                          disabled
                          fill
                          bottomAligned={transcriptLive}
                        />
                      </div>
                    ) : timeline.length > 0 ? (
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
                ) : recActive || isTranscribing ? (
                  <SkeletonLines lines={4} />
                ) : (
                  <PanelEmpty
                    icon={<MessageSquare size={22} strokeWidth={1.5} />}
                    text={
                      // #146: a note with recorded audio and no text is not the
                      // same empty as a note with nothing recorded, and telling
                      // the user to start a recording would be wrong advice —
                      // the meeting is already on disk.
                      pendingTranscription
                        ? "This recording hasn't been transcribed yet. Use Transcribe in the toolbar to run it now."
                        : "No transcript yet. Start a recording from the toolbar to capture and transcribe audio."
                    }
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
export function NoteToolbar({
  noteId,
  backTo,
  backLabel,
  readOnly,
  recActive,
  canRecord,
  panelOpen,
  onTogglePanel,
  onSummarizeFailed,
  onRegenerateTitle,
  pendingTranscription,
  isTranscribing,
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
  // This note has a take captured with "Transcribe manually" on whose audio is
  // still on disk (#146). The action's presence is the only signal in v1 —
  // there is no per-take badge on the carousel.
  pendingTranscription: boolean;
  isTranscribing: boolean;
  // Summarize streams into the Summary panel, whose state lives in `Note` —
  // so a failure here has to reach up there to drop the partial response.
  onSummarizeFailed: () => void;
  // Regenerate title (#90). Owned by `Note` rather than run here: the busy
  // state belongs on the title box, not in a menu Radix closes on select.
  onRegenerateTitle: () => void;
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

  // How the row degrades when the body column gets narrow.
  //
  // `@container`, not a viewport breakpoint: this row's width is the BODY
  // COLUMN's, which shrinks as the user drags the context panel wider — so at a
  // fixed window size the same toolbar has to survive anywhere from ~320px to
  // full width. `.nd-btn` is `flex-shrink: 0; white-space: nowrap`
  // (globals.css) and rightly so — a label compressed into two clipped lines
  // would be worse than one that's hidden — so the row cannot absorb the
  // squeeze by shrinking. It gives way in three steps instead: the back label's
  // cap tightens, then the action labels drop and the buttons stand as their
  // icons, then the back label goes entirely. `title` / `aria-label` carry
  // every name a label stops showing.
  //
  // The thresholds are against the row's CONTENT box — `container-type:
  // inline-size` excludes padding — which is what makes one set of numbers
  // cover both sidebar states. A collapsed sidebar spends 104px of this row on
  // clearing the macOS traffic lights (`pl-[116px]` below), and because that is
  // padding it is already outside what the query compares. Getting this
  // backwards buys a set of thresholds that fire ~100px too early.
  //
  // Measured, not guessed: `?case=toolbar-*` in the mock harness sweeps
  // 300–900px in both themes and both sidebar states, and every band clears its
  // content by ≥6px. The floor is 275px of content, which `BODY_MIN` keeps out
  // of reach.
  const BACK_LABEL = "truncate max-w-[180px] @max-[630px]:max-w-[90px] @max-[380px]:hidden";
  const ACTION_LABEL = "@max-[570px]:hidden";

  return (
    <div data-tauri-drag-region className={cn("@container relative z-30 h-12 shrink-0 flex items-center gap-2 pr-3", sidebarCollapsed ? "pl-[116px]" : "pl-3")}>
      <Link
        to={backTo}
        className="no-drag inline-flex items-center gap-1.5 pl-1.5 pr-2.5 py-1.5 rounded-[var(--radius)] text-[13px] text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-pill-hover)] transition-colors"
      >
        <ChevronLeft size={15} strokeWidth={1.6} />
        {/* First to give: a long folder name is the least load-bearing text in
            the row, and the chevron alone still reads as "back". */}
        <span className={BACK_LABEL}>{backLabel}</span>
      </Link>
      <div className="flex-1" />
      {!readOnly && !recActive && (
        <>
          <button onClick={record} disabled={!canRecord} className="no-drag nd-btn" title="Record (⌘R)" aria-label="Record">
            <Circle size={10} fill="currentColor" strokeWidth={0} className="text-[var(--color-record)]" />
            <span className={ACTION_LABEL}>Record</span>
          </button>
          {(pendingTranscription || isTranscribing) && (
            <button
              onClick={() => void runTranscribe(noteId, "pending")}
              disabled={isTranscribing}
              className="no-drag nd-btn"
              title="Transcribe the recorded audio"
              // The label is hidden below ACTION_LABEL's threshold, so the
              // accessible name has to come from here — and has to track the
              // state, or a run in flight reads as an idle button.
              aria-label={isTranscribing ? "Transcribing…" : "Transcribe"}
            >
              <FileText size={15} strokeWidth={1.6} />
              <span className={ACTION_LABEL}>
                {isTranscribing ? "Transcribing…" : "Transcribe"}
              </span>
            </button>
          )}
          <button
            onClick={() => void runSummarize(noteId, onSummarizeFailed)}
            className="no-drag nd-btn nd-btn-primary"
            title="Summarize"
            aria-label="Summarize"
          >
            <Sparkles size={15} strokeWidth={1.6} />
            <span className={ACTION_LABEL}>Summarize</span>
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
          <ContextMenuItem
            onClick={() => {
              setMenuPos(null);
              onRegenerateTitle();
            }}
          >
            Regenerate title
          </ContextMenuItem>
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

// Small copy-to-clipboard button rendered in a panel header — Summary and
// Transcript (#166) both use it, which is why the payload arrives as a
// closure rather than the component reaching for a field itself.
// 1.5s "Copied" feedback via a Check icon swap. stopPropagation keeps
// the click from reaching a header row that acts on clicks of its own.
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

export const TranscriptEditor = memo(function TranscriptEditor({
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

  // Announce the mode change (#171). The controls are reachable and labelled,
  // but the *transition* was silent: activating `Edit` swapped the reader for
  // a textarea with nothing said about it. The region is rendered
  // unconditionally, outside the header, so the announcement survives the
  // header's contents being swapped out from under it — a live region that
  // mounts at the same moment as its text is not reliably read.
  // `null` on first render (and only then) keeps opening the panel silent:
  // the ref guard skips the effect's initial run, so nothing is announced
  // until an actual transition happens.
  const [announcement, setAnnouncement] = useState<string | null>(null);
  const announcedOnce = useRef(false);
  useEffect(() => {
    if (!announcedOnce.current) {
      announcedOnce.current = true;
      return;
    }
    setAnnouncement(showEditor ? "Editing transcript" : "Transcript no longer editable");
  }, [showEditor]);

  // Persistent header slot (#168). Before this, entering edit mode was an
  // undocumented click on the body and leaving it was Escape or an outside
  // click — neither visible. The slot always occupies the same place: `Edit`
  // in view mode, `Editing` + `Done` while the textarea is open, so the mode
  // is stated rather than inferred from the speaker pills disappearing.
  // Nothing interactive is offered while `disabled` — a recording in flight,
  // or a teammate's read-only note — since on those there is no mode to enter
  // and an inert `Edit` would promise one. The row itself stays (#171): when
  // it vanished with its control, the transcript below jumped up as a
  // recording started and back down when it stopped. The placeholder carries
  // the control's own text so the line box matches exactly whatever the font
  // and size happen to be, rather than a min-height guess that drifts; it is
  // `aria-hidden` because there is nothing there to offer.
  const modeLink =
    "nd-bare text-xs text-[var(--color-text-muted)] underline hover:text-[var(--color-text)] shrink-0";
  const header = (
    <div
      data-testid="transcript-mode-header"
      className={cn("flex items-center justify-end gap-2 mb-2", fill && "shrink-0")}
    >
      {disabled ? (
        <span aria-hidden="true" className="invisible text-xs shrink-0">
          Edit
        </span>
      ) : showEditor ? (
        <>
          <span className="text-xs text-[var(--color-text-muted)]">Editing</span>
          <button
            type="button"
            // blur fires before the click would land, and the blur handler
            // exits edit mode — which unmounts this button mid-gesture. Keep
            // focus in the textarea by killing the mousedown default; the
            // click then reaches onClick as normal.
            onMouseDown={(e) => e.preventDefault()}
            onClick={() => setEditing(false)}
            className={modeLink}
          >
            Done
          </button>
        </>
      ) : (
        <button type="button" onClick={() => setEditing(true)} className={modeLink}>
          Edit
        </button>
      )}
    </div>
  );

  return (
    <div className={cn("flex flex-col", fill && "min-h-0 flex-1")}>
      <span data-testid="transcript-mode-live" className="sr-only" role="status" aria-live="polite">
        {announcement ?? ""}
      </span>
      {header}
      {showEditor ? (
        <textarea
          ref={taRef}
          aria-label="Transcript"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onBlur={() => setEditing(false)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              setEditing(false);
            }
          }}
          // No `.nd-bare` (#168). That opt-out exists to strip the field
          // chrome when the surrounding Card is the surface — which is what
          // left this textarea borderless and indistinguishable from the
          // reader. The base `textarea` rule in globals.css already gives a
          // 1px border plus a focus border-colour shift, and being unlayered
          // it beats any Tailwind border utility we could ask for here, so
          // dropping the opt-out IS the focus border the issue asks for.
          className={cn(
            "w-full resize-none text-sm leading-relaxed text-[var(--color-text-muted)]",
            fill && "flex-1 min-h-0 overflow-y-auto",
          )}
        />
      ) : (
        // TranscriptView owns its own scroll container so the virtualizer can
        // measure visible items. Bypass CollapsibleScroll here — its
        // bottomAligned + maxHeight role is taken over by TranscriptView's
        // built-in scroller.
        <TranscriptView
          transcript={value}
          onClick={() => {
            if (!disabled) setEditing(true);
          }}
          disabled={disabled}
          fill={fill}
          bottomAligned={bottomAligned}
        />
      )}
    </div>
  );
});

// Styled transcript reader. Each speaker line is a two-column flex row:
// a narrow gutter holding the coloured dot, then the words. Same shape
// as the playback view below, so the two readers line up.
//
// The dot lives *inside* the line box (`.nd-speaker-dot` at `left: 0`,
// positioned within the gutter column) rather than hanging in a negative
// margin: this view's scroll container clips anything at a negative x,
// because CSS resolves the other axis of a scroll container to `auto`.
// That is what made every dot here invisible. See globals.css.
//
// The label text itself ("Speaker 1: ") is dropped in read mode — the
// dot carries the identity, and the chip strip above names it. It used
// to be rendered as a transparent prefix so the wrap matched the
// textarea exactly, but at real label widths that reserved a blank
// hole the width of a name beside every dot. The names reappear when
// the user clicks into edit mode, which is the moment the raw text
// matters; a modest reflow between the two modes is the cheaper trade.
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

/** A turn's speaker, above the words they said (#176).
 *
 * Shared by BOTH readers — `TranscriptPlayer` over a timeline and
 * `TranscriptView` over the text — because which one a note gets is not a
 * property of the note but of whether its timeline is on this machine, and a
 * teammate's note must not look like a different app. Written twice, the two
 * would drift; written once, they cannot.
 *
 * What each reader still supplies for itself is what it actually has: the
 * player's `dot` is a button that reassigns the turn and its `meta` is the
 * turn's position in its own take, while the fallback has plain text, so its
 * dot is inert and it has no time to show.
 */
function TurnTitle({
  dot,
  name,
  meta,
  trailing,
  className,
}: {
  dot: React.ReactNode;
  name: string;
  /** Small print beside the name — the player's take-local timestamp. */
  meta?: string;
  /** Pushed to the far end: the player's per-turn edit + delete (#170). */
  trailing?: React.ReactNode;
  className?: string;
}) {
  return (
    <div data-turn-title className={cn("flex items-center gap-1.5 mb-0.5", className)}>
      <div className="relative w-3 shrink-0 self-stretch">{dot}</div>
      <span className="text-[13px] font-semibold text-[var(--color-text)] truncate">{name}</span>
      {meta && (
        <span className="text-[11px] text-[var(--color-text-disabled)] tabular-nums shrink-0">
          {meta}
        </span>
      )}
      {trailing && (
        <>
          <div className="flex-1" />
          {trailing}
        </>
      )}
    </div>
  );
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
              // Same turn title as the timeline-backed reader (#176), and for
              // the same reason: this view stripped the `Hege: ` prefix off the
              // line and put a dot where it had been, so past four speakers two
              // turns were the same colour with the name nowhere on either. The
              // two readers must agree — which one a note gets depends on
              // whether its timeline happens to be on this machine, and a
              // teammate's note must not look like a different app.
              //
              // Titled at the START OF A RUN only. `groupTimeline` does this
              // for the other reader off the timeline, splitting on label AND
              // session; there are no session ids in plain text, so here the
              // run is the label alone — one line longer, by exactly that.
              const prev = lines[vrow.index - 1];
              const startsRun =
                !prev || prev.kind !== "speaker" || prev.trimmedLabel !== line.trimmedLabel;
              content = (
                <>
                  {startsRun && (
                    <TurnTitle
                      name={line.trimmedLabel}
                      dot={
                        <span
                          className="nd-speaker-dot"
                          style={{ background: color }}
                          title={line.trimmedLabel}
                          // Decorative now that the name is beside it — a
                          // labelled span here made a screen reader say the
                          // speaker twice per turn. This dot is not a control
                          // in this reader (the player's is).
                          aria-hidden
                        />
                      }
                      // `pt-`, not `mt-`: the virtualizer measures each row off
                      // its bounding box, which excludes margins — a margin here
                      // would leave every row positioned short of where it
                      // paints. Matches the 8px the other reader gets from its
                      // per-turn `py-1`.
                      className={vrow.index > 0 ? "pt-2" : undefined}
                    />
                  )}
                  <div className="whitespace-pre-wrap">{line.rest || " "}</div>
                </>
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
// Edit mode: per-turn (#170), never the whole string. This view renders
// from the timeline, and `note.transcript` is a projection of it, so a
// textarea bound to that string wrote the derived copy and orphaned the
// source: the edit was invisible here (permanently, and across reopens)
// while summary, chat and embeddings read the edited text. Editing a turn
// now writes the timeline and the backend re-derives the transcript, the
// same path the label cycle and the chunk delete already take. The
// no-timeline reader (`TranscriptEditor`) keeps its whole-transcript
// textarea — with no timeline there is no second copy to orphan.
// Exported for unit tests (session-switch seek behaviour, BUG A/B).
export const TranscriptPlayer = memo(function TranscriptPlayer({
  noteId,
  timeline,
  setTimeline,
  sessions,
  fallbackPlaybackUrl,
  audioAvailable,
  keepAudio,
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
  // Which turn is open for editing (#170), identified by its session and its
  // first constituent chunk — stable across the re-group that follows a commit,
  // unlike a group position. `null` is view mode.
  const [editingKey, setEditingKey] = useState<string | null>(null);
  const [editText, setEditText] = useState("");
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

  // Focus the open turn's textarea and park the cursor at the end. Keyed on
  // which turn is open, NOT on its text — an `editText` dependency would send
  // the cursor to the end after every keystroke.
  useEffect(() => {
    if (!editingKey) return;
    const el = taRef.current;
    if (!el) return;
    el.focus({ preventScroll: true });
    const len = el.value.length;
    el.setSelectionRange(len, len);
  }, [editingKey]);

  // Grow the open textarea with its content. Separate from the focus effect
  // above precisely because this one must re-run on every keystroke: `rows={1}`
  // plus `resize-none` would otherwise clip a turn the user types past.
  useEffect(() => {
    if (!editingKey) return;
    const el = taRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }, [editingKey, editText]);

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

  // Rewrite one turn's text (#170). The edit lands in the timeline — the
  // source the reader renders from — and the backend re-derives
  // `note.transcript` from every session's timeline, so the two can't drift.
  // Word timings on this turn are dropped: they describe the words that were
  // there. The turn's bounds survive, so it still highlights during playback;
  // only per-word karaoke is lost, on this turn alone.
  async function commitGroupText(g: TimelineGroup, next: string) {
    const text = next.trim().replace(/\s+/g, " ");
    setEditingKey(null);
    if (!text || text === g.text.trim()) return;
    const sessionId = timeline[g.indices[0]]?.sessionId;
    if (sessionId == null) return;
    const chunkIdxs = g.indices
      .map((ci) => timeline[ci]?.chunkIdx)
      .filter((ci): ci is number => ci != null);
    if (chunkIdxs.length === 0) return;
    const lowest = Math.min(...g.indices);
    // Optimistic: mirror exactly what the backend does to the file, so the
    // reader shows the edit before `transcript_replaced` comes back.
    setTimeline((tl) =>
      tl.map((e, i) =>
        i === lowest
          ? { ...e, text, words: [] }
          : g.indices.includes(i)
            ? { ...e, text: "", words: [] }
            : e,
      ),
    );
    try {
      await ipc.noteTimelineSetChunkText(noteId, sessionId, chunkIdxs, text);
    } catch (err) {
      console.error("noteTimelineSetChunkText failed", err);
    }
    // Push the rewritten session timeline to the workspace (#16), or a
    // teammate keeps the pre-edit text; Personal notes short-circuit.
    void ipc.uploadNoteSessions(noteId);
  }

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
        {/* The no-audio line carries no "— Settings → Recording" pointer: the
            user turned retention off themselves, so being told where the switch
            is reads as nagging. The greyed re-diarize control's tooltip still
            names it, for the case where someone wants the capability back. */}
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
              : "Audio not stored on this device."}
          </p>
        )}
      </div>
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
            // Identity of the turn for edit mode (#170): session + first
            // constituent chunk, so it survives the re-group after a commit.
            const groupKey = `${g.sessionId}:${g.indices[0]}`;
            const isEditingGroup = editingKey === groupKey && !disabled;
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
            const body = (
              <>
                {isEditingGroup ? (
                  // Per-turn edit (#170). One textarea over this turn's text,
                  // committing into the timeline. Enter commits — a turn is one
                  // transcript line, so a newline inside it has nothing to
                  // mean. Blur commits too; Escape discards.
                  <textarea
                    ref={taRef}
                    value={editText}
                    onChange={(e) => setEditText(e.target.value)}
                    onBlur={() => void commitGroupText(g, editText)}
                    onKeyDown={(e) => {
                      if (e.key === "Escape") {
                        e.preventDefault();
                        setEditingKey(null);
                      } else if (e.key === "Enter") {
                        e.preventDefault();
                        void commitGroupText(g, editText);
                      }
                    }}
                    rows={1}
                    aria-label="Edit this turn"
                    // `w-full` AND `flex-1`: a titled turn's body is a block
                    // child of the row, where `flex-1` is inert and a textarea
                    // falls back to its ~20-column intrinsic width (measured:
                    // 188px inside a 388px row). An unlabelled turn still puts
                    // it in a flex row, where `flex-1` governs. jsdom has no
                    // layout, so only the harness can see this.
                    className="w-full flex-1 resize-none text-sm leading-relaxed bg-transparent"
                  />
                ) : g.words.length > 0 ? (
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
                    // `w-full` for the same reason as the textarea above: a
                    // button shrink-wraps its text outside a flex row.
                    className="text-left w-full flex-1 nd-bare cursor-text"
                  >
                    {g.text}
                  </button>
                )}
              </>
            );
            // Pencil + delete, defined once and placed by whether the turn
            // has a title to hang them on (#176).
            // Whether this turn has a name to hang a title — and therefore a
            // title line for its actions to sit on.
            const titled = !!g.label && !!color;
            const actions = (
              <>
                {!disabled && !isEditingGroup && (
                  <button
                    type="button"
                    // mousedown with the default prevented, not click: an open
                    // textarea's blur would otherwise re-render this row out
                    // from under the click. The cost is that the open turn
                    // never blurs, so this handler has to commit it — dropping
                    // it here would silently discard a typed edit.
                    onMouseDown={(e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      const open =
                        editingKey && editingKey !== groupKey
                          ? groups.find(
                              (x) => `${x.sessionId}:${x.indices[0]}` === editingKey,
                            )
                          : undefined;
                      if (open) void commitGroupText(open, editText);
                      setEditText(g.text);
                      setEditingKey(groupKey);
                    }}
                    title="Edit this turn"
                    aria-label="Edit this turn"
                    className="nd-bare shrink-0 self-start opacity-0 group-hover:opacity-100 transition-opacity p-0.5 rounded text-[var(--color-text-muted)] hover:text-[var(--color-text)] hover:bg-[var(--color-pill-hover)]"
                  >
                    <Pencil size={14} strokeWidth={1.5} />
                  </button>
                )}
                {!disabled && !isEditingGroup && (
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
              </>
            );
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
                  "group px-2 py-1 rounded transition-colors " +
                  (isActive
                    ? "bg-[var(--color-pill-hover)] text-[var(--color-text)]"
                    : "hover:bg-[var(--color-pill-hover)]")
                }
              >
                {/* The turn's title: who spoke, and when in their take (#176).
                    The reader used to carry the name nowhere at all — only a
                    coloured dot, and `speakerColorMap` cycles four colours, so
                    the first and fifth speaker of a meeting wore the same blue
                    and the turn was unattributable. The NAME is the identity
                    now; the dot is the scanning aid, and still the control that
                    reassigns the turn.

                    The text below is NOT indented under the title: a turn runs
                    the full width of a panel that can be 320px, and an indent
                    would spend some of it re-stating an alignment the title
                    already gives. Chosen from five prototyped shapes — branch
                    `prototype/176-transcript-turns` holds the losing four.

                    The row's actions live up here rather than beside the text
                    for the same reason. An UNLABELLED turn therefore keeps the
                    old inline layout (below): with no title there is no line
                    for them to sit on, and a title bar holding nothing but two
                    hover-revealed buttons would cost every turn of a
                    never-diarized note a line of height to say nothing. */}
                {titled && (
                  <TurnTitle
                    name={g.label}
                    // Local to the turn's own take — timeline times are never
                    // rebased onto a note-wide clock.
                    meta={formatDuration(g.startMs)}
                    trailing={actions}
                    dot={
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
                        // Named for what the control DOES, not for who spoke:
                        // the name is visible beside it now, so repeating it
                        // here read the speaker out twice per turn.
                        aria-label={`Reassign ${g.label}`}
                      />
                    }
                  />
                )}
                {/* A titled turn's text runs the full width under its
                    title; an unlabelled one keeps the old inline row, actions
                    and all. */}
                {titled ? (
                  body
                ) : (
                  <div className="flex items-start gap-1">
                    {body}
                    {actions}
                  </div>
                )}
              </div>
              </div>
            );
          });
          })()}
        </div>
      </div>
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
  const title = sessionTitle(index);
  return (
    <div className="flex items-center gap-2 px-2 pt-3 pb-1 select-none">
      <span className="nd-label shrink-0">{title}</span>
      {caption && caption !== title && (
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
