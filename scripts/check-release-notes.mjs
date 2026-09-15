import { readFile } from "node:fs/promises";
import ts from "typescript";

export function validateReleaseNotes(releases, version) {
  const matches = releases.filter((release) => release.version === version);
  if (matches.length !== 1) throw new Error(`Expected exactly one changelog entry for ${version}.`);
  const release = matches[0];
  const filled = (value) => typeof value === "string" && value.trim().length > 0;
  if (![release.title, release.summary].every(filled)) throw new Error("Release title and summary are required.");
  if (!/^\d{4}-\d{2}-\d{2}$/.test(release.date) || !Number.isFinite(Date.parse(release.date)) || new Date(release.date).toISOString().slice(0, 10) !== release.date) {
    throw new Error("A valid release date (YYYY-MM-DD) is required.");
  }
  for (const field of ["paragraphs", "highlights"]) {
    if (!Array.isArray(release[field]) || !release[field].length || !release[field].every(filled)) {
      throw new Error(`Release ${field} must contain non-empty text.`);
    }
  }
  if (!Array.isArray(release.issues) || release.issues.some((issue) =>
    !filled(issue.title) || !/^#\d+$/.test(issue.reference) ||
    !["Open", "Closed"].includes(issue.status) ||
    issue.url !== `https://github.com/michaelwilhelmsen/humla/issues/${issue.reference.slice(1)}`
  )) throw new Error("Related issues must be complete references to michaelwilhelmsen/humla issues.");
}

async function main() {
  const source = await readFile(new URL("../src/content/releases.ts", import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } });
  const { releases } = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
  const version = process.argv[2];
  validateReleaseNotes(releases, version);
  console.log(`Changelog ready for ${version}.`);
}

if (process.argv[1]?.endsWith("check-release-notes.mjs")) {
  main().catch((error) => {
    console.error(`error: ${error.message} Update src/content/releases.ts before releasing.`);
    process.exitCode = 1;
  });
}
