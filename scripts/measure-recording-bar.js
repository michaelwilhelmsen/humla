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
// then paste this into the browser console. Repeat for every `recbar-*` case, in
// both palettes — graphite is the wider theme, so it is the one that binds:
//
//   - a live capture: `recbar-380` / `-420` / `-default` / `-wide` / `-long`,
//     and the three-pill row a running summary adds (`-summary`,
//     `-summary-380`, `-long-summary`)
//   - the stop chain's steps, each with its `-summary` and narrow variants:
//     `-stopping*`, `-saving*`, `-diarizing*`, `-playback*`, `-unify*`, and
//     `-deferred`, whose row must be EMPTY
//   - a deferred transcription's replay: `-replay*` (`-takes`, `-streams`,
//     `-playback`, `-unify`, `-unknown`, `-380`, `-summary`) and its diarize
//     half, `-diarize-replay*`
//
// The widest labels live in the three-pill arrangements — a replay's
// "Identifying speakers in take 2 of 3…" is the widest of all of them.
//
// The timer is forced to its widest honest reading (`123:45`, an hour-plus
// meeting) on every step, because the mock's clock starts at zero and two more
// digits are two more digits the row has to hold.
(() => {
  const pills = [...document.querySelectorAll(".nd-recpill")];
  // The controls pill anchors the row while a capture runs; after stop it is
  // gone, so fall back to any pill in the row.
  const anchor = pills.find((p) => p.querySelector("button")) ?? pills[pills.length - 1];
  if (!anchor) throw new Error("no recording pill here — use ?case=recbar-*");
  const row = anchor.parentElement;
  const bar = row.parentElement;
  const column = bar.parentElement;
  const restore = column.style.width;
  // The bar's own `px-4`: the container-query thresholds are its CONTENT box,
  // and this is what turns a column width into one.
  const PADDING = 32;

  const widest = () => {
    const t = [...row.querySelectorAll("span")].find((s) =>
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
