import { beforeEach, describe, expect, it } from "vitest";
import { handleDeepLinks, linkSource, parseDeepLink, useDeepLinkStore } from "./deepLink";

beforeEach(() => {
  useDeepLinkStore.setState({ newWorkspace: null });
});

describe("parseDeepLink", () => {
  it("reads the new-workspace route and its source", () => {
    expect(parseDeepLink("humla://new-workspace?source=email")).toEqual({
      kind: "new-workspace",
      source: "email",
    });
    expect(parseDeepLink("humla://new-workspace?source=web")).toEqual({
      kind: "new-workspace",
      source: "web",
    });
  });

  it("accepts the route with a trailing slash, in the path, or in another case", () => {
    for (const raw of [
      "humla://new-workspace/?source=web",
      "humla:///new-workspace?source=web",
      "humla:new-workspace?source=web",
      "HUMLA://New-Workspace?source=web",
    ]) {
      expect(parseDeepLink(raw)).toEqual({ kind: "new-workspace", source: "web" });
    }
  });

  it("leaves the source unset when it is missing or not allowed", () => {
    expect(parseDeepLink("humla://new-workspace")).toEqual({ kind: "new-workspace", source: undefined });
    for (const s of ["onboarding", "note_banner", "EMAIL", "web ", "", "email&x=1"]) {
      expect(parseDeepLink(`humla://new-workspace?source=${encodeURIComponent(s)}`)).toEqual({
        kind: "new-workspace",
        source: undefined,
      });
    }
  });

  it("turns an unknown route into a plain focus", () => {
    expect(parseDeepLink("humla://")).toEqual({ kind: "focus" });
    expect(parseDeepLink("humla://settings?tab=account")).toEqual({ kind: "focus" });
    expect(parseDeepLink("humla://new-workspace/extra")).toEqual({ kind: "focus" });
  });

  it("rejects anything that isn't a humla link", () => {
    expect(parseDeepLink("https://humla.team/new-workspace?source=web")).toBeNull();
    expect(parseDeepLink("not a url")).toBeNull();
  });
});

describe("linkSource", () => {
  it("allows only email and web", () => {
    expect(linkSource("email")).toBe("email");
    expect(linkSource("web")).toBe("web");
    expect(linkSource("new_workspace")).toBeUndefined();
    expect(linkSource(null)).toBeUndefined();
  });
});

describe("handleDeepLinks", () => {
  it("queues a create request until something takes it", () => {
    handleDeepLinks(["humla://new-workspace?source=email"]);
    expect(useDeepLinkStore.getState().take()).toEqual({ source: "email" });
    expect(useDeepLinkStore.getState().take()).toBeNull();
  });

  it("queues nothing for other links or no links", () => {
    handleDeepLinks(["humla://something-else", "https://example.com"]);
    handleDeepLinks(null);
    expect(useDeepLinkStore.getState().newWorkspace).toBeNull();
  });
});
