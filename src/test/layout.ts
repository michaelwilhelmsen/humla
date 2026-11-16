/**
 * Report a non-zero layout box for every element in jsdom.
 *
 * Both transcript readers virtualize their lines via @tanstack/react-virtual,
 * which sizes the scroll window from clientWidth/clientHeight and measures
 * rows by offsetHeight. jsdom pins all of them to 0, so the virtualizer decides nothing is visible and
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
 *
 * `clientBox: false` leaves clientWidth/clientHeight at jsdom's 0, for a file
 * whose assertions depend on a container reading as unmeasured.
 */
export function mockLayoutBox({ width = 400, height = 600, clientBox = true } = {}): void {
  const props: [string, number][] = [
    ["offsetHeight", height],
    ["offsetWidth", width],
  ];
  if (clientBox) props.push(["clientHeight", height], ["clientWidth", width]);
  for (const [prop, value] of props) {
    Object.defineProperty(window.HTMLElement.prototype, prop, {
      configurable: true,
      get: () => value,
    });
  }
}
