import { ipc, type DiarizeDownloadProgress, type DiarizeEngine } from "./ipc";

export const DIARIZE_ENGINES: DiarizeEngine[] = ["nemotron3", "community1"];

export const DIARIZE_ENGINE_LABEL: Record<DiarizeEngine, string> = {
  nemotron3: "Nemotron 3",
  community1: "Community-1",
};

/** The most speakers Nemotron 3 tells apart. Mirrors `NEMOTRON_MAX_SPEAKERS`
 *  in diarize.rs. */
export const NEMOTRON_MAX_SPEAKERS = 8;

// Mirrors `DEFAULT_DIARIZE_MODEL` (commands.rs) and `Engine::from_setting`
// (diarize.rs): an unset `diarize_model` is a fresh install, on Nemotron 3,
// since the upgrade migration writes a row for every other install.
export const DEFAULT_DIARIZE_ENGINE: DiarizeEngine = "nemotron3";

export function selectedDiarizeEngine(stored: string | null): DiarizeEngine {
  if (stored === null) return DEFAULT_DIARIZE_ENGINE;
  return stored === "nemotron3" ? "nemotron3" : "community1";
}

/** Community-1 is always wanted: it diarizes any note set above eight speakers,
 *  and it is what a note falls back to while the selected model is missing
 *  (`engine_preference` in diarize.rs). It goes first because it is the small
 *  one. */
export function enginesToDownload(selected: DiarizeEngine): DiarizeEngine[] {
  return selected === "community1" ? ["community1"] : ["community1", selected];
}

/** Fetches whichever of those models aren't on disk yet, one at a time. Never
 *  throws: a model that fails is left for Settings to retry. */
export async function downloadDiarizeModels(): Promise<void> {
  const stored = await ipc.getSetting("diarize_model").catch(() => null);
  for (const engine of enginesToDownload(selectedDiarizeEngine(stored))) {
    try {
      const status = await ipc.diarizeStatus(engine);
      if (!status.downloaded) await ipc.diarizeDownload(engine);
    } catch (e) {
      console.warn(`[diarize] ${engine} download failed:`, e);
    }
  }
}

/** What a model download shows at `phase`: its label, and the progress track's
 *  value, which is `null` for a step that reports no fraction. */
export function downloadProgress(
  phase: DiarizeDownloadProgress["phase"] | null,
  fraction: number,
): { label: string; value: number | null } {
  switch (phase) {
    case "warming":
      return { label: "Preparing for the Neural Engine… This takes about two minutes, once.", value: null };
    case "compiling":
      return { label: "Compiling for the Neural Engine…", value: null };
    case "listing":
      return { label: "Listing files…", value: null };
    default: {
      const value = Math.min(1, Math.max(0, fraction));
      return { label: `Downloading… ${Math.round(value * 100)}%`, value };
    }
  }
}
