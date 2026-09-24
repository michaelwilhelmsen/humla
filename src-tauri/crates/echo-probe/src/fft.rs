//! Iterative radix-2 complex FFT, in f64. Small and dependency-free on purpose:
//! everything here runs over power-of-two frames, and double precision keeps a
//! 131 072-point correlation's peak clean enough for sub-sample interpolation.

use std::ops::{Add, AddAssign, Mul, Sub};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct C64 {
    pub re: f64,
    pub im: f64,
}

impl C64 {
    pub const ZERO: C64 = C64 { re: 0.0, im: 0.0 };

    pub fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    pub fn conj(self) -> Self {
        Self { re: self.re, im: -self.im }
    }

    pub fn norm_sqr(self) -> f64 {
        self.re * self.re + self.im * self.im
    }

    pub fn abs(self) -> f64 {
        self.norm_sqr().sqrt()
    }

    pub fn scale(self, k: f64) -> Self {
        Self { re: self.re * k, im: self.im * k }
    }
}

impl Add for C64 {
    type Output = C64;
    fn add(self, o: C64) -> C64 {
        C64::new(self.re + o.re, self.im + o.im)
    }
}

impl AddAssign for C64 {
    fn add_assign(&mut self, o: C64) {
        self.re += o.re;
        self.im += o.im;
    }
}

impl Sub for C64 {
    type Output = C64;
    fn sub(self, o: C64) -> C64 {
        C64::new(self.re - o.re, self.im - o.im)
    }
}

impl Mul for C64 {
    type Output = C64;
    fn mul(self, o: C64) -> C64 {
        C64::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
}

/// A plan for one power-of-two size: twiddles and the bit-reversal table.
#[derive(Clone)]
pub struct Fft {
    n: usize,
    twiddles: Vec<C64>,
    rev: Vec<u32>,
}

impl Fft {
    pub fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 2, "FFT size must be a power of two");
        let twiddles = (0..n / 2)
            .map(|k| {
                let a = -2.0 * std::f64::consts::PI * k as f64 / n as f64;
                C64::new(a.cos(), a.sin())
            })
            .collect();
        let bits = n.trailing_zeros();
        let rev = (0..n as u32).map(|i| i.reverse_bits() >> (32 - bits)).collect();
        Self { n, twiddles, rev }
    }

    pub fn len(&self) -> usize {
        self.n
    }

    /// In place, unscaled: `X[k] = Σ x[n]·e^(−2πikn/N)`.
    pub fn forward(&self, buf: &mut [C64]) {
        assert_eq!(buf.len(), self.n);
        for i in 0..self.n {
            let j = self.rev[i] as usize;
            if i < j {
                buf.swap(i, j);
            }
        }
        let mut m = 2;
        while m <= self.n {
            let half = m / 2;
            let step = self.n / m;
            for start in (0..self.n).step_by(m) {
                for j in 0..half {
                    let w = self.twiddles[j * step];
                    let t = w * buf[start + j + half];
                    let u = buf[start + j];
                    buf[start + j] = u + t;
                    buf[start + j + half] = u - t;
                }
            }
            m *= 2;
        }
    }

    /// In place, scaled by 1/N, so `inverse(forward(x)) == x`.
    pub fn inverse(&self, buf: &mut [C64]) {
        for v in buf.iter_mut() {
            *v = v.conj();
        }
        self.forward(buf);
        let k = 1.0 / self.n as f64;
        for v in buf.iter_mut() {
            *v = v.conj().scale(k);
        }
    }

    /// Forward transform of real samples, zero-padded (or truncated) to N.
    pub fn forward_real(&self, input: &[f64]) -> Vec<C64> {
        let mut buf = vec![C64::ZERO; self.n];
        for (dst, &x) in buf.iter_mut().zip(input) {
            dst.re = x;
        }
        self.forward(&mut buf);
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_dft(x: &[C64]) -> Vec<C64> {
        let n = x.len();
        (0..n)
            .map(|k| {
                let mut acc = C64::ZERO;
                for (t, v) in x.iter().enumerate() {
                    let a = -2.0 * std::f64::consts::PI * (k * t) as f64 / n as f64;
                    acc += *v * C64::new(a.cos(), a.sin());
                }
                acc
            })
            .collect()
    }

    #[test]
    fn matches_a_naive_dft() {
        let x: Vec<C64> = (0..32)
            .map(|i| C64::new((i as f64 * 0.7).sin(), (i as f64 * 1.3).cos() * 0.5))
            .collect();
        let mut fast = x.clone();
        Fft::new(32).forward(&mut fast);
        for (a, b) in fast.iter().zip(naive_dft(&x)) {
            assert!((a.re - b.re).abs() < 1e-9 && (a.im - b.im).abs() < 1e-9);
        }
    }

    #[test]
    fn inverse_undoes_forward() {
        let fft = Fft::new(1024);
        let x: Vec<f64> = (0..1024).map(|i| ((i * 37) % 101) as f64 / 50.0 - 1.0).collect();
        let mut buf = fft.forward_real(&x);
        fft.inverse(&mut buf);
        for (v, &orig) in buf.iter().zip(&x) {
            assert!((v.re - orig).abs() < 1e-12 && v.im.abs() < 1e-12);
        }
    }
}
