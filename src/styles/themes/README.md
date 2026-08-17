# Themes

A **theme** is one file in this directory. It declares the whole token contract
and nothing else — no component rules, no utilities. Everything structural lives
in `../globals.css` and reads these tokens, so a theme can restate the app's
typeface, type scale, control metrics and colours without touching a component.

## Adding a theme

1. Copy an existing file (`graphite.css` is the shortest) to `<id>.css`.
2. Fill in every token. The contract is `src/lib/themeContract.ts`; the test
   `themes.contract.test.ts` fails and names each token you missed.
3. Register it in `src/lib/palette.ts` (`PALETTE_REGISTRY`) — id, label, one-line
   description. Settings picks it up from there; there is no second list to edit.
4. `@import` it from `../globals.css`.
5. If it needs a typeface Humla doesn't bundle, add the `@fontsource` package and
   import it in **both** `src/main.tsx` and `src/mockBoot.tsx`.

That's the whole surface: one file, one registry row, one import.

## The three blocks

Every theme file has exactly three blocks, in this order:

```css
[data-palette="<id>"], :root:not([data-palette]) { /* only the default theme carries the :root fallback */
  /* shape tokens (type + metrics) — declared once, they don't vary by mode */
  /* colour tokens — light values */
}
@media (prefers-color-scheme: dark) {
  [data-palette="<id>"]:not([data-theme="light"]) { /* colour tokens — dark values */ }
}
[data-palette="<id>"][data-theme="dark"] { /* colour tokens — dark values, identical */ }
```

The dark values appear **twice** on purpose: once for "System" (the theme picker
wrote no `data-theme`) and once for an explicit Dark choice. CSS has no way to
share one declaration set between a media query and a plain selector, so the
duplication is structural. `themes.contract.test.ts` asserts the two dark blocks
are byte-identical declaration sets, which is what stops them drifting apart.

Specificity is uniform by construction: the light block is `0,1,0` (or `0,2,0`
for the `:root` fallback), both dark blocks are one attribute higher, and two
themes can never collide because `data-palette` holds a single value.

## Shape vs colour

Shape tokens (`--font-*`, `--text-*`, `--icon-*`, `--control-*`, `--row-*`,
`--nav-*`, `--bar-*`, `--badge-*`, `--switch-*`, `--radius*`) are declared in the
light block only and inherited by both modes — a typeface doesn't change when the
lights go out. Colour tokens (`--color-*`, `--shadow-card`, `color-scheme`) are
declared in all three.

`--font-sans`, `--font-code` and `--radius-card` also appear in `globals.css`'s
`@theme` block. That is where Tailwind generates the `font-sans` / `font-code` /
`rounded-card` utilities from; the values there are defaults only. A theme file is
unlayered CSS and so outranks `@layer theme` regardless of source order.

## Deriving what a recipe doesn't give you

A design recipe names a dozen values; the contract wants ~70. Every value that
wasn't handed to us is marked `/* derived */` in the theme file, with the reason.
Anything the recipe *did* specify is marked `/* recipe */` so a later reader can
tell which numbers are load-bearing and which are ours to re-tune.
