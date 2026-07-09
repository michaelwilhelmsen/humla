import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockTauri } from "../../test/tauri";
import { ProviderKeyCard } from "./ProviderKeyCard";

function renderCard(handlers: Parameters<typeof mockTauri>[0] = {}) {
  mockTauri(handlers);
  return render(
    <ProviderKeyCard
      provider="openai"
      label="OpenAI"
      description="Used for cloud transcription and summaries."
    />,
  );
}

describe("ProviderKeyCard", () => {
  it("reflects a stored key: masked placeholder, Test enabled", async () => {
    renderCard({
      provider_key_get: (args) =>
        (args as { provider: string }).provider === "openai" ? "stored" : null,
    });

    expect(
      await screen.findByPlaceholderText(/•+ stored/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /test/i })).toBeEnabled();
    // The row explains itself (design-review amendment).
    expect(screen.getByText("OpenAI")).toBeInTheDocument();
    expect(
      screen.getByText(/used for cloud transcription/i),
    ).toBeInTheDocument();
  });

  it("with no stored key, Test is disabled", async () => {
    renderCard({ provider_key_get: () => null });

    const test = await screen.findByRole("button", { name: /test/i });
    expect(test).toBeDisabled();
  });

  it("saving a typed key persists it and flips to the stored state", async () => {
    const saved: unknown[] = [];
    renderCard({
      provider_key_get: () => null,
      provider_key_set: (args) => {
        saved.push(args);
        return null;
      },
    });
    const input = await screen.findByLabelText(/openai api key/i);

    await userEvent.type(input, "sk-test-123");
    await userEvent.click(screen.getByRole("button", { name: /save/i }));

    expect(saved).toHaveLength(1);
    expect(saved[0]).toMatchObject({ provider: "openai", key: "sk-test-123" });
    // Input clears, masked placeholder appears, Test unlocks.
    expect(input).toHaveValue("");
    expect(
      await screen.findByPlaceholderText(/•+ stored/),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /test/i })).toBeEnabled();
  });

  it("Test reports success and failure from the backend probe", async () => {
    let verdict = { ok: true, status: 200, error: null as string | null };
    renderCard({
      provider_key_get: () => "stored",
      provider_key_test: () => verdict,
    });
    const test = await screen.findByRole("button", { name: /^test$/i });

    await userEvent.click(test);
    expect(await screen.findByText(/connected ✓/i)).toBeInTheDocument();

    verdict = { ok: false, status: 401, error: "invalid api key" };
    await userEvent.click(test);
    expect(
      await screen.findByText(/401: invalid api key/i),
    ).toBeInTheDocument();
    expect(screen.queryByText(/connected ✓/i)).not.toBeInTheDocument();
  });
});
