import { describe, expect, it } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { mockTauri } from "../test/tauri";
import { OrgAudioNotice } from "./OrgAudioNotice";

describe("OrgAudioNotice", () => {
  it("shows nothing and sweeps nothing without an organisation", async () => {
    const sweeps: string[] = [];
    let asked = false;
    mockTauri({
      org_audio_notice: () => {
        asked = true;
        return null;
      },
      org_audio_sweep: (args) => {
        sweeps.push((args as { orgId: string }).orgId);
        return 0;
      },
    });
    render(<OrgAudioNotice />);
    await waitFor(() => expect(asked).toBe(true));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(sweeps).toEqual([]);
  });

  it("names the organisation and sweeps only on its button", async () => {
    let due: unknown = { org_id: "o1", org_name: "Northwind" };
    const sweeps: string[] = [];
    mockTauri({
      org_audio_notice: () => due,
      org_audio_sweep: (args) => {
        sweeps.push((args as { orgId: string }).orgId);
        due = null;
        return 3;
      },
    });
    render(<OrgAudioNotice />);
    expect(await screen.findByText("Northwind doesn't allow stored audio. Audio of its notes on this Mac will be deleted.")).toBeTruthy();
    expect(sweeps).toEqual([]);
    await userEvent.click(screen.getByRole("button", { name: "Delete audio" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(sweeps).toEqual(["o1"]);
  });

  it("says a managed profile's sweep reaches every note", async () => {
    mockTauri({ org_audio_notice: () => ({ org_id: "mac-configuration", org_name: "This Mac's configuration", all: true }) });
    render(<OrgAudioNotice />);
    expect(
      await screen.findByText("This Mac's configuration doesn't allow stored audio. Audio on this Mac will be deleted."),
    ).toBeTruthy();
  });
});
