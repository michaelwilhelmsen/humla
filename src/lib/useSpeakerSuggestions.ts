import { useEffect, useState } from "react";
import { ipc } from "./ipc";
import { useCloudStore } from "./cloud";
import type { SpeakerLabelStat, SpeakerSuggestionSource } from "./speakerSuggest";

/**
 * Fetch the rename picker's suggestion source (#116 part 1) — see
 * `SpeakerSuggestionSource` for why it has two halves.
 *
 * One fetch of each when the strip mounts; all filtering and ranking happens in
 * TS per keystroke, never over IPC.
 *
 * Both calls are best-effort. Losing suggestions must never break the rename
 * itself, which still accepts free text.
 */
export function useSpeakerSuggestions(enabled: boolean): SpeakerSuggestionSource {
  const [stats, setStats] = useState<SpeakerLabelStat[]>([]);
  const [roster, setRoster] = useState<string[]>([]);
  // Your own name needs no network: some servers omit the caller from the
  // roster, and you are the person most likely to need labelling.
  const myName = useCloudStore((s) => s.status.user?.name?.trim());

  useEffect(() => {
    if (!enabled) return;
    let alive = true;
    void ipc.speakerLabelStats().then(
      (rows) => alive && setStats(rows),
      () => {},
    );
    void ipc.speakerRoster().then(
      (names) => alive && setRoster(names),
      () => {},
    );
    return () => {
      alive = false;
    };
  }, [enabled]);

  return {
    stats,
    roster: myName && !roster.includes(myName) ? [...roster, myName] : roster,
  };
}
