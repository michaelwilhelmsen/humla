// Sweep the note toolbar's width and assert it never overflows its column.
//
// The toolbar degrades by container query (see `NoteToolbar`'s BACK_LABEL /
// ACTION_LABEL), and `.nd-btn` is `flex-shrink: 0; white-space: nowrap` — so
// the row either fits or spills out of its card. jsdom pins every box to 0, so
// no unit test can answer this; the numbers in `NoteToolbar` and `BODY_MIN`
// came from here and should be re-derived here after any change to the row's
// contents or to a theme's control metrics.
//
// Usage: `pnpm mock`, open
//   http://localhost:1420/mock.html?case=toolbar-wide&palette=warm&theme=light
// then paste this into the browser console. Repeat for `palette=graphite`.
// Both gutters are swept for you (the 116px one is a collapsed sidebar
// clearing the macOS traffic lights).
(() => {
  const bar = document.querySelector("[data-tauri-drag-region]");
  if (!bar) throw new Error("no toolbar on this page — use ?case=toolbar-wide");
  const card = bar.parentElement;
  const restore = { cardWidth: card.style.width, padLeft: bar.style.paddingLeft };
  const report = [];

  for (const gutter of ["12px", "116px"]) {
    bar.style.paddingLeft = gutter;
    const overflows = [];
    let tightest = Infinity;
    let tightestAt = 0;
    // 275px is the row's irreducible CONTENT width once every label has
    // dropped; below that there is nothing left to hide, which is what
    // `BODY_MIN` exists to keep out of reach.
    const from = parseInt(gutter, 10) + 12 + 275;
    for (let w = from; w <= 900; w += 1) {
      card.style.width = `${w}px`;
      void bar.offsetWidth; // force layout so the container query re-evaluates
      const over = bar.scrollWidth - bar.clientWidth;
      if (over > 0) overflows.push(`${w}px:+${over}`);
      const spacer = [...bar.children].find(
        (c) => c.tagName === "DIV" && !c.textContent.trim(),
      );
      const slack = spacer ? Math.round(spacer.getBoundingClientRect().width) : 0;
      if (slack < tightest) {
        tightest = slack;
        tightestAt = w;
      }
    }
    report.push({
      gutter,
      sweptFrom: `${from}px`,
      overflows: overflows.length,
      firstOverflows: overflows.slice(0, 5),
      tightestSlackPx: tightest,
      atWidth: `${tightestAt}px`,
    });
  }

  card.style.width = restore.cardWidth;
  bar.style.paddingLeft = restore.padLeft;
  const palette = document.documentElement.dataset.palette ?? "warm";
  console.table(report);
  if (report.some((r) => r.overflows > 0)) {
    console.error(`FAIL (${palette}): the toolbar overflows — retune the thresholds.`);
  } else {
    console.log(`OK (${palette}): no overflow at any swept width.`);
  }
  return report;
})();
