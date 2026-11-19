import { useEffect, useState } from "react";
import { MonitorSpeaker } from "lucide-react";
import { ipc } from "../lib/ipc";
import { offersSystemAudioSwitch } from "../lib/systemAudio";

// Checked once per launch: every status read spawns the audio-capture sidecar.
let offered: Promise<boolean> | null = null;

async function shouldOffer(): Promise<boolean> {
  const [declined, status] = await Promise.all([
    ipc.getSetting("system_audio_offer").catch(() => null),
    ipc.permissionsStatus().catch(() => null),
  ]);
  return declined !== "declined" && status !== null && offersSystemAudioSwitch(status);
}

export function resetSystemAudioOffer() {
  offered = null;
}

// Home's card moving a Screen Recording user to System Audio Recording. Until
// they allow it, recordings keep using Screen Recording.
export function SystemAudioOffer() {
  const [show, setShow] = useState(false);
  const [asking, setAsking] = useState(false);

  useEffect(() => {
    let live = true;
    offered ??= shouldOffer();
    void offered.then((v) => live && setShow(v));
    return () => {
      live = false;
    };
  }, []);

  function hide() {
    offered = Promise.resolve(false);
    setShow(false);
  }

  async function allow() {
    setAsking(true);
    try {
      await ipc.permissionsRequest("systemAudio");
    } catch {
      // Settings → Permissions still offers it.
    } finally {
      setAsking(false);
      hide();
    }
  }

  function decline() {
    void ipc.setSetting("system_audio_offer", "declined").catch(() => {});
    hide();
  }

  if (!show) return null;

  return (
    <section className="home-offer" aria-labelledby="system-audio-offer-heading">
      <h2 id="system-audio-offer-heading">
        <MonitorSpeaker size={16} strokeWidth={1.7} className="home-offer-icon" />
        Record calls without Screen Recording
      </h2>
      <div className="home-offer-actions">
        <button
          type="button"
          className="nd-btn nd-btn-primary no-drag"
          disabled={asking}
          onClick={() => void allow()}
        >
          {asking ? "Waiting for macOS…" : "Allow system audio"}
        </button>
        <button type="button" className="nd-btn no-drag" disabled={asking} onClick={decline}>
          Not now
        </button>
      </div>
    </section>
  );
}
