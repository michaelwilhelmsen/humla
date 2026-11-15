//! The level of a take's `playback.wav` (#234): the two streams summed, then
//! the speech brought to one loudness and its peaks held under full scale.
//! Only the file the player plays goes through this; `mic.wav` and `sys.wav`
//! keep the samples the analysis passes need.

const RATE: usize = 16_000;
const BLOCK: usize = RATE / 10;
/// Blocks quieter than this are silence and don't count toward the loudness.
const GATE: f32 = 0.003_162; // −50 dBFS
const TARGET_RMS: f32 = 0.1; // −20 dBFS
const MAX_GAIN: f32 = 10.0; // +20 dB
const CEILING: f32 = 0.891; // −1 dBFS
const ATTACK_SAMPLES: f32 = 0.005 * RATE as f32;
const RELEASE_SAMPLES: f32 = 0.1 * RATE as f32;

/// Sums the two streams sample for sample. Halving the sum, as the mix once
/// did, cost every take 6 dB even where one side was silent.
pub fn mix(a: Vec<f32>, b: Vec<f32>) -> Vec<f32> {
    let (mut long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    for (x, y) in long.iter_mut().zip(short.iter()) {
        *x += y;
    }
    long
}

/// RMS over the 100 ms blocks above the silence gate, so a meeting's pauses
/// don't pull the gain up.
pub fn speech_rms(samples: &[f32]) -> Option<f32> {
    let mut sum = 0.0f64;
    let mut count = 0usize;
    for block in samples.chunks(BLOCK) {
        let ms = block.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / block.len() as f64;
        if ms.sqrt() as f32 > GATE {
            sum += ms * block.len() as f64;
            count += block.len();
        }
    }
    (count > 0).then(|| (sum / count as f64).sqrt() as f32)
}

/// Brings speech to `TARGET_RMS` (never boosting past `MAX_GAIN`), then
/// limits so no sample exceeds `CEILING`.
pub fn level(samples: &mut [f32]) {
    if let Some(rms) = speech_rms(samples) {
        let gain = (TARGET_RMS / rms).min(MAX_GAIN);
        for s in samples.iter_mut() {
            *s *= gain;
        }
    }
    limit(samples);
}

/// The gain each sample needs to stay under the ceiling, ramped down ahead of
/// a peak and back up after it, so the limiter never clicks.
fn limit(samples: &mut [f32]) {
    if !samples.iter().any(|s| s.abs() > CEILING) {
        return;
    }
    let mut env: Vec<f32> = samples
        .iter()
        .map(|s| if s.abs() > CEILING { CEILING / s.abs() } else { 1.0 })
        .collect();
    let release = 1.0 / RELEASE_SAMPLES;
    for i in 1..env.len() {
        env[i] = env[i].min(env[i - 1] + release);
    }
    let attack = 1.0 / ATTACK_SAMPLES;
    for i in (0..env.len().saturating_sub(1)).rev() {
        env[i] = env[i].min(env[i + 1] + attack);
    }
    for (s, g) in samples.iter_mut().zip(env) {
        *s *= g;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amplitude: f32, seconds: f32) -> Vec<f32> {
        (0..(seconds * RATE as f32) as usize)
            .map(|i| amplitude * (i as f32 * 2.0 * std::f32::consts::PI * 220.0 / RATE as f32).sin())
            .collect()
    }

    fn db(v: f32) -> f32 {
        20.0 * v.log10()
    }

    #[test]
    fn mix_does_not_halve_a_stream_beside_a_silent_one() {
        let mic = tone(0.1, 1.0);
        let mixed = mix(mic.clone(), vec![0.0; mic.len()]);
        assert_eq!(mixed, mic);
    }

    #[test]
    fn mix_keeps_the_longer_stream_s_tail() {
        assert_eq!(mix(vec![0.1], vec![0.2, 0.3]), vec![0.1 + 0.2, 0.3]);
        assert_eq!(mix(vec![0.2, 0.3], vec![]), vec![0.2, 0.3]);
    }

    #[test]
    fn quiet_speech_is_brought_to_the_target() {
        // −40 dBFS RMS, the level a laptop mic records a room at.
        let mut s = tone(0.01 * std::f32::consts::SQRT_2, 3.0);
        level(&mut s);
        let rms = speech_rms(&s).unwrap();
        assert!((db(rms) - db(TARGET_RMS)).abs() < 0.5, "{} dBFS", db(rms));
    }

    #[test]
    fn pauses_do_not_count_toward_the_loudness() {
        let speech = tone(0.01 * std::f32::consts::SQRT_2, 2.0);
        let mut s = speech.clone();
        s.extend(vec![0.0; RATE * 20]);
        level(&mut s);
        let rms = speech_rms(&s[..speech.len()]).unwrap();
        assert!((db(rms) - db(TARGET_RMS)).abs() < 0.5, "{} dBFS", db(rms));
    }

    #[test]
    fn the_boost_is_capped() {
        // −45 dBFS would need +25 dB; it gets +20.
        let mut s = tone(0.005_623 * std::f32::consts::SQRT_2, 2.0);
        level(&mut s);
        let rms = speech_rms(&s).unwrap();
        assert!((db(rms) - (-25.0)).abs() < 0.5, "{} dBFS", db(rms));
    }

    #[test]
    fn silence_stays_silent() {
        let mut s = vec![0.0; RATE];
        level(&mut s);
        assert!(s.iter().all(|&x| x == 0.0));
        let mut faint = vec![0.0001; RATE];
        level(&mut faint);
        assert!(faint.iter().all(|&x| x == 0.0001), "below the gate is left alone");
    }

    #[test]
    fn nothing_exceeds_the_ceiling() {
        let mut s = mix(tone(0.9, 1.0), tone(0.9, 1.0));
        // A click on a quiet take, which the boost would otherwise push far past full scale.
        let mut quiet = tone(0.01, 2.0);
        quiet[RATE] = 1.0;
        s.extend(quiet);
        level(&mut s);
        let peak = s.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak <= CEILING + 1e-6, "peak {peak}");
    }

    #[test]
    fn the_limiter_leaves_audio_away_from_a_peak_alone() {
        let mut s = tone(0.5, 2.0);
        let before = s.clone();
        s[RATE] = 2.0;
        limit(&mut s);
        assert_eq!(s[..RATE / 2], before[..RATE / 2]);
        assert_eq!(s[RATE + RATE / 2..], before[RATE + RATE / 2..]);
        assert!(s[RATE] <= CEILING + 1e-6);
    }
}
