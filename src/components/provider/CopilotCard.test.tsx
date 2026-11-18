import { describe, it, expect, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { emit } from "@tauri-apps/api/event";
import { CopilotCard } from "./CopilotCard";
import { copilotHint, useCopilotStatus } from "./useCopilot";
import { mockTauri } from "../../test/tauri";
import type { CopilotStatus } from "../../lib/ipc";

const SIGNED_OUT: CopilotStatus = {
  login: null,
  cliPath: "/usr/local/bin/copilot",
  cliVersion: "1.2.3",
  cliError: null,
  cliIsDownload: false,
  downloadVersion: "1.2.3",
};
const SIGNED_IN: CopilotStatus = { ...SIGNED_OUT, login: "octocat" };

function Harness({ onModelChange = () => {} }: { onModelChange?: (v: string) => void }) {
  const copilot = useCopilotStatus({ enabled: true });
  return (
    <CopilotCard
      copilot={copilot}
      model="auto"
      onModelChange={onModelChange}
      modelDescription="Model"
    />
  );
}

function setup(handlers: Record<string, (args: unknown) => unknown>) {
  mockTauri(handlers, { events: true });
  return render(<Harness />);
}

describe("CopilotCard: the CLI", () => {
  it("offers a download when the CLI is missing, shows progress, and settles on the installed version", async () => {
    let status: CopilotStatus = { ...SIGNED_IN, cliPath: null, cliVersion: null };
    const download = vi.fn(() => null);
    setup({ copilot_status: () => status, copilot_cli_download: download });

    await userEvent.click(await screen.findByRole("button", { name: "Download Copilot CLI" }));
    expect(download).toHaveBeenCalled();

    await act(() => emit("copilot_cli_download", { status: "progress", fraction: 0.4, message: null }));
    expect(screen.getByText("Downloading — 40%")).toBeInTheDocument();
    expect(screen.getByRole("progressbar", { name: "Copilot CLI download" })).toHaveAttribute(
      "aria-valuenow",
      "40",
    );

    status = { ...SIGNED_IN };
    await act(() => emit("copilot_cli_download", { status: "done", fraction: 1, message: null }));
    expect(await screen.findByText("Installed · 1.2.3")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Download Copilot CLI" })).toBeNull();
  });

  it("offers the supported version when the installed CLI won't start", async () => {
    const download = vi.fn(() => null);
    setup({
      copilot_status: () => ({
        ...SIGNED_IN,
        cliVersion: "0.9.0",
        cliError: "Couldn't start the Copilot CLI: protocol version 2 is not supported",
      }),
      copilot_cli_download: download,
    });
    expect(await screen.findByText(/version 0\.9\.0\) didn't start/)).toBeInTheDocument();
    expect(screen.getByText(/protocol version 2 is not supported/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Download Copilot CLI" }));
    expect(download).toHaveBeenCalled();
  });

  it("doesn't offer a download when Humla's own download won't start", async () => {
    setup({
      copilot_status: () => ({ ...SIGNED_IN, cliIsDownload: true, cliError: "Couldn't start the Copilot CLI: crashed" }),
    });
    expect(await screen.findByText("Installed · 1.2.3")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Download Copilot CLI" })).toBeNull();
  });

  it("offers a download for the user's own CLI that won't start, whatever version it reports", async () => {
    setup({ copilot_status: () => ({ ...SIGNED_IN, cliError: "Couldn't start the Copilot CLI: version mismatch" }) });
    expect(await screen.findByText(/version 1\.2\.3\) didn't start/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Download Copilot CLI" })).toBeInTheDocument();
  });

  it("says why a download failed and offers to try again", async () => {
    setup({ copilot_status: () => ({ ...SIGNED_OUT, cliPath: null, cliVersion: null }) });
    await userEvent.click(await screen.findByRole("button", { name: "Download Copilot CLI" }));
    await act(() =>
      emit("copilot_cli_download", { status: "failed", fraction: null, message: "npm said no" }),
    );
    expect(screen.getByText("npm said no")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Try again" })).toBeInTheDocument();
  });
});

describe("CopilotCard: signing in", () => {
  it("shows the device code, opens GitHub, and lands signed in when GitHub says so", async () => {
    const opened = vi.fn(() => undefined);
    setup({
      copilot_status: () => SIGNED_OUT,
      copilot_sign_in_start: () => ({
        userCode: "WXYZ-9876",
        verificationUri: "https://github.com/login/device",
        expiresIn: 900,
      }),
      "plugin:shell|open": opened,
    });

    await userEvent.click(await screen.findByRole("button", { name: "Sign in with GitHub" }));
    expect(await screen.findByLabelText("GitHub sign-in code")).toHaveTextContent("WXYZ-9876");
    expect(screen.getByText(/Waiting for GitHub/)).toBeInTheDocument();
    await waitFor(() => expect(opened).toHaveBeenCalled());
    expect(JSON.stringify(opened.mock.calls[0])).toContain("https://github.com/login/device");

    await act(() => emit("copilot_sign_in", { status: "signed_in", login: "octocat", message: null }));
    expect(screen.getByText("octocat")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sign out" })).toBeInTheDocument();
    expect(screen.queryByLabelText("GitHub sign-in code")).toBeNull();
  });

  it("explains a declined sign-in and offers to start over", async () => {
    setup({ copilot_status: () => SIGNED_OUT });
    await userEvent.click(await screen.findByRole("button", { name: "Sign in with GitHub" }));
    await screen.findByLabelText("GitHub sign-in code");
    await act(() => emit("copilot_sign_in", { status: "denied", login: null, message: null }));
    expect(screen.getByText(/GitHub sign-in was declined/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sign in with GitHub" })).toBeInTheDocument();
  });

  it("cancel stops the poll and returns to the sign-in button", async () => {
    const cancel = vi.fn(() => null);
    setup({ copilot_status: () => SIGNED_OUT, copilot_sign_in_cancel: cancel });
    await userEvent.click(await screen.findByRole("button", { name: "Sign in with GitHub" }));
    await userEvent.click(await screen.findByRole("button", { name: "Cancel" }));
    expect(cancel).toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Sign in with GitHub" })).toBeInTheDocument();
  });

  it("signs out", async () => {
    const signOut = vi.fn(() => null);
    setup({ copilot_status: () => SIGNED_IN, copilot_sign_out: signOut });
    await userEvent.click(await screen.findByRole("button", { name: "Sign out" }));
    expect(signOut).toHaveBeenCalled();
    expect(await screen.findByRole("button", { name: "Sign in with GitHub" })).toBeInTheDocument();
  });
});

describe("CopilotCard: the model picker", () => {
  it("lists models only once Copilot is ready", async () => {
    const models = vi.fn(() => [{ id: "auto", name: "Auto", vendor: null }]);
    setup({ copilot_status: () => SIGNED_OUT, copilot_models: models });
    await screen.findByRole("button", { name: "Sign in with GitHub" });
    expect(models).not.toHaveBeenCalled();
    expect(screen.queryByRole("combobox")).toBeNull();
  });

  it("writes the chosen model through onModelChange", async () => {
    const onModelChange = vi.fn();
    mockTauri(
      {
        copilot_status: () => SIGNED_IN,
        copilot_models: () => [
          { id: "auto", name: "Auto", vendor: null },
          { id: "gpt-5.4", name: "GPT-5.4", vendor: "OpenAI" },
        ],
      },
      { events: true },
    );
    render(<Harness onModelChange={onModelChange} />);
    await userEvent.click(await screen.findByRole("combobox", { name: "Auto" }));
    await userEvent.click(await screen.findByRole("option", { name: "GPT-5.4" }));
    expect(onModelChange).toHaveBeenCalledWith("gpt-5.4");
  });
});

describe("copilotHint", () => {
  const hint = (status: CopilotStatus | null) => copilotHint({ status, error: null }, "above");
  it("asks for the CLI first, then the sign-in, then nothing", () => {
    expect(hint({ ...SIGNED_IN, cliPath: null })).toMatch(/Download the Copilot CLI above/);
    expect(hint(SIGNED_OUT)).toBe("Sign in to GitHub Copilot above.");
    expect(hint(SIGNED_IN)).toBe("");
  });
});
