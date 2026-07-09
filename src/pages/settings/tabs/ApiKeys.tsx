import { Section } from "../components/Section";
import { ProviderKeyCard } from "../../../components/provider/ProviderKeyCard";

// Interim home for the provider keys (rendered under Transcription since the
// standalone tab dissolved). The full 2/2 rebuild inlines each card next to
// its provider picker; the cards are already the shared, self-contained kind.
export function ApiKeysTab() {
  return (
    <Section title="API keys">
      <ProviderKeyCard
        provider="openai"
        label="OpenAI"
        description="Cloud transcription (whisper-1, gpt-4o-transcribe) and cloud summaries."
        placeholder="sk-…"
      />
      <ProviderKeyCard
        provider="deepgram"
        label="Deepgram"
        description="Nova-3 and Nova-2 cloud transcription."
        placeholder="Deepgram API key"
      />
      <ProviderKeyCard
        provider="groq"
        label="Groq"
        description="Fast cloud Whisper (whisper-large-v3-turbo)."
        placeholder="gsk_…"
      />
    </Section>
  );
}
