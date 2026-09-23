//! Synthetic takes with a known answer, for the tests and for `selftest`.
//!
//! A "voice" is a pulse train at a gliding pitch through three formant
//! resonators, in syllables with pauses between — not speech, but colored,
//! harmonic, bursty and silent in the gaps, which is what delay estimation and
//! echo cancellation are sensitive to. The echo path is a laptop's: a
//! bass-shy speaker, a strong direct path, a few early reflections and a short
//! room tail, at a bulk delay that stands for the capture's start offset plus
//! the output path.

use crate::fft::Fft;

/// xorshift64*: deterministic, so every test run hears the same take.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.uniform()
    }

    pub fn gauss(&mut self) -> f64 {
        let u1 = self.uniform().max(1e-12);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }
}

pub struct Voice {
    pub f0: (f64, f64),
    pub formants: &'static [[f64; 3]],
}

/// A low voice, remote.
pub const VOICE_A: Voice = Voice {
    f0: (95.0, 140.0),
    formants: &[
        [730.0, 1090.0, 2440.0],
        [270.0, 2290.0, 3010.0],
        [300.0, 870.0, 2240.0],
        [530.0, 1840.0, 2480.0],
        [570.0, 840.0, 2410.0],
    ],
};

/// A second remote voice — the system stream can carry several.
pub const VOICE_C: Voice = Voice {
    f0: (120.0, 170.0),
    formants: &[
        [660.0, 1700.0, 2400.0],
        [440.0, 1020.0, 2240.0],
        [390.0, 1990.0, 2550.0],
        [490.0, 1350.0, 1690.0],
    ],
};

/// The user, at the laptop.
pub const VOICE_B: Voice = Voice {
    f0: (180.0, 250.0),
    formants: &[
        [850.0, 1220.0, 2810.0],
        [310.0, 2790.0, 3310.0],
        [370.0, 950.0, 2670.0],
        [610.0, 2330.0, 2990.0],
        [590.0, 920.0, 2710.0],
    ],
};

/// `voice` speaking inside `spans` (seconds), scaled so its active part sits
/// at `rms_dbfs`.
pub fn speech(voice: &Voice, rate: u32, len: usize, spans: &[(f64, f64)], rms_dbfs: f64, rng: &mut Rng) -> Vec<f64> {
    let rate_f = rate as f64;
    let mut out = vec![0.0; len];
    for &(a, b) in spans {
        let mut t = a;
        while t < b {
            let end = (t + rng.range(0.12, 0.30)).min(b);
            let (i0, i1) = ((t * rate_f) as usize, ((end * rate_f) as usize).min(len));
            if i1 <= i0 {
                break;
            }
            let n = i1 - i0;
            let voiced = rng.uniform() > 0.2;
            let mut sig = vec![0.0; n];
            if voiced {
                let f0a = rng.range(voice.f0.0, voice.f0.1);
                let f0b = f0a * rng.range(0.9, 1.1);
                let mut phase = 0.0;
                for (i, v) in sig.iter_mut().enumerate() {
                    phase += (f0a + (f0b - f0a) * i as f64 / n as f64) / rate_f;
                    if phase >= 1.0 {
                        phase -= 1.0;
                        *v = 1.0;
                    }
                    *v += 0.03 * rng.gauss();
                }
                let formants = voice.formants[(rng.next_u64() % voice.formants.len() as u64) as usize];
                for (f, bw) in formants.iter().zip([90.0, 120.0, 170.0]) {
                    let r = (-std::f64::consts::PI * bw / rate_f).exp();
                    let th = 2.0 * std::f64::consts::PI * f / rate_f;
                    let (a1, a2) = (2.0 * r * th.cos(), -r * r);
                    let (mut y1, mut y2) = (0.0, 0.0);
                    for v in sig.iter_mut() {
                        let y = *v + a1 * y1 + a2 * y2;
                        y2 = y1;
                        y1 = y;
                        *v = y * (1.0 - r);
                    }
                }
            } else {
                // A fricative: differenced noise, tilted towards the top.
                let mut prev = 0.0;
                for v in sig.iter_mut() {
                    let g = rng.gauss();
                    *v = 0.3 * (g - prev);
                    prev = g;
                }
            }
            for (i, v) in sig.iter().enumerate() {
                let e = (std::f64::consts::PI * i as f64 / n as f64).sin();
                out[i0 + i] += v * e * e;
            }
            t = end + if rng.uniform() < 0.15 { rng.range(0.3, 0.8) } else { rng.range(0.03, 0.15) };
        }
    }
    let active: Vec<f64> = spans
        .iter()
        .flat_map(|&(a, b)| ((a * rate_f) as usize).min(len)..((b * rate_f) as usize).min(len))
        .map(|i| out[i])
        .collect();
    let current = (active.iter().map(|v| v * v).sum::<f64>() / active.len().max(1) as f64).sqrt();
    if current > 0.0 {
        let k = 10f64.powf(rms_dbfs / 20.0) / current;
        out.iter_mut().for_each(|v| *v *= k);
    }
    out
}

/// A laptop's speaker-to-mic path: the direct tap (inverted when `polarity`
/// is negative), six early reflections, then an exponential room tail that
/// sits ~10 dB under the direct sound.
pub fn room_response(rate: u32, len_ms: f64, rt60_s: f64, polarity: f64, rng: &mut Rng) -> Vec<f64> {
    let rate_f = rate as f64;
    let n = ((len_ms / 1000.0 * rate_f) as usize).max(1);
    let mut h = vec![0.0; n];
    h[0] = polarity;
    for _ in 0..6 {
        let at = (rng.range(1.5, 15.0) / 1000.0 * rate_f) as usize;
        if at < n {
            let sign = if rng.uniform() < 0.5 { -1.0 } else { 1.0 };
            h[at] += sign * rng.range(0.15, 0.45);
        }
    }
    for (i, v) in h.iter_mut().enumerate().skip((0.015 * rate_f) as usize) {
        let t = i as f64 / rate_f;
        *v += 0.02 * rng.gauss() * (-6.9 * t / rt60_s).exp();
    }
    h
}

/// Linear convolution by FFT overlap-add, truncated to `x.len()`.
pub fn convolve(x: &[f64], h: &[f64]) -> Vec<f64> {
    let block = 8192;
    let n = (block + h.len()).next_power_of_two();
    let fft = Fft::new(n);
    let hf = fft.forward_real(h);
    let mut out = vec![0.0; x.len() + h.len()];
    for start in (0..x.len()).step_by(block) {
        let end = (start + block).min(x.len());
        let mut buf = fft.forward_real(&x[start..end]);
        for (v, &g) in buf.iter_mut().zip(&hf) {
            *v = *v * g;
        }
        fft.inverse(&mut buf);
        for (i, v) in buf.iter().take(end - start + h.len() - 1).enumerate() {
            out[start + i] += v.re;
        }
    }
    out.truncate(x.len());
    out
}

fn highpass(x: &[f64], fc: f64, rate: f64) -> Vec<f64> {
    let rc = 1.0 / (2.0 * std::f64::consts::PI * fc);
    let a = rc / (rc + 1.0 / rate);
    let mut out = vec![0.0; x.len()];
    let (mut y, mut prev) = (0.0, 0.0);
    for (o, &v) in out.iter_mut().zip(x) {
        y = a * (y + v - prev);
        prev = v;
        *o = y;
    }
    out
}

#[derive(Clone, Debug)]
pub struct ScenarioConfig {
    pub dur_s: f64,
    /// Where the echo lands in the mic behind the system stream, by index.
    pub lag_ms: f64,
    /// −1 for a path that inverts.
    pub polarity: f64,
    /// Echo level in the mic relative to the system stream. A real take
    /// measured −16 to −18 dB with the laptop's speakers loud.
    pub echo_db: f64,
    /// The user's level relative to the echo — the same take had them about
    /// equal.
    pub near_db: f64,
    /// The echo's lag growing by this much per unit time: two clocks.
    pub drift_ppm: f64,
    /// (at_s, ms): the system stream loses `ms` of frames at `at_s`.
    pub gap: Option<(f64, f64)>,
    /// Soft-clip the speaker, as a small speaker driven hard does.
    pub nonlinear: bool,
    pub noise_dbfs: f64,
    pub seed: u64,
}

impl Default for ScenarioConfig {
    fn default() -> Self {
        Self {
            dur_s: 90.0,
            lag_ms: 157.0,
            polarity: 1.0,
            echo_db: -17.0,
            near_db: 0.0,
            drift_ppm: 0.0,
            gap: None,
            nonlinear: false,
            noise_dbfs: -85.0,
            seed: 7,
        }
    }
}

/// One synthetic take, with its parts kept apart.
pub struct Scenario {
    pub rate: u32,
    pub mic: Vec<f32>,
    /// The system stream as captured: gap included, when there is one.
    pub sys: Vec<f32>,
    /// The user's voice and the echo, as the mic received each.
    pub near: Vec<f32>,
    pub echo: Vec<f32>,
    /// Per 10 ms frame of the mic: the echo is sounding / the user is.
    pub echo_active: Vec<bool>,
    pub near_active: Vec<bool>,
}

pub const FRAME: usize = 160;

impl Scenario {
    /// Frames where only the echo sounds, with a margin either side.
    pub fn far_only(&self) -> Vec<bool> {
        let near = dilate(&self.near_active, 30);
        self.echo_active.iter().zip(&near).map(|(&e, &n)| e && !n).collect()
    }

    pub fn near_only(&self) -> Vec<bool> {
        let echo = dilate(&self.echo_active, 30);
        self.near_active.iter().zip(&echo).map(|(&n, &e)| n && !e).collect()
    }

    pub fn double_talk(&self) -> Vec<bool> {
        self.echo_active.iter().zip(&self.near_active).map(|(&e, &n)| e && n).collect()
    }
}

fn dilate(mask: &[bool], frames: usize) -> Vec<bool> {
    (0..mask.len())
        .map(|i| mask[i.saturating_sub(frames)..(i + frames + 1).min(mask.len())].iter().any(|&v| v))
        .collect()
}

/// Remote voices trade turns; the user speaks alone twice and over them once,
/// per 30 s.
fn pattern(dur_s: f64) -> (Vec<(f64, f64)>, Vec<(f64, f64)>, Vec<(f64, f64)>) {
    let (mut a, mut c, mut b) = (Vec::new(), Vec::new(), Vec::new());
    let mut t = 0.0;
    while t < dur_s {
        let push = |v: &mut Vec<(f64, f64)>, s: f64, e: f64| {
            if t + s < dur_s {
                v.push((t + s, (t + e).min(dur_s)));
            }
        };
        push(&mut a, 0.5, 5.0);
        push(&mut c, 5.2, 9.5);
        push(&mut b, 10.0, 14.0);
        push(&mut a, 15.0, 18.0);
        push(&mut b, 17.0, 20.0);
        push(&mut c, 18.2, 21.0);
        push(&mut b, 22.0, 23.5);
        push(&mut a, 24.0, 27.0);
        push(&mut c, 27.2, 29.5);
        t += 30.0;
    }
    (a, c, b)
}

fn mask(spans: &[(f64, f64)], shift_s: f64, tail_s: f64, frames: usize) -> Vec<bool> {
    let mut m = vec![false; frames];
    let per_s = 16_000.0 / FRAME as f64;
    for &(a, b) in spans {
        let from = (((a + shift_s) * per_s).floor().max(0.0)) as usize;
        let to = (((b + shift_s + tail_s) * per_s).ceil().max(0.0)) as usize;
        for v in m.iter_mut().take(to.min(frames)).skip(from) {
            *v = true;
        }
    }
    m
}

pub fn scenario(cfg: &ScenarioConfig) -> Scenario {
    let rate = 16_000u32;
    let rate_f = rate as f64;
    let len = (cfg.dur_s * rate_f) as usize;
    let mut rng = Rng::new(cfg.seed);
    let (far_a, far_c, near_b) = pattern(cfg.dur_s);
    let a = speech(&VOICE_A, rate, len, &far_a, -21.0, &mut rng);
    let c = speech(&VOICE_C, rate, len, &far_c, -22.0, &mut rng);
    let sys_true: Vec<f64> = a.iter().zip(&c).map(|(x, y)| x + y).collect();

    let mut speaker = highpass(&sys_true, 250.0, rate_f);
    if cfg.nonlinear {
        let peak = speaker.iter().fold(0.0f64, |m, v| m.max(v.abs())).max(1e-9);
        speaker.iter_mut().for_each(|v| *v = (2.5 * *v / peak).tanh() * peak / 2.5);
    }
    let h = room_response(rate, 300.0, 0.35, cfg.polarity, &mut rng);
    let at_mic = convolve(&speaker, &h);
    let d0 = cfg.lag_ms / 1000.0 * rate_f;
    let eps = cfg.drift_ppm * 1e-6;
    let mut echo: Vec<f64> = (0..len)
        .map(|n| {
            let pos = n as f64 - d0 - eps * n as f64;
            let i = pos.floor();
            if i < 0.0 || i as usize + 1 >= len {
                return 0.0;
            }
            let f = pos - i;
            at_mic[i as usize] * (1.0 - f) + at_mic[i as usize + 1] * f
        })
        .collect();

    let rms = |x: &[f64]| (x.iter().map(|v| v * v).sum::<f64>() / x.len().max(1) as f64).sqrt();
    let echo_gain = 10f64.powf(cfg.echo_db / 20.0) * rms(&sys_true) / rms(&echo).max(1e-12);
    echo.iter_mut().for_each(|v| *v *= echo_gain);
    let echo_level = 20.0 * rms(&echo).max(1e-12).log10();
    // Scaled against the echo's level over the take; the voice's own RMS is
    // over its active spans, so compare like with like.
    let far_share = far_a.iter().chain(&far_c).map(|(s, e)| e - s).sum::<f64>() / cfg.dur_s;
    let near_target = echo_level - 10.0 * far_share.log10() + cfg.near_db;
    let near = speech(&VOICE_B, rate, len, &near_b, near_target, &mut rng);

    let noise_amp = 10f64.powf(cfg.noise_dbfs / 20.0);
    let mic: Vec<f32> = (0..len)
        .map(|i| (near[i] + echo[i] + noise_amp * rng.gauss()) as f32)
        .collect();

    let mut sys: Vec<f32> = sys_true.iter().map(|&v| v as f32).collect();
    if let Some((at_s, ms)) = cfg.gap {
        let at = (at_s * rate_f) as usize;
        let lost = (ms / 1000.0 * rate_f).round() as usize;
        if at < len {
            sys.drain(at..(at + lost).min(len));
            sys.resize(len, 0.0);
        }
    }

    let frames = len / FRAME;
    Scenario {
        rate,
        mic,
        sys,
        near: near.iter().map(|&v| v as f32).collect(),
        echo: echo.iter().map(|&v| v as f32).collect(),
        echo_active: {
            let mut m = mask(&far_a, cfg.lag_ms / 1000.0, 0.3, frames);
            for (v, w) in m.iter_mut().zip(mask(&far_c, cfg.lag_ms / 1000.0, 0.3, frames)) {
                *v |= w;
            }
            m
        },
        near_active: mask(&near_b, 0.0, 0.05, frames),
    }
}

/// Energy of `x` over the frames `mask` selects.
pub fn masked_energy(x: &[f32], mask: &[bool]) -> f64 {
    mask.iter()
        .enumerate()
        .filter(|(_, &m)| m)
        .map(|(f, _)| {
            x[f * FRAME..((f + 1) * FRAME).min(x.len())]
                .iter()
                .map(|&v| v as f64 * v as f64)
                .sum::<f64>()
        })
        .sum()
}

