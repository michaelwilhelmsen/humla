//! The capture-timing file the app writes beside a take's diagnostics —
//! `diagnostics/<note_id>/capture-<session_id>.json`, from
//! `write_capture_timing` in `src-tauri/src/commands.rs`, over the sidecar's
//! `capture_timing` event. Only the fields this tool reads, all defaulted, so a
//! file from a newer or older app still loads.

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct CaptureFile {
    pub session_id: Option<String>,
    pub alignment: Alignment,
    pub capture_timing: Timing,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Alignment {
    pub start_offset_ms: Option<f64>,
    pub end_offset_ms: Option<f64>,
    pub input_latency_ms: Option<f64>,
    pub output_latency_ms: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Timing {
    pub gap_threshold_ms: f64,
    pub streams: Vec<StreamTiming>,
    pub devices: Devices,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct StreamTiming {
    pub source: String,
    pub frames_written: u64,
    pub intervals: Vec<Interval>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Interval {
    pub opened_by: String,
    pub start_frame: u64,
    pub first_stamp_ms: f64,
    pub first_arrival_ms: f64,
    pub stamp_from_arrival: bool,
    pub last_end_stamp_ms: f64,
    pub media_ms: f64,
    pub gaps: u64,
    pub gap_ms: f64,
    pub max_gap_ms: f64,
    pub overlaps: u64,
    pub max_jitter_ms: f64,
    /// `(frame, ms)` per gap (+) or overlap (−), the first 64 of each interval.
    pub jumps: Vec<(u64, f64)>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Devices {
    pub input: Option<Device>,
    pub output: Option<Device>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Device {
    pub transport: Option<String>,
    pub clock_domain: Option<u64>,
}

/// Frames per second of the WAVs the sidecar writes.
const WAV_RATE: f64 = 16_000.0;

impl CaptureFile {
    pub fn stream(&self, source: &str) -> Option<&StreamTiming> {
        self.capture_timing.streams.iter().find(|s| s.source == source)
    }

    /// Every place the capture itself says the mic's lag behind sys changed,
    /// as `(seconds into the WAV, ms the lag moved by)`: each recorded gap or
    /// overlap, and each interval edge (a resume, a device change) by how long
    /// the stream was dark there. Sys losing time pushes the lag up; the mic
    /// losing time pulls it down.
    pub fn known_discontinuities(&self) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for (source, sign) in [("sys", 1.0), ("mic", -1.0)] {
            let Some(stream) = self.stream(source) else { continue };
            for (k, interval) in stream.intervals.iter().enumerate() {
                for &(frame, ms) in &interval.jumps {
                    out.push((frame as f64 / WAV_RATE, sign * ms));
                }
                if k > 0 {
                    let dark = interval.first_stamp_ms - stream.intervals[k - 1].last_end_stamp_ms;
                    out.push((interval.start_frame as f64 / WAV_RATE, sign * dark));
                }
            }
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out
    }
}

pub fn read(path: &Path) -> Result<CaptureFile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The HAL transport's four-char code, in words.
pub fn transport_label(code: Option<&str>) -> &str {
    match code {
        Some("bltn") => "built-in",
        Some("blue") => "bluetooth",
        Some("blea") => "bluetooth-le",
        Some("usb ") => "usb",
        Some("grup") => "aggregate",
        Some("virt") => "virtual",
        Some("hdmi") => "hdmi",
        Some("dprt") => "displayport",
        Some("airp") => "airplay",
        Some(other) => other,
        None => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sys_gap_raises_the_lag_and_a_resume_moves_it_by_the_difference() {
        let json = r#"{"session_id":"s","capture_timing":{"streams":[
          {"source":"mic","frames_written":1440000,"intervals":[
            {"opened_by":"start","start_frame":0,"first_stamp_ms":120,"last_end_stamp_ms":50120},
            {"opened_by":"resume","start_frame":800000,"first_stamp_ms":60100,"last_end_stamp_ms":100100}]},
          {"source":"sys","frames_written":1426176,"intervals":[
            {"opened_by":"start","start_frame":0,"first_stamp_ms":492,"last_end_stamp_ms":50130,"jumps":[[320000,12]]},
            {"opened_by":"resume","start_frame":794016,"first_stamp_ms":60600,"last_end_stamp_ms":100110}]}]}}"#;
        let file: CaptureFile = serde_json::from_str(json).unwrap();
        let known = file.known_discontinuities();
        assert_eq!(known.len(), 3);
        assert_eq!(known[0], (20.0, 12.0));
        // The resume: sys dark 10 470 ms (+), mic dark 9 980 ms (−).
        assert!(known.iter().any(|&(t, ms)| (t - 49.626).abs() < 1e-9 && (ms - 10_470.0).abs() < 1e-6));
        assert!(known.iter().any(|&(t, ms)| (t - 50.0).abs() < 1e-9 && (ms + 9_980.0).abs() < 1e-6));
    }
}
