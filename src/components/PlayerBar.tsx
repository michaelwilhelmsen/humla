import { useEffect, useState, type CSSProperties, type RefObject } from "react";
import { Pause, Play, RotateCcw, RotateCw } from "lucide-react";
import type { NoteSession } from "../lib/ipc";
import { formatDuration } from "../lib/sessions";
import { PlaybackVolume } from "./PlaybackVolume";
import { Select } from "./ui/Select";

const SKIP_SECONDS = 15;
const ARROW_SECONDS = 5;
export const PLAYBACK_RATES = [1, 1.25, 1.5, 2, 0.75];

/** The rate after `rate` in `PLAYBACK_RATES`, wrapping round. */
export function nextPlaybackRate(rate: number): number {
  const at = PLAYBACK_RATES.indexOf(rate);
  return PLAYBACK_RATES[(at + 1) % PLAYBACK_RATES.length];
}

/** The Verbatim view's player, docked under the transcript: a scrubber, play
 *  and skip, speed, volume, and which recording plays when a note has more
 *  than one. It drives the `<audio>` element the reader owns. */
export function PlayerBar({
  audioRef,
  src,
  sessions,
  activeSessionId,
  onSelectSession,
}: {
  audioRef: RefObject<HTMLAudioElement | null>;
  src: string | null;
  sessions: NoteSession[];
  activeSessionId: string | null;
  onSelectSession: (sessionId: string) => void;
}) {
  const [position, setPosition] = useState(0);
  const [duration, setDuration] = useState(0);
  const [paused, setPaused] = useState(true);
  const [rate, setRate] = useState(1);

  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    const sync = () => {
      setPosition(audio.currentTime);
      setDuration(Number.isFinite(audio.duration) ? audio.duration : 0);
      setPaused(audio.paused);
    };
    const events = ["timeupdate", "durationchange", "loadedmetadata", "play", "pause", "ended", "seeked", "emptied"];
    events.forEach((e) => audio.addEventListener(e, sync));
    sync();
    return () => events.forEach((e) => audio.removeEventListener(e, sync));
  }, [audioRef, src]);

  // A new source resets the element's rate to its default, so both are set.
  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    audio.defaultPlaybackRate = rate;
    audio.playbackRate = rate;
  }, [audioRef, rate, src]);

  function toggle() {
    const audio = audioRef.current;
    if (!audio) return;
    if (audio.paused) audio.play().catch(() => {});
    else audio.pause();
  }

  function skip(seconds: number) {
    const audio = audioRef.current;
    if (!audio) return;
    const end = Number.isFinite(audio.duration) ? audio.duration : Infinity;
    audio.currentTime = Math.min(end, Math.max(0, audio.currentTime + seconds));
  }

  const fraction = duration > 0 ? Math.min(1, position / duration) : 0;
  const iconButton =
    "inline-flex items-center justify-center size-8 rounded-full text-[var(--color-text)] hover:bg-[var(--color-pill-hover)] transition-colors";

  return (
    <div className="rounded-[14px] bg-[var(--color-surface-2)] px-3 py-2 flex flex-col gap-1">
      {sessions.length > 1 && (
        <div className="flex min-w-0">
          <RecordingSelect sessions={sessions} activeSessionId={activeSessionId} onSelect={onSelectSession} />
        </div>
      )}
      <div className="flex items-center gap-2.5 text-xs tabular-nums text-[var(--color-text-muted)]">
        <span>{formatDuration(position * 1000)}</span>
        <input
          type="range"
          min={0}
          max={duration || 0}
          step={0.1}
          value={Math.min(position, duration || 0)}
          disabled={duration === 0}
          onChange={(e) => {
            const audio = audioRef.current;
            if (audio) audio.currentTime = Number(e.currentTarget.value);
          }}
          onKeyDown={(e) => {
            const delta = e.key === "ArrowLeft" || e.key === "ArrowDown" ? -1 : e.key === "ArrowRight" || e.key === "ArrowUp" ? 1 : 0;
            if (!delta) return;
            e.preventDefault();
            skip(delta * ARROW_SECONDS);
          }}
          aria-label="Position"
          aria-valuetext={formatDuration(position * 1000)}
          style={{ "--value": fraction } as CSSProperties}
          className="nd-scrub flex-1 min-w-0"
        />
        <span>{formatDuration(duration * 1000)}</span>
      </div>
      <div className="grid grid-cols-[1fr_auto_1fr] items-center gap-1">
        <div />
        <div className="flex items-center gap-1">
          <button
            type="button"
            onClick={() => skip(-SKIP_SECONDS)}
            aria-label={`Back ${SKIP_SECONDS} seconds`}
            title={`Back ${SKIP_SECONDS} seconds`}
            className={iconButton}
          >
            <RotateCcw size={17} strokeWidth={1.6} />
          </button>
          <button
            type="button"
            onClick={toggle}
            aria-label={paused ? "Play" : "Pause"}
            className="inline-flex items-center justify-center size-9 rounded-full bg-[var(--color-text)] text-[var(--color-canvas)] hover:opacity-90 transition-opacity"
          >
            {paused ? (
              <Play size={15} strokeWidth={0} fill="currentColor" className="ml-0.5" />
            ) : (
              <Pause size={15} strokeWidth={0} fill="currentColor" />
            )}
          </button>
          <button
            type="button"
            onClick={() => skip(SKIP_SECONDS)}
            aria-label={`Forward ${SKIP_SECONDS} seconds`}
            title={`Forward ${SKIP_SECONDS} seconds`}
            className={iconButton}
          >
            <RotateCw size={17} strokeWidth={1.6} />
          </button>
        </div>
        <div className="flex items-center justify-end gap-0.5">
          <button
            type="button"
            onClick={() => setRate(nextPlaybackRate)}
            aria-label={`Speed ${rate}×`}
            title="Playback speed"
            className="h-7 min-w-9 px-1.5 rounded-full text-xs tabular-nums text-[var(--color-text)] hover:bg-[var(--color-pill-hover)] transition-colors"
          >
            {rate}×
          </button>
          <PlaybackVolume audioRef={audioRef} src={src} />
        </div>
      </div>
    </div>
  );
}

/** Which of a note's recordings plays, and where the reader scrolls to. */
export function RecordingSelect({
  sessions,
  activeSessionId,
  onSelect,
}: {
  sessions: NoteSession[];
  activeSessionId: string | null;
  onSelect: (sessionId: string) => void;
}) {
  return (
    <Select
      ariaLabel="Recording"
      value={activeSessionId ?? sessions[0].id}
      onChange={onSelect}
      options={sessions.map((s) => ({ value: s.id, label: `Recording ${s.index}` }))}
      className="inline-flex items-center gap-1.5 max-w-full h-7 px-2.5 rounded-full text-xs border border-[var(--color-line-visible)] bg-[var(--color-surface)] hover:bg-[var(--color-pill-hover)] transition-colors"
    />
  );
}
