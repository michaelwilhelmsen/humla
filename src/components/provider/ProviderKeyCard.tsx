import { Btn } from "../../pages/settings/components/Btn";
import { useProviderKey, type KeyProvider } from "./useProviderKey";

export type { KeyProvider };

// Default row copy per provider — consumers can override, but usually
// shouldn't have to.
const DEFAULT_COPY: Record<
  KeyProvider,
  { label: string; description: string; placeholder: string }
> = {
  openai: {
    label: "OpenAI",
    description:
      "Cloud transcription (whisper-1, gpt-4o-transcribe) and cloud summaries.",
    placeholder: "sk-…",
  },
  deepgram: {
    label: "Deepgram",
    description: "Nova-3 and Nova-2 cloud transcription.",
    placeholder: "Deepgram API key",
  },
  groq: {
    label: "Groq",
    description: "Fast cloud Whisper (whisper-large-v3-turbo).",
    placeholder: "gsk_…",
  },
};

// Settings-flavored key row over the shared useProviderKey mechanics.
export function ProviderKeyCard({
  provider,
  label = DEFAULT_COPY[provider].label,
  description = DEFAULT_COPY[provider].description,
  placeholder = DEFAULT_COPY[provider].placeholder,
}: {
  provider: KeyProvider;
  label?: string;
  description?: string;
  placeholder?: string;
}) {
  const key = useProviderKey(provider);

  return (
    <div className="py-3.5">
      <div className="text-sm">{label}</div>
      <p className="text-xs text-[var(--color-text-muted)] mt-0.5 mb-2">
        {description}
      </p>
      <div className="flex gap-2">
        <input
          type="password"
          value={key.draft}
          onChange={(e) => key.setDraft(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && key.save()}
          placeholder={key.hasKey ? "•••••••• stored" : placeholder}
          aria-label={`${label} API key`}
          className="flex-1 min-w-0 text-sm px-3 py-2 rounded-md border border-[var(--color-line-visible)] bg-[var(--color-surface)] focus:border-[var(--color-text-muted)] transition-colors"
        />
        <Btn onClick={key.save} disabled={!key.draft.trim()}>
          Save
        </Btn>
        <Btn onClick={key.test} disabled={!key.hasKey || key.testing}>
          {key.testing ? "Testing…" : "Test"}
        </Btn>
      </div>
      {key.result?.ok === true && (
        <p className="text-xs text-[var(--color-success)] mt-2">Connected ✓</p>
      )}
      {key.result?.ok === false && (
        <p className="text-xs text-[var(--color-danger)] mt-2 break-all">
          {key.result.message}
        </p>
      )}
    </div>
  );
}
