//! A stream's samples, held either as the WAV's own 16-bit integers or as
//! floats. The app holds whole takes, so it keeps them as the sidecar wrote
//! them; the CLI and the synthetic takes work in floats.

pub trait Sample: Copy + Send + Sync {
    /// The sample on the ±1 scale every measure here works in.
    fn f(self) -> f64;
}

impl Sample for f32 {
    fn f(self) -> f64 {
        self as f64
    }
}

impl Sample for i16 {
    fn f(self) -> f64 {
        self as f64 / 32768.0
    }
}

/// A float sample as a 16-bit WAV stores it, saturating at ±1.
pub fn pcm16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_16_bit_sample_reads_exactly_as_its_float_decoding_does() {
        // So a take held either way gives the same figures, bit for bit.
        for s in i16::MIN..=i16::MAX {
            assert_eq!(s.f().to_bits(), ((s as f32 / 32768.0) as f64).to_bits(), "{s}");
        }
    }

    #[test]
    fn pcm16_saturates_at_full_scale() {
        assert_eq!([pcm16(1.5), pcm16(-1.5), pcm16(0.5)], [32767, -32767, 16383]);
    }
}
