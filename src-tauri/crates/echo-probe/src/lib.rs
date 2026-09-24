//! Measures how a Humla take's mic and system-audio streams line up, and
//! cancels the system audio's echo out of the mic, offline.
//!
//! The CLI is a measurement tool, and it reads and writes WAVs only where it is
//! pointed. The app's echo pass (#196, `src-tauri/src/echo.rs`) runs [`delay`]
//! and [`aec`] in memory. See `docs/research/stream-alignment-and-echo.md` for
//! what it is for and how to read what it prints.
//!
//! - [`delay`] — GCC-PHAT lag of the mic behind the system stream, per window,
//!   with drift and steps.
//! - [`autocorr`] — the same lag from a mixed `playback.wav` alone.
//! - [`aec`] — reference-based echo cancellation for the diarize input.
//! - [`timing`] — the app's `capture-<session>.json`, to split a lag into the
//!   capture's start offset and the output path.

pub mod aec;
pub mod autocorr;
pub mod delay;
pub mod fft;
pub mod stats;
pub mod synth;
pub mod timing;
pub mod wav;
