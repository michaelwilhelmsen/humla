// The architecture test for themes. It reads the CSS files as text — no jsdom,
// no Tailwind — and holds every theme to the same four rules:
//
//   1. every theme in the registry has a file, and every file is registered;
//   2. every theme declares the full colour contract in all three blocks and
//      the full shape contract in its base block;
//   3. the two dark blocks (via-system and explicit) are the same declarations,
//      which is the only guard against the duplication CSS forces on us
//      drifting apart;
//   4. a theme file declares tokens and nothing else — no component rules — and
//      only `warm` carries the no-attribute fallback.
//
// When a fourth theme is added, this test is the checklist: it names each
// missing token instead of leaving the gap to be found by eye in the running
// app, where an undefined token renders as "inherit" or nothing at all.

import { readFileSync, readdirSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { COLOUR_TOKENS, SHAPE_TOKENS } from "../../lib/themeContract";
import { DEFAULT_PALETTE, PALETTE_REGISTRY } from "../../lib/palette";

// `node:fs` and `__dirname` are declared in src/test/node-fs.d.ts — see that
// file for why this is neither ?raw nor @types/node.
function read(id: string): string {
  return readFileSync(`${__dirname}/${id}.css`, "utf8");
}

// Every top-level rule body in the file, paired with its selector text. Nested
// bodies (a rule inside @media) are returned too; the @media wrapper itself is
// not, so `blocks()` is a flat list of "selector → declarations".
function blocks(css: string): { selector: string; body: string }[] {
  const out: { selector: string; body: string }[] = [];
  const stripped = css.replace(/\/\*[\s\S]*?\*\//g, "");
  // Rule bodies never nest in these files beyond @media > rule, so a
  // non-greedy match up to the first closing brace is exact.
  const re = /([^{}]+)\{([^{}]*)\}/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(stripped))) {
    const selector = m[1].trim().replace(/\s+/g, " ");
    if (selector.startsWith("@")) continue; // the @media wrapper's own "selector"
    out.push({ selector, body: m[2] });
  }
  return out;
}

// The declared custom properties (plus `color-scheme`, which behaves like one
// here) in a rule body, as a name → value map.
function declarations(body: string): Map<string, string> {
  const out = new Map<string, string>();
  for (const raw of body.split(";")) {
    const decl = raw.trim();
    if (!decl) continue;
    const i = decl.indexOf(":");
    if (i === -1) continue;
    out.set(decl.slice(0, i).trim(), decl.slice(i + 1).trim());
  }
  return out;
}

function baseBlock(css: string, id: string) {
  const b = blocks(css).find(
    (x) => x.selector.includes(`[data-palette="${id}"]`) && !x.selector.includes("data-theme"),
  );
  if (!b) throw new Error(`${id}.css has no base block`);
  return declarations(b.body);
}

function darkBlocks(css: string, id: string) {
  const found = blocks(css).filter(
    (x) =>
      x.selector.includes(`[data-palette="${id}"]`) &&
      (x.selector.includes('[data-theme="dark"]') ||
        x.selector.includes(':not([data-theme="light"])')),
  );
  return found.map((x) => ({ selector: x.selector, decls: declarations(x.body) }));
}

const THEME_FILES = readdirSync(__dirname)
  .filter((f) => f.endsWith(".css"))
  .map((f) => f.replace(/\.css$/, ""))
  .sort();

describe("theme registry ↔ theme files", () => {
  it("has exactly one file per registered theme", () => {
    expect(THEME_FILES).toEqual([...PALETTE_REGISTRY.map((p) => p.id)].sort());
  });

  it("gives every theme a distinct label and description", () => {
    const labels = PALETTE_REGISTRY.map((p) => p.label);
    const descriptions = PALETTE_REGISTRY.map((p) => p.description);
    expect(new Set(labels).size).toBe(labels.length);
    expect(new Set(descriptions).size).toBe(descriptions.length);
  });
});

describe.each(PALETTE_REGISTRY.map((p) => p.id))("theme: %s", (id) => {
  const css = read(id);

  it("declares every shape token in its base block", () => {
    const declared = baseBlock(css, id);
    const missing = SHAPE_TOKENS.filter((t) => !declared.has(t));
    expect(missing).toEqual([]);
  });

  it("declares every colour token in its base block", () => {
    const declared = baseBlock(css, id);
    const missing = COLOUR_TOKENS.filter((t) => !declared.has(t));
    expect(missing).toEqual([]);
  });

  it("has both dark blocks — system-preference and explicit", () => {
    const dark = darkBlocks(css, id);
    expect(dark).toHaveLength(2);
    expect(dark.some((d) => d.selector.includes(':not([data-theme="light"])'))).toBe(true);
    expect(dark.some((d) => d.selector.includes('[data-theme="dark"]'))).toBe(true);
  });

  it("declares every colour token in both dark blocks", () => {
    for (const { selector, decls } of darkBlocks(css, id)) {
      const missing = COLOUR_TOKENS.filter((t) => !decls.has(t));
      expect(missing, `missing in ${selector}`).toEqual([]);
    }
  });

  it("keeps the two dark blocks identical, value for value", () => {
    const [a, b] = darkBlocks(css, id);
    expect(Object.fromEntries(a.decls)).toEqual(Object.fromEntries(b.decls));
  });

  it("restates no shape token in a dark block (they don't vary by mode)", () => {
    for (const { selector, decls } of darkBlocks(css, id)) {
      const restated = SHAPE_TOKENS.filter((t) => decls.has(t));
      expect(restated, `restated in ${selector}`).toEqual([]);
    }
  });

  it("declares tokens only — a theme file carries no component rules", () => {
    for (const { selector, body } of blocks(css)) {
      // Every selector in a theme file must be a data-palette block (the
      // default theme's :root fallback included).
      expect(selector, `unexpected selector in ${id}.css`).toMatch(
        /\[data-palette="[a-z-]+"\]|:root:not\(\[data-palette\]\)/,
      );
      // …and every declaration in it must be a token, never a property that
      // paints something.
      for (const name of declarations(body).keys()) {
        expect(name, `non-token declaration in ${id}.css (${selector})`).toMatch(
          /^(--|color-scheme$)/,
        );
      }
    }
  });

  it("carries the no-attribute :root fallback only if it is the default", () => {
    const hasFallback = css.includes(":root:not([data-palette])");
    expect(hasFallback).toBe(id === DEFAULT_PALETTE);
  });
});
