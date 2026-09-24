import { beforeEach, describe, expect, it } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { emit } from "@tauri-apps/api/event";
import { mockTauri } from "../test/tauri";
import { useNemotronOffer } from "../lib/nemotronOffer";
import { NemotronOffer } from "./NemotronOffer";

function deferred() {
  let resolve!: () => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<null>((res, rej) => {
    resolve = () => res(null);
    reject = rej;
  });
  return { promise, resolve, reject };
}

function setup(opts: {
  offer?: string | null;
  engine?: string | null;
  download?: () => Promise<null> | null;
  events?: boolean;
}) {
  const writes: Record<string, string> = {};
  const downloads: string[] = [];
  mockTauri(
    {
      settings_get: (args) => {
        const key = (args as { key: string }).key;
        if (key in writes) return writes[key];
        if (key === "nemotron_offer") return opts.offer ?? null;
        if (key === "diarize_model") return opts.engine ?? null;
        return null;
      },
      settings_set: (args) => {
        const { key, value } = args as { key: string; value: string };
        writes[key] = value;
        return null;
      },
      diarize_download: (args) => {
        downloads.push((args as { engine: string }).engine);
        return opts.download ? opts.download() : null;
      },
    },
    { events: opts.events },
  );
  return { writes, downloads };
}

const offerText = () => screen.findByText(/193 MB/);
const switchButton = () => screen.getByRole("button", { name: /upgrade speaker labels/i });

beforeEach(() => {
  useNemotronOffer.setState({ offer: { stage: "hidden" } });
});

describe("NemotronOffer", () => {
  it("is not shown to a fresh install, which is on Nemotron 3 already", async () => {
    setup({ offer: null, engine: null });
    const { container } = render(<NemotronOffer />);
    await act(async () => {});
    expect(container).toBeEmptyDOMElement();
  });

  it("is not shown once the install is on Nemotron 3", async () => {
    setup({ offer: "pending", engine: "nemotron3" });
    const { container } = render(<NemotronOffer />);
    await act(async () => {});
    expect(container).toBeEmptyDOMElement();
  });

  it("offers an upgraded install the switch and names the download", async () => {
    setup({ offer: "pending", engine: "community1" });
    render(<NemotronOffer />);
    expect(await screen.findByRole("heading", { name: "Make it clearer who said what" })).toBeInTheDocument();
    expect(screen.getByText("One-time setup: 193 MB download and about 2 minutes to prepare.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Not now" })).toBeInTheDocument();
  });

  it("switches the engine only once the model is downloaded and warmed", async () => {
    const dl = deferred();
    const { writes, downloads } = setup({
      offer: "pending",
      engine: "community1",
      download: () => dl.promise,
    });
    render(<NemotronOffer />);
    await offerText();

    await userEvent.click(switchButton());
    expect(downloads).toEqual(["nemotron3"]);
    expect(writes.diarize_model).toBeUndefined();

    await act(async () => dl.resolve());
    await waitFor(() => expect(writes.diarize_model).toBe("nemotron3"));
    expect(writes.nemotron_offer).toBe("accepted");
    expect(await screen.findByText(/nemotron 3 is ready/i)).toBeInTheDocument();
  });

  it("names the Neural Engine setup while the model is prepared", async () => {
    const dl = deferred();
    setup({ offer: "pending", engine: "community1", download: () => dl.promise, events: true });
    render(<NemotronOffer />);
    await offerText();
    await userEvent.click(switchButton());

    await act(async () => {
      await emit("diarize_download_progress", { engine: "nemotron3", phase: "warming", fraction: 0 });
    });
    expect(await screen.findByText(/preparing for the neural engine/i)).toBeInTheDocument();
    dl.resolve();
  });

  it("keeps the engine and the offer when the download fails", async () => {
    const { writes } = setup({
      offer: "pending",
      engine: "community1",
      download: () => Promise.reject(new Error("download failed: offline")),
    });
    render(<NemotronOffer />);
    await offerText();

    await userEvent.click(switchButton());

    expect(await screen.findByText(/offline/)).toBeInTheDocument();
    expect(writes.diarize_model).toBeUndefined();
    expect(writes.nemotron_offer).toBeUndefined();
    expect(switchButton()).toBeEnabled();
  });

  it("still says why the last download failed when the card comes back", async () => {
    setup({
      offer: "pending",
      engine: "community1",
      download: () => Promise.reject(new Error("download failed: offline")),
    });
    const first = render(<NemotronOffer />);
    await offerText();
    await userEvent.click(switchButton());
    await screen.findByText(/offline/);
    first.unmount();

    render(<NemotronOffer />);
    expect(await screen.findByText(/offline/)).toBeInTheDocument();
  });

  it("declining changes nothing but the offer", async () => {
    const { writes, downloads } = setup({ offer: "pending", engine: "community1" });
    const { container } = render(<NemotronOffer />);
    await offerText();

    await userEvent.click(screen.getByRole("button", { name: /not now/i }));

    await waitFor(() => expect(container).toBeEmptyDOMElement());
    expect(writes).toEqual({ nemotron_offer: "declined" });
    expect(downloads).toEqual([]);
  });

  it("picks up a download still running after the card was left", async () => {
    const dl = deferred();
    const { downloads } = setup({
      offer: "pending",
      engine: "community1",
      download: () => dl.promise,
    });
    const first = render(<NemotronOffer />);
    await offerText();
    await userEvent.click(switchButton());
    first.unmount();

    render(<NemotronOffer />);
    expect(await screen.findByRole("progressbar")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /upgrade speaker labels/i })).toBeNull();
    expect(downloads).toEqual(["nemotron3"]);
    dl.resolve();
  });
});
