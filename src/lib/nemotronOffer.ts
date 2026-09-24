import { create } from "zustand";
import { selectedDiarizeEngine } from "./diarizeEngine";
import { ipc, onDiarizeDownloadProgress, type DiarizeDownloadProgress } from "./ipc";
import { writeSetting } from "./settingsBus";

// The offer to switch to Nemotron 3 that an upgraded install gets: the upgrade
// kept it on community-1 and stored the offer as pending. Held outside React
// because the download outlives the card that started it.
type OfferSetting = "pending" | "accepted" | "declined";

export type NemotronOfferState =
  | { stage: "hidden" }
  | { stage: "offered"; error: string | null }
  | { stage: "downloading"; fraction: number; phase: DiarizeDownloadProgress["phase"] | null }
  | { stage: "done" };

export const useNemotronOffer = create<{ offer: NemotronOfferState }>(() => ({
  offer: { stage: "hidden" },
}));

const current = () => useNemotronOffer.getState().offer;
const show = (offer: NemotronOfferState) => useNemotronOffer.setState({ offer });
const busy = () => current().stage === "downloading" || current().stage === "done";
const writeOffer = (value: OfferSetting) => writeSetting("nemotron_offer", value);

export async function loadNemotronOffer(): Promise<void> {
  if (busy()) return;
  const [offer, engine] = await Promise.all([
    ipc.getSetting("nemotron_offer").catch(() => null),
    ipc.getSetting("diarize_model").catch(() => null),
  ]);
  // An accept started while the settings were read has already moved it on.
  if (busy()) return;
  const was = current();
  const pending: OfferSetting = "pending";
  show(
    offer === pending && selectedDiarizeEngine(engine) !== "nemotron3"
      ? { stage: "offered", error: was.stage === "offered" ? was.error : null }
      : { stage: "hidden" },
  );
}

/** Downloads the model, which ends with its Neural Engine warm-up, and only
 *  then switches the engine. A failure leaves both the engine and the offer. */
export async function acceptNemotronOffer(): Promise<void> {
  if (busy()) return;
  show({ stage: "downloading", fraction: 0, phase: null });
  const unlisten = await onDiarizeDownloadProgress((p) => {
    if (p.engine === "nemotron3" && current().stage === "downloading") {
      show({ stage: "downloading", fraction: p.fraction, phase: p.phase });
    }
  });
  try {
    await ipc.diarizeDownload("nemotron3");
    await writeSetting("diarize_model", "nemotron3");
    await writeOffer("accepted");
    show({ stage: "done" });
  } catch (e) {
    show({ stage: "offered", error: e instanceof Error ? e.message : String(e) });
  } finally {
    unlisten();
  }
}

export async function declineNemotronOffer(): Promise<void> {
  await writeOffer("declined");
  show({ stage: "hidden" });
}

export function closeNemotronOffer(): void {
  show({ stage: "hidden" });
}
