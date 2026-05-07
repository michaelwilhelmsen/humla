import { DiarizeModelManager } from "../components/DiarizeModelManager";
import { LocalModelManager } from "../components/LocalModelManager";
import { Row, Section } from "../components/Section";
import { Select } from "../components/Select";
import { useDeveloperMode } from "../../../lib/useDeveloperMode";
import {
  DEEPGRAM_MODELS,
  GROQ_MODELS,
  LOCAL_PROVIDER,
  PROVIDERS_BASE,
  TRANSCRIBE_MODELS,
  WHISPER_PRESETS,
  inputClass,
  type Provider,
} from "../types";
import type { SettingsHook } from "../useSettings";

export function TranscriptionTab({
  s,
  update,
  providerConfig,
  updateProviderConfig,
  local,
  downloadModel,
  deleteModel,
  diarize,
  downloadDiarize,
  deleteDiarize,
  sortformer,
  downloadSortformer,
  deleteSortformer,
}: Pick<
  SettingsHook,
  | "s"
  | "update"
  | "providerConfig"
  | "updateProviderConfig"
  | "local"
  | "downloadModel"
  | "deleteModel"
  | "diarize"
  | "downloadDiarize"
  | "deleteDiarize"
  | "sortformer"
  | "downloadSortformer"
  | "deleteSortformer"
>) {
  const provider = providerConfig.provider;
  const devMode = useDeveloperMode();

  return (
    <>
      <Section title="Provider">
        <Row label="Source">
          <Select
            value={provider}
            onChange={(v) => {
              const p = v as Provider;
              if (p === "openai") {
                updateProviderConfig({ provider: "openai", model: "whisper-1" });
              } else if (p === "local") {
                updateProviderConfig({
                  provider: "local",
                  model_id:
                    local.models.find((m) => m.kind === "multilingual" && m.downloaded)
                      ?.id ?? "large-v3-turbo-q5",
                  preset: "quality",
                  use_gpu: true,
                });
              } else if (p === "deepgram") {
                updateProviderConfig({ provider: "deepgram", model: "nova-3" });
              } else if (p === "groq") {
                updateProviderConfig({
                  provider: "groq",
                  model: "whisper-large-v3-turbo",
                });
              }
            }}
            options={
              local.models.some((m) => m.downloaded)
                ? [...PROVIDERS_BASE, LOCAL_PROVIDER]
                : PROVIDERS_BASE
            }
          />
          {provider === "local" && !local.models.some((m) => m.downloaded) && (
            <p className="text-xs text-red-600 dark:text-red-400 mt-2">
              No local model is downloaded. Download one below before recording.
            </p>
          )}
        </Row>
        {providerConfig.provider === "openai" && (
          <Row label="Model">
            <Select
              value={providerConfig.model}
              onChange={(v) =>
                updateProviderConfig({ provider: "openai", model: v })
              }
              options={TRANSCRIBE_MODELS.map((m) => ({ value: m, label: m }))}
            />
            {providerConfig.model === "gpt-4o-transcribe-diarize" && (
              <p className="text-xs text-[var(--color-text-muted)] mt-2">
                Note: <code>gpt-4o-transcribe-diarize</code> treats the
                language setting as a hint and does not accept a biasing
                prompt. For strict language locking, use{" "}
                <code>whisper-1</code> or <code>gpt-4o-transcribe</code>.
              </p>
            )}
          </Row>
        )}
        {providerConfig.provider === "deepgram" && (
          <Row label="Model">
            <Select
              value={providerConfig.model}
              onChange={(v) =>
                updateProviderConfig({ provider: "deepgram", model: v })
              }
              options={DEEPGRAM_MODELS.map((m) => ({ value: m, label: m }))}
            />
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              <code>nova-3</code> is the current best for English; falls
              back gracefully to other languages. Word timestamps and
              vocabulary biasing (via <code>keywords</code> param) work on
              every model. Add your Deepgram API key under Settings → API
              keys.
            </p>
          </Row>
        )}
        {providerConfig.provider === "groq" && (
          <Row label="Model">
            <Select
              value={providerConfig.model}
              onChange={(v) =>
                updateProviderConfig({ provider: "groq", model: v })
              }
              options={GROQ_MODELS.map((m) => ({ value: m, label: m }))}
            />
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              Groq hosts <code>whisper-large-v3-turbo</code> at OpenAI-
              compatible endpoints — same Whisper quality, ~10× cheaper
              and faster than OpenAI's hosted Whisper. Add your Groq API
              key under Settings → API keys.
            </p>
          </Row>
        )}
      </Section>

      {providerConfig.provider === "local" && (
        <Section title="Local model behaviour">
          <Row label="Quality preset">
            <Select
              value={providerConfig.preset}
              onChange={(v) =>
                updateProviderConfig({ ...providerConfig, preset: v })
              }
              options={WHISPER_PRESETS}
            />
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              Trades latency for accuracy. Quality runs beam search with
              an aggressive no-speech threshold so almost no segments are
              silently dropped — best for meetings and dense speech. Fast
              falls back to greedy decoding for live-caption snappiness.
            </p>
          </Row>
          <Row label="GPU acceleration">
            <label className="flex items-center gap-2 cursor-pointer text-sm">
              <input
                type="checkbox"
                checked={providerConfig.use_gpu}
                onChange={(e) =>
                  updateProviderConfig({
                    ...providerConfig,
                    use_gpu: e.target.checked,
                  })
                }
              />
              Use Metal (Apple GPU) for Whisper inference
            </label>
            <p className="text-xs text-[var(--color-text-muted)] mt-2">
              On by default — gives ~10× speedup over CPU on Apple
              Silicon. Turn off if Whisper logs Metal compile errors
              like <code>ggml_backend_metal_init: failed to allocate
              context</code>; the app falls back to CPU/BLAS, which is
              slower but reliable.
            </p>
          </Row>
        </Section>
      )}

      <Section title="Local models">
        <LocalModelManager
          state={local}
          activeId={
            providerConfig.provider === "local" ? providerConfig.model_id : ""
          }
          language={s.language}
          onDownload={downloadModel}
          onDelete={deleteModel}
          onSelect={(id) => {
            if (providerConfig.provider === "local") {
              updateProviderConfig({ ...providerConfig, model_id: id });
            } else {
              updateProviderConfig({
                provider: "local",
                model_id: id,
                preset: "quality",
                use_gpu: true,
              });
            }
          }}
        />
      </Section>

      <Section title="Speaker diarization">
        <p className="text-xs text-[var(--color-text-muted)]">
          When downloaded and active, every recording is automatically
          tagged with <code>Speaker 1:</code> / <code>Speaker 2:</code>
          labels after stop. Both engines run locally via CoreML / Apple
          Neural Engine; pick whichever works better for your recordings.
        </p>
        <Row label="Community-1 (clustering)">
          <label className="flex items-center gap-2 cursor-pointer text-sm mb-2">
            <input
              type="radio"
              name="diarize_model"
              checked={s.diarize_model === "community1"}
              disabled={!diarize.status?.downloaded}
              onChange={() => update("diarize_model", "community1")}
            />
            Use Community-1 for new recordings
          </label>
          <DiarizeModelManager
            state={diarize}
            onDownload={downloadDiarize}
            onDelete={deleteDiarize}
          />
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            Pyannote community-1 segmentation + WeSpeaker embeddings + VBx
            clustering. Strong baseline; auto-detects speaker count;
            occasionally collapses on rapid back-and-forth in the same
            channel.
          </p>
        </Row>
        <Row label="Sortformer (end-to-end)">
          <label className="flex items-center gap-2 cursor-pointer text-sm mb-2">
            <input
              type="radio"
              name="diarize_model"
              checked={s.diarize_model === "sortformer"}
              disabled={!sortformer.status?.downloaded}
              onChange={() => update("diarize_model", "sortformer")}
            />
            Use Sortformer for new recordings
          </label>
          <DiarizeModelManager
            state={sortformer}
            onDownload={downloadSortformer}
            onDelete={deleteSortformer}
          />
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            NVIDIA Sortformer running in batch over the saved WAV. Fixed
            4-speaker cap, no count hint. Designed to handle rapid speaker
            changes that the clustering approach struggles with — the
            architectural answer if Community-1 keeps confusing your
            speakers.
          </p>
        </Row>
        {devMode && <Row label="Advanced thresholds">
          <details className="text-sm">
            <summary className="cursor-pointer text-[var(--color-text-muted)]">
              Tune detection thresholds
            </summary>
            <div className="flex flex-col gap-3 mt-3">
              <div>
                <label className="text-xs uppercase tracking-wide text-[var(--color-text-muted)] block mb-1">
                  Community-1 clustering threshold
                </label>
                <input
                  type="text"
                  value={s.community1_threshold}
                  onChange={(e) => update("community1_threshold", e.target.value)}
                  placeholder="0.5"
                  className={inputClass + " w-32"}
                  style={{ fontFamily: "var(--font-mono)" }}
                />
                <p className="text-xs text-[var(--color-text-muted)] mt-1">
                  Higher = more aggressive separation (more speakers).
                  Lower = more merging. Default 0.5. Community-1 only.
                </p>
              </div>
              <div>
                <label className="text-xs uppercase tracking-wide text-[var(--color-text-muted)] block mb-1">
                  Sortformer silence threshold
                </label>
                <input
                  type="text"
                  value={s.sortformer_silence_threshold}
                  onChange={(e) => update("sortformer_silence_threshold", e.target.value)}
                  placeholder="0.5"
                  className={inputClass + " w-32"}
                  style={{ fontFamily: "var(--font-mono)" }}
                />
                <p className="text-xs text-[var(--color-text-muted)] mt-1">
                  Sum of speaker probabilities below which a frame is
                  treated as silence. Default 0.5.
                </p>
              </div>
              <div>
                <label className="text-xs uppercase tracking-wide text-[var(--color-text-muted)] block mb-1">
                  Sortformer prediction threshold
                </label>
                <input
                  type="text"
                  value={s.sortformer_pred_threshold}
                  onChange={(e) => update("sortformer_pred_threshold", e.target.value)}
                  placeholder="0.25"
                  className={inputClass + " w-32"}
                  style={{ fontFamily: "var(--font-mono)" }}
                />
                <p className="text-xs text-[var(--color-text-muted)] mt-1">
                  Speech-probability threshold for crediting a speaker.
                  Default 0.25.
                </p>
              </div>
              <div>
                <label className="text-xs uppercase tracking-wide text-[var(--color-text-muted)] block mb-1">
                  Silence RMS threshold
                </label>
                <input
                  type="text"
                  value={s.silence_rms_threshold}
                  onChange={(e) => update("silence_rms_threshold", e.target.value)}
                  placeholder="0.008"
                  className={inputClass + " w-32"}
                  style={{ fontFamily: "var(--font-mono)" }}
                />
                <p className="text-xs text-[var(--color-text-muted)] mt-1">
                  Chunks with RMS below this are skipped before
                  Whisper sees them — prevents hallucinations on
                  near-silence and HVAC / mic-hiss audio. Higher =
                  drops more borderline chunks (less hallucination,
                  but quiet speech can be cut). Default 0.008. Pure
                  silence ≈ 0.0001, room tone ≈ 0.001, soft speech ≈
                  0.01+.
                </p>
              </div>
            </div>
          </details>
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            Tweaks apply on the next recording or re-diarize. Diagnostic
            JSON is dumped per run — open the Note's diagnostics folder
            from its header to inspect where shifts landed.
          </p>
        </Row>}
      </Section>

      <Section title="Audio retention">
        <Row label="Keep recorded audio">
          <label className="flex items-center gap-2 cursor-pointer text-sm">
            <input
              type="checkbox"
              checked={s.keep_audio === "true"}
              onChange={(e) =>
                update("keep_audio", e.target.checked ? "true" : "false")
              }
            />
            Save the recording's WAV files for re-use after stop
          </label>
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            Off by default — recordings live in the temp dir during
            post-processing and are deleted at the end. Turn on to keep
            both mic and system tracks under{" "}
            <code>{`<app data>/recordings/<note_id>/`}</code> so you can
            re-run diarize at different thresholds, listen back, or
            inspect later. Storage cost: roughly 1 MB per minute of audio
            per channel.
          </p>
        </Row>
      </Section>

      <Section title="Vocabulary">
        <Row label="Custom terms">
          <textarea
            value={s.custom_vocabulary}
            onChange={(e) => update("custom_vocabulary", e.target.value)}
            rows={3}
            placeholder="Tauri, Humla, ScreenCaptureKit, Granola"
            className={inputClass + " leading-relaxed"}
            style={{ fontFamily: "var(--font-mono)" }}
          />
          <p className="text-xs text-[var(--color-text-muted)] mt-2">
            Comma- or newline-separated. Names, jargon, and uncommon
            spellings — biases the transcriber toward these tokens.
            <code> gpt-4o-transcribe-diarize </code> ignores it.
          </p>
        </Row>
      </Section>
    </>
  );
}
