import { describe, it, expect } from "vitest";
import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderApp, openSettingsFromSidebar } from "../../test/app";

describe("settings dialog", () => {
  it("opens over the current view instead of replacing it", async () => {
    renderApp("/");
    // Home is up once the onboarding flag resolves.
    await screen.findByRole("button", { name: /new note/i });

    await openSettingsFromSidebar();
    // The view we came from is still mounted behind the dialog.
    expect(
      screen.getByRole("button", { name: /new note/i }),
    ).toBeInTheDocument();
  });

  it("closes on Escape and restores the previous URL", async () => {
    renderApp("/");
    await screen.findByRole("button", { name: /new note/i });
    await openSettingsFromSidebar();
    expect(screen.getByTestId("location")).toHaveTextContent("/settings");

    await userEvent.keyboard("{Escape}");

    expect(
      screen.queryByRole("dialog", { name: /settings/i }),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId("location")).toHaveTextContent(/^\/$/);
    expect(
      screen.getByRole("button", { name: /new note/i }),
    ).toBeInTheDocument();
  });

  it("closes via the ✕ button", async () => {
    renderApp("/");
    await screen.findByRole("button", { name: /new note/i });
    const dialog = await openSettingsFromSidebar();

    await userEvent.click(
      within(dialog).getByRole("button", { name: /close settings/i }),
    );

    expect(
      screen.queryByRole("dialog", { name: /settings/i }),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId("location")).toHaveTextContent(/^\/$/);
  });

  it("opens directly with no prior view: dialog over Home, closes to Home", async () => {
    renderApp("/settings");
    await screen.findByRole("dialog", { name: /settings/i });
    // Home is the fallback background.
    expect(
      await screen.findByRole("button", { name: /new note/i }),
    ).toBeInTheDocument();

    await userEvent.keyboard("{Escape}");

    expect(
      screen.queryByRole("dialog", { name: /settings/i }),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId("location")).toHaveTextContent(/^\/$/);
  });

  it("restores the exact view it was opened from", async () => {
    renderApp("/trash");
    await screen.findByRole("link", { name: /settings/i });
    await openSettingsFromSidebar();
    expect(screen.getByTestId("location")).toHaveTextContent("/settings");

    await userEvent.keyboard("{Escape}");

    expect(screen.getByTestId("location")).toHaveTextContent(/^\/trash$/);
  });

  it("persists the keep-audio toggle in the Recording section", async () => {
    const saved: Record<string, string> = {};
    renderApp("/settings?tab=recording", {
      settings_set: (args) => {
        const { key, value } = args as { key: string; value: string };
        saved[key] = value;
        return null;
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    const toggle = await within(dialog).findByRole("switch", {
      name: /keep recorded audio/i,
    });
    expect(toggle).not.toBeChecked();

    await userEvent.click(toggle);

    expect(toggle).toBeChecked();
    expect(saved.keep_audio).toBe("true");
  });

  it("switches theme via the segmented control in General", async () => {
    renderApp("/settings?tab=general");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    const group = await within(dialog).findByRole("radiogroup", {
      name: /theme/i,
    });
    const dark = within(group).getByRole("radio", { name: /dark/i });
    expect(dark).not.toBeChecked();

    await userEvent.click(dark);

    expect(dark).toBeChecked();
  });

  it("Escape in an open select closes the select, not the dialog", async () => {
    renderApp("/settings?tab=general");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    // Open the Language select's listbox.
    const trigger = await within(dialog).findByRole("button", {
      name: /norwegian/i,
    });
    await userEvent.click(trigger);
    expect(within(dialog).getByRole("listbox")).toBeInTheDocument();

    await userEvent.keyboard("{Escape}");

    // First Escape only dismisses the listbox — the dialog survives.
    expect(within(dialog).queryByRole("listbox")).not.toBeInTheDocument();
    expect(
      screen.getByRole("dialog", { name: /settings/i }),
    ).toBeInTheDocument();
  });

  it("Escape in a nested modal closes only that modal, not the dialog", async () => {
    renderApp("/settings?tab=summaries");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    // The summary-prompt editor is a Modal nested inside the dialog.
    await userEvent.click(
      await within(dialog).findByRole("button", { name: /new prompt/i }),
    );
    const editor = await screen.findByRole("dialog", { name: /new prompt/i });
    expect(editor).toBeInTheDocument();

    await userEvent.keyboard("{Escape}");

    // Editor dismissed; the settings dialog (and any typed work behind it)
    // survives.
    expect(
      screen.queryByRole("dialog", { name: /new prompt/i }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("dialog", { name: /settings/i }),
    ).toBeInTheDocument();
  });

  it("backdrop click with an open select dismisses only the select", async () => {
    renderApp("/settings?tab=general");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    const trigger = await within(dialog).findByRole("button", {
      name: /norwegian/i,
    });
    await userEvent.click(trigger);
    expect(within(dialog).getByRole("listbox")).toBeInTheDocument();

    await userEvent.click(screen.getByTestId("settings-backdrop"));

    // First backdrop click only dismisses the popover.
    expect(within(dialog).queryByRole("listbox")).not.toBeInTheDocument();
    expect(
      screen.getByRole("dialog", { name: /settings/i }),
    ).toBeInTheDocument();

    await userEvent.click(screen.getByTestId("settings-backdrop"));
    expect(
      screen.queryByRole("dialog", { name: /settings/i }),
    ).not.toBeInTheDocument();
  });

  it("⌘, with the dialog already open doesn't stack history entries", async () => {
    renderApp("/");
    await screen.findByRole("button", { name: /new note/i });
    await openSettingsFromSidebar();

    // ⌘, again while open — must not push a second /settings entry.
    await userEvent.keyboard("{Meta>},{/Meta}");
    expect(
      screen.getByRole("dialog", { name: /settings/i }),
    ).toBeInTheDocument();

    await userEvent.keyboard("{Escape}");

    // One Escape closes it and we're back where we started.
    expect(
      screen.queryByRole("dialog", { name: /settings/i }),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId("location")).toHaveTextContent(/^\/$/);
  });

  it("has a search field in the sidebar (filtering is a fast-follow)", async () => {
    renderApp("/settings");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    expect(
      within(dialog).getByRole("searchbox", { name: /search settings/i }),
    ).toBeInTheDocument();
  });

  it("shows the five sections in the sidebar nav", async () => {
    renderApp("/settings");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    const tabs = within(dialog)
      .getAllByRole("tab")
      .map((t) => t.textContent);
    expect(tabs).toEqual([
      "Recording",
      "Transcription",
      "Summaries",
      "Account",
      "General",
    ]);
  });

  it("moves focus into the dialog on open and back to the trigger on close", async () => {
    renderApp("/");
    await screen.findByRole("button", { name: /new note/i });
    const dialog = await openSettingsFromSidebar();

    // Keyboard/AT users land inside the dialog, not on the page behind it.
    expect(dialog).toContainElement(document.activeElement as HTMLElement);

    await userEvent.keyboard("{Escape}");

    expect(screen.getByRole("link", { name: /settings/i })).toHaveFocus();
  });

  it("keeps the app sidebar mounted behind the dialog", async () => {
    renderApp("/");
    await screen.findByRole("button", { name: /new note/i });
    await openSettingsFromSidebar();

    // The nav we clicked is still there behind the dim — settings no longer
    // swaps the app shell out or auto-collapses the sidebar.
    expect(
      screen.getByRole("link", { name: /settings/i }),
    ).toBeInTheDocument();
  });

  it("resolves legacy deep links to their new sections", async () => {
    // `keys` tab is dissolved: it anchors to Transcription, where the
    // provider key cards live.
    renderApp("/settings?tab=keys");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    expect(
      within(dialog).getByRole("tab", { name: "Transcription" }),
    ).toHaveAttribute("aria-selected", "true");
    expect(
      await within(dialog).findByLabelText(/openai api key/i),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByLabelText(/deepgram api key/i),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByLabelText(/groq api key/i),
    ).toBeInTheDocument();
  });

  it("default provider renders as self-describing rows", async () => {
    renderApp("/settings?tab=transcription");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    // Every control is a labeled row with an explanation — no bare selects
    // (maintainer design-review amendment on #15).
    expect(await within(dialog).findByText("Provider")).toBeInTheDocument();
    expect(
      within(dialog).getByText(/where audio is transcribed/i),
    ).toBeInTheDocument();
    expect(within(dialog).getByText("Model")).toBeInTheDocument();
    // The selected provider shows in the picker trigger.
    expect(
      within(dialog).getByRole("button", { name: /openai/i }),
    ).toBeInTheDocument();
  });

  it("local provider exposes labeled Quality and Metal rows", async () => {
    const writes: unknown[] = [];
    renderApp("/settings?tab=transcription", {
      get_transcribe_config: () => ({
        default: {
          provider: "local",
          model_id: "large-v3-turbo-q5",
          preset: "quality",
          use_gpu: true,
        },
        per_language: {},
      }),
      local_whisper_models: () => [
        {
          id: "large-v3-turbo-q5",
          label: "Large v3 Turbo (quantized)",
          description: "",
          filename: "x.bin",
          sizeBytesHint: 1,
          kind: "multilingual",
          specificLanguage: null,
          downloaded: true,
          sizeBytes: 1,
          path: null,
        },
      ],
      set_transcribe_config: (args) => {
        writes.push(args);
        return null;
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    expect(await within(dialog).findByText("Quality")).toBeInTheDocument();
    const metal = await within(dialog).findByRole("switch", {
      name: /metal/i,
    });
    expect(metal).toBeChecked();

    await userEvent.click(metal);

    const write = writes.at(-1) as {
      config?: { default?: { use_gpu?: boolean } };
    };
    expect(write?.config?.default?.use_gpu).toBe(false);
  });

  it("deep-links ?tab=account to the merged Account section", async () => {
    renderApp("/settings?tab=account");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    expect(
      within(dialog).getByRole("tab", { name: "Account" }),
    ).toHaveAttribute("aria-selected", "true");
    // Signed out: the merged section is just the connect surface — workspace
    // management appears in the same section once signed in (covered in
    // AccountSection.test.tsx).
    expect(
      await within(dialog).findByRole("heading", { name: /connect to sync/i }),
    ).toBeInTheDocument();
  });

  it("switches sections from the nav and reflects it in the URL", async () => {
    renderApp("/settings");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    // Default landing is the first section, Recording.
    expect(
      within(dialog).getByRole("tab", { name: "Recording" }),
    ).toHaveAttribute("aria-selected", "true");
    expect(
      await within(dialog).findByRole("heading", { name: /audio retention/i }),
    ).toBeInTheDocument();

    await userEvent.click(within(dialog).getByRole("tab", { name: "General" }));

    expect(screen.getByTestId("location")).toHaveTextContent(
      "/settings?tab=general",
    );
    // General owns appearance + the absorbed About content.
    expect(
      await within(dialog).findByRole("heading", { name: /appearance/i }),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByText(/source code/i),
    ).toBeInTheDocument();
  });

  it("closes when the backdrop is clicked", async () => {
    renderApp("/");
    await screen.findByRole("button", { name: /new note/i });
    await openSettingsFromSidebar();

    await userEvent.click(screen.getByTestId("settings-backdrop"));

    expect(
      screen.queryByRole("dialog", { name: /settings/i }),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId("location")).toHaveTextContent(/^\/$/);
  });
});
