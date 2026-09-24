//! Offline echo cancellation of the system stream out of the mic — for the
//! diarize input, where the echo of the speakers is heard as extra voices.
//!
//! Reference-based and content-agnostic: the system stream is the exact
//! far-end signal, so one filter removes every remote voice at once, however
//! many the call carries, and nothing here knows or cares what was said. The
//! user's own voice survives where it overlaps the echo, because the filter
//! slows itself down whenever the error holds more than the echo it expects.
//!
//! Three stages:
//!
//! 1. **Bulk delay** — the reference is re-read at the lag the delay track
//!    measured ([`DelayMap`]), less a few milliseconds of margin, so the
//!    adaptive filter only has to model the room, not the capture's offset.
//! 2. **Adaptive filter** — a partitioned-block frequency-domain filter
//!    (MDF, after SpeexDSP's `mdf.c`), whose per-bin step follows the
//!    estimated residual-to-error ratio (Valin, 2007). No double-talk detector
//!    is needed: when the user talks, the error outgrows the residual echo the
//!    filter predicts, and the step shrinks with it.
//! 3. **Residual suppression** — a Wiener gain per bin against what the
//!    filter could not cancel: its own leak estimate times the echo estimate,
//!    held over a short reverberant decay.
//!
//! Two passes: the first converges, the second restarts from the converged
//! filter, so the opening seconds are cancelled as well as the rest.
//! [`cancel_threaded`], the app's, gives a short take the same two passes and
//! spreads a long one over threads.

use crate::delay::DelayMap;
use crate::fft::{Fft, C64};
use crate::sample::{pcm16, Sample};
use crate::stats::{db_ratio, median, percentile};
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct AecConfig {
    /// Samples per block; the FFT is twice this.
    pub block: usize,
    /// Room response the filter can model after the bulk delay.
    pub tail_ms: f64,
    /// How far into the filter the direct path is placed, so a bulk delay a
    /// little too long still leaves it inside.
    pub margin_ms: f64,
    pub passes: usize,
    /// Run stage 3. Off leaves the linear filter's output.
    pub suppress: bool,
    /// Lowest gain residual suppression applies.
    pub floor_db: f64,
    /// How much the residual estimate is trusted over the error.
    pub overdrive: f64,
}

impl Default for AecConfig {
    fn default() -> Self {
        Self {
            block: 256,
            tail_ms: 256.0,
            margin_ms: 5.0,
            passes: 2,
            suppress: true,
            floor_db: -30.0,
            overdrive: 1.5,
        }
    }
}

/// The system stream as the mic hears it, by index: re-read `lag − margin`
/// behind, so the echo's direct path lands `margin` into the filter.
pub fn align_reference(sys: &[f32], len: usize, rate: u32, map: &DelayMap, margin_ms: f64) -> Vec<f32> {
    let rate_f = rate as f64;
    (0..len)
        .map(|n| {
            let back = ((map.lag_ms_at(n as f64 / rate_f) - margin_ms) / 1000.0 * rate_f).round() as isize;
            let at = n as isize - back;
            if at < 0 || at as usize >= sys.len() {
                0.0
            } else {
                sys[at as usize]
            }
        })
        .collect()
}

/// Floor on the leak estimate, as in Speex: a filter is never taken to be
/// perfect.
const MIN_LEAK: f64 = 0.005;

#[derive(Clone)]
struct Mdf {
    b: usize,
    n: usize,
    p: usize,
    fft: Fft,
    x_prev: Vec<f64>,
    /// Far-end spectra, newest at `head`, partition j at `(head + j) % p`.
    xs: Vec<Vec<C64>>,
    head: usize,
    /// The background filter, which adapts every block.
    w: Vec<Vec<C64>>,
    /// The foreground filter, which the output is made with. It takes the
    /// background's coefficients only when they are significantly better, and
    /// gives its own back when the background has diverged — SpeexDSP's
    /// two-path scheme, and what keeps a stretch of double-talk that fooled the
    /// step size from reaching the output.
    fg: Vec<Vec<C64>>,
    davg1: f64,
    davg2: f64,
    dvar1: f64,
    dvar2: f64,
    /// Smoothed far-end power per bin, for the step normalisation.
    power: Vec<f64>,
    rf_avg: Vec<f64>,
    yf_avg: Vec<f64>,
    pey: f64,
    pyy: f64,
    leak: f64,
    adapted: bool,
    sum_adapt: f64,
    blocks: u64,
    beta0: f64,
    beta_max: f64,
    y_bg: Vec<C64>,
    y_fg: Vec<C64>,
    e_spec: Vec<C64>,
    y_spec: Vec<C64>,
    step: Vec<f64>,
    tmp: Vec<C64>,
    e_bg: Vec<f64>,
    yb: Vec<f64>,
}

impl Mdf {
    fn new(b: usize, p: usize, rate: f64) -> Self {
        let n = 2 * b;
        Self {
            b,
            n,
            p,
            fft: Fft::new(n),
            x_prev: vec![0.0; b],
            xs: vec![vec![C64::ZERO; n]; p],
            head: 0,
            w: vec![vec![C64::ZERO; n]; p],
            fg: vec![vec![C64::ZERO; n]; p],
            davg1: 0.0,
            davg2: 0.0,
            dvar1: 0.0,
            dvar2: 0.0,
            power: vec![0.0; n],
            rf_avg: vec![0.0; b + 1],
            yf_avg: vec![0.0; b + 1],
            pey: 0.0,
            pyy: 0.0,
            leak: 0.0,
            adapted: false,
            sum_adapt: 0.0,
            blocks: 0,
            beta0: 2.0 * b as f64 / rate,
            beta_max: 0.5 * b as f64 / rate,
            y_bg: vec![C64::ZERO; n],
            y_fg: vec![C64::ZERO; n],
            e_spec: vec![C64::ZERO; n],
            y_spec: vec![C64::ZERO; n],
            step: vec![0.0; n],
            tmp: vec![C64::ZERO; n],
            e_bg: vec![0.0; b],
            yb: vec![0.0; b],
        }
    }

    /// Forget the far-end history but keep what was learned — the start of a
    /// second pass.
    fn reset_history(&mut self) {
        self.x_prev.iter_mut().for_each(|v| *v = 0.0);
        for x in &mut self.xs {
            x.iter_mut().for_each(|v| *v = C64::ZERO);
        }
        self.head = 0;
    }

    /// Σ_j filter_j · X_j into `out`, back in the time domain.
    fn estimate(fft: &Fft, filter: &[Vec<C64>], xs: &[Vec<C64>], head: usize, out: &mut [C64]) {
        let p = filter.len();
        out.iter_mut().for_each(|v| *v = C64::ZERO);
        for (j, fj) in filter.iter().enumerate() {
            let xj = &xs[(head + j) % p];
            for ((o, &f), &x) in out.iter_mut().zip(fj).zip(xj) {
                *o += f * x;
            }
        }
        fft.inverse(out);
    }

    fn process(&mut self, d: &[f64], x: &[f64], e_out: &mut [f64], y_out: &mut [f64]) {
        let (b, n, p) = (self.b, self.n, self.p);

        // 1. Far-end spectrum of [previous block, this block].
        self.head = (self.head + p - 1) % p;
        {
            let slot = &mut self.xs[self.head];
            for i in 0..b {
                slot[i] = C64::new(self.x_prev[i], 0.0);
                slot[b + i] = C64::new(x[i], 0.0);
            }
            self.fft.forward(slot);
        }
        self.x_prev.copy_from_slice(x);

        // 2. Both filters' echo estimates: the last B samples of Σ_j W_j·X_j.
        Self::estimate(&self.fft, &self.w, &self.xs, self.head, &mut self.y_bg);
        Self::estimate(&self.fft, &self.fg, &self.xs, self.head, &mut self.y_fg);
        let (mut see, mut sff, mut dbf) = (0.0, 0.0, 1e-20);
        for i in 0..b {
            let (yb, yf) = (self.y_bg[b + i].re, self.y_fg[b + i].re);
            self.yb[i] = yb;
            self.e_bg[i] = d[i] - yb;
            y_out[i] = yf;
            e_out[i] = d[i] - yf;
            see += self.e_bg[i] * self.e_bg[i];
            sff += e_out[i] * e_out[i];
            dbf += (yb - yf) * (yb - yf);
        }

        // 3. Two-path: is the background's advantage over the foreground
        //    bigger than their difference can explain as noise? (The
        //    thresholds are Speex's.)
        let diff = sff - see;
        self.davg1 = 0.6 * self.davg1 + 0.4 * diff;
        self.davg2 = 0.85 * self.davg2 + 0.15 * diff;
        self.dvar1 = 0.36 * self.dvar1 + 0.16 * sff * dbf;
        self.dvar2 = 0.7225 * self.dvar2 + 0.0225 * sff * dbf;
        let better = diff * diff.abs() > sff * dbf
            || self.davg1 * self.davg1.abs() > 0.5 * self.dvar1
            || self.davg2 * self.davg2.abs() > 0.25 * self.dvar2;
        let worse = -diff * diff.abs() > 4.0 * sff * dbf
            || -self.davg1 * self.davg1.abs() > 4.0 * self.dvar1
            || -self.davg2 * self.davg2.abs() > 4.0 * self.dvar2;
        if better {
            for (f, w) in self.fg.iter_mut().zip(&self.w) {
                f.copy_from_slice(w);
            }
            // Cross-fade the output onto the new filter within this block.
            for i in 0..b {
                let r = (i as f64 + 0.5) / b as f64;
                y_out[i] = (1.0 - r) * y_out[i] + r * self.yb[i];
                e_out[i] = d[i] - y_out[i];
            }
            self.davg1 = 0.0;
            self.davg2 = 0.0;
            self.dvar1 = 0.0;
            self.dvar2 = 0.0;
        } else if worse {
            for (w, f) in self.w.iter_mut().zip(&self.fg) {
                w.copy_from_slice(f);
            }
            self.yb.copy_from_slice(y_out);
            self.e_bg.copy_from_slice(e_out);
            see = sff;
            self.davg1 = 0.0;
            self.davg2 = 0.0;
            self.dvar1 = 0.0;
            self.dvar2 = 0.0;
        }

        // Everything below adapts the background, on its own error.
        let (mut syy, mut sxx, mut sey) = (0.0, 0.0, 0.0);
        for i in 0..b {
            syy += self.yb[i] * self.yb[i];
            sxx += x[i] * x[i];
            sey += self.e_bg[i] * self.yb[i];
        }

        // 4. The error and the estimate as spectra, zero-padded in front.
        for i in 0..b {
            self.e_spec[i] = C64::ZERO;
            self.e_spec[b + i] = C64::new(self.e_bg[i], 0.0);
            self.y_spec[i] = C64::ZERO;
            self.y_spec[b + i] = C64::new(self.yb[i], 0.0);
        }
        self.fft.forward(&mut self.e_spec);
        self.fft.forward(&mut self.y_spec);

        // 5. Far-end power per bin.
        let ss = 0.35 / p as f64;
        let x0 = &self.xs[self.head];
        for k in 0..n {
            self.power[k] = (1.0 - ss) * self.power[k] + ss * x0[k].norm_sqr();
        }

        // 6. Leak: how much of the echo estimate's power fluctuation shows up
        //    in the error's. Near-end speech doesn't track the estimate, so it
        //    barely moves this.
        let (mut pey, mut pyy) = (0.0, 0.0);
        for k in 0..=b {
            let rf = self.e_spec[k].norm_sqr();
            let yf = self.y_spec[k].norm_sqr();
            self.rf_avg[k] = (1.0 - ss) * self.rf_avg[k] + ss * rf;
            self.yf_avg[k] = (1.0 - ss) * self.yf_avg[k] + ss * yf;
            let (eh, yh) = (rf - self.rf_avg[k], yf - self.yf_avg[k]);
            pey += eh * yh;
            pyy += yh * yh;
        }
        let alpha = (self.beta0 * syy / (see + 1e-12)).min(self.beta_max);
        self.pey = (1.0 - alpha) * self.pey + alpha * pey;
        self.pyy = (1.0 - alpha) * self.pyy + alpha * pyy;
        if self.pyy > 1e-30 {
            self.pey = self.pey.clamp(MIN_LEAK * self.pyy, self.pyy);
            self.leak = self.pey / self.pyy;
        }

        // 7. Residual-to-error ratio over the whole block.
        let mut rer = (1e-4 * sxx + 3.0 * self.leak * syy) / (see + 1e-12);
        let floor = sey * sey / (see * syy + 1e-24);
        if rer < floor {
            rer = floor;
        }
        let rer = rer.min(0.5);

        // 8. Step per bin: the share of this bin's error that is still echo,
        //    normalised by the far-end power the filter spans.
        let reg = 1e-8 * n as f64;
        if self.adapted {
            for k in 0..=b {
                let e = self.e_spec[k].norm_sqr() + 1e-20;
                let r = (self.leak * self.y_spec[k].norm_sqr()).min(0.5 * e);
                let r = 0.7 * r + 0.3 * rer * e;
                self.step[k] = r / e / (p as f64 * self.power[k] + reg);
            }
        } else {
            // Until the filter has learned something there is no estimate to
            // compare with; adapt at a rate set by how much of the error the
            // far end could explain.
            let rate = if sxx > 1e-9 * b as f64 {
                (0.25 * sxx).min(0.25 * see) / (see + 1e-12)
            } else {
                0.0
            };
            for k in 0..=b {
                self.step[k] = rate / (p as f64 * self.power[k] + reg);
            }
            self.sum_adapt += rate;
            if self.sum_adapt > p as f64 {
                self.adapted = true;
            }
        }
        for k in 1..b {
            self.step[n - k] = self.step[k];
        }

        // 9. Update every partition, then keep two of them causal (AUMDF:
        //    partition 0 and one rotating), as Speex does.
        for j in 0..p {
            let xj = &self.xs[(self.head + j) % p];
            let wj = &mut self.w[j];
            for k in 0..n {
                wj[k] += (xj[k].conj() * self.e_spec[k]).scale(self.step[k]);
            }
        }
        let rotating = (p > 1).then(|| 1 + (self.blocks as usize % (p - 1)));
        for j in std::iter::once(0).chain(rotating) {
            self.constrain(j);
        }
        self.blocks += 1;
    }

    /// Make partition `j` a causal B-tap filter again: back to the time
    /// domain, drop the half a circular update leaks into, and return.
    fn constrain(&mut self, j: usize) {
        self.tmp.copy_from_slice(&self.w[j]);
        self.fft.inverse(&mut self.tmp);
        for (i, v) in self.tmp.iter_mut().enumerate() {
            if i >= self.b {
                *v = C64::ZERO;
            } else {
                v.im = 0.0;
            }
        }
        self.fft.forward(&mut self.tmp);
        self.w[j].copy_from_slice(&self.tmp);
    }
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct AecReport {
    pub partitions: usize,
    pub blocks: usize,
    /// Blocks where the echo estimate explains at least half the mic's energy.
    pub echo_blocks: usize,
    /// Blocks where the mic carries speech and the estimate almost nothing.
    pub near_blocks: usize,
    /// Echo estimate level against the reference, where the reference plays.
    pub erl_db: Option<f64>,
    /// Mic → linear output, per echo block: the median. Without ground truth
    /// a block where the user talks over the echo reads low by construction —
    /// their voice is supposed to stay — so a median over blocks, not a ratio
    /// of sums, is what reflects the echo-only majority of a real take.
    pub erle_linear_db: Option<f64>,
    /// Mic → final output, per echo block: the median...
    pub erle_db: Option<f64>,
    /// ...and the 90th percentile, what the echo-only blocks alone reach.
    pub erle_p90_db: Option<f64>,
    /// Final output against the mic over near blocks: ~0 dB means the user's
    /// own voice went through untouched.
    pub near_change_db: Option<f64>,
    pub leak_median: Option<f64>,
}

pub struct AecOutput {
    pub linear: Vec<f32>,
    pub cleaned: Vec<f32>,
    pub echo: Vec<f32>,
    pub report: AecReport,
}

/// Cancel `reference` — already aligned by [`align_reference`] — out of `mic`.
pub fn cancel(mic: &[f32], reference: &[f32], rate: u32, cfg: &AecConfig) -> AecOutput {
    let b = cfg.block.max(16);
    let p = ((cfg.tail_ms / 1000.0 * rate as f64) / b as f64).ceil().max(1.0) as usize;
    let len = mic.len();
    let blocks = len.div_ceil(b);
    let get = |v: &[f32], i: usize| if i < v.len() { v[i] as f64 } else { 0.0 };
    let mut mdf = Mdf::new(b, p, rate as f64);
    // Whole-take buffers are f32 — an hour of them in f64 is gigabytes — while
    // every block is computed in f64.
    let mut e = vec![0.0f32; blocks * b];
    let mut y = vec![0.0f32; blocks * b];
    let mut leak = vec![0.0f64; blocks];
    let (mut d_blk, mut x_blk) = (vec![0.0; b], vec![0.0; b]);
    let (mut e_blk, mut y_blk) = (vec![0.0; b], vec![0.0; b]);
    for pass in 0..cfg.passes.max(1) {
        if pass > 0 {
            mdf.reset_history();
        }
        for blk in 0..blocks {
            for i in 0..b {
                d_blk[i] = get(mic, blk * b + i);
                x_blk[i] = get(reference, blk * b + i);
            }
            mdf.process(&d_blk, &x_blk, &mut e_blk, &mut y_blk);
            for i in 0..b {
                e[blk * b + i] = e_blk[i] as f32;
                y[blk * b + i] = y_blk[i] as f32;
            }
            leak[blk] = mdf.leak;
        }
    }
    e.truncate(len);
    y.truncate(len);
    let cleaned = if cfg.suppress { suppress(&e, &y, &leak, b, cfg) } else { e.clone() };

    // Levels per block, for a report that needs no ground truth.
    let mut r = AecReport { partitions: p, blocks, ..Default::default() };
    let (mut erl_y, mut erl_x) = (0.0, 0.0);
    let (mut per_block_linear, mut per_block) = (Vec::new(), Vec::new());
    let (mut near_d, mut near_c) = (0.0, 0.0);
    let level = |db: f64| 10f64.powf(db / 10.0) * b as f64;
    for blk in 0..blocks {
        let range = blk * b..((blk + 1) * b).min(len);
        let sum = |v: &dyn Fn(usize) -> f64| range.clone().map(v).sum::<f64>();
        let sdd = sum(&|i| (mic[i] as f64).powi(2));
        let sxx = sum(&|i| get(reference, i).powi(2));
        let syy = sum(&|i| (y[i] as f64).powi(2));
        let see = sum(&|i| (e[i] as f64).powi(2));
        let scc = sum(&|i| (cleaned[i] as f64).powi(2));
        if sxx > level(-60.0) {
            erl_y += syy;
            erl_x += sxx;
        }
        if syy >= 0.5 * sdd && sdd > level(-65.0) {
            r.echo_blocks += 1;
            per_block_linear.extend(db_ratio(sdd, see.max(1e-20)));
            per_block.extend(db_ratio(sdd, scc.max(1e-20)));
        }
        if syy < 0.01 * sdd && sdd > level(-55.0) {
            r.near_blocks += 1;
            near_d += sdd;
            near_c += scc;
        }
    }
    r.erl_db = db_ratio(erl_y, erl_x);
    r.erle_linear_db = median(&per_block_linear);
    r.erle_db = median(&per_block);
    r.erle_p90_db = percentile(&per_block, 0.9);
    r.near_change_db = db_ratio(near_c, near_d);
    r.leak_median = median(&leak);

    AecOutput { linear: e, cleaned, echo: y, report: r }
}

/// Stage 3: a decision-directed Wiener gain per bin, against the residual
/// echo the filter predicts it left (leak × echo estimate), held over a short
/// decay for the reverberation beyond the filter. Weighted overlap-add with
/// square-root Hann windows, so a gain of 1 everywhere returns `e` exactly.
fn suppress(e: &[f32], y: &[f32], leak: &[f64], block: usize, cfg: &AecConfig) -> Vec<f32> {
    const ALPHA: f64 = 0.9;
    const DECAY: f64 = 0.5;
    let l = 2 * block;
    let h = block;
    let fft = Fft::new(l);
    let win: Vec<f64> = (0..l)
        .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / l as f64).cos()).sqrt())
        .collect();
    let floor = 10f64.powf(cfg.floor_db / 20.0);
    let bins = l / 2 + 1;
    let (mut r_prev, mut post_prev) = (vec![0.0; bins], vec![0.0; bins]);
    let len = e.len();
    let mut out = vec![0.0f32; len];
    let (mut ef, mut yf) = (vec![C64::ZERO; l], vec![C64::ZERO; l]);
    for f in 0..len / h + 2 {
        let start = f as isize * h as isize - h as isize;
        for i in 0..l {
            let at = start + i as isize;
            let (ev, yv) = if at >= 0 && (at as usize) < len {
                (e[at as usize] as f64, y[at as usize] as f64)
            } else {
                (0.0, 0.0)
            };
            ef[i] = C64::new(ev * win[i], 0.0);
            yf[i] = C64::new(yv * win[i], 0.0);
        }
        fft.forward(&mut ef);
        fft.forward(&mut yf);
        let centre = ((start + h as isize).max(0) as usize / block).min(leak.len().saturating_sub(1));
        let lk = leak.get(centre).copied().unwrap_or(0.0);
        for k in 0..bins {
            let ee = ef[k].norm_sqr();
            let r = (cfg.overdrive * lk * yf[k].norm_sqr()).max(DECAY * r_prev[k]);
            r_prev[k] = r;
            let g = if r <= 1e-30 {
                1.0
            } else {
                let gamma = ee / r;
                let xi = ALPHA * post_prev[k] / r + (1.0 - ALPHA) * (gamma - 1.0).max(0.0);
                (xi / (1.0 + xi)).max(floor)
            };
            post_prev[k] = g * g * ee;
            ef[k] = ef[k].scale(g);
            if k != 0 && k != l / 2 {
                ef[l - k] = ef[l - k].scale(g);
            }
        }
        fft.inverse(&mut ef);
        for i in 0..l {
            let at = start + i as isize;
            if at >= 0 && (at as usize) < len {
                out[at as usize] += (ef[i].re * win[i]) as f32;
            }
        }
    }
    out
}

/// How [`cancel_threaded`] spreads a long take over threads.
#[derive(Clone, Debug)]
pub struct Threading {
    /// A take no longer than this is cancelled whole, exactly as [`cancel`]
    /// cancels it.
    pub whole_up_to_s: f64,
    /// Far-end signal a longer take's filter converges over before it is split.
    pub converge_s: f64,
    /// The shortest range one thread is given.
    pub min_range_s: f64,
    /// How far ahead of its range each thread after the first starts, so the
    /// filter's history and stage 3 have settled by then.
    pub lead_s: f64,
    pub threads: usize,
}

impl Default for Threading {
    fn default() -> Self {
        Self {
            whole_up_to_s: 600.0,
            converge_s: 180.0,
            min_range_s: 180.0,
            lead_s: 20.0,
            threads: crate::default_threads(),
        }
    }
}

/// What the app's echo pass keeps of a cancelled take.
pub struct Cancelled {
    /// The mic after the canceller and stage 3, as a 16-bit WAV holds it.
    pub cleaned: Vec<i16>,
    /// The linear canceller's output's energy per 10 ms frame, the last one
    /// partial.
    pub linear_energy: Vec<f64>,
    pub report: AecReport,
}

/// [`cancel`] for the app's echo pass, whose takes can run for hours: the
/// reference is read out of `sys` through `map` as it is needed, stage 3 and
/// the report run block by block, and nothing but [`Cancelled`] is kept for
/// the whole take, which is split over threads as `threading` says. Always two
/// passes and stage 3, whatever `cfg.passes` and `cfg.suppress` say.
pub fn cancel_threaded<S: Sample>(
    mic: &[S],
    sys: &[S],
    map: &DelayMap,
    rate: u32,
    cfg: &AecConfig,
    threading: &Threading,
) -> Cancelled {
    let b = cfg.block.max(16);
    let rate_f = rate as f64;
    let p = ((cfg.tail_ms / 1000.0 * rate_f) / b as f64).ceil().max(1.0) as usize;
    let frame = (rate as usize / 100).max(1);
    let take = Take { mic, sys, map, rate: rate_f, margin_ms: cfg.margin_ms, b, frame };
    let blocks = take.blocks();
    let (window, threads) = if blocks <= (threading.whole_up_to_s * rate_f) as usize / b {
        (0..blocks, 1)
    } else {
        let need = (threading.converge_s * rate_f) as usize / b;
        let min_blocks = ((threading.min_range_s * rate_f) as usize / b).max(1);
        (
            converge_window(|blk| take.far_energy(blk), blocks, need, block_level(FAR_DB, b)),
            threading.threads.max(1).min((blocks / min_blocks).max(1)),
        )
    };
    let mut converged = Mdf::new(b, p, rate_f);
    let (mut d, mut x, mut e, mut y) = (vec![0.0; b], vec![0.0; b], vec![0.0; b], vec![0.0; b]);
    for blk in window {
        take.fill(blk, &mut d, &mut x);
        converged.process(&d, &x, &mut e, &mut y);
    }
    converged.reset_history();

    // Ranges start where a block and a 10 ms frame both do, so no frame's
    // energy is split between two threads.
    let align = frame / gcd(b, frame);
    let per = blocks.div_ceil(threads).div_ceil(align).max(1) * align;
    let lead = (threading.lead_s * rate_f) as usize / b;
    let mut cleaned = vec![0i16; mic.len()];
    let mut linear_energy = vec![0.0; mic.len().div_ceil(frame)];
    let tally = std::thread::scope(|scope| {
        let ranges: Vec<_> = cleaned
            .chunks_mut(per * b)
            .zip(linear_energy.chunks_mut(per * b / frame))
            .enumerate()
            .map(|(i, (cleaned, energy))| {
                let blocks = i * per..((i + 1) * per).min(blocks);
                let from = if i == 0 { 0 } else { blocks.start.saturating_sub(lead) };
                let share = Share { from, blocks, cleaned, energy };
                let (take, converged) = (&take, &converged);
                scope.spawn(move || cancel_range(take, converged.clone(), cfg, share))
            })
            .collect();
        ranges.into_iter().map(|r| r.join().expect("a canceller thread")).reduce(Tally::merge)
    });
    let report = tally.unwrap_or_else(|| Tally::new(b)).report(p, blocks);
    Cancelled { cleaned, linear_energy, report }
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Where a long take's filter converges: its last `need` blocks of far-end
/// signal (`far_energy` above `level`), ending on the last of them. Over a
/// silent stretch after it, the filter's far-end power estimate would decay
/// while its coefficients stayed, and every range starts from this filter.
fn converge_window(far_energy: impl Fn(usize) -> f64, blocks: usize, need: usize, level: f64) -> Range<usize> {
    let mut last = blocks;
    while last > 0 && far_energy(last - 1) <= level {
        last -= 1;
    }
    let (mut first, mut active) = (last, 0);
    while first > 0 && active < need {
        first -= 1;
        if far_energy(first) > level {
            active += 1;
        }
    }
    first..last
}

/// A take as the canceller reads it, a block at a time.
struct Take<'a, S> {
    mic: &'a [S],
    sys: &'a [S],
    map: &'a DelayMap,
    rate: f64,
    margin_ms: f64,
    /// Samples per block.
    b: usize,
    /// Samples per 10 ms frame.
    frame: usize,
}

impl<S: Sample> Take<'_, S> {
    fn blocks(&self) -> usize {
        self.mic.len().div_ceil(self.b)
    }

    /// How many of block `blk`'s samples lie inside the take.
    fn block_len(&self, blk: usize) -> usize {
        ((blk + 1) * self.b).min(self.mic.len()).saturating_sub(blk * self.b)
    }

    /// Sample `n` of [`align_reference`]'s stream.
    fn reference(&self, n: usize) -> f64 {
        let back = ((self.map.lag_ms_at(n as f64 / self.rate) - self.margin_ms) / 1000.0 * self.rate).round() as isize;
        let at = n as isize - back;
        if at < 0 || at as usize >= self.sys.len() {
            0.0
        } else {
            self.sys[at as usize].f()
        }
    }

    /// Block `blk` of the mic and of the reference, zero past the end of the
    /// take.
    fn fill(&self, blk: usize, d: &mut [f64], x: &mut [f64]) {
        for i in 0..self.b {
            let n = blk * self.b + i;
            (d[i], x[i]) = if n < self.mic.len() { (self.mic[n].f(), self.reference(n)) } else { (0.0, 0.0) };
        }
    }

    fn far_energy(&self, blk: usize) -> f64 {
        (blk * self.b..blk * self.b + self.block_len(blk)).map(|n| self.reference(n).powi(2)).sum()
    }
}

/// One thread's part of a take.
struct Share<'a> {
    /// Where the thread starts, ahead of the blocks it keeps.
    from: usize,
    blocks: Range<usize>,
    /// The samples and 10 ms frames `blocks` cover.
    cleaned: &'a mut [i16],
    energy: &'a mut [f64],
}

/// `share` of the take, cancelled by `mdf` from `share.from` on.
fn cancel_range<S: Sample>(take: &Take<'_, S>, mut mdf: Mdf, cfg: &AecConfig, share: Share<'_>) -> Tally {
    let Share { from, blocks, cleaned, energy } = share;
    let b = take.b;
    let first_sample = blocks.start * b;
    let first_frame = first_sample / take.frame;
    let mut stage = Suppressor::new(b, cfg);
    let mut tally = Tally::new(b);
    let (mut d, mut x, mut e, mut y) = (vec![0.0; b], vec![0.0; b], vec![0.0; b], vec![0.0; b]);
    let (mut e32, mut y32) = (vec![0.0f32; b], vec![0.0f32; b]);
    // Stage 3 finishes a block only once the next one is in, so the range runs
    // a block past its end, and each block waits here for its output.
    let mut pending: Option<Pending> = None;
    let end = (blocks.end + 1).min(take.blocks());
    for blk in from..end {
        take.fill(blk, &mut d, &mut x);
        mdf.process(&d, &x, &mut e, &mut y);
        let n = take.block_len(blk);
        for i in 0..b {
            // As `cancel` keeps them: f32, and nothing past the end of the take.
            (e32[i], y32[i]) = if i < n { (e[i] as f32, y[i] as f32) } else { (0.0, 0.0) };
        }
        let done = stage.push(&e32, &y32, mdf.leak);
        if let Some(p) = pending.take() {
            p.finish(done, cleaned, &mut tally);
        }
        if blocks.contains(&blk) {
            let mut block = Block { leak: mdf.leak, ..Block::default() };
            for i in 0..n {
                block.mic += d[i].powi(2);
                block.reference += x[i].powi(2);
                block.echo += (y32[i] as f64).powi(2);
                block.linear += (e32[i] as f64).powi(2);
                energy[(blk * b + i) / take.frame - first_frame] += e32[i] as f64 * e32[i] as f64;
            }
            pending = Some(Pending { at: blk * b - first_sample, n, block });
        }
    }
    if end == take.blocks() {
        // One block past the take's last, which finishes it.
        let zeros = vec![0.0f32; b];
        let done = stage.push(&zeros, &zeros, mdf.leak);
        if let Some(p) = pending.take() {
            p.finish(done, cleaned, &mut tally);
        }
    }
    tally
}

/// A block the filter is done with, waiting for stage 3's output.
struct Pending {
    /// Where it starts in its range's samples.
    at: usize,
    /// How many of its samples lie inside the take.
    n: usize,
    block: Block,
}

impl Pending {
    /// `done` is the block through stage 3.
    fn finish(mut self, done: &[f32], cleaned: &mut [i16], tally: &mut Tally) {
        for i in 0..self.n {
            self.block.cleaned += (done[i] as f64).powi(2);
            cleaned[self.at + i] = pcm16(done[i]);
        }
        tally.add(&self.block);
    }
}

/// [`suppress`] a block at a time, with its arithmetic in its order, so that a
/// short take comes out of [`cancel_threaded`] exactly as out of [`cancel`]. A
/// frame spans two blocks, so a block comes out finished once the next is in.
struct Suppressor {
    b: usize,
    fft: Fft,
    win: Vec<f64>,
    floor: f64,
    overdrive: f64,
    r_prev: Vec<f64>,
    post_prev: Vec<f64>,
    e_hist: Vec<f32>,
    y_hist: Vec<f32>,
    acc: Vec<f32>,
    done: Vec<f32>,
    ef: Vec<C64>,
    yf: Vec<C64>,
}

impl Suppressor {
    fn new(b: usize, cfg: &AecConfig) -> Self {
        let l = 2 * b;
        Self {
            b,
            fft: Fft::new(l),
            win: (0..l)
                .map(|i| (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / l as f64).cos()).sqrt())
                .collect(),
            floor: 10f64.powf(cfg.floor_db / 20.0),
            overdrive: cfg.overdrive,
            r_prev: vec![0.0; b + 1],
            post_prev: vec![0.0; b + 1],
            e_hist: vec![0.0; l],
            y_hist: vec![0.0; l],
            acc: vec![0.0; l],
            done: vec![0.0; b],
            ef: vec![C64::ZERO; l],
            yf: vec![C64::ZERO; l],
        }
    }

    /// The next block of the linear output and of the echo estimate, and the
    /// filter's leak after it. Returns the block before it, finished.
    fn push(&mut self, e: &[f32], y: &[f32], leak: f64) -> &[f32] {
        const ALPHA: f64 = 0.9;
        const DECAY: f64 = 0.5;
        let (b, l) = (self.b, 2 * self.b);
        self.e_hist.copy_within(b..l, 0);
        self.y_hist.copy_within(b..l, 0);
        self.e_hist[b..].copy_from_slice(e);
        self.y_hist[b..].copy_from_slice(y);
        for i in 0..l {
            self.ef[i] = C64::new(self.e_hist[i] as f64 * self.win[i], 0.0);
            self.yf[i] = C64::new(self.y_hist[i] as f64 * self.win[i], 0.0);
        }
        self.fft.forward(&mut self.ef);
        self.fft.forward(&mut self.yf);
        for k in 0..=b {
            let ee = self.ef[k].norm_sqr();
            let r = (self.overdrive * leak * self.yf[k].norm_sqr()).max(DECAY * self.r_prev[k]);
            self.r_prev[k] = r;
            let g = if r <= 1e-30 {
                1.0
            } else {
                let gamma = ee / r;
                let xi = ALPHA * self.post_prev[k] / r + (1.0 - ALPHA) * (gamma - 1.0).max(0.0);
                (xi / (1.0 + xi)).max(self.floor)
            };
            self.post_prev[k] = g * g * ee;
            self.ef[k] = self.ef[k].scale(g);
            if k != 0 && k != l / 2 {
                self.ef[l - k] = self.ef[l - k].scale(g);
            }
        }
        self.fft.inverse(&mut self.ef);
        for i in 0..l {
            self.acc[i] += (self.ef[i].re * self.win[i]) as f32;
        }
        self.done.copy_from_slice(&self.acc[..b]);
        self.acc.copy_within(b..l, 0);
        self.acc[b..].iter_mut().for_each(|v| *v = 0.0);
        &self.done
    }
}

/// The report counts a block as having far-end signal above this level, and a
/// long take's filter converges only on blocks that do.
const FAR_DB: f64 = -60.0;

/// A per-sample level, in dB, as a block's energy.
fn block_level(db: f64, b: usize) -> f64 {
    10f64.powf(db / 10.0) * b as f64
}

/// One block's energy in each signal, and the filter's leak after it.
#[derive(Default)]
struct Block {
    mic: f64,
    reference: f64,
    echo: f64,
    linear: f64,
    cleaned: f64,
    leak: f64,
}

/// The report's running figures, and what a block must reach to count towards
/// them. Threads keep their own and merge them in order.
struct Tally {
    far_level: f64,
    echo_level: f64,
    near_level: f64,
    erl_y: f64,
    erl_x: f64,
    near_d: f64,
    near_c: f64,
    echo_blocks: usize,
    near_blocks: usize,
    per_block_linear: Vec<f64>,
    per_block: Vec<f64>,
    leak: Vec<f64>,
}

impl Tally {
    fn new(b: usize) -> Self {
        Self {
            far_level: block_level(FAR_DB, b),
            echo_level: block_level(-65.0, b),
            near_level: block_level(-55.0, b),
            erl_y: 0.0,
            erl_x: 0.0,
            near_d: 0.0,
            near_c: 0.0,
            echo_blocks: 0,
            near_blocks: 0,
            per_block_linear: Vec::new(),
            per_block: Vec::new(),
            leak: Vec::new(),
        }
    }

    fn add(&mut self, block: &Block) {
        if block.reference > self.far_level {
            self.erl_y += block.echo;
            self.erl_x += block.reference;
        }
        if block.echo >= 0.5 * block.mic && block.mic > self.echo_level {
            self.echo_blocks += 1;
            self.per_block_linear.extend(db_ratio(block.mic, block.linear.max(1e-20)));
            self.per_block.extend(db_ratio(block.mic, block.cleaned.max(1e-20)));
        }
        if block.echo < 0.01 * block.mic && block.mic > self.near_level {
            self.near_blocks += 1;
            self.near_d += block.mic;
            self.near_c += block.cleaned;
        }
        self.leak.push(block.leak);
    }

    fn merge(mut self, next: Tally) -> Tally {
        self.erl_y += next.erl_y;
        self.erl_x += next.erl_x;
        self.near_d += next.near_d;
        self.near_c += next.near_c;
        self.echo_blocks += next.echo_blocks;
        self.near_blocks += next.near_blocks;
        self.per_block_linear.extend(next.per_block_linear);
        self.per_block.extend(next.per_block);
        self.leak.extend(next.leak);
        self
    }

    fn report(self, partitions: usize, blocks: usize) -> AecReport {
        AecReport {
            partitions,
            blocks,
            echo_blocks: self.echo_blocks,
            near_blocks: self.near_blocks,
            erl_db: db_ratio(self.erl_y, self.erl_x),
            erle_linear_db: median(&self.per_block_linear),
            erle_db: median(&self.per_block),
            erle_p90_db: percentile(&self.per_block, 0.9),
            near_change_db: db_ratio(self.near_c, self.near_d),
            leak_median: median(&self.leak),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delay::{lag_track, refine_steps, summarize, DelayConfig};
    use crate::synth::{masked_energy, scenario, Scenario, ScenarioConfig};

    struct Scores {
        /// Echo removed where only the echo sounds, against the truth.
        erle_linear: f64,
        erle: f64,
        /// The user's voice through, where only it sounds.
        near_change: f64,
        /// The user's voice against everything else, where both sound.
        dt_sdr_linear: f64,
        dt_sdr: f64,
    }

    impl Scores {
        fn line(&self) -> String {
            format!(
                "ERLE {:.1} dB linear / {:.1} dB suppressed, user alone {:+.2} dB, double-talk SDR {:.1} / {:.1} dB",
                self.erle_linear, self.erle, self.near_change, self.dt_sdr_linear, self.dt_sdr
            )
        }
    }

    fn run(cfg: &ScenarioConfig, aec: &AecConfig) -> (Scores, AecReport) {
        let s = scenario(cfg);
        let dc = DelayConfig::default();
        let mut summary = summarize(&lag_track(&s.mic, &s.sys, s.rate, &dc), &dc);
        refine_steps(&s.mic, &s.sys, s.rate, &mut summary, &dc);
        let map = DelayMap::from_summary(&summary).expect("an echo to align on");
        let reference = align_reference(&s.sys, s.mic.len(), s.rate, &map, aec.margin_ms);
        let out = cancel(&s.mic, &reference, s.rate, aec);

        // Score from 10 s on: the first pass converges, but a score over the
        // opening would still be measuring convergence rather than the filter.
        let skip = |m: Vec<bool>| -> Vec<bool> {
            m.into_iter().enumerate().map(|(i, v)| v && i >= 1000).collect()
        };
        let far = skip(s.far_only());
        let near = skip(s.near_only());
        let dt = skip(s.double_talk());
        let diff = |a: &[f32], b: &[f32]| -> Vec<f32> { a.iter().zip(b).map(|(x, y)| x - y).collect() };
        let db = |num: f64, den: f64| 10.0 * (num / den).log10();
        let scores = Scores {
            erle_linear: db(masked_energy(&s.echo, &far), masked_energy(&out.linear, &far)),
            erle: db(masked_energy(&s.echo, &far), masked_energy(&out.cleaned, &far)),
            near_change: db(masked_energy(&out.cleaned, &near), masked_energy(&s.mic, &near)),
            dt_sdr_linear: db(masked_energy(&s.near, &dt), masked_energy(&diff(&out.linear, &s.near), &dt)),
            dt_sdr: db(masked_energy(&s.near, &dt), masked_energy(&diff(&out.cleaned, &s.near), &dt)),
        };
        (scores, out.report)
    }

    // Thresholds sit several dB under what this filter measures on these
    // takes, so a regression fails and a last-bit libm difference doesn't.
    // Synthetic echo is linear and time-invariant: these figures bound what the
    // code can do, not what a real room will give.

    #[test]
    fn cancels_a_multi_voice_echo_and_keeps_the_user() {
        let (s, report) = run(&ScenarioConfig { polarity: -1.0, ..Default::default() }, &AecConfig::default());
        eprintln!("inverted, two remote voices: {} | {report:?}", s.line());
        assert!(s.erle_linear > 35.0, "linear ERLE {:.1} dB", s.erle_linear);
        assert!(s.erle > 40.0, "ERLE {:.1} dB", s.erle);
        assert!(s.near_change.abs() < 0.5, "user alone {:+.2} dB", s.near_change);
        assert!(s.dt_sdr_linear > 30.0, "double-talk SDR {:.1} dB", s.dt_sdr_linear);
        assert!(s.dt_sdr > 15.0, "double-talk SDR after suppression {:.1} dB", s.dt_sdr);
    }

    #[test]
    fn double_talk_never_reaches_the_output() {
        // Before the two-path scheme, one of these seeds let a stretch of the
        // user's voice pull the filter off and cost 20 dB of it.
        for seed in 1..=3u64 {
            let (s, _) = run(&ScenarioConfig { seed, ..Default::default() }, &AecConfig::default());
            eprintln!("seed {seed}: {}", s.line());
            assert!(s.dt_sdr_linear > 30.0, "seed {seed}: double-talk SDR {:.1} dB", s.dt_sdr_linear);
            assert!(s.erle > 40.0, "seed {seed}: ERLE {:.1} dB", s.erle);
        }
    }

    #[test]
    fn a_long_offset_and_a_gap_are_absorbed_by_the_delay_map() {
        let (s, _) = run(
            &ScenarioConfig { lag_ms: 420.0, gap: Some((50.0, 12.0)), dur_s: 120.0, ..Default::default() },
            &AecConfig::default(),
        );
        eprintln!("420 ms + 12 ms gap: {}", s.line());
        assert!(s.erle > 38.0, "ERLE {:.1} dB", s.erle);
        assert!(s.dt_sdr_linear > 30.0, "double-talk SDR {:.1} dB", s.dt_sdr_linear);
    }

    #[test]
    fn a_driven_speaker_still_loses_most_of_its_echo() {
        // Soft-clipped: no linear filter can model it all, so this is where
        // residual suppression earns its place.
        let (s, _) = run(&ScenarioConfig { nonlinear: true, ..Default::default() }, &AecConfig::default());
        eprintln!("soft-clipped speaker: {}", s.line());
        assert!(s.erle_linear > 12.0, "linear ERLE {:.1} dB", s.erle_linear);
        assert!(s.erle > 20.0, "ERLE {:.1} dB", s.erle);
        assert!(s.near_change.abs() < 1.0, "user alone {:+.2} dB", s.near_change);
    }

    fn map_of(mic: &[f32], sys: &[f32], rate: u32) -> DelayMap {
        let dc = DelayConfig::default();
        let mut summary = summarize(&lag_track(mic, sys, rate, &dc), &dc);
        refine_steps(mic, sys, rate, &mut summary, &dc);
        DelayMap::from_summary(&summary).expect("an echo to align on")
    }

    /// The energy of each 10 ms frame of `x`, the last one partial, as the
    /// app's echo pass takes it.
    fn frame_energy(x: &[f32]) -> Vec<u64> {
        x.chunks(160).map(|c| c.iter().map(|&v| v as f64 * v as f64).sum::<f64>().to_bits()).collect()
    }

    fn two_passes_then_threaded<S: Sample>(mic: &[S], sys: &[S], threading: &Threading) {
        let rate = 16_000;
        let aec = AecConfig::default();
        let decode = |x: &[S]| x.iter().map(|&v| v.f() as f32).collect::<Vec<f32>>();
        let (mic_f, sys_f) = (decode(mic), decode(sys));
        let map = map_of(&mic_f, &sys_f, rate);
        let reference = align_reference(&sys_f, mic_f.len(), rate, &map, aec.margin_ms);
        let whole = cancel(&mic_f, &reference, rate, &aec);
        let threaded = cancel_threaded(mic, sys, &map, rate, &aec, threading);
        let cleaned: Vec<i16> = whole.cleaned.iter().map(|&v| pcm16(v)).collect();
        assert!(threaded.cleaned == cleaned, "the cancelled mic differs");
        let energy: Vec<u64> = threaded.linear_energy.iter().map(|v| v.to_bits()).collect();
        assert!(energy == frame_energy(&whole.linear), "the linear output's energy differs");
        assert_eq!(format!("{:?}", threaded.report), format!("{:?}", whole.report));
    }

    #[test]
    fn a_short_take_is_cancelled_exactly_as_two_whole_passes_are() {
        let s = scenario(&ScenarioConfig { polarity: -1.0, gap: Some((50.0, 12.0)), ..Default::default() });
        // Short of a whole block and a whole 10 ms frame, as a real take is.
        let len = s.mic.len() - 77;
        let (mic, sys) = (&s.mic[..len], &s.sys[..len]);
        two_passes_then_threaded(mic, sys, &Threading { threads: 8, ..Threading::default() });
        let pcm = |x: &[f32]| x.iter().map(|&v| pcm16(v)).collect::<Vec<i16>>();
        two_passes_then_threaded(&pcm(mic), &pcm(sys), &Threading::default());
        // A take as long as `whole_up_to_s` is still short.
        let limit = (SPLIT.whole_up_to_s * 16_000.0) as usize;
        two_passes_then_threaded(&s.mic[..limit], &s.sys[..limit], &SPLIT);
    }

    /// Echo removed where only the echo sounds, against the truth, and how far
    /// the user's voice moved where only it sounds, in dB.
    #[derive(Debug)]
    struct Removed {
        suppressed: f64,
        /// From the linear canceller's energy alone.
        linear: f64,
        user: f64,
    }

    fn removed(s: &Scenario, cleaned: &[f32], linear_energy: &[f64], (from_s, to_s): (f64, f64)) -> Removed {
        let within = |m: Vec<bool>| -> Vec<bool> {
            m.into_iter().enumerate().map(|(f, v)| v && (from_s * 100.0..to_s * 100.0).contains(&(f as f64))).collect()
        };
        let (far, near) = (within(s.far_only()), within(s.near_only()));
        let db = |num: f64, den: f64| 10.0 * (num / den).log10();
        let linear: f64 = far.iter().zip(linear_energy).filter(|(f, _)| **f).map(|(_, e)| e).sum();
        Removed {
            suppressed: db(masked_energy(&s.echo, &far), masked_energy(cleaned, &far)),
            linear: db(masked_energy(&s.echo, &far), linear),
            user: db(masked_energy(cleaned, &near), masked_energy(&s.mic, &near)),
        }
    }

    impl Removed {
        fn as_much_as(&self, whole: &Removed) -> bool {
            self.suppressed > whole.suppressed - 1.0 && self.linear > whole.linear - 1.0
        }
    }

    /// Splits a take of minutes the way the app splits one of hours.
    const SPLIT: Threading = Threading { whole_up_to_s: 60.0, converge_s: 60.0, min_range_s: 50.0, lead_s: 20.0, threads: 4 };

    /// `s` cancelled by two whole passes, then in [`SPLIT`]'s ranges: each as
    /// the cleaned mic and the linear output's energy per frame.
    fn both_ways(s: &Scenario) -> [(Vec<f32>, Vec<f64>); 2] {
        let aec = AecConfig::default();
        let map = map_of(&s.mic, &s.sys, s.rate);
        let reference = align_reference(&s.sys, s.mic.len(), s.rate, &map, aec.margin_ms);
        let whole = cancel(&s.mic, &reference, s.rate, &aec);
        let whole_energy = frame_energy(&whole.linear).into_iter().map(f64::from_bits).collect();
        let split = cancel_threaded(&s.mic, &s.sys, &map, s.rate, &aec, &SPLIT);
        let cleaned = split.cleaned.iter().map(|&v| v.f() as f32).collect();
        [(whole.cleaned, whole_energy), (cleaned, split.linear_energy)]
    }

    #[test]
    fn a_longer_take_cancelled_in_ranges_on_threads_loses_as_much_echo() {
        for cfg in [
            ScenarioConfig { polarity: -1.0, dur_s: 240.0, ..Default::default() },
            ScenarioConfig { nonlinear: true, dur_s: 240.0, ..Default::default() },
        ] {
            let s = scenario(&cfg);
            let [whole, split] = both_ways(&s);
            // Every 60 s a range starts from the converged filter, so its
            // opening seconds should lose the echo as the rest do.
            for window in [(10.0, f64::INFINITY), (60.0, 70.0), (120.0, 130.0), (180.0, 190.0)] {
                let a = removed(&s, &whole.0, &whole.1, window);
                let b = removed(&s, &split.0, &split.1, window);
                assert!(b.as_much_as(&a), "{cfg:?} {window:?}: {b:.2?} against {a:.2?}");
            }
            let user = removed(&s, &split.0, &split.1, (10.0, f64::INFINITY)).user;
            assert!(user.abs() < 0.5, "the user alone {user:+.2} dB");
        }
    }

    #[test]
    fn a_take_that_carries_on_in_silence_after_the_call() {
        let mut s = scenario(&ScenarioConfig { polarity: -1.0, dur_s: 240.0, ..Default::default() });
        let tail = 120 * 16_000;
        for x in [&mut s.mic, &mut s.sys, &mut s.echo, &mut s.near] {
            x.extend(std::iter::repeat_n(0.0, tail));
        }
        for m in [&mut s.echo_active, &mut s.near_active] {
            m.extend(std::iter::repeat_n(false, tail / 160));
        }
        let [whole, split] = both_ways(&s);
        let after_10s = (10.0, f64::INFINITY);
        let (a, b) = (removed(&s, &whole.0, &whole.1, after_10s), removed(&s, &split.0, &split.1, after_10s));
        assert!(b.as_much_as(&a), "{b:.2?} against {a:.2?}");
        assert!(b.user.abs() < 0.5, "the user alone {:+.2} dB", b.user);
        // A range starts inside the silence at 270 s.
        let silent = split.0.len() - tail + 16_000;
        assert!(split.0[silent..].iter().all(|&v| v == 0.0), "the silence stays silent");
    }

    #[test]
    fn the_convergence_window_ends_on_the_last_far_end_block() {
        // Far end in blocks 0..100 and 150..300, silent after.
        let energy = |blk: usize| if blk < 100 || (150..300).contains(&blk) { 1.0 } else { 0.0 };
        assert_eq!(converge_window(energy, 400, 120, 0.5), 180..300);
        // Silent blocks inside the window don't count towards it.
        assert_eq!(converge_window(energy, 400, 200, 0.5), 50..300);
        // Less far end than it asks for: all of it.
        assert_eq!(converge_window(energy, 400, 1_000, 0.5), 0..300);
    }

    #[test]
    fn suppression_with_unit_gain_is_transparent() {
        let e: Vec<f32> = (0..10_000).map(|i| ((i * 31) % 97) as f32 / 97.0 - 0.5).collect();
        let y = vec![0.0; e.len()];
        let out = suppress(&e, &y, &vec![0.1; 40], 256, &AecConfig::default());
        for (a, b) in out.iter().zip(&e) {
            assert!((a - b).abs() < 1e-6);
        }
    }
}
