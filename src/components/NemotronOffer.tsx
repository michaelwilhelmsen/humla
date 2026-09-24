import { useEffect } from "react";
import { Check, Users, X } from "lucide-react";
import { NEMOTRON_MAX_SPEAKERS, downloadProgress } from "../lib/diarizeEngine";
import type { DiarizeDownloadProgress } from "../lib/ipc";
import {
  acceptNemotronOffer,
  closeNemotronOffer,
  declineNemotronOffer,
  loadNemotronOffer,
  useNemotronOffer,
} from "../lib/nemotronOffer";
import { ProgressTrack } from "./ui/ProgressTrack";

// Home's card offering an upgraded install the switch to Nemotron 3.
export function NemotronOffer() {
  const offer = useNemotronOffer((s) => s.offer);

  useEffect(() => {
    void loadNemotronOffer();
  }, []);

  if (offer.stage === "hidden") return null;

  if (offer.stage === "done") {
    return (
      <section className="home-offer" aria-label="Nemotron 3 speaker labels">
        <div className="home-offer-done" role="status">
          <Check size={16} strokeWidth={1.8} className="home-offer-icon" />
          <p>Nemotron 3 is ready. New recordings get their speaker labels from it.</p>
          <button
            type="button"
            className="nd-btn-icon-sm no-drag"
            onClick={closeNemotronOffer}
            aria-label="Close"
          >
            <X size={14} />
          </button>
        </div>
      </section>
    );
  }

  return (
    <section className="home-offer" aria-labelledby="home-offer-heading">
      <h2 id="home-offer-heading">
        <Users size={16} strokeWidth={1.7} className="home-offer-icon" />
        Sharper speaker labels
      </h2>
      <p>
        Nemotron 3 tells up to {NEMOTRON_MAX_SPEAKERS} speakers apart and counts them
        itself, so a meeting no longer ends up under one speaker when its speaker count
        is left on Auto.
      </p>
      {offer.stage === "downloading" ? (
        <DownloadProgress fraction={offer.fraction} phase={offer.phase} />
      ) : (
        <>
          <p className="home-offer-meta">
            A 193 MB download, then about two minutes preparing it for your Mac, once.
            You can also switch later in Settings → Transcription.
          </p>
          {offer.error && (
            <p className="home-offer-error" role="alert">
              The download didn’t finish: {offer.error}
            </p>
          )}
          <div className="home-offer-actions">
            <button
              type="button"
              className="nd-btn nd-btn-primary no-drag"
              onClick={() => void acceptNemotronOffer()}
            >
              Switch to Nemotron 3
            </button>
            <button
              type="button"
              className="nd-btn no-drag"
              onClick={() => void declineNemotronOffer()}
            >
              Not now
            </button>
          </div>
        </>
      )}
    </section>
  );
}

function DownloadProgress({
  fraction,
  phase,
}: {
  fraction: number;
  phase: DiarizeDownloadProgress["phase"] | null;
}) {
  const { label, value } = downloadProgress(phase, fraction);
  return (
    <div className="home-offer-progress">
      <p className="home-offer-meta" aria-live="polite">
        {label}
      </p>
      <ProgressTrack value={value} label={label} />
    </div>
  );
}
