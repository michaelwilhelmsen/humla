import { describe, expect, it } from "vitest";
import { accessLabel, addableMembers, firstName, withMember, withoutMember } from "./noteAccess";
import type { CloudMember } from "./cloud";

const member = (id: string, name: string, email = `${id}@example.no`): CloudMember => ({
  id,
  name,
  email,
  role: "member",
});
const members = {
  me: member("me", "Michael Wilhelmsen"),
  stian: member("stian", "Stian Berg"),
  petter: member("petter", "Petter"),
  anne: member("anne", "", "anne.k@example.no"),
};

describe("accessLabel", () => {
  it("names the workspace when everyone can read the note", () => {
    expect(accessLabel({ private: false, shared_with: [] }, "me", "me", members, "Screenpartner")).toBe(
      "Screenpartner",
    );
  });

  it("says Only you when the author alone can", () => {
    expect(accessLabel({ private: true, shared_with: [] }, "me", "me", members, "S")).toBe("Only you");
  });

  it("names up to two people, the author included", () => {
    expect(accessLabel({ private: true, shared_with: ["stian"] }, "me", "me", members, "S")).toBe(
      "Michael, Stian",
    );
  });

  it("counts three or more", () => {
    expect(
      accessLabel({ private: true, shared_with: ["stian", "petter"] }, "me", "me", members, "S"),
    ).toBe("3 people");
  });
});

describe("firstName", () => {
  it("falls back to the email's local part, then to Someone", () => {
    expect(firstName(members.anne)).toBe("anne.k");
    expect(firstName(undefined)).toBe("Someone");
  });
});

describe("addableMembers", () => {
  it("leaves out the author and everyone who already has access", () => {
    const out = addableMembers(members, "me", { private: true, shared_with: ["stian"] }, "");
    expect(out.map((m) => m.id)).toEqual(["anne", "petter"]);
  });

  it("matches the query against name or email", () => {
    expect(addableMembers(members, "me", { private: false, shared_with: [] }, "ber").map((m) => m.id)).toEqual([
      "stian",
    ]);
    expect(addableMembers(members, "me", { private: false, shared_with: [] }, "anne.k").map((m) => m.id)).toEqual([
      "anne",
    ]);
  });
});

describe("withMember / withoutMember", () => {
  it("adding someone to a note everyone reads restricts it to them", () => {
    expect(withMember({ private: false, shared_with: [] }, "stian")).toEqual({ private: true, shared_with: ["stian"] });
  });

  it("adds once and removes cleanly", () => {
    const a = withMember(withMember({ private: true, shared_with: [] }, "stian"), "stian");
    expect(a.shared_with).toEqual(["stian"]);
    expect(withoutMember(a, "stian")).toEqual({ private: true, shared_with: [] });
  });
});
