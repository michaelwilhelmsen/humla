import { describe, expect, it } from "vitest";
import commandsRs from "../../src-tauri/src/commands.rs?raw";
import diarizeRs from "../../src-tauri/src/diarize.rs?raw";
import { mockTauri } from "../test/tauri";
import type { DiarizeEngine } from "./ipc";
import {
  DEFAULT_DIARIZE_ENGINE,
  NEMOTRON_MAX_SPEAKERS,
  downloadDiarizeModels,
  downloadProgress,
  enginesToDownload,
  selectedDiarizeEngine,
} from "./diarizeEngine";

describe("diarizeEngine.ts mirrors the Rust side", () => {
  it("has the same default engine as commands.rs", () => {
    expect(commandsRs).toContain(`const DEFAULT_DIARIZE_MODEL: &str = "${DEFAULT_DIARIZE_ENGINE}";`);
  });

  it("has the same Nemotron speaker limit as diarize.rs", () => {
    expect(diarizeRs).toContain(`pub const NEMOTRON_MAX_SPEAKERS: i64 = ${NEMOTRON_MAX_SPEAKERS};`);
  });
});

describe("selectedDiarizeEngine", () => {
  it("is Nemotron 3 when nothing is stored, which only a fresh install is", () => {
    expect(selectedDiarizeEngine(null)).toBe("nemotron3");
  });

  it("reads a stored engine, and anything unknown as community-1", () => {
    expect(selectedDiarizeEngine("nemotron3")).toBe("nemotron3");
    expect(selectedDiarizeEngine("community1")).toBe("community1");
    expect(selectedDiarizeEngine("sortformer")).toBe("community1");
  });
});

describe("enginesToDownload", () => {
  it("fetches community-1 first, then the selected engine", () => {
    expect(enginesToDownload("nemotron3")).toEqual(["community1", "nemotron3"]);
    expect(enginesToDownload("community1")).toEqual(["community1"]);
  });
});

describe("downloadDiarizeModels", () => {
  function run(opts: {
    stored?: string | null;
    downloaded?: DiarizeEngine[];
    failing?: DiarizeEngine[];
  }) {
    const downloads: string[] = [];
    mockTauri({
      settings_get: (args) =>
        (args as { key: string }).key === "diarize_model" ? (opts.stored ?? null) : null,
      diarize_status: (args) => ({
        downloaded: (opts.downloaded ?? []).includes((args as { engine: DiarizeEngine }).engine),
        sizeBytes: null,
        path: null,
      }),
      diarize_download: (args) => {
        const engine = (args as { engine: DiarizeEngine }).engine;
        downloads.push(engine);
        if ((opts.failing ?? []).includes(engine)) throw new Error("offline");
        return null;
      },
    });
    return { downloads, done: downloadDiarizeModels() };
  }

  it("downloads both models on a fresh install, community-1 first", async () => {
    const { downloads, done } = run({});
    await done;
    expect(downloads).toEqual(["community1", "nemotron3"]);
  });

  it("skips a model that is already on disk", async () => {
    const { downloads, done } = run({ downloaded: ["community1"] });
    await done;
    expect(downloads).toEqual(["nemotron3"]);
  });

  it("leaves Nemotron alone for an install that hasn't switched to it", async () => {
    const { downloads, done } = run({ stored: "community1" });
    await done;
    expect(downloads).toEqual(["community1"]);
  });

  it("still fetches Nemotron when community-1 fails", async () => {
    const { downloads, done } = run({ failing: ["community1"] });
    await done;
    expect(downloads).toEqual(["community1", "nemotron3"]);
  });
});

describe("downloadProgress", () => {
  it("shows a download's fraction", () => {
    expect(downloadProgress("downloading", 0.424)).toEqual({ label: "Downloading… 42%", value: 0.424 });
    expect(downloadProgress(null, 0)).toEqual({ label: "Downloading… 0%", value: 0 });
  });

  it("has no fraction for the steps that report none", () => {
    for (const phase of ["listing", "compiling", "warming"] as const) {
      expect(downloadProgress(phase, 0.5).value).toBeNull();
    }
    expect(downloadProgress("warming", 0).label).toMatch(/preparing for the neural engine/i);
  });
});
