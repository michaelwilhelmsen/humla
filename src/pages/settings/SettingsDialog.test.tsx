import { describe, it, expect } from "vitest";
import { fireEvent, screen, within } from "@testing-library/react";
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

  // #180: defaults on for a fresh install and for a library with no
  // `keep_awake` row, and only an explicit off persists "false".
  it("shows the keep-awake toggle on by default and persists turning it off", async () => {
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
      name: /keep mac awake while recording/i,
    });
    expect(toggle).toBeChecked();

    await userEvent.click(toggle);

    expect(toggle).not.toBeChecked();
    expect(saved.keep_awake).toBe("false");
  });

  // #24: the toggle's copy is the privacy promise, so it has to state which of
  // the two regimes is in force rather than describing the feature generically.
  it("states that nothing is stored while keep-audio is off", async () => {
    renderApp("/settings?tab=recording", {
      settings_set: () => null,
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    const toggle = await within(dialog).findByRole("switch", {
      name: /keep recorded audio/i,
    });

    expect(within(dialog).getByText(/no audio is stored on this mac/i)).toBeInTheDocument();

    await userEvent.click(toggle);

    expect(
      within(dialog).getByText(/keep recordings for playback and speaker re-detection/i),
    ).toBeInTheDocument();
    expect(
      within(dialog).queryByText(/no audio is stored on this mac/i),
    ).not.toBeInTheDocument();
  });

  // #146: deferring transcription only makes sense when the audio survives the
  // recording, so the toggle is *absent* until retention is on rather than
  // present-and-disabled — a greyed switch invites the user to wonder why.
  it("reveals Transcribe manually only once keep-audio is on", async () => {
    const saved: Record<string, string> = {};
    renderApp("/settings?tab=recording", {
      settings_set: (args) => {
        const { key, value } = args as { key: string; value: string };
        saved[key] = value;
        return null;
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    const keep = await within(dialog).findByRole("switch", {
      name: /keep recorded audio/i,
    });

    expect(
      within(dialog).queryByRole("switch", { name: /transcribe manually/i }),
    ).not.toBeInTheDocument();

    await userEvent.click(keep);

    const manual = within(dialog).getByRole("switch", {
      name: /transcribe manually/i,
    });
    // Off by default: turning retention on must not silently change how
    // recordings are transcribed.
    expect(manual).not.toBeChecked();
    expect(saved.transcribe_manually).toBeUndefined();

    await userEvent.click(manual);
    expect(manual).toBeChecked();
    expect(saved.transcribe_manually).toBe("true");

    // Turning retention back off hides it again — and the backend gate means
    // it is inert regardless of the stored value.
    await userEvent.click(keep);
    expect(
      within(dialog).queryByRole("switch", { name: /transcribe manually/i }),
    ).not.toBeInTheDocument();
  });

  it("deletes stored audio for existing notes behind an inline confirm", async () => {
    let deleted = 0;
    renderApp("/settings?tab=recording", {
      settings_set: () => null,
      // Stateful, like the real backend: once swept, there is nothing left.
      stored_audio_stats: () =>
        deleted > 0
          ? { notes: 0, files: 0, bytes: 0, noteIds: [] }
          : { notes: 3, files: 5, bytes: 12 * 1024 * 1024, noteIds: ["a", "b", "c"] },
      delete_stored_audio: () => {
        deleted += 1;
        return 5;
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    // The action names what it would remove — the deletion is irreversible.
    const start = await within(dialog).findByRole("button", {
      name: /delete stored audio/i,
    });
    expect(within(dialog).getByText(/3 notes/i)).toBeInTheDocument();
    expect(within(dialog).getByText(/12 MB/i)).toBeInTheDocument();

    // First click arms, it does not delete (Tauri's webview no-ops
    // window.confirm, so the confirm is a second button).
    await userEvent.click(start);
    expect(deleted).toBe(0);

    await userEvent.click(
      within(dialog).getByRole("button", { name: /^delete 5 files$/i }),
    );
    expect(deleted).toBe(1);
    // Nothing left to delete → the action retires itself.
    expect(
      await within(dialog).findByText(/no audio stored/i),
    ).toBeInTheDocument();
  });

  it("offers no cleanup action when no audio is stored", async () => {
    renderApp("/settings?tab=recording", { settings_set: () => null });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    await within(dialog).findByRole("switch", { name: /keep recorded audio/i });

    expect(
      within(dialog).queryByRole("button", { name: /delete stored audio/i }),
    ).not.toBeInTheDocument();
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
    renderApp("/settings?tab=transcription");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    // Open the Language select's listbox.
    const trigger = await within(dialog).findByRole("combobox", {
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
    renderApp("/settings?tab=transcription");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    const trigger = await within(dialog).findByRole("combobox", {
      name: /norwegian/i,
    });
    await userEvent.click(trigger);
    expect(within(dialog).getByRole("listbox")).toBeInTheDocument();

    // The open listbox is modal, so the backdrop underneath is inert — the
    // pointerdown lands on the select's dismiss layer and never reaches it.
    fireEvent.pointerDown(screen.getByTestId("settings-backdrop"));

    // First backdrop press only dismisses the popover.
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

  it("has no search field until filtering actually works", async () => {
    // Maintainer call (2026-07-09): a dead search box promises something
    // the dialog can't do — removed until the filter ships.
    renderApp("/settings");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    await within(dialog).findByRole("tab", { name: "Recording" });
    expect(within(dialog).queryByRole("searchbox")).not.toBeInTheDocument();
  });

  it("shows the six sections in the sidebar nav", async () => {
    renderApp("/settings");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    const tabs = within(dialog)
      .getAllByRole("tab")
      .map((t) => t.textContent);
    expect(tabs).toEqual([
      "Recording",
      "Transcription",
      "Summaries",
      "Chat",
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
    // `keys` tab is dissolved: it anchors to Transcription. The active
    // provider's key is inline; the other providers' keys live under the
    // section's Advanced disclosure alongside overrides.
    renderApp("/settings?tab=keys");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    expect(
      within(dialog).getByRole("tab", { name: "Transcription" }),
    ).toHaveAttribute("aria-selected", "true");
    expect(
      await within(dialog).findByLabelText(/openai api key/i),
    ).toBeInTheDocument();

    await userEvent.click(
      (await within(dialog).findAllByRole("button", { name: /advanced/i }))[0],
    );
    expect(
      await within(dialog).findByLabelText(/deepgram api key/i),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByLabelText(/groq api key/i),
    ).toBeInTheDocument();
  });

  it("the default language lives in Transcription and persists", async () => {
    const writes: Record<string, string> = {};
    renderApp("/settings?tab=transcription", {
      settings_set: (args) => {
        const { key, value } = args as { key: string; value: string };
        writes[key] = value;
        return null;
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    // Relocated from General (#15): the language sits with the engine that
    // consumes it.
    const trigger = await within(dialog).findByRole("combobox", {
      name: /norwegian/i,
    });
    await userEvent.click(trigger);
    await userEvent.click(
      within(dialog).getByRole("option", { name: /^english/i }),
    );
    expect(writes.language).toBe("en");

    // And General no longer offers it.
    await userEvent.click(within(dialog).getByRole("tab", { name: "General" }));
    expect(
      within(dialog).queryByRole("button", { name: /norwegian|english/i }),
    ).not.toBeInTheDocument();
  });

  it("per-language overrides and local models sit behind Advanced", async () => {
    renderApp("/settings?tab=transcription");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });
    await within(dialog).findByText("Provider");

    // Collapsed: expert routing is out of sight.
    expect(
      within(dialog).queryByText("Per-language overrides"),
    ).not.toBeInTheDocument();
    expect(
      within(dialog).queryByText(/pick a multilingual model/i),
    ).not.toBeInTheDocument();

    await userEvent.click(
      within(dialog).getAllByRole("button", { name: /advanced/i })[0],
    );

    expect(
      await within(dialog).findByText("Per-language overrides"),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByText(/pick a multilingual model/i),
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
      within(dialog).getByRole("combobox", { name: /openai/i }),
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

  it("detection thresholds live behind an Advanced disclosure as labeled rows", async () => {
    const writes: Record<string, string> = {};
    renderApp("/settings?tab=transcription", {
      settings_set: (args) => {
        const { key, value } = args as { key: string; value: string };
        writes[key] = value;
        return null;
      },
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    // Collapsed by default — expert knobs don't shout (and no developer-mode
    // gate: tuning re-diarize thresholds is user-relevant).
    await within(dialog).findByText(/speaker labels/i);
    expect(
      within(dialog).queryByText(/community-1 clustering threshold/i),
    ).not.toBeInTheDocument();

    // Two per-section disclosures exist (Transcription, Speaker labels);
    // the thresholds live under the Speaker labels one — last in order.
    const advanced = within(dialog).getAllByRole("button", {
      name: /advanced/i,
    });
    await userEvent.click(advanced[advanced.length - 1]);

    const field = within(dialog).getByLabelText(
      /community-1 clustering threshold/i,
    );
    await userEvent.clear(field);
    await userEvent.type(field, "0.6");

    expect(writes.community1_threshold).toBe("0.6");
    // Sentence-case labels — the uppercase .nd-label style is retired.
    expect(
      within(dialog).getByText(/silence rms threshold/i),
    ).toBeInTheDocument();
  });

  it("summaries: labeled provider rows with the OpenAI key inline, preset relocated", async () => {
    renderApp("/settings?tab=summaries");
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    expect(await within(dialog).findByText("Provider")).toBeInTheDocument();
    expect(
      within(dialog).getByText(/local keeps the transcript on your mac/i),
    ).toBeInTheDocument();
    // Key inline under the cloud provider — no separate keys surface.
    expect(
      await within(dialog).findByLabelText(/openai api key/i),
    ).toBeInTheDocument();
    // Default preset moved here from General.
    expect(
      within(dialog).getByRole("combobox", { name: /meeting/i }),
    ).toBeInTheDocument();

    await userEvent.click(within(dialog).getByRole("tab", { name: "General" }));
    expect(
      within(dialog).queryByRole("combobox", { name: /meeting/i }),
    ).not.toBeInTheDocument();
  });

  it("summaries local provider connects through OllamaConnect", async () => {
    const writes: Record<string, string> = {};
    renderApp("/settings?tab=summaries", {
      settings_get: (args) => {
        const key = (args as { key?: string }).key;
        if (key === "onboarding_completed") return "true";
        if (key === "summary_provider") return "local";
        return null;
      },
      settings_set: (args) => {
        const { key, value } = args as { key: string; value: string };
        writes[key] = value;
        return null;
      },
      local_llm_list_models: () => ["qwen3:8b", "llama3.2:3b"],
    });
    const dialog = await screen.findByRole("dialog", { name: /settings/i });

    // Reachable server → connected state, no manual Refresh dance.
    expect(await within(dialog).findByText(/connected/i)).toBeInTheDocument();
    // Thinking mode is a Toggle, not a checkbox.
    expect(
      within(dialog).getByRole("switch", { name: /thinking/i }),
    ).toBeInTheDocument();

    await userEvent.click(
      within(dialog).getByRole("combobox", { name: /choose a model|qwen3/i }),
    );
    await userEvent.click(
      within(dialog).getByRole("option", { name: "qwen3:8b" }),
    );
    expect(writes.local_llm_model).toBe("qwen3:8b");
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
