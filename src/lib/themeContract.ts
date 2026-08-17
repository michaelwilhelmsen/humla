// The token contract every theme in src/styles/themes/ must satisfy.
//
// This list is the interface between a theme file and the rest of the app: the
// structural CSS in globals.css reads only these names, so a theme that
// declares all of them can restate Humla's typeface, type scale, control
// metrics and colours without a component knowing. themes.contract.test.ts
// walks the theme files and names anything missing, which is what makes adding
// a fourth theme a mechanical job rather than an archaeology one.
//
// Two groups, because they behave differently across light/dark:
//
// SHAPE  — declared once, in the theme's base block. A typeface and a row
//          height don't change when the lights go out, so restating them in the
//          dark blocks would be noise that can drift.
// COLOUR — declared in all three blocks (light, dark-via-system,
//          dark-explicit). These are the values a mode exists to change.

export const SHAPE_TOKENS = [
  // typeface
  "--font-sans",
  "--font-code",
  // type scale
  "--text-title-size",
  "--text-title-weight",
  "--text-title-line",
  "--text-title-tracking",
  "--text-heading-size",
  "--text-body-size",
  "--text-body-line",
  "--text-body-tracking",
  "--text-ui-size",
  "--text-label-size",
  "--text-code-size",
  // icons
  "--icon-size",
  "--icon-gap",
  "--icon-btn-size",
  // controls
  "--control-height",
  "--control-pad-x",
  "--control-radius",
  "--control-min-width",
  // rows + rhythm
  "--row-height",
  "--row-height-lg",
  "--row-gap",
  "--col-gap",
  // navigation
  "--nav-row-height",
  "--nav-label-size",
  "--nav-radius",
  // bars
  "--bar-height",
  "--bar-pad-left",
  "--bar-radius",
  // badge + switch
  "--badge-height",
  "--badge-radius",
  "--switch-w",
  "--switch-h",
  "--switch-thumb",
  // radii
  "--radius",
  "--radius-card",
] as const;

export const COLOUR_TOKENS = [
  // surfaces, in elevation order
  "--color-canvas",
  "--color-sidebar-bg",
  "--color-surface",
  "--color-surface-2",
  "--color-surface-raised",
  "--color-sidebar-active",
  // text, strongest first
  "--color-text-display",
  "--color-text",
  "--color-text-muted",
  "--color-text-disabled",
  // lines + fills
  "--color-line",
  "--color-line-visible",
  "--color-card-border",
  "--color-pill",
  "--color-pill-hover",
  "--color-input-bg",
  "--color-shadow",
  "--color-icon",
  // accent family
  "--color-accent",
  "--color-accent-2",
  "--color-accent-soft",
  "--color-on-accent",
  "--color-accent-text",
  // reds, which are never the accent
  "--color-record",
  "--color-danger",
  // semantic / speaker hues
  "--color-interactive",
  "--color-success",
  "--color-warning",
  "--color-warning-text",
  "--color-speaker-4",
  // usage-meter status (#69) — held to AAA on --color-surface
  "--color-status-warning",
  "--color-status-danger",
  // components with their own colour pair
  "--color-badge-bg",
  "--color-badge-border",
  "--color-badge-text",
  "--color-switch-on",
  // legacy aliases, still referenced in a few places
  "--color-ink",
  "--color-ink-muted",
  // not a --color-*, but mode-dependent all the same
  "--shadow-card",
  "color-scheme",
] as const;

export type ShapeToken = (typeof SHAPE_TOKENS)[number];
export type ColourToken = (typeof COLOUR_TOKENS)[number];
