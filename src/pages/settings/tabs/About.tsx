import { useEffect, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { ipc } from "../../../lib/ipc";
import { emitDeveloperModeChange } from "../../../lib/useDeveloperMode";
import { Btn } from "../components/Btn";
import { Row, Section } from "../components/Section";

const REPO_URL = "https://github.com/michaelwilhelmsen/humla";

// Number of taps on the version number required to enable developer
// mode. Borrowed from the Android pattern — quietly discoverable, no UI
// chrome cluttering the About tab for everyone else.
const TAPS_TO_ENABLE_DEV_MODE = 7;
const TAP_RESET_MS = 3000;

export function AboutTab() {
  const [version, setVersion] = useState<string>("");
  const [dataDir, setDataDir] = useState<string>("");
  const [devMode, setDevMode] = useState<boolean>(false);
  const [tapCount, setTapCount] = useState<number>(0);
  const [tapHint, setTapHint] = useState<string>("");
  const tapResetTimer = useRef<number | null>(null);

  useEffect(() => {
    let cancelled = false;
    getVersion()
      .then((v) => {
        if (!cancelled) setVersion(v);
      })
      .catch(() => {
        if (!cancelled) setVersion("?");
      });
    ipc.appDataDir()
      .then((d) => {
        if (!cancelled) setDataDir(d);
      })
      .catch(() => {
        if (!cancelled) setDataDir("");
      });
    ipc.getSetting("developer_mode")
      .then((v) => {
        if (!cancelled) setDevMode(v === "true");
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      if (tapResetTimer.current !== null) {
        window.clearTimeout(tapResetTimer.current);
      }
    };
  }, []);

  async function handleVersionTap() {
    if (devMode) return;
    const next = tapCount + 1;
    setTapCount(next);
    if (tapResetTimer.current !== null) {
      window.clearTimeout(tapResetTimer.current);
    }
    if (next >= TAPS_TO_ENABLE_DEV_MODE) {
      setTapCount(0);
      setTapHint("");
      setDevMode(true);
      await ipc.setSetting("developer_mode", "true");
      emitDeveloperModeChange(true);
      return;
    }
    // Show the hint only once the user has clearly started tapping —
    // a single accidental click on the version number shouldn't reveal
    // the easter egg.
    if (next >= 3) {
      const remaining = TAPS_TO_ENABLE_DEV_MODE - next;
      setTapHint(`${remaining} more tap${remaining === 1 ? "" : "s"} to enable developer mode`);
    }
    tapResetTimer.current = window.setTimeout(() => {
      setTapCount(0);
      setTapHint("");
    }, TAP_RESET_MS);
  }

  async function disableDevMode() {
    setDevMode(false);
    await ipc.setSetting("developer_mode", "false");
    emitDeveloperModeChange(false);
  }

  return (
    <>
      <Section title="Humla">
        <Row label="Version">
          <div
            className="text-sm cursor-default select-none inline-block"
            onClick={handleVersionTap}
          >
            {version || "—"}
          </div>
          {tapHint && (
            <p className="text-xs text-[var(--color-text-muted)] mt-1">
              {tapHint}
            </p>
          )}
          {devMode && (
            <p className="text-xs text-[var(--color-text-muted)] mt-1">
              Developer mode is on.{" "}
              <button
                type="button"
                onClick={disableDevMode}
                className="underline hover:text-[var(--color-text)]"
              >
                Turn off
              </button>
            </p>
          )}
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            Updates are checked automatically on launch and can be triggered
            manually from <code>Humla → Check for Updates…</code> in the
            menu bar.
          </p>
        </Row>
        <Row label="Source code">
          <button
            type="button"
            onClick={() => openExternal(REPO_URL)}
            className="text-sm underline hover:text-[var(--color-text)] text-left"
          >
            {REPO_URL}
          </button>
        </Row>
        <Row label="License">
          <div className="text-sm">MIT</div>
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            Open source. Use, modify, and share freely. See the LICENSE
            file in the repo for the full text.
          </p>
        </Row>
        <Row label="Privacy">
          <p className="text-sm">No telemetry, no tracking, no analytics.</p>
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            Your notes, audio, and transcripts stay on your Mac. Cloud
            transcription / summarisation only sends data to OpenAI when
            you explicitly select cloud providers.
          </p>
        </Row>
      </Section>

      <Section title="Storage">
        <Row label="Data directory">
          <div className="flex items-center gap-2">
            <code className="text-xs px-2 py-1 rounded bg-[var(--color-pill-hover)] flex-1 break-all font-mono">
              {dataDir || "—"}
            </code>
            <Btn onClick={() => dataDir && openExternal(dataDir)} disabled={!dataDir}>
              Open in Finder
            </Btn>
          </div>
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            Notes, settings, downloaded models, and the audio temp dir all
            live here. Back up <code>notes.sqlite</code> from this folder
            to copy your library to another Mac.
          </p>
        </Row>
      </Section>
    </>
  );
}
