//! How far a signal lags a reference, window by window: GCC-PHAT over the
//! whole take, reduced to a level, a drift and the steps between levels.
//!
//! For Humla the signal is the mic and the reference is the system stream, so
//! the lag is where the echo of the speakers lands in the mic *by WAV index*:
//! the capture's own start offset plus the output and acoustic path. A lag
//! that holds steady is a fixed offset; one that slopes is two clocks drifting;
//! one that steps is a stream that lost (or gained) frames.
//!
//! The peak is picked on |r|, not r: an echo path can invert polarity (a real
//! take measured a −0.28 waveform correlation at its lag), and the sign is
//! reported rather than assumed.

use crate::fft::{Fft, C64};
use crate::stats::{dbfs, mad, median, rms, theil_sen};

#[derive(Clone, Debug)]
pub struct DelayConfig {
    /// Reference window length. Rounded to whole samples.
    pub window_s: f64,
    pub hop_s: f64,
    /// Lags searched, both ways from zero.
    pub max_lag_ms: f64,
    /// Band the correlation is computed over. Laptop speakers carry little
    /// below ~150 Hz and a 16 kHz stream nothing above 8 kHz.
    pub band_hz: (f64, f64),
    /// PHAT weighting exponent: 1 whitens fully, 0 is plain correlation.
    /// Slightly below 1 keeps noise-only bins from getting full weight.
    pub phat_beta: f64,
    /// A window counts only when the reference carries this much level...
    pub min_ref_dbfs: f64,
    /// ...and its peak stands this far above the rest of the correlation.
    pub min_prominence: f64,
    /// A change of level bigger than this between neighbouring windows is a
    /// step rather than drift or noise.
    pub step_ms: f64,
}

impl Default for DelayConfig {
    fn default() -> Self {
        Self {
            window_s: 4.096,
            hop_s: 4.096,
            max_lag_ms: 2_000.0,
            band_hz: (150.0, 4_000.0),
            phat_beta: 0.85,
            min_ref_dbfs: -55.0,
            min_prominence: 8.0,
            step_ms: 2.0,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct LagPoint {
    /// Centre of the reference window, seconds into the take.
    pub t_s: f64,
    /// Positive: the signal lags the reference.
    pub lag_ms: f64,
    pub prominence: f64,
    /// Sign of the correlation at the peak: −1 for an inverting path.
    pub polarity: i8,
    pub ref_dbfs: f64,
    pub sig_dbfs: f64,
    pub valid: bool,
}

/// One window's correlation peak: (lag in samples, prominence, polarity).
struct Peak {
    lag: f64,
    prominence: f64,
    polarity: i8,
}

/// GCC-PHAT-β of `sig` against `reference`, searched over `-max_lag..=max_lag`
/// samples. `sig` must be `reference.len() + 2 * max_lag` long, starting
/// `max_lag` samples before the reference window, so every searched lag sees
/// the whole reference window overlap.
/// GCC-PHAT-β of `sig` against `reference` for lags `-max_lag..=max_lag`,
/// indexed `lag + max_lag`. `sig` starts `max_lag` samples before `reference`
/// and runs `max_lag` past its end.
fn gcc_corr(fft: &Fft, sig: &[f64], reference: &[f64], max_lag: usize, rate: f64, cfg: &DelayConfig) -> Vec<f64> {
    let n = fft.len();
    let s = fft.forward_real(sig);
    let r = fft.forward_real(reference);
    let (lo, hi) = cfg.band_hz;
    let mut g = vec![C64::ZERO; n];
    for k in 0..=n / 2 {
        let f = k as f64 * rate / n as f64;
        if f < lo || f > hi.min(rate / 2.0) {
            continue;
        }
        let cross = s[k] * r[k].conj();
        let mag = cross.abs();
        if mag < 1e-30 {
            continue;
        }
        let w = cross.scale(1.0 / mag.powf(cfg.phat_beta));
        g[k] = w;
        if k != 0 && k != n / 2 {
            g[n - k] = w.conj();
        }
    }
    fft.inverse(&mut g);
    // r[k] = Σ sig[m + k]·ref[m], and sig starts `max_lag` early, so lag = k − max_lag.
    (0..=2 * max_lag).map(|k| g[k].re).collect()
}

fn gcc_peak(fft: &Fft, sig: &[f64], reference: &[f64], max_lag: usize, rate: f64, cfg: &DelayConfig) -> Option<Peak> {
    let span = 2 * max_lag;
    let corr = gcc_corr(fft, sig, reference, max_lag, rate, cfg);
    let (best, _) = corr
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))?;
    let peak = corr[best].abs();
    if peak <= 0.0 {
        return None;
    }
    let polarity: i8 = if corr[best] < 0.0 { -1 } else { 1 };
    let guard = (0.002 * rate) as usize;
    let rest: Vec<f64> = corr
        .iter()
        .enumerate()
        .filter(|(k, _)| k.abs_diff(best) > guard)
        .map(|(_, v)| v * v)
        .collect();
    let floor = (rest.iter().sum::<f64>() / rest.len().max(1) as f64).sqrt();
    let prominence = if floor > 0.0 { peak / floor } else { f64::INFINITY };
    // Parabolic refinement on the peak's own sign.
    let mut frac = 0.0;
    if best > 0 && best < span {
        let p = polarity as f64;
        let (a, b, c) = (p * corr[best - 1], p * corr[best], p * corr[best + 1]);
        let denom = a - 2.0 * b + c;
        if denom.abs() > 1e-30 {
            frac = (0.5 * (a - c) / denom).clamp(-0.5, 0.5);
        }
    }
    Some(Peak {
        lag: best as f64 + frac - max_lag as f64,
        prominence,
        polarity,
    })
}

/// Lag of `sig` behind `reference`, per window across the whole take.
pub fn lag_track(sig: &[f32], reference: &[f32], rate: u32, cfg: &DelayConfig) -> Vec<LagPoint> {
    let rate_f = rate as f64;
    let window = (cfg.window_s * rate_f).round() as usize;
    let hop = ((cfg.hop_s * rate_f).round() as usize).max(1);
    let max_lag = (cfg.max_lag_ms / 1000.0 * rate_f).round() as usize;
    let len = sig.len().min(reference.len());
    if window == 0 || len < window {
        return Vec::new();
    }
    let fft = Fft::new((window + 2 * max_lag).next_power_of_two());
    let taper = tukey(window, 0.1);
    let mut out = Vec::new();
    let mut start = 0;
    while start + window <= len {
        let ref_win = &reference[start..start + window];
        let sig_win = &sig[start..start + window];
        let ref_db = dbfs(rms(ref_win));
        let sig_db = dbfs(rms(sig_win));
        let mut point = LagPoint {
            t_s: (start + window / 2) as f64 / rate_f,
            lag_ms: 0.0,
            prominence: 0.0,
            polarity: 0,
            ref_dbfs: ref_db,
            sig_dbfs: sig_db,
            valid: false,
        };
        if ref_db >= cfg.min_ref_dbfs {
            let (sig_f, reference_f) = window_pair(sig, reference, start, window, max_lag, &taper);
            if let Some(peak) = gcc_peak(&fft, &sig_f, &reference_f, max_lag, rate_f, cfg) {
                point.lag_ms = peak.lag / rate_f * 1000.0;
                point.prominence = peak.prominence;
                point.polarity = peak.polarity;
                point.valid = peak.prominence >= cfg.min_prominence;
            }
        }
        out.push(point);
        start += hop;
    }
    out
}

/// The reference window at `start` (tapered) and the signal around it, running
/// `max_lag` either side, zero past either end of the take.
fn window_pair(sig: &[f32], reference: &[f32], start: usize, window: usize, max_lag: usize, taper: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let reference_f = reference[start..start + window].iter().zip(taper).map(|(&v, &w)| v as f64 * w).collect();
    let sig_f = (0..window + 2 * max_lag)
        .map(|i| {
            let at = (start + i) as isize - max_lag as isize;
            if at < 0 || at as usize >= sig.len() {
                0.0
            } else {
                sig[at as usize] as f64
            }
        })
        .collect();
    (sig_f, reference_f)
}

/// A cosine-tapered window: flat in the middle, `edge` of its length tapered
/// at each end, so a window's hard edges don't smear across the correlation.
fn tukey(len: usize, edge: f64) -> Vec<f64> {
    let taper = ((len as f64) * edge / 2.0).round() as usize;
    (0..len)
        .map(|i| {
            let from_end = i.min(len - 1 - i);
            if from_end >= taper || taper == 0 {
                1.0
            } else {
                0.5 - 0.5 * (std::f64::consts::PI * from_end as f64 / taper as f64).cos()
            }
        })
        .collect()
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Step {
    /// Midway between the last window before the change and the first after.
    pub at_s: f64,
    pub from_ms: f64,
    pub to_ms: f64,
}

/// A stretch of the take between steps.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Segment {
    pub start_s: f64,
    pub end_s: f64,
    pub windows: usize,
    pub median_ms: f64,
    /// Theil–Sen slope, when the segment spans long enough to show one.
    pub drift_ppm: Option<f64>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct TrackSummary {
    pub windows: usize,
    pub valid: usize,
    /// Majority sign of the valid peaks; 0 with none.
    pub polarity: i8,
    pub median_ms: Option<f64>,
    pub spread_ms: Option<f64>,
    /// Drift over the longest segment.
    pub drift_ppm: Option<f64>,
    pub steps: Vec<Step>,
    pub segments: Vec<Segment>,
}

impl TrackSummary {
    /// The level at the start of the take — what the capture's start offset
    /// should be compared with.
    pub fn first_ms(&self) -> Option<f64> {
        self.segments.first().map(|s| s.median_ms)
    }

    pub fn last_ms(&self) -> Option<f64> {
        self.segments.last().map(|s| s.median_ms)
    }
}

pub fn summarize(points: &[LagPoint], cfg: &DelayConfig) -> TrackSummary {
    let valid: Vec<&LagPoint> = points.iter().filter(|p| p.valid).collect();
    let lags: Vec<f64> = valid.iter().map(|p| p.lag_ms).collect();
    let polarity_sum: i32 = valid.iter().map(|p| p.polarity as i32).sum();
    let polarity = polarity_sum.signum() as i8;

    // A step at i when the median of the few windows after it differs from the
    // median of the few before it — medians, so one bad window can't make one.
    const SIDE: usize = 3;
    let n = valid.len();
    let mut change = vec![0.0; n];
    for i in 2..n.saturating_sub(1) {
        let left = median(&lags[i.saturating_sub(SIDE)..i]).unwrap_or(0.0);
        let right = median(&lags[i..(i + SIDE).min(n)]).unwrap_or(0.0);
        change[i] = right - left;
    }
    // One step lights up a run of neighbouring candidates — a median of three
    // moves over two windows — so the run is resolved by where it best splits
    // the level before it from the level after it.
    let mut cuts = Vec::new();
    let mut i = 0;
    while i < n {
        if change[i].abs() > cfg.step_ms {
            let first = i;
            while i + 1 < n && change[i + 1].abs() > cfg.step_ms {
                i += 1;
            }
            let (lo, hi) = (first.saturating_sub(SIDE), (i + SIDE).min(n));
            let left = median(&lags[lo..first]).unwrap_or(lags[first]);
            let right = median(&lags[i..hi]).unwrap_or(lags[i]);
            let cost = |c: usize| -> f64 {
                lags[lo..c].iter().map(|v| (v - left).abs()).sum::<f64>()
                    + lags[c..hi].iter().map(|v| (v - right).abs()).sum::<f64>()
            };
            let best = (first.max(1)..=(i + 1).min(n - 1))
                .min_by(|&a, &b| cost(a).total_cmp(&cost(b)))
                .unwrap_or(first);
            cuts.push(best);
        }
        i += 1;
    }

    let mut segments = Vec::new();
    let mut steps = Vec::new();
    let mut from = 0;
    for &cut in cuts.iter().chain(std::iter::once(&n)) {
        if cut > from {
            let pts: Vec<(f64, f64)> = valid[from..cut].iter().map(|p| (p.t_s, p.lag_ms)).collect();
            let seg_lags: Vec<f64> = pts.iter().map(|p| p.1).collect();
            let span = pts.last().unwrap().0 - pts[0].0;
            let drift = if pts.len() >= 4 && span >= 20.0 {
                theil_sen(&pts).map(|(slope, _)| slope * 1000.0)
            } else {
                None
            };
            segments.push(Segment {
                start_s: pts[0].0,
                end_s: pts.last().unwrap().0,
                windows: pts.len(),
                median_ms: median(&seg_lags).unwrap_or(0.0),
                drift_ppm: drift,
            });
        }
        if cut < n && cut > 0 {
            steps.push(Step {
                at_s: (valid[cut - 1].t_s + valid[cut].t_s) / 2.0,
                from_ms: median(&lags[cut.saturating_sub(SIDE)..cut]).unwrap_or(0.0),
                to_ms: median(&lags[cut..(cut + SIDE).min(n)]).unwrap_or(0.0),
            });
        }
        from = cut;
    }
    let drift_ppm = segments
        .iter()
        .max_by(|a, b| (a.end_s - a.start_s).total_cmp(&(b.end_s - b.start_s)))
        .and_then(|s| s.drift_ppm);

    TrackSummary {
        windows: points.len(),
        valid: n,
        polarity,
        median_ms: median(&lags),
        spread_ms: mad(&lags),
        drift_ppm,
        steps,
        segments,
    }
}

/// Pin each step down to well under a window. Short windows are scanned
/// either side of it, and each asks how much stronger the correlation is at the
/// new level than at the old; the step goes where that balance changes sides,
/// every window weighted by how clearly it shows an echo at all. A window-long
/// uncertainty is seconds of misaligned reference for the canceller; this makes
/// it a fraction of one.
pub fn refine_steps(sig: &[f32], reference: &[f32], rate: u32, summary: &mut TrackSummary, cfg: &DelayConfig) {
    let rate_f = rate as f64;
    let len = sig.len().min(reference.len());
    let window = (1.024 * rate_f) as usize;
    let hop = (0.064 * rate_f) as usize;
    let taper = tukey(window, 0.1);
    let near = (0.0005 * rate_f).ceil() as usize;
    for i in 0..summary.steps.len() {
        // The step lies somewhere between the last window that saw the old
        // level and the first that saw the new one — each is a window wide.
        let (Some(before), Some(after)) = (summary.segments.get(i), summary.segments.get(i + 1)) else {
            continue;
        };
        let lo_s = before.end_s - cfg.window_s / 2.0 - 1.0;
        let hi_s = after.start_s + cfg.window_s / 2.0 + 1.0;
        let step = &mut summary.steps[i];
        let max_lag = ((step.from_ms.abs().max(step.to_ms.abs()) + 20.0) / 1000.0 * rate_f) as usize;
        let fft = Fft::new((window + 2 * max_lag).next_power_of_two());
        let first = ((lo_s.max(0.0)) * rate_f) as usize;
        let last = (((hi_s * rate_f) as usize).min(len)).saturating_sub(window);
        let at = |lag_ms: f64| ((lag_ms / 1000.0 * rate_f).round() as isize + max_lag as isize).max(0) as usize;
        let (from_k, to_k) = (at(step.from_ms), at(step.to_ms));
        let mut scored: Vec<(f64, f64, f64)> = Vec::new(); // (t, score, weight)
        let mut start = first;
        while start <= last {
            if dbfs(rms(&reference[start..start + window])) >= cfg.min_ref_dbfs {
                let (s_f, r_f) = window_pair(sig, reference, start, window, max_lag, &taper);
                let corr = gcc_corr(&fft, &s_f, &r_f, max_lag, rate_f, cfg);
                let peak_near = |k: usize| {
                    corr[k.saturating_sub(near)..(k + near + 1).min(corr.len())]
                        .iter()
                        .fold(0.0f64, |m, v| m.max(v.abs()))
                };
                let (a, b) = (peak_near(from_k), peak_near(to_k));
                let floor = (corr.iter().map(|v| v * v).sum::<f64>() / corr.len() as f64).sqrt();
                let clarity = a.max(b) / floor.max(1e-30);
                if clarity >= 4.0 {
                    scored.push(((start + window / 2) as f64 / rate_f, (b - a) / (a + b), clarity));
                }
            }
            start += hop;
        }
        // The cut with the least weight on its wrong side: new-level evidence
        // before it, old-level evidence after it.
        let cost = |s: usize| -> f64 {
            scored[..s].iter().map(|&(_, x, w)| w * x.max(0.0)).sum::<f64>()
                + scored[s..].iter().map(|&(_, x, w)| w * (-x).max(0.0)).sum::<f64>()
        };
        if let Some(s) = (1..scored.len()).min_by(|&a, &b| cost(a).total_cmp(&cost(b))) {
            step.at_s = (scored[s - 1].0 + scored[s].0) / 2.0;
        }
    }
}

/// Move each step onto a discontinuity the capture itself recorded, when one
/// lies within `within_s` and has the step's sign: a system-stream gap pushes
/// the lag up (sys content arrives early by index), a mic gap pulls it down.
/// `known` is `(seconds into the WAV, lag change in ms)`.
pub fn snap_steps(summary: &mut TrackSummary, known: &[(f64, f64)], within_s: f64) -> usize {
    let mut snapped = 0;
    for step in summary.steps.iter_mut() {
        let change = step.to_ms - step.from_ms;
        let hit = known
            .iter()
            .filter(|(t, ms)| (t - step.at_s).abs() <= within_s && ms.signum() == change.signum())
            .min_by(|a, b| (a.0 - step.at_s).abs().total_cmp(&(b.0 - step.at_s).abs()));
        if let Some(&(t, _)) = hit {
            step.at_s = t;
            snapped += 1;
        }
    }
    snapped
}

/// Where the reference has to be read from, at each point of the take, for it
/// to line up with the signal: each segment's level, drifting at its own rate
/// when it has one, switching at the steps between segments.
#[derive(Clone, Debug)]
pub struct DelayMap {
    /// (from_s, lag_ms at from_s, ms per s)
    pieces: Vec<(f64, f64, f64)>,
}

impl DelayMap {
    pub fn constant(lag_ms: f64) -> Self {
        Self { pieces: vec![(0.0, lag_ms, 0.0)] }
    }

    pub fn from_summary(summary: &TrackSummary) -> Option<Self> {
        if summary.segments.is_empty() {
            return None;
        }
        let mut pieces = Vec::new();
        for (i, seg) in summary.segments.iter().enumerate() {
            let from = if i == 0 { 0.0 } else { summary.steps.get(i - 1).map(|s| s.at_s).unwrap_or(seg.start_s) };
            let slope = seg.drift_ppm.unwrap_or(0.0) / 1000.0;
            let mid = (seg.start_s + seg.end_s) / 2.0;
            // The segment's median sits at its middle; carry it back to `from`.
            pieces.push((from, seg.median_ms - slope * (mid - from), slope));
        }
        Some(Self { pieces })
    }

    pub fn lag_ms_at(&self, t_s: f64) -> f64 {
        let piece = self
            .pieces
            .iter()
            .rev()
            .find(|p| t_s >= p.0)
            .unwrap_or(&self.pieces[0]);
        piece.1 + piece.2 * (t_s - piece.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{scenario, ScenarioConfig};

    fn track(cfg: &ScenarioConfig) -> (TrackSummary, Vec<LagPoint>) {
        let s = scenario(cfg);
        let dc = DelayConfig::default();
        let points = lag_track(&s.mic, &s.sys, s.rate, &dc);
        (summarize(&points, &dc), points)
    }

    #[test]
    fn finds_an_inverting_echo_to_a_fraction_of_a_millisecond() {
        let (summary, _) = track(&ScenarioConfig { lag_ms: 157.3, polarity: -1.0, ..Default::default() });
        assert!(summary.valid >= 8, "only {} valid windows", summary.valid);
        assert_eq!(summary.polarity, -1);
        let lag = summary.median_ms.unwrap();
        assert!((lag - 157.3).abs() < 0.1, "lag {lag}");
        assert!(summary.steps.is_empty(), "{:?}", summary.steps);
    }

    #[test]
    fn finds_a_long_lag_and_a_negative_one() {
        for lag_ms in [420.0, -80.0] {
            let (summary, _) = track(&ScenarioConfig { lag_ms, ..Default::default() });
            let lag = summary.median_ms.unwrap();
            assert!((lag - lag_ms).abs() < 0.1, "wanted {lag_ms}, got {lag}");
            assert_eq!(summary.polarity, 1);
        }
    }

    #[test]
    fn measures_drift_in_parts_per_million() {
        let (summary, _) = track(&ScenarioConfig { drift_ppm: 40.0, dur_s: 120.0, ..Default::default() });
        let ppm = summary.drift_ppm.unwrap();
        assert!((ppm - 40.0).abs() < 3.0, "drift {ppm}");
    }

    #[test]
    fn a_gap_in_the_reference_is_a_step_up() {
        let cfg = ScenarioConfig { gap: Some((50.0, 12.0)), dur_s: 120.0, ..Default::default() };
        let (mut summary, _) = track(&cfg);
        assert_eq!(summary.steps.len(), 1, "{:?}", summary.steps);
        let step = summary.steps[0].clone();
        assert!((step.to_ms - step.from_ms - 12.0).abs() < 0.5, "{step:?}");
        assert!((step.at_s - 50.0).abs() < 6.0, "{step:?}");
        let map = DelayMap::from_summary(&summary).unwrap();
        assert!((map.lag_ms_at(10.0) - 157.0).abs() < 0.2);
        assert!((map.lag_ms_at(100.0) - 169.0).abs() < 0.2);

        // Short windows put it within a fraction of a second of where the
        // frames went missing — on either polarity, and past a long offset.
        let s = scenario(&cfg);
        refine_steps(&s.mic, &s.sys, s.rate, &mut summary, &DelayConfig::default());
        assert!((summary.steps[0].at_s - 50.0).abs() < 0.3, "{:?}", summary.steps);
        for other in [
            ScenarioConfig { polarity: -1.0, ..cfg.clone() },
            ScenarioConfig { lag_ms: 420.0, ..cfg.clone() },
        ] {
            let (mut summary, _) = track(&other);
            let s = scenario(&other);
            refine_steps(&s.mic, &s.sys, s.rate, &mut summary, &DelayConfig::default());
            assert_eq!(summary.steps.len(), 1, "{:?}", summary.steps);
            assert!((summary.steps[0].at_s - 50.0).abs() < 0.3, "{other:?}: {:?}", summary.steps);
        }

        // And a gap the capture recorded settles it exactly.
        let snapped = snap_steps(&mut summary, &[(49.2, 12.0), (50.0, 12.0), (80.0, -5.0)], 3.0);
        assert_eq!(snapped, 1);
        assert_eq!(summary.steps[0].at_s, 50.0);
    }

    #[test]
    fn a_silent_reference_yields_no_lag() {
        let mic: Vec<f32> = (0..16_000u64 * 20).map(|i| ((i * 7919) % 1000) as f32 / 1e4).collect();
        let sys = vec![0.0f32; mic.len()];
        let dc = DelayConfig::default();
        let s = summarize(&lag_track(&mic, &sys, 16_000, &dc), &dc);
        assert_eq!(s.valid, 0);
        assert_eq!(s.median_ms, None);
    }
}
