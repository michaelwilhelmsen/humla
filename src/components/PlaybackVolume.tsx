import { useEffect, useRef, useState, type CSSProperties, type RefObject } from "react";
import { Volume1, Volume2, VolumeX } from "lucide-react";
import { Popover, PopoverContent, PopoverTrigger } from "./ui/Popover";
import { cn } from "../lib/cn";
import {
  VOLUME_DEFAULT,
  VOLUME_MAX,
  applyPlaybackVolume,
  readPlaybackVolume,
  snapToUnity,
  writePlaybackVolume,
} from "../lib/playbackVolume";

/** The volume beside the transcript's player (#234). `src` is passed so the
 * level is applied again when the player loads another take. */
export function PlaybackVolume({
  audioRef,
  src,
}: {
  audioRef: RefObject<HTMLAudioElement | null>;
  src: string | null;
}) {
  const [volume, setVolume] = useState(readPlaybackVolume);
  const dragging = useRef(false);

  useEffect(() => {
    if (audioRef.current) applyPlaybackVolume(audioRef.current, volume);
  }, [audioRef, volume, src]);

  function change(next: number) {
    setVolume(next);
    writePlaybackVolume(next);
  }

  const Icon = volume === 0 ? VolumeX : volume < 0.5 ? Volume1 : Volume2;
  const percent = Math.round(volume * 100);
  const track = {
    "--value": volume / VOLUME_MAX,
    "--unity-at": 1 / VOLUME_MAX,
  } as CSSProperties;

  return (
    <Popover>
      <PopoverTrigger asChild>
        <button
          type="button"
          className="nd-btn-icon nd-btn-icon-sm"
          aria-label={`Volume ${percent}%`}
          title={`Volume ${percent}%`}
        >
          <Icon size={16} />
        </button>
      </PopoverTrigger>
      <PopoverContent align="end" className="w-[240px] px-3 py-2">
        <div className="flex items-center gap-3">
          <input
            type="range"
            min={0}
            max={VOLUME_MAX}
            step={0.05}
            value={volume}
            onPointerDown={() => (dragging.current = true)}
            onPointerUp={() => (dragging.current = false)}
            onChange={(e) => {
              const next = Number(e.currentTarget.value);
              change(dragging.current ? snapToUnity(next) : next);
            }}
            onDoubleClick={() => change(VOLUME_DEFAULT)}
            aria-label="Volume"
            aria-valuetext={`${percent}%`}
            style={track}
            className={cn("nd-volume flex-1", volume > 1 && "is-boost")}
          />
          <span
            className={cn(
              "w-11 text-right text-xs tabular-nums",
              volume > 1 ? "text-[var(--color-danger)]" : "text-[var(--color-text-muted)]",
            )}
          >
            {percent}%
          </span>
        </div>
      </PopoverContent>
    </Popover>
  );
}
