/**
 * Report a non-zero layout box for every element in jsdom.
 *
 * Both transcript readers virtualize their lines via @tanstack/react-virtual,
 * which reads offsetWidth/offsetHeight to size the scroll window and measure
 * rows. jsdom pins both to 0, so the virtualizer decides nothing is visible and
 * renders no rows at all — there is no transcript text to assert on or click.
 *
 * Deliberately NOT in src/test/setup.ts. Everything there fills in an API jsdom
 * is missing, guarded by an `if (!…)`; this instead *fakes* a value jsdom does
 * implement, so applying it suite-wide would silently change what unrelated
 * components render (anything branching on a measured size would take the
 * has-room path it never takes today). Opt in per file, from a `beforeAll`.
 *
 * The properties are defined on HTMLElement.prototype and are `configurable`,
 * so they last for the file's module realm and can be redefined.
 */
export function mockLayoutBox({ width = 400, height = 600 } = {}): void {
  Object.defineProperty(window.HTMLElement.prototype, "offsetHeight", {
    configurable: true,
    get: () => height,
  });
  Object.defineProperty(window.HTMLElement.prototype, "offsetWidth", {
    configurable: true,
    get: () => width,
  });
}
