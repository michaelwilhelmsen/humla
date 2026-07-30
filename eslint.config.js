import js from "@eslint/js";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";

// Lint config for the frontend, added after a hook placed below an early return
// shipped and broke the whole note view ("Rendered more hooks than during the
// previous render"). `react-hooks/rules-of-hooks` catches exactly that class,
// and nothing else in the toolchain does: `tsc` can't see it and a component
// without a unit test never exercises the render order.
//
// Deliberately narrow. This is a correctness net, NOT a style pass: there is no
// formatter in this repo, so a broad ruleset would bury the rules that catch
// bugs under hundreds of cosmetic findings nobody reads. Type-aware linting is
// off for the same reason — `pnpm build` already runs `tsc -b`.
//
// Run with `pnpm lint`.
export default tseslint.config(
  { ignores: ["dist", "src-tauri", "node_modules"] },
  {
    files: ["src/**/*.{ts,tsx}"],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    languageOptions: {
      globals: { ...globals.browser, ...globals.es2022 },
    },
    plugins: { "react-hooks": reactHooks },
    rules: {
      // The reason this config exists.
      "react-hooks/rules-of-hooks": "error",
      // A warning, not an error: the codebase has deliberate suppressions where
      // a dep would re-run an effect that must not re-run, and each is commented.
      "react-hooks/exhaustive-deps": "warn",

      // `catch {}` on a best-effort path is an idiom here (a failed suggestion
      // fetch must never break a rename), so an empty block is intentional.
      "no-empty": ["error", { allowEmptyCatch: true }],
      // Off: it flags `let x = <default>` followed by branches that all assign,
      // which is a deliberate readability idiom here, not dead code.
      "no-useless-assignment": "off",
      // `_`-prefixed args are the convention for "required by the signature,
      // unused here".
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_", caughtErrors: "none" },
      ],
    },
  },
  {
    // Tests reach for `any` when stubbing IPC payloads, and asserting on a
    // non-null query result is the point of the assertion.
    files: ["src/**/*.test.{ts,tsx}", "src/test/**"],
    rules: {
      "@typescript-eslint/no-explicit-any": "off",
      "@typescript-eslint/no-non-null-assertion": "off",
    },
  },
);
