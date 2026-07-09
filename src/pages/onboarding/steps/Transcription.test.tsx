import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, act, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { LocalWhisperModelStatus, TranscribeConfig } from "../../../lib/ipc";
import { useDownloadStore } from "../../../lib/store";
import { mockTauri } from "../../../test/tauri";
import { TranscriptionStep } from "./Transcription";
import type { StepContext } from "../types";

// Characterization tests pinned BEFORE the #22 download-machinery
// consolidation: the wizard's staged download presentation (progress panel,
// "continue while it finishes" skip semantics, no double-start on
// back-navigation) must survive the move onto useDownloadStore byte-for-byte
// in behavior. This flow is where fresh-install bug rounds #9–#12 lived.
//
// Outcome signals are deliberately driven through BOTH channels — the
// download invoke's promise (the old implementation's source) and
// useDownloadStore (the global slice fed by bindBackendListeners, the new
// source) — so the same tests hold on either side of the refactor.

const TURBO = "large-v3-turbo-q5";
const NB = "nb-whisper-large-q5";

function whisperModels(over: { turboDownloaded?: boolean } = {}): LocalWhisperModelStatus[] {
  return [
    {
      id: TURBO,
      label: "Large v3 Turbo (quantized)",
      description: "The recommended default for almost all use.",
      filename: "ggml-large-v3-turbo-q5_0.bin",
      sizeBytesHint: 574_000_000,
      kind: "multilingual",
      specificLanguage: null,
      downloaded: over.turboDownloaded ?? false,
      sizeBytes: over.turboDownloaded ? 574_000_000 : null,
      path: null,
    },
    {
      id: NB,
      label: "NB Whisper Large (Norwegian)",
      description: "Norwegian-specific model from the National Library.",
      filename: "nb-whisper-large-q5_0.bin",
      sizeBytesHint: 1_160_000_000,
      kind: "language_specific",
      specificLanguage: "no",
      downloaded: false,
      sizeBytes: null,
      path: null,
    },
  ];
}

function ctx(): StepContext {
  return {
    stepId: "transcription",
    index: 3,
    total: 6,
    goNext: vi.fn(),
    goBack: vi.fn(),
    goTo: vi.fn(),
    canGoBack: true,
    complete: vi.fn(),
  } as unknown as StepContext;
}

function renderStep(handlers: Parameters<typeof mockTauri>[0] = {}) {
  mockTauri({
    local_whisper_models: () => whisperModels(),
    ...handlers,
  });
  const c = ctx();
  const view = render(<TranscriptionStep ctx={c} />);
  return { ...view, ctx: c };
}

// The invoke promise for a download; controllable so tests can settle it
// (the module-level guard in the pre-refactor implementation only clears
// when this promise settles — leaving it pending would leak across tests).
function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

async function onDeviceCard() {
  const btn = await screen.findByRole("button", { name: /on-device/i });
  await waitFor(() => expect(btn).toBeEnabled());
  return btn;
}

function storeProgress(modelId: string, received: number, total: number | null) {
  act(() => {
    useDownloadStore.getState().setProgress({ modelId, received, total });
  });
}

beforeEach(() => {
  useDownloadStore.getState().clear();
});

describe("onboarding TranscriptionStep — layout", () => {
  it("shows both cards with On-device recommended on Apple Silicon, Continue gated", async () => {
    renderStep();

    const local = await onDeviceCard();
    expect(screen.getByText("Cloud API")).toBeInTheDocument();
    // Exactly one Recommended badge, inside the On-device card.
    const badges = screen.getAllByText(/recommended/i);
    expect(badges).toHaveLength(1);
    expect(local).toContainElement(badges[0]);
    // Nothing chosen yet → can't continue, but the quiet skip is there.
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /skip for now/i })).toBeEnabled();
  });

  it("moves the recommendation to Cloud on Intel with a slowness warning", async () => {
    renderStep({ system_arch: () => "x86_64" });

    await onDeviceCard();
    expect(
      screen.getByText(/on-device transcription is slow on intel macs/i),
    ).toBeInTheDocument();
    const badges = screen.getAllByText(/recommended/i);
    expect(badges).toHaveLength(1);
    expect(
      screen.getByRole("button", { name: /on-device/i }),
    ).not.toContainElement(badges[0]);
  });

  it("routes Norwegian to the National Library model", async () => {
    const downloads: string[] = [];
    let written: TranscribeConfig | null = null;
    const dl = deferred<null>();
    renderStep({
      settings_get: (args) =>
        (args as { key: string }).key === "language" ? "no" : null,
      set_transcribe_config: (args) => {
        written = (args as { config: TranscribeConfig }).config;
        return null;
      },
      local_whisper_download: (args) => {
        downloads.push((args as { modelId: string }).modelId);
        return dl.promise;
      },
    });

    expect(
      await screen.findByText(/national library of norway/i),
    ).toBeInTheDocument();

    await userEvent.click(await onDeviceCard());
    await waitFor(() => expect(downloads).toEqual([NB]));
    expect(written!.default).toMatchObject({ provider: "local", model_id: NB });

    dl.resolve(null);
  });
});

describe("onboarding TranscriptionStep — on-device download flow", () => {
  it("selecting On-device commits the local config and starts the download, with Continue open mid-download", async () => {
    const downloads: string[] = [];
    const diarizeDownloads: unknown[] = [];
    let written: TranscribeConfig | null = null;
    const dl = deferred<null>();
    const { ctx: c } = renderStep({
      get_transcribe_config: () => ({
        default: { provider: "openai", model: "whisper-1" },
        per_language: { fr: { provider: "deepgram", model: "nova-3" } },
      }),
      set_transcribe_config: (args) => {
        written = (args as { config: TranscribeConfig }).config;
        return null;
      },
      local_whisper_download: (args) => {
        downloads.push((args as { modelId: string }).modelId);
        return dl.promise;
      },
      diarize_download: (args) => {
        diarizeDownloads.push(args);
        return null;
      },
    });

    await userEvent.click(await onDeviceCard());

    // Commit point: local + the language-routed model, per-language overrides preserved.
    await waitFor(() =>
      expect(written!.default).toMatchObject({ provider: "local", model_id: TURBO }),
    );
    expect(written!.per_language).toMatchObject({ fr: { provider: "deepgram" } });
    await waitFor(() => expect(downloads).toEqual([TURBO]));

    // Staged presentation: the download runs inline but never blocks the step.
    expect(
      await screen.findByText(/you can continue while it finishes/i),
    ).toBeInTheDocument();
    const cont = screen.getByRole("button", { name: /^continue$/i });
    expect(cont).toBeEnabled();

    // Live progress reaches the panel.
    storeProgress(TURBO, 287_000_000, 574_000_000);
    expect(await screen.findByText(/downloading/i)).toBeInTheDocument();

    // Continuing mid-download advances and quietly kicks off the diarize model.
    await userEvent.click(cont);
    expect(c.goNext).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(diarizeDownloads).toHaveLength(1));

    dl.resolve(null);
    act(() => useDownloadStore.getState().clear());
  });

  it("shows Model ready without re-downloading when the model is already on disk", async () => {
    const downloads: string[] = [];
    renderStep({
      local_whisper_models: () => whisperModels({ turboDownloaded: true }),
      local_whisper_download: (args) => {
        downloads.push((args as { modelId: string }).modelId);
        return null;
      },
    });

    await userEvent.click(await onDeviceCard());

    expect(await screen.findByText(/model ready/i)).toBeInTheDocument();
    expect(downloads).toHaveLength(0);
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeEnabled();
  });

  it("flips to Model ready when the download completes", async () => {
    let turboDownloaded = false;
    const dl = deferred<null>();
    renderStep({
      local_whisper_models: () => whisperModels({ turboDownloaded }),
      local_whisper_download: () => dl.promise,
    });

    await userEvent.click(await onDeviceCard());
    expect(
      await screen.findByText(/you can continue while it finishes/i),
    ).toBeInTheDocument();
    storeProgress(TURBO, 287_000_000, 574_000_000);

    // Completion: the file lands on disk, the invoke resolves, and the
    // global download slice clears (what bindBackendListeners does on the
    // terminal progress event).
    turboDownloaded = true;
    dl.resolve(null);
    act(() => useDownloadStore.getState().clear());

    expect(await screen.findByText(/model ready/i)).toBeInTheDocument();
  });

  it("surfaces a failed download start inline", async () => {
    renderStep({
      local_whisper_download: () => {
        throw new Error("no space left on device");
      },
    });

    await userEvent.click(await onDeviceCard());

    expect(await screen.findByText(/download failed/i)).toBeInTheDocument();
    expect(screen.getByText(/no space left on device/i)).toBeInTheDocument();
  });

  it("does not restart an in-flight download when the user navigates back to the step", async () => {
    const downloads: string[] = [];
    const dl = deferred<null>();
    const handlers: Parameters<typeof mockTauri>[0] = {
      // A resuming user: the local choice is already committed.
      get_transcribe_config: () => ({
        default: { provider: "local", model_id: TURBO, preset: "quality", use_gpu: true },
        per_language: {},
      }),
      local_whisper_download: (args) => {
        downloads.push((args as { modelId: string }).modelId);
        return dl.promise;
      },
    };
    const first = renderStep(handlers);

    await userEvent.click(await onDeviceCard());
    await waitFor(() => expect(downloads).toEqual([TURBO]));
    storeProgress(TURBO, 100_000_000, 574_000_000);

    // Navigate away and back: the download keeps running on the backend.
    first.unmount();
    mockTauri({ local_whisper_models: () => whisperModels(), ...handlers });
    render(<TranscriptionStep ctx={ctx()} />);

    // The panel reflects the still-running download…
    expect(
      await screen.findByText(/you can continue while it finishes/i),
    ).toBeInTheDocument();

    // …and re-selecting the card must not start a second download.
    await userEvent.click(await onDeviceCard());
    await new Promise((r) => setTimeout(r, 50));
    expect(downloads).toEqual([TURBO]);

    dl.resolve(null);
    act(() => useDownloadStore.getState().clear());
  });
});

describe("onboarding TranscriptionStep — cloud path", () => {
  it("a passing key Test commits the provider as the transcription default", async () => {
    let written: TranscribeConfig | null = null;
    renderStep({
      provider_key_get: () => null,
      provider_key_set: () => null,
      provider_key_test: () => ({ ok: true, status: 200, error: null }),
      set_transcribe_config: (args) => {
        written = (args as { config: TranscribeConfig }).config;
        return null;
      },
    });

    await onDeviceCard(); // wait out the loading gate
    await userEvent.click(screen.getByRole("button", { name: /cloud api/i }));

    // Revealed cloud surface, still can't continue without a tested key.
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeDisabled();
    await userEvent.type(await screen.findByPlaceholderText("sk-…"), "sk-test");
    await userEvent.click(screen.getByRole("button", { name: /save/i }));
    await userEvent.click(screen.getByRole("button", { name: /^test$/i }));

    expect(await screen.findByText(/connected — openai/i)).toBeInTheDocument();
    expect(written!.default).toMatchObject({ provider: "openai", model: "whisper-1" });
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeEnabled();
  });

  it("a failing Test keeps Continue closed and shows the error", async () => {
    renderStep({
      provider_key_get: () => null,
      provider_key_set: () => null,
      provider_key_test: () => ({ ok: false, status: 401, error: "invalid api key" }),
      set_transcribe_config: () => {
        throw new Error("must not commit a failed provider");
      },
    });

    await onDeviceCard();
    await userEvent.click(screen.getByRole("button", { name: /cloud api/i }));
    await userEvent.type(await screen.findByPlaceholderText("sk-…"), "sk-bad");
    await userEvent.click(screen.getByRole("button", { name: /save/i }));
    await userEvent.click(screen.getByRole("button", { name: /^test$/i }));

    expect(await screen.findByText(/invalid api key/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeDisabled();
  });
});

describe("onboarding TranscriptionStep — skip semantics", () => {
  it("Skip advances with nothing chosen and quietly fires the diarize download", async () => {
    const diarizeDownloads: unknown[] = [];
    const { ctx: c } = renderStep({
      diarize_download: (args) => {
        diarizeDownloads.push(args);
        return null;
      },
    });

    await onDeviceCard();
    await userEvent.click(screen.getByRole("button", { name: /skip for now/i }));

    expect(c.goNext).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(diarizeDownloads).toHaveLength(1));
  });

  it("skips the diarize download when the model is already present", async () => {
    const diarizeDownloads: unknown[] = [];
    const { ctx: c } = renderStep({
      diarize_status: () => ({ downloaded: true, sizeBytes: 30_000_000, path: "/x" }),
      diarize_download: (args) => {
        diarizeDownloads.push(args);
        return null;
      },
    });

    await onDeviceCard();
    await userEvent.click(screen.getByRole("button", { name: /skip for now/i }));

    expect(c.goNext).toHaveBeenCalledTimes(1);
    await new Promise((r) => setTimeout(r, 50));
    expect(diarizeDownloads).toHaveLength(0);
  });
});
