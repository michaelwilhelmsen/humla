import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, act, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type {
  LocalWhisperModelStatus,
  ProviderConfig,
  TranscribeConfig,
} from "../../../lib/ipc";
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

  // #148 — the on-device model is derived from the meeting language, but the
  // config was only ever written from the card's click handler. Going back to
  // change the language left the card advertising NB Whisper while the stored
  // config still routed to the multilingual turbo model, with Continue enabled
  // and NB Whisper never downloaded. Same root cause as #147, but silent.
  it("reconciles the stored config when the language changed after On-device was chosen", async () => {
    const downloads: string[] = [];
    let written: TranscribeConfig | null = null;
    const dl = deferred<null>();
    renderStep({
      // Language is now Norwegian…
      settings_get: (args) =>
        (args as { key: string }).key === "language" ? "no" : null,
      // …but the stored config is from the earlier English pass.
      get_transcribe_config: () => ({
        default: { provider: "local", model_id: TURBO, preset: "quality", use_gpu: true },
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
    });

    // The card resumes as selected and advertises the Norwegian model...
    expect(
      await screen.findByText(/national library of norway/i),
    ).toBeInTheDocument();

    // ...so the stored config must agree, with no user interaction at all.
    await waitFor(() =>
      expect(written!.default).toMatchObject({ provider: "local", model_id: NB }),
    );
    // Per-language overrides survive the reconciliation.
    expect(written!.per_language).toMatchObject({ fr: { provider: "deepgram" } });
    // And the model it now claims is actually being fetched.
    await waitFor(() => expect(downloads).toEqual([NB]));

    // The card's download state describes NB, not the turbo model it used to.
    expect(
      await screen.findByText(/you can continue while it finishes/i),
    ).toBeInTheDocument();
    storeProgress(NB, 580_000_000, 1_160_000_000);
    expect(await screen.findByText(/downloading/i)).toBeInTheDocument();
    // A stale progress event for the old model must not be adopted.
    storeProgress(TURBO, 574_000_000, 574_000_000);
    await waitFor(() => expect(screen.queryByText(/model ready/i)).toBeNull());

    dl.resolve(null);
    act(() => useDownloadStore.getState().clear());
  });

  // The reconciliation must be idempotent: this step re-renders on every
  // download-store tick, and a write per tick would hammer the DB.
  it("writes nothing when the stored config already names the derived model", async () => {
    let writes = 0;
    const downloads: string[] = [];
    renderStep({
      settings_get: (args) =>
        (args as { key: string }).key === "language" ? "no" : null,
      local_whisper_models: () => whisperModels(),
      get_transcribe_config: () => ({
        default: { provider: "local", model_id: NB, preset: "quality", use_gpu: true },
        per_language: {},
      }),
      set_transcribe_config: () => {
        writes++;
        return null;
      },
      local_whisper_download: (args) => {
        downloads.push((args as { modelId: string }).modelId);
        return null;
      },
    });

    expect(
      await screen.findByText(/national library of norway/i),
    ).toBeInTheDocument();

    // Several store ticks' worth of re-renders (this is the back-navigation
    // path: a live download for this model makes the panel show).
    storeProgress(NB, 100, 1000);
    storeProgress(NB, 400, 1000);
    storeProgress(NB, 900, 1000);
    await waitFor(() => expect(screen.getByText(/downloading/i)).toBeInTheDocument());
    expect(writes).toBe(0);

    // Re-selecting the card explicitly is also not a reason to rewrite.
    await userEvent.click(await onDeviceCard());
    await new Promise((r) => setTimeout(r, 50));
    expect(writes).toBe(0);
    expect(downloads).toHaveLength(0); // already in flight per the store

    act(() => useDownloadStore.getState().clear());
  });

  // Found reviewing the #148 fix. `language` is read with a catch, and a failed
  // read is indistinguishable from an unset one — both route to the default
  // model. Left unguarded, the reconcile effect would "correct" a perfectly good
  // Norwegian config down to the default and fetch a gigabyte to do it.
  it("does not rewrite a stored config when the language read failed", async () => {
    let writes = 0;
    const downloads: string[] = [];
    renderStep({
      settings_get: (args) => {
        if ((args as { key: string }).key === "language") throw new Error("db locked");
        return null;
      },
      get_transcribe_config: () => ({
        default: { provider: "local", model_id: NB, preset: "quality", use_gpu: true },
        per_language: {},
      }),
      set_transcribe_config: () => {
        writes++;
        return null;
      },
      local_whisper_download: (args) => {
        downloads.push((args as { modelId: string }).modelId);
        return null;
      },
    });

    await onDeviceCard(); // the step still renders and is usable
    await new Promise((r) => setTimeout(r, 50));

    expect(writes).toBe(0);
    expect(downloads).toHaveLength(0);
  });

  // Found reviewing the #148 fix. A failed config write must not be silent:
  // Continue stays open by design, so with no message the user walks out with a
  // card advertising one model and a config routing to another — #148 again.
  it("surfaces a failed config write and retries it on re-selection", async () => {
    let attempts = 0;
    renderStep({
      set_transcribe_config: () => {
        attempts++;
        if (attempts === 1) throw new Error("disk full");
        return null;
      },
      local_whisper_download: () => null,
    });

    await userEvent.click(await onDeviceCard());

    expect(await screen.findByText(/couldn't save this choice/i)).toBeInTheDocument();
    expect(screen.getByText(/disk full/i)).toBeInTheDocument();
    // Continue must NOT become harder to reach than before.
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeEnabled();

    // Re-selecting the card retries, and the message clears.
    await userEvent.click(await onDeviceCard());
    await waitFor(() => expect(attempts).toBe(2));
    await waitFor(() =>
      expect(screen.queryByText(/couldn't save this choice/i)).toBeNull(),
    );
  });

  // Found reviewing the #148 fix. The per-mount download guard made a failed
  // download a dead end: the card is the only retry affordance, and the guard
  // swallowed the second click.
  it("retries a failed download when the card is selected again", async () => {
    const downloads: string[] = [];
    renderStep({
      local_whisper_download: (args) => {
        downloads.push((args as { modelId: string }).modelId);
        throw new Error("connection reset");
      },
    });

    await userEvent.click(await onDeviceCard());
    expect(await screen.findByText(/download failed/i)).toBeInTheDocument();

    await userEvent.click(await onDeviceCard());
    await waitFor(() => expect(downloads).toEqual([TURBO, TURBO]));

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

  it("still shows a failure that landed while the user was on another step", async () => {
    // Post-consolidation behavior: the failure lives in the global download
    // slice (bindBackendListeners fails it on the download-error event), so
    // it survives this step's unmount — the invoke promise that could have
    // reported it died with the mount that started the download.
    const handlers: Parameters<typeof mockTauri>[0] = {
      get_transcribe_config: () => ({
        default: { provider: "local", model_id: TURBO, preset: "quality", use_gpu: true },
        per_language: {},
      }),
      local_whisper_download: () => new Promise(() => {}),
    };
    const first = renderStep(handlers);
    await userEvent.click(await onDeviceCard());
    storeProgress(TURBO, 100_000_000, 574_000_000);
    first.unmount();

    // The download dies while the user is elsewhere in the wizard.
    act(() => {
      useDownloadStore.getState().fail({ modelId: TURBO, message: "connection reset" });
    });

    mockTauri({ local_whisper_models: () => whisperModels(), ...handlers });
    render(<TranscriptionStep ctx={ctx()} />);

    expect(await screen.findByText(/download failed/i)).toBeInTheDocument();
    expect(screen.getByText(/connection reset/i)).toBeInTheDocument();

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

// #149 — a returning user's stored choice must be readable off the config.
// The old guard pre-selected "any cloud provider that isn't openai", because
// openai is ALSO the fresh-install fallback and the step couldn't tell "nobody
// has chosen yet" from "the user chose OpenAI". Key presence is what separates
// them — the same rule computeSetupStatus already uses to decide whether a
// cloud default is working or is the fresh-install "none" state.
describe("onboarding TranscriptionStep — resuming a stored config", () => {
  // Config + Keychain state for a returning user, with per-provider keys so a
  // test can store a key for one provider and not another.
  function resume(
    def: ProviderConfig,
    keys: Partial<Record<"openai" | "deepgram" | "groq", string>> = {},
  ): Parameters<typeof mockTauri>[0] {
    return {
      get_transcribe_config: () => ({ default: def, per_language: {} }),
      provider_key_get: (args) =>
        keys[(args as { provider: "openai" | "deepgram" | "groq" }).provider] ?? null,
    };
  }

  function providerTrigger() {
    return screen.getByRole("combobox", { name: "Provider" });
  }

  it("pre-selects Cloud for a stored OpenAI default whose key is present", async () => {
    renderStep(
      resume({ provider: "openai", model: "whisper-1" }, { openai: "sk-stored" }),
    );

    await onDeviceCard(); // wait out the loading gate

    // The panel is open, showing OpenAI and the stored-key sentinel...
    expect(await screen.findByPlaceholderText("•••••••• stored")).toBeInTheDocument();
    expect(providerTrigger()).toHaveTextContent("OpenAI");
    // ...and Test is live with no re-Save.
    expect(screen.getByRole("button", { name: /^test$/i })).toBeEnabled();
  });

  it("pre-selects Cloud for a stored Deepgram or Groq default", async () => {
    const { unmount } = renderStep(
      resume({ provider: "deepgram", model: "nova-3" }, { deepgram: "dg-stored" }),
    );
    await onDeviceCard();
    expect(await screen.findByPlaceholderText("•••••••• stored")).toBeInTheDocument();
    expect(providerTrigger()).toHaveTextContent("Deepgram");
    unmount();

    renderStep(
      resume(
        { provider: "groq", model: "whisper-large-v3-turbo" },
        { groq: "gsk-stored" },
      ),
    );
    await onDeviceCard();
    expect(await screen.findByPlaceholderText("•••••••• stored")).toBeInTheDocument();
    expect(providerTrigger()).toHaveTextContent("Groq");
  });

  // The whole point of keying off the Keychain: openai-with-no-key IS the
  // fresh-install fallback, and must stay indistinguishable from it.
  it("selects nothing for an OpenAI default with no stored key (fresh install)", async () => {
    renderStep(resume({ provider: "openai", model: "whisper-1" }));

    await onDeviceCard();

    // Cloud panel closed — its fields aren't in the DOM at all.
    expect(screen.queryByRole("combobox", { name: "Provider" })).not.toBeInTheDocument();
    expect(screen.queryByPlaceholderText("sk-…")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeDisabled();
  });

  // Same for a cloud provider the user configured elsewhere and then dropped
  // the key for: without a key there is nothing to resume onto.
  it("selects nothing for a cloud default whose key has gone missing", async () => {
    renderStep(resume({ provider: "deepgram", model: "nova-3" }));

    await onDeviceCard();

    expect(screen.queryByRole("combobox", { name: "Provider" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeDisabled();
  });

  it("selects nothing when the config read fails, and writes nothing", async () => {
    let writes = 0;
    renderStep({
      get_transcribe_config: () => {
        throw new Error("db is on fire");
      },
      provider_key_get: () => "sk-stored",
      set_transcribe_config: () => {
        writes++;
        return null;
      },
    });

    await onDeviceCard();

    expect(screen.queryByRole("combobox", { name: "Provider" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeDisabled();
    await new Promise((r) => setTimeout(r, 50));
    expect(writes).toBe(0);
  });

  // A failed Keychain read is not evidence of a choice. It must degrade to
  // "no key" (select nothing) rather than throwing or pre-selecting blind.
  it("selects nothing when the key read fails, and writes nothing", async () => {
    let writes = 0;
    renderStep({
      get_transcribe_config: () => ({
        default: { provider: "openai", model: "whisper-1" },
        per_language: {},
      }),
      provider_key_get: () => {
        throw new Error("keychain locked");
      },
      set_transcribe_config: () => {
        writes++;
        return null;
      },
    });

    await onDeviceCard();

    expect(screen.queryByRole("combobox", { name: "Provider" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeDisabled();
    await new Promise((r) => setTimeout(r, 50));
    expect(writes).toBe(0);
  });

  // Pre-selection is display-only. The commit points are unchanged: selecting
  // On-device (or its reconcile effect) and a passed cloud key Test.
  it("writes no config merely by resuming onto the Cloud card", async () => {
    let writes = 0;
    renderStep({
      ...resume({ provider: "openai", model: "whisper-1" }, { openai: "sk-stored" }),
      set_transcribe_config: () => {
        writes++;
        return null;
      },
    });

    await onDeviceCard();
    expect(await screen.findByPlaceholderText("•••••••• stored")).toBeInTheDocument();
    await new Promise((r) => setTimeout(r, 50));
    expect(writes).toBe(0);
  });

  // Continue still gates on a Test that passed in THIS mount — a stored key is
  // not a verified one. Deliberately unchanged by #149 (it applies equally to
  // all three cloud providers and is a separate design call).
  it("still gates Continue on a fresh Test, even with a stored key", async () => {
    renderStep({
      ...resume({ provider: "openai", model: "whisper-1" }, { openai: "sk-stored" }),
      provider_key_test: () => ({ ok: true, status: 200, error: null }),
    });

    await onDeviceCard();
    await screen.findByPlaceholderText("•••••••• stored");
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeDisabled();

    await userEvent.click(screen.getByRole("button", { name: /^test$/i }));

    expect(await screen.findByText(/connected — openai/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^continue$/i })).toBeEnabled();
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
