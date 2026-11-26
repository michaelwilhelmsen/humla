import { describe, expect, it } from "vitest";
import commandsRs from "../../src-tauri/src/commands.rs?raw";
import cleanRs from "../../src-tauri/src/commands/clean.rs?raw";
import { CLEAN_OPENAI_MODEL, cleanModelFor, cleanModelSetting } from "./cleanModel";

describe("the clean_model setting (#217)", () => {
  it("names a model for one provider, as the backend reads it", () => {
    expect(cleanModelFor("openai:gpt-6-luna", "openai")).toBe("gpt-6-luna");
    expect(cleanModelFor("local:qwen3:14b", "local")).toBe("qwen3:14b");
    expect(cleanModelFor("openai:gpt-6-luna", "anthropic")).toBe("");
    expect(cleanModelFor("openai:", "openai")).toBe("");
    expect(cleanModelFor("", "openai")).toBe("");
    expect(cleanModelFor(null, "openai")).toBe("");
  });

  it("writes the provider with the model, and nothing for the default", () => {
    expect(cleanModelSetting("openai", "gpt-6-luna")).toBe("openai:gpt-6-luna");
    expect(cleanModelSetting("openai", "")).toBe("");
    expect(cleanModelFor(cleanModelSetting("local", "qwen3:14b"), "local")).toBe("qwen3:14b");
  });

  it("mirrors the backend's default model and reading of the setting", () => {
    expect(commandsRs).toContain(`CLEAN_OPENAI_MODEL: &str = "${CLEAN_OPENAI_MODEL}"`);
    expect(cleanRs).toContain("pub(crate) fn clean_model_for(");
    expect(cleanRs).toContain("Mirrors `cleanModelFor` (`src/lib/cleanModel.ts`)");
  });
});
