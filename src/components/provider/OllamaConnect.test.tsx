import { describe, it, expect, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockTauri } from "../../test/tauri";
import { OllamaConnect } from "./OllamaConnect";

function renderConnect(
  handlers: Parameters<typeof mockTauri>[0] = {},
  props: Partial<React.ComponentProps<typeof OllamaConnect>> = {},
) {
  mockTauri(handlers);
  const onModelChange = vi.fn();
  const onBaseUrlChange = vi.fn();
  render(
    <OllamaConnect
      baseUrl="http://localhost:11434"
      onBaseUrlChange={onBaseUrlChange}
      model=""
      onModelChange={onModelChange}
      {...props}
    />,
  );
  return { onModelChange, onBaseUrlChange };
}

describe("OllamaConnect", () => {
  it("lists models from a reachable server and picks one", async () => {
    const { onModelChange } = renderConnect({
      local_llm_list_models: () => ["qwen3:8b", "llama3.2:3b"],
    });

    // Reachable: connected state + the server's models offered.
    expect(await screen.findByText(/connected/i)).toBeInTheDocument();

    await userEvent.click(
      screen.getByRole("button", { name: /choose a model|qwen3/i }),
    );
    const listbox = screen.getByRole("listbox");
    await userEvent.click(
      within(listbox).getByRole("option", { name: "llama3.2:3b" }),
    );

    expect(onModelChange).toHaveBeenCalledWith("llama3.2:3b");
  });

  it("waits for an unreachable server and detects it appearing", async () => {
    let up = false;
    renderConnect(
      {
        local_llm_list_models: () => {
          if (!up) throw new Error("connection refused");
          return ["qwen3:8b"];
        },
      },
      { pollMs: 30 },
    );

    // Down: the install/waiting hint, no model picker.
    expect(await screen.findByText(/waiting for the server/i)).toBeInTheDocument();
    expect(screen.queryByText(/connected/i)).not.toBeInTheDocument();

    // Server comes up → next poll flips to connected, no user action needed.
    up = true;
    expect(await screen.findByText(/connected/i)).toBeInTheDocument();
    expect(
      screen.queryByText(/waiting for the server/i),
    ).not.toBeInTheDocument();
  });

  it("warns when the stored model is gone from the server", async () => {
    renderConnect(
      { local_llm_list_models: () => ["llama3.2:3b"] },
      { model: "qwen3:8b" },
    );

    expect(
      await screen.findByText(/isn't installed on this server anymore/i),
    ).toBeInTheDocument();
  });
});
