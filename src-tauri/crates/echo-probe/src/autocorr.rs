//! The echo lag of a take that kept only its mixed `playback.wav` — every note
//! that arrived from the cloud. The mix holds the remote voice twice: once from
//! the system stream, and again off the speakers into the mic, `lag` later. So
//! the lag shows in the mix's own self-similarity.
//!
//! Coarse by onset envelope (rises in 10 ms log-energy), which ignores phase
//! and polarity entirely. Then refined to the sample by the averaged power
//! cepstrum around the coarse peak — the classic single-signal echo detector,
//! whose peak also carries the echo's sign.

use crate::delay::LagPoint;
use crate::fft::{Fft, C64};
use crate::stats::{dbfs, rms};

#[derive(Clone, Debug)]
pub struct AutocorrConfig {
    pub window_s: f64,
    pub hop_s: f64,
    pub min_lag_ms: f64,
    pub max_lag_ms: f64,
    pub min_prominence: f64,
    /// Cepstral search either side of the coarse peak.
    pub refine_ms: f64,
}

impl Default for AutocorrConfig {
    fn default() -> Self {
        Self {
            window_s: 60.0,
            hop_s: 30.0,
            min_lag_ms: 50.0,
            max_lag_ms: 1_500.0,
            min_prominence: 5.0,
            refine_ms: 15.0,
        }
    }
}

const FRAME_MS: f64 = 10.0;

fn onset_envelope(x: &[f32], frame: usize) -> Vec<f64> {
    let energy: Vec<f64> = x
        .chunks(frame)
        .map(|c| (c.iter().map(|&v| v as f64 * v as f64).sum::<f64>() + 1e-10).ln())
        .collect();
    let mut out = vec![0.0; energy.len()];
    for i in 1..energy.len() {
        out[i] = (energy[i] - energy[i - 1]).max(0.0);
    }
    out
}

/// Coarse lag in frames, with its prominence, from one window's onsets.
fn coarse(onsets: &[f64], min_lag: usize, max_lag: usize) -> Option<(usize, f64)> {
    let n = onsets.len();
    if n <= max_lag + 1 {
        return None;
    }
    let mean = onsets.iter().sum::<f64>() / n as f64;
    let o: Vec<f64> = onsets.iter().map(|v| v - mean).collect();
    let energy: f64 = o.iter().map(|v| v * v).sum();
    if energy <= 0.0 {
        return None;
    }
    let ac: Vec<f64> = (min_lag..=max_lag)
        .map(|l| (0..n - l).map(|i| o[i] * o[i + l]).sum::<f64>() / energy)
        .collect();
    let (best, &peak) = ac.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    if peak <= 0.0 {
        return None;
    }
    let rest: Vec<f64> = ac
        .iter()
        .enumerate()
        .filter(|(i, _)| i.abs_diff(best) > 3)
        .map(|(_, v)| v * v)
        .collect();
    let floor = (rest.iter().sum::<f64>() / rest.len().max(1) as f64).sqrt();
    Some((best + min_lag, if floor > 0.0 { peak / floor } else { f64::INFINITY }))
}

/// Averaged power cepstrum of `x`, over Hann frames of `n` samples at 50%
/// overlap, for quefrencies `0..n/2`. Frames too quiet to say anything are
/// left out of the average.
fn mean_cepstrum(x: &[f32], fft: &Fft) -> Option<Vec<f64>> {
    let n = fft.len();
    let hop = n / 2;
    let win: Vec<f64> = (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos())
        .collect();
    let mut acc = vec![0.0; n / 2];
    let mut frames = 0;
    let mut start = 0;
    while start + n <= x.len() {
        let frame = &x[start..start + n];
        start += hop;
        if dbfs(rms(frame)) < -60.0 {
            continue;
        }
        let mut buf: Vec<C64> = frame.iter().zip(&win).map(|(&v, &w)| C64::new(v as f64 * w, 0.0)).collect();
        fft.forward(&mut buf);
        let mean_power = buf.iter().map(|v| v.norm_sqr()).sum::<f64>() / n as f64;
        let floor = mean_power * 1e-6 + 1e-30;
        for v in buf.iter_mut() {
            *v = C64::new((v.norm_sqr() + floor).ln(), 0.0);
        }
        fft.inverse(&mut buf);
        for (a, v) in acc.iter_mut().zip(&buf) {
            *a += v.re;
        }
        frames += 1;
    }
    if frames == 0 {
        return None;
    }
    acc.iter_mut().for_each(|v| *v /= frames as f64);
    Some(acc)
}

/// Sample-level lag and sign from the cepstral peak within `around ± span`.
fn refine(cep: &[f64], around: usize, span: usize) -> Option<(f64, i8)> {
    let lo = around.saturating_sub(span).max(1);
    let hi = (around + span).min(cep.len() - 2);
    if lo >= hi {
        return None;
    }
    let best = (lo..=hi).max_by(|&a, &b| cep[a].abs().total_cmp(&cep[b].abs()))?;
    let polarity: i8 = if cep[best] < 0.0 { -1 } else { 1 };
    let p = polarity as f64;
    let (a, b, c) = (p * cep[best - 1], p * cep[best], p * cep[best + 1]);
    let denom = a - 2.0 * b + c;
    let frac = if denom.abs() > 1e-30 { (0.5 * (a - c) / denom).clamp(-0.5, 0.5) } else { 0.0 };
    Some((best as f64 + frac, polarity))
}

pub fn autocorr_track(mix: &[f32], rate: u32, cfg: &AutocorrConfig) -> Vec<LagPoint> {
    let rate_f = rate as f64;
    let frame = (rate_f * FRAME_MS / 1000.0) as usize;
    let onsets = onset_envelope(mix, frame);
    let per_frame = FRAME_MS / 1000.0;
    let window = (cfg.window_s / per_frame) as usize;
    let hop = ((cfg.hop_s / per_frame) as usize).max(1);
    let (min_lag, max_lag) = (
        (cfg.min_lag_ms / FRAME_MS).round() as usize,
        (cfg.max_lag_ms / FRAME_MS).round() as usize,
    );
    // Frames long enough to hold the longest lag twice over.
    let cep_fft = Fft::new(((2.0 * cfg.max_lag_ms / 1000.0 * rate_f) as usize).next_power_of_two().max(4096));
    let refine_span = (cfg.refine_ms / 1000.0 * rate_f) as usize;
    let mut out = Vec::new();
    let mut start = 0;
    while start + window <= onsets.len() || (start == 0 && onsets.len() > max_lag + 1) {
        let end = (start + window).min(onsets.len());
        let samples = &mix[(start * frame).min(mix.len())..(end * frame).min(mix.len())];
        let level = dbfs(rms(samples));
        let mut point = LagPoint {
            t_s: (start + end) as f64 / 2.0 * per_frame,
            lag_ms: 0.0,
            prominence: 0.0,
            polarity: 0,
            ref_dbfs: level,
            sig_dbfs: level,
            valid: false,
        };
        if let Some((lag_frames, prominence)) = coarse(&onsets[start..end], min_lag, max_lag) {
            point.lag_ms = lag_frames as f64 * FRAME_MS;
            point.prominence = prominence;
            point.valid = prominence >= cfg.min_prominence;
            if let Some(cep) = mean_cepstrum(samples, &cep_fft) {
                let around = (point.lag_ms / 1000.0 * rate_f).round() as usize;
                if let Some((lag, polarity)) = refine(&cep, around, refine_span) {
                    point.lag_ms = lag / rate_f * 1000.0;
                    point.polarity = polarity;
                }
            }
        }
        out.push(point);
        if end == onsets.len() {
            break;
        }
        start += hop;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delay::{summarize, DelayConfig};
    use crate::synth::{scenario, ScenarioConfig};

    fn mix_of(cfg: &ScenarioConfig) -> (Vec<f32>, u32) {
        let s = scenario(cfg);
        // What `build_playback_wav` makes: the two streams summed from index 0.
        (s.mic.iter().zip(&s.sys).map(|(m, x)| (m + x) * 0.5).collect(), s.rate)
    }

    #[test]
    fn recovers_the_lag_and_sign_from_a_mix_alone() {
        for (lag_ms, polarity) in [(420.0, 1.0), (157.0, -1.0)] {
            let (mix, rate) = mix_of(&ScenarioConfig { lag_ms, polarity, dur_s: 180.0, ..Default::default() });
            let points = autocorr_track(&mix, rate, &AutocorrConfig::default());
            let summary = summarize(&points, &DelayConfig::default());
            let lag = summary.median_ms.expect("a lag");
            assert!((lag - lag_ms).abs() < 0.2, "wanted {lag_ms}, got {lag} ({points:?})");
            assert_eq!(summary.polarity as f64, polarity);
        }
    }
}
