import { beforeEach, describe, expect, it } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockTauri } from "../test/tauri";
import { resetSystemAudioOffer, SystemAudioOffer } from "./SystemAudioOffer";

function setup(opts: { screen?: string; systemAudio?: string; declined?: boolean }) {
  const calls = { requested: [] as unknown[], writes: {} as Record<string, string>, statusReads: 0 };
  mockTauri({
    permissions_status: () => {
      calls.statusReads += 1;
      return {
        microphone: "granted",
        screen: opts.screen ?? "granted",
        systemAudio: opts.systemAudio ?? "not_determined",
        notifications: "granted",
      };
    },
    permissions_request: (args) => {
      calls.requested.push(args);
      return null;
    },
    settings_get: (args) =>
      (args as { key: string }).key === "system_audio_offer" && opts.declined ? "declined" : null,
    settings_set: (args) => {
      const { key, value } = args as { key: string; value: string };
      calls.writes[key] = value;
      return null;
    },
  });
  return calls;
}

describe("Home: the system audio offer", () => {
  beforeEach(() => resetSystemAudioOffer());

  it("asks a Screen Recording user for system audio, then goes away", async () => {
    const calls = setup({});
    render(<SystemAudioOffer />);

    await userEvent.click(await screen.findByRole("button", { name: "Allow system audio" }));
    expect(calls.requested).toEqual([{ kind: "systemAudio" }]);
    await waitFor(() => expect(screen.queryByText(/without Screen Recording/)).toBeNull());
  });

  it("remembers Not now", async () => {
    const calls = setup({});
    render(<SystemAudioOffer />);

    await userEvent.click(await screen.findByRole("button", { name: "Not now" }));
    expect(calls.writes).toEqual({ system_audio_offer: "declined" });
    expect(screen.queryByText(/without Screen Recording/)).toBeNull();
  });

  it.each([
    ["already allowed", { systemAudio: "granted" }],
    ["turned down in macOS", { systemAudio: "denied" }],
    ["a new user without Screen Recording", { screen: "denied" }],
    ["macOS before 14.2", { systemAudio: "unsupported" }],
    ["declined before", { declined: true }],
  ])("stays hidden when %s", async (_, opts) => {
    const calls = setup(opts);
    render(<SystemAudioOffer />);

    await waitFor(() => expect(calls.statusReads).toBe(1));
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByText(/without Screen Recording/)).toBeNull();
  });

  it("reads the permissions once per launch", async () => {
    const calls = setup({ systemAudio: "granted" });
    const { unmount } = render(<SystemAudioOffer />);
    await waitFor(() => expect(calls.statusReads).toBe(1));
    unmount();
    render(<SystemAudioOffer />);
    await new Promise((r) => setTimeout(r, 0));
    expect(calls.statusReads).toBe(1);
  });
});
