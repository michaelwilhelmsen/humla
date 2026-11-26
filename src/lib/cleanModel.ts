// Which model cleans transcripts (#217). The `clean_model` setting is
// `<provider>:<model>`, so a choice made for one summary provider never
// applies to another; empty means the default. Mirrors
// `clean::clean_model_for` — change both.

/** What cleans on OpenAI by default. Mirrors `CLEAN_OPENAI_MODEL`. */
export const CLEAN_OPENAI_MODEL = "gpt-6.2-sol";

/** The model `setting` names for `provider`, or "" for the default. */
export function cleanModelFor(setting: string | null | undefined, provider: string): string {
  if (!setting) return "";
  const at = setting.indexOf(":");
  if (at < 0 || setting.slice(0, at) !== provider) return "";
  return setting.slice(at + 1).trim();
}

/** The setting for `model` on `provider`; "" for the default. */
export function cleanModelSetting(provider: string, model: string): string {
  return model ? `${provider}:${model}` : "";
}
