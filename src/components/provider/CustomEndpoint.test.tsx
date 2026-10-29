import { describe, it, expect, beforeEach, vi } from "vitest";
import { useState } from "react";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { CustomEndpointCard, CustomModelField } from "./CustomEndpoint";
import { mockTauri } from "../../test/tauri";

beforeEach(() => {
  mockTauri();
});

function Card({ testModel, onBaseUrl }: { testModel?: string; onBaseUrl?: (v: string) => void }) {
  const [url, setUrl] = useState("");
  return (
    <CustomEndpointCard
      baseUrl={url}
      onBaseUrlChange={(v) => {
        setUrl(v);
        onBaseUrl?.(v);
      }}
      testModel={testModel}
    />
  );
}

describe("CustomEndpointCard", () => {
  it("writes the base URL exactly as typed", async () => {
    const onBaseUrl = vi.fn();
    render(<Card onBaseUrl={onBaseUrl} />);
    await userEvent.type(screen.getByLabelText("Custom endpoint base URL"), "https://openrouter.ai/api/v1/");
    expect(onBaseUrl).toHaveBeenLastCalledWith("https://openrouter.ai/api/v1/");
  });

  it("stores an optional key under the custom provider", async () => {
    const set = vi.fn();
    mockTauri({ provider_key_set: set });
    render(<Card />);
    await userEvent.type(screen.getByLabelText("Custom endpoint API key"), "sk-or-test");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(set).toHaveBeenCalledWith({ provider: "custom", key: "sk-or-test" });
  });

  it("removes a stored key, for an endpoint that takes none", async () => {
    const set = vi.fn();
    mockTauri({ provider_key_get: () => "stored", provider_key_set: set });
    render(<Card />);
    await userEvent.click(await screen.findByRole("button", { name: "Remove key" }));
    expect(set).toHaveBeenCalledWith({ provider: "custom", key: "" });
  });

  it("tests the listing and a completion with the surface's model, reported apart", async () => {
    const test = vi.fn(() => ({
      listing: { outcome: "failed", message: "HTTP 404 from https://gw.example/v1/models." },
      completion: { outcome: "ok", message: "deepseek-chat answered." },
    }));
    mockTauri({ custom_endpoint_test: test });
    render(<Card testModel="deepseek-chat" />);
    await userEvent.type(screen.getByLabelText("Custom endpoint base URL"), "https://gw.example/v1");
    await userEvent.click(screen.getByRole("button", { name: "Test" }));
    expect(await screen.findByText(/HTTP 404 from https:\/\/gw\.example\/v1\/models/)).toBeInTheDocument();
    expect(screen.getByText(/deepseek-chat answered/)).toBeInTheDocument();
    expect(test).toHaveBeenCalledWith({ model: "deepseek-chat" });
  });

  it("can't be tested before it has a base URL", () => {
    render(<Card />);
    expect(screen.getByRole("button", { name: "Test" })).toBeDisabled();
  });
});

describe("CustomModelField", () => {
  it("offers what the endpoint lists", async () => {
    mockTauri({ custom_list_models: () => ["deepseek/deepseek-chat", "openai/gpt-5.5"] });
    const onChange = vi.fn();
    render(<CustomModelField value="" onChange={onChange} description="For summaries." />);
    await userEvent.click(screen.getByRole("button", { name: "Type a model…" }));
    await userEvent.click(await screen.findByRole("option", { name: "openai/gpt-5.5" }));
    expect(onChange).toHaveBeenCalledWith("openai/gpt-5.5");
  });

  // A gateway that serves no listing still answers completions, so a failed
  // listing must never stand between the user and a model.
  it("takes any typed model, listing or not", async () => {
    mockTauri({
      custom_list_models: () => {
        throw new Error("HTTP 404");
      },
    });
    const onChange = vi.fn();
    render(<CustomModelField value="old-model" onChange={onChange} description="For chat." />);
    await userEvent.click(screen.getByRole("button", { name: "old-model" }));
    const input = screen.getByRole("combobox", { name: "Custom endpoint model" });
    await userEvent.clear(input);
    await userEvent.type(input, "  my/model {Enter}");
    expect(onChange).toHaveBeenCalledWith("my/model");
  });

  it("Escape leaves the model as it was", async () => {
    const onChange = vi.fn();
    render(<CustomModelField value="kept" onChange={onChange} description="For chat." />);
    await userEvent.click(screen.getByRole("button", { name: "kept" }));
    await userEvent.type(screen.getByRole("combobox", { name: "Custom endpoint model" }), "x{Escape}");
    expect(onChange).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "kept" })).toBeInTheDocument();
  });
});
