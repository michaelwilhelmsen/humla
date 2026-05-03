import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { ipc } from "../../../lib/ipc";
import { Btn } from "../components/Btn";
import { Row, Section } from "../components/Section";

const REPO_URL = "https://github.com/michaelwilhelmsen/humla";

export function AboutTab() {
  const [version, setVersion] = useState<string>("");
  const [dataDir, setDataDir] = useState<string>("");

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
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <>
      <Section title="Humla">
        <Row label="Version">
          <div className="text-sm">{version || "—"}</div>
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
