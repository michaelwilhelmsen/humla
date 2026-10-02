import { useEffect, useState } from "react";
import { ipc } from "../../../lib/ipc";
import { Toggle } from "./Toggle";

// Open at login (#207). Driven by the login item on disk rather than the
// settings map, so a failed install puts the switch back instead of showing a
// login item that doesn't exist.
export function LaunchAtLoginField() {
  // `null` while loading, so the switch doesn't flash off on the way in.
  const [on, setOn] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    ipc
      .launchAtLoginGet()
      .then((v) => {
        if (!cancelled) setOn(v);
      })
      .catch(() => {
        if (!cancelled) setOn(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function apply(next: boolean) {
    const previous = on;
    setError(null);
    setOn(next);
    try {
      await ipc.launchAtLoginSet(next);
    } catch (e) {
      setOn(previous);
      setError(String(e));
    }
  }

  if (on === null) return null;

  return (
    <div className="flex flex-col items-end gap-1">
      <Toggle label="Open at login" checked={on} onChange={(next) => void apply(next)} />
      {error && (
        <p className="text-xs text-[var(--color-danger)] max-w-[280px] text-right">
          {error}
        </p>
      )}
    </div>
  );
}
