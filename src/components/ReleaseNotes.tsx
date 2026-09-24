import { useCallback, useState } from "react";
import { ArrowUpRight, Check, ChevronDown, CircleCheck, CircleDot, Building2, Folder, Languages, Lock, Tag, Users, X } from "lucide-react";
import { isTauri } from "@tauri-apps/api/core";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { latestReleases, type Release } from "../content/releases";
import { speakerCountLabel } from "../lib/speakerCount";
import { speakerColorMap } from "./SpeakerLabels";
import { rowClass } from "./ui/surface";
import { Modal } from "../pages/settings/components/Modal";
import "../styles/releases.css";

function releaseDate(date: string) {
  return new Date(`${date}T12:00:00`).toLocaleDateString("en-GB", {
    day: "numeric", month: "short", year: "numeric",
  });
}

function PreviewNote({ recording = false }: { recording?: boolean }) {
  return (
    <div className="nd-notecard release-preview-note">
      <div className="release-preview-date">15 Sept · 09:30</div>
      <h4>Product catch-up</h4>
      <p>Review the latest designs and agree on what we want to focus on next.</p>
      <div className="release-preview-note-meta">
        <span className={recording ? "release-preview-recording" : "release-preview-summarized"}>
          <i />{recording ? "Recording" : "Summarized"}{recording && <span>04:32</span>}
        </span>
        <span><Folder size={12} strokeWidth={1.7} />Product</span>
      </div>
    </div>
  );
}

const PREVIEW_TURNS = [
  { speaker: "Speaker 1", at: "0:04", text: "Let’s start with the new designs." },
  { speaker: "Speaker 2", at: "0:09", text: "The home screen feels much calmer now." },
  { speaker: "Speaker 3", at: "0:15", text: "I’d like one more pass on Settings." },
  { speaker: "Speaker 4", at: "0:21", text: "Then that’s our focus for next week." },
];
const PREVIEW_TURN_COLORS = speakerColorMap(PREVIEW_TURNS.map((turn) => turn.speaker));

const VISUAL_CAPTION: Record<NonNullable<Release["visual"]>, string> = {
  updates: "Catch up on the latest changes.",
  filters: "Find a note by what it contains.",
  privacy: "Choose who can see your note.",
  activity: "Follow the recording from your library.",
  speakers: "See who said what, even on Auto.",
};

function ReleaseVisual({ kind }: { kind: NonNullable<Release["visual"]> }) {
  return (
    <div className={`release-visual release-visual-${kind}`} aria-hidden="true">
      <div className="release-demo">
        {kind === "updates" && <>
          <div className="release-demo-heading"><span>What’s new</span></div>
          <div className="release-preview-updates">{latestReleases.map((release) => <div key={release.version}>
            <span className="release-version"><Tag size={13} />{release.version}</span>
            <span>{release.title}</span><ArrowUpRight size={14} />
          </div>)}</div>
        </>}
        {kind === "filters" && <>
          <div className="release-demo-heading"><span>All notes</span><small>1 of 8 notes</small></div>
          <div className="release-demo-filters">
            <span className="nd-meta is-selected"><CircleDot size={14} strokeWidth={1.6} />Summarized</span>
            <span className="nd-meta"><Building2 size={14} strokeWidth={1.6} />Any client</span>
            <span className="nd-meta"><X size={14} strokeWidth={1.8} />Clear</span>
          </div>
          <PreviewNote />
        </>}
        {kind === "privacy" && <>
          <div className="release-demo-heading"><span>A thought for later</span></div>
          <div className="release-preview-meta">
            <span className="nd-meta"><Users size={14} strokeWidth={1.7} />Design team</span>
            <div className="release-preview-visibility">
              <span className="nd-meta"><Lock size={14} strokeWidth={1.7} />Private</span>
              <div className="release-preview-menu">
                <div className={rowClass}><span className="release-preview-check" />Shared</div>
                <div className={rowClass}><span className="release-preview-check"><Check size={14} strokeWidth={2} /></span>Private</div>
              </div>
            </div>
          </div>
          <p className="release-preview-body">A few ideas to think through before our next team meeting.</p>
        </>}
        {kind === "activity" && <>
          <div className="release-demo-heading"><span>All notes</span><small>8 notes</small></div>
          <PreviewNote recording />
        </>}
        {kind === "speakers" && <>
          <div className="release-demo-heading"><span>Product catch-up</span><small>{speakerCountLabel(PREVIEW_TURNS.length)}</small></div>
          <div className="release-preview-pickers">
            <span className="nd-meta is-filled"><Languages size={14} strokeWidth={1.6} />English<ChevronDown size={12} strokeWidth={2} /></span>
            <span className="nd-meta is-filled"><Users size={14} strokeWidth={1.6} />Auto<ChevronDown size={12} strokeWidth={2} /></span>
          </div>
          <div className="release-preview-turns">{PREVIEW_TURNS.map((turn) => <div key={turn.speaker}>
            <div className="release-preview-turn-title"><i style={{ background: PREVIEW_TURN_COLORS.get(turn.speaker) }} />{turn.speaker}<small>{turn.at}</small></div>
            <p>{turn.text}</p>
          </div>)}</div>
        </>}
      </div>
      <span className="release-visual-caption">{VISUAL_CAPTION[kind]}</span>
    </div>
  );
}

export function ReleaseNotes() {
  const [selected, setSelected] = useState<Release | null>(null);
  const [linkError, setLinkError] = useState<string | null>(null);
  const close = useCallback(() => setSelected(null), []);
  return (
    <section className="release-section" aria-labelledby="release-heading">
      <div className="release-section-heading"><h2 id="release-heading">What’s new</h2><span>The latest from Humla</span></div>
      <ol className="release-list">
        {latestReleases.map((release, index) => <li key={release.version}>
          <button className="release-row no-drag" onClick={() => { setLinkError(null); setSelected(release); }} aria-haspopup="dialog">
            <span className="release-version"><Tag size={13} strokeWidth={1.6} />{release.version}</span>
            <span className="release-row-copy"><span className="release-row-title">{release.title}{index === 0 && <span className="release-new">New</span>}</span><time dateTime={release.date}>{releaseDate(release.date)}</time></span>
            <ArrowUpRight size={17} className="release-arrow" />
          </button>
        </li>)}
      </ol>
      <Modal open={selected !== null} onClose={close} title={selected?.title} showHeader={false} padded={false}>
        {selected && <article className="release-post">
          <header className="release-post-bar"><span>WHAT’S NEW <span>/</span> {selected.version}</span><button className="nd-btn-icon no-drag" onClick={close} aria-label="Close release notes"><X size={18} /></button></header>
          {selected.visual && <ReleaseVisual kind={selected.visual} />}
          <div className="release-post-body">
            <time dateTime={selected.date}>{releaseDate(selected.date)}</time>
            <h2>{selected.title}</h2>
            <p className="release-post-lead">{selected.summary}</p>
            {selected.paragraphs.map((paragraph) => <p key={paragraph}>{paragraph}</p>)}
            <h3>In this release</h3>
            <ul>{selected.highlights.map((highlight) => <li key={highlight}><Check size={15} /><span>{highlight}</span></li>)}</ul>
            <h3 className="release-issues-heading"><svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M12 .75a11.25 11.25 0 0 0-3.558 21.923c.563.104.768-.244.768-.542 0-.267-.01-.974-.015-1.912-3.13.68-3.79-1.508-3.79-1.508-.512-1.3-1.25-1.646-1.25-1.646-1.023-.7.078-.686.078-.686 1.13.08 1.725 1.16 1.725 1.16 1.005 1.723 2.636 1.225 3.278.937.102-.729.393-1.225.715-1.507-2.498-.284-5.124-1.249-5.124-5.562 0-1.229.439-2.233 1.16-3.02-.116-.285-.503-1.43.11-2.98 0 0 .945-.303 3.094 1.154a10.79 10.79 0 0 1 5.634 0c2.148-1.457 3.092-1.154 3.092-1.154.615 1.55.228 2.695.112 2.98.722.787 1.158 1.791 1.158 3.02 0 4.324-2.63 5.275-5.136 5.554.404.349.766 1.034.766 2.084 0 1.505-.014 2.719-.014 3.088 0 .3.203.65.774.54A11.25 11.25 0 0 0 12 .75Z" /></svg>Related Github issues</h3>
            {selected.issues.length > 0 ? <ul className="release-issues">
              {selected.issues.map((issue) => <li key={issue.url}>
                <a href={issue.url} target="_blank" rel="noopener noreferrer" onClick={(event) => {
                  if (!isTauri()) return;
                  event.preventDefault();
                  setLinkError(null);
                  void openExternal(issue.url).catch(() => setLinkError("Couldn’t open GitHub. Please try again."));
                }}>
                  <span className={`release-issue-state-icon ${issue.status.toLowerCase()}`} aria-hidden="true">
                    {issue.status === "Closed" ? <CircleCheck size={18} /> : <CircleDot size={18} />}
                  </span>
                  <span className="release-issue-content">
                    <span className="release-issue-title">{issue.title}</span>
                    <span className="release-issue-meta"><span>humla {issue.reference}</span><span className={`release-issue-status ${issue.status.toLowerCase()}`}>{issue.status}</span></span>
                  </span>
                  <ArrowUpRight size={14} className="release-issue-arrow" aria-hidden="true" />
                </a>
              </li>)}
            </ul> : <p className="release-issues-empty">No linked issues for this release.</p>}
            {linkError && <p role="alert">{linkError}</p>}
          </div>
          <footer className="release-post-footer"><span>Made for your everyday meetings.</span><button className="nd-btn no-drag" onClick={close}>Back to Humla</button></footer>
        </article>}
      </Modal>
    </section>
  );
}
