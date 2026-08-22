// Sweep the body column's width and assert the recording bar's controls row
// never leaves it.
//
// The row degrades by container query (see `ROW_STEPS` in `RecordingBar`), and
// every pill in it is `shrink-0 whitespace-nowrap` — so the row either fits or
// paints outside the column, over the nav card on one side and under the
// context panel on the other (#177). jsdom pins every box to 0, so no unit test
// can answer this; the numbers in `ROW_STEPS` came from here and should be
// re-derived here after any change to the row's contents or to a theme's
// control metrics.
//
// Usage: `pnpm mock`, open
//   http://localhost:1420/mock.html?case=recbar-long&palette=graphite&theme=light
// then paste this into the browser console. Repeat for `?case=recbar-long-summary`
// (the three-pill row a summary running during a recording produces) and for
// `palette=warm`. Graphite is the wider theme, so it is the one that binds.
//
// The timer is forced to its widest honest reading (`123:45`, an hour-plus
// meeting) on every step, because the mock's clock starts at zero and two more
// digits are two more digits the row has to hold.
(() => {
  const pills = [...document.querySelectorAll(".nd-recpill")];
  const controls = pills.find((p) => p.querySelector("button"));
  if (!controls) throw new Error("no controls pill here — use ?case=recbar-long");
  const row = controls.parentElement;
  const bar = row.parentElement;
  const column = bar.parentElement;
  const restore = column.style.width;
  // The bar's own `px-4`: the container-query thresholds are its CONTENT box,
  // and this is what turns a column width into one.
  const PADDING = 32;

  const widest = () => {
    const t = [...controls.querySelectorAll("span")].find((s) =>
      /^\d+:\d\d$/.test(s.textContent.trim()),
    );
    if (t) t.textContent = "123:45";
  };

  const overflows = [];
  let tightest = Infinity;
  let tightestAt = 0;
  // 348 is `BODY_MIN` (420) minus the padding, less the 40px of slack the
  // clamp can spend on a minimum-size window — below that there is nothing
  // left for the ladder to hide.
  for (let w = 348 + PADDING; w <= 900; w += 1) {
    column.style.width = `${w}px`;
    void bar.offsetWidth; // force layout so the container query re-evaluates
    widest();
    const rowW = row.getBoundingClientRect().width;
    const content = bar.clientWidth - PADDING;
    const slack = Math.round(content - rowW);
    if (slack < 0) overflows.push(`${w}px:${slack}`);
    if (slack < tightest) {
      tightest = slack;
      tightestAt = w;
    }
  }

  column.style.width = restore;
  const palette = document.documentElement.dataset.palette ?? "warm";
  const scenario = new URLSearchParams(location.search).get("case");
  const report = {
    palette,
    scenario,
    overflows: overflows.length,
    firstOverflows: overflows.slice(0, 5),
    tightestSlackPx: tightest,
    atColumnWidth: `${tightestAt}px`,
  };
  console.table([report]);
  if (overflows.length) {
    console.error(`FAIL (${palette} / ${scenario}): the row leaves its column — retune ROW_STEPS.`);
  } else {
    console.log(`OK (${palette} / ${scenario}): no overflow at any swept width.`);
  }
  return report;
})();
