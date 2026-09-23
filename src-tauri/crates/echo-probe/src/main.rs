//! `echo-probe` — how a take's mic and system-audio streams line up, and the
//! system audio's echo cancelled out of the mic, offline. See
//! `docs/research/stream-alignment-and-echo.md`.

use echo_probe::aec::{self, AecConfig};
use echo_probe::autocorr::{autocorr_track, AutocorrConfig};
use echo_probe::delay::{lag_track, refine_steps, snap_steps, summarize, DelayConfig, DelayMap, LagPoint, TrackSummary};
use echo_probe::stats::db_ratio;
use echo_probe::synth::{masked_energy, scenario, ScenarioConfig};
use echo_probe::timing::{self, CaptureFile};
use echo_probe::wav::{self, Wav};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
echo-probe — how a take's mic and system streams line up, and its echo removed

  echo-probe session <session-dir> [--out DIR] [--no-cancel]
      The whole measurement for one take: reads mic.wav + sys.wav from a
      recordings/<note>/<session>/ dir, finds the app's capture-<session>.json
      beside it, prints the lag analysis and writes mic-aec.wav + report.json
      into DIR (default ./echo-probe-<session>).

  echo-probe delay <mic.wav> <sys.wav> [--timing FILE] [--window S] [--hop S]
                   [--max-lag MS] [--csv FILE] [--json FILE]
      Lag of the mic behind the system stream, window by window: level,
      polarity, drift, steps. With --timing, split into the capture's start
      offset and the output path, and match steps to recorded gaps.

  echo-probe autocorr <playback.wav> [--window S] [--hop S] [--max-lag MS] [--csv FILE]
      The same lag from a mixed playback.wav alone (a note synced from another
      device keeps nothing else).

  echo-probe cancel <mic.wav> <sys.wav> --out FILE [--timing FILE] [--delay MS]
                    [--tail MS] [--passes N] [--floor DB] [--no-suppress]
                    [--linear-out FILE] [--echo-out FILE] [--report FILE]
      Cancel the system stream's echo out of the mic, for the diarize input.

  echo-probe selftest
      Run synthetic takes with known answers through all of the above.

  echo-probe synth <dir> [--lag MS] [--polarity -1] [--gap AT_S:MS] [--seconds S]
      Write a synthetic take with a known answer — mic.wav, sys.wav and the
      playback.wav the app would mix from them — to try the rest on.

Output WAVs are refused inside Humla's own data directory: audio there is
governed by keep_audio and swept by \"Delete stored audio\".
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]);
    let result = match args.first().map(String::as_str) {
        Some("session") => cmd_session(rest),
        Some("delay") => cmd_delay(rest),
        Some("autocorr") => cmd_autocorr(rest),
        Some("cancel") => cmd_cancel(rest),
        Some("selftest") => cmd_selftest(),
        Some("synth") => cmd_synth(rest),
        None | Some("-h") | Some("--help") | Some("help") => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("echo-probe: {e}");
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

struct Args {
    positional: Vec<String>,
    values: HashMap<String, String>,
    switches: Vec<String>,
}

fn parse(args: &[String], valued: &[&str], switches: &[&str]) -> Result<Args, String> {
    let mut out = Args { positional: Vec::new(), values: HashMap::new(), switches: Vec::new() };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if valued.contains(&a.as_str()) {
            let v = args.get(i + 1).ok_or_else(|| format!("{a} needs a value"))?;
            out.values.insert(a.clone(), v.clone());
            i += 2;
        } else if switches.contains(&a.as_str()) {
            out.switches.push(a.clone());
            i += 1;
        } else if a.starts_with("--") {
            return Err(format!("unknown option {a}"));
        } else {
            out.positional.push(a.clone());
            i += 1;
        }
    }
    Ok(out)
}

impl Args {
    fn f64(&self, name: &str) -> Result<Option<f64>, String> {
        self.values
            .get(name)
            .map(|v| v.parse::<f64>().map_err(|_| format!("{name}: not a number: {v}")))
            .transpose()
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        self.values.get(name).map(PathBuf::from)
    }

    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }

    fn two_paths(&self, what: &str) -> Result<(PathBuf, PathBuf), String> {
        match self.positional.as_slice() {
            [a, b] => Ok((PathBuf::from(a), PathBuf::from(b))),
            _ => Err(format!("expected {what}\n\n{USAGE}")),
        }
    }
}

fn delay_config(a: &Args) -> Result<DelayConfig, String> {
    let mut cfg = DelayConfig::default();
    if let Some(v) = a.f64("--window")? {
        cfg.window_s = v;
    }
    if let Some(v) = a.f64("--hop")? {
        cfg.hop_s = v;
    }
    if let Some(v) = a.f64("--max-lag")? {
        cfg.max_lag_ms = v;
    }
    Ok(cfg)
}

/// Refuse to write audio anywhere under Humla's own data directory. A WAV left
/// there would sit outside everything that governs audio in the app — the
/// `keep_audio` setting and "Delete stored audio" among them. Checked before
/// anything is created, against the nearest part of the path that exists
/// (resolved, so a symlink can't hide it) and against the path as given.
fn refuse_app_data(path: &Path) -> Result<(), String> {
    let inside = |p: &Path| p.components().any(|c| c.as_os_str() == "no.humla.app");
    let mut probe = path.to_path_buf();
    let resolved = loop {
        if let Ok(c) = std::fs::canonicalize(&probe) {
            break c;
        }
        match probe.parent() {
            Some(p) if !p.as_os_str().is_empty() => probe = p.to_path_buf(),
            _ => break std::env::current_dir().map_err(|e| e.to_string())?,
        }
    };
    if inside(&resolved) || inside(path) {
        return Err(format!(
            "refusing to write {} inside Humla's data directory — audio there is governed by \
             keep_audio and swept by \"Delete stored audio\". Write it somewhere else.",
            path.display()
        ));
    }
    Ok(())
}

fn read_pair(mic: &Path, sys: &Path) -> Result<(Wav, Wav), String> {
    let (m, s) = (wav::read(mic)?, wav::read(sys)?);
    if m.rate != s.rate {
        return Err(format!("sample rates differ: mic {} Hz, sys {} Hz", m.rate, s.rate));
    }
    Ok((m, s))
}

// ---------------------------------------------------------------------------
// The analysis shared by delay, cancel and session
// ---------------------------------------------------------------------------

struct Analysis {
    points: Vec<LagPoint>,
    summary: TrackSummary,
    /// Steps moved onto a discontinuity the capture recorded.
    snapped: usize,
}

fn analyze(mic: &Wav, sys: &Wav, cfg: &DelayConfig, capture: Option<&CaptureFile>) -> Analysis {
    let points = lag_track(&mic.samples, &sys.samples, mic.rate, cfg);
    let mut summary = summarize(&points, cfg);
    refine_steps(&mic.samples, &sys.samples, mic.rate, &mut summary, cfg);
    let snapped = capture
        .map(|c| snap_steps(&mut summary, &c.known_discontinuities(), cfg.window_s))
        .unwrap_or(0);
    Analysis { points, summary, snapped }
}

fn ms(v: Option<f64>) -> String {
    v.map(|v| format!("{v:+.2} ms")).unwrap_or_else(|| "—".into())
}

fn print_streams(mic_path: &Path, mic: &Wav, sys_path: &Path, sys: &Wav) {
    println!("streams");
    for (name, path, w) in [("mic", mic_path, mic), ("sys", sys_path, sys)] {
        println!(
            "  {name}  {:>10.3} s  {:>9} frames @ {} Hz{}  {}",
            w.duration_s(),
            w.samples.len(),
            w.rate,
            if w.channels > 1 { format!(", {} ch averaged", w.channels) } else { String::new() },
            path.display()
        );
        if w.size_mismatch {
            println!("       ⚠ its header's data size disagrees with the file — a WAV that was never closed");
        }
    }
    println!(
        "  mic − sys  {:+.1} ms",
        (mic.samples.len() as f64 - sys.samples.len() as f64) / mic.rate as f64 * 1000.0
    );
}

fn print_track(what: &str, cfg_window_s: f64, s: &TrackSummary, min_prominence: f64) {
    println!("{what}");
    println!(
        "  {} windows of {:.1} s, {} with a clear echo (prominence ≥ {min_prominence})",
        s.windows, cfg_window_s, s.valid
    );
    if s.valid == 0 {
        println!("  no echo found — was the call on headphones, or the speakers muted?");
        return;
    }
    println!(
        "  lag        {}  (median; spread ±{:.2} ms)   polarity {}",
        ms(s.median_ms),
        s.spread_ms.unwrap_or(0.0),
        match s.polarity {
            -1 => "inverted",
            1 => "normal",
            _ => "mixed",
        }
    );
    match s.drift_ppm {
        Some(ppm) => println!("  drift      {ppm:+.1} ppm  ({:+.1} ms per hour)", ppm * 3.6),
        None => println!("  drift      — (no segment long enough to measure one)"),
    }
    if s.steps.is_empty() {
        println!("  steps      none");
    }
    for step in &s.steps {
        println!(
            "  step       at {:>8.2} s   {:+.2} → {:+.2} ms  ({:+.2} ms)",
            step.at_s,
            step.from_ms,
            step.to_ms,
            step.to_ms - step.from_ms
        );
    }
    if s.segments.len() > 1 {
        for seg in &s.segments {
            println!(
                "  segment    {:>8.1}–{:<8.1} s  {:+.2} ms over {} windows",
                seg.start_s, seg.end_s, seg.median_ms, seg.windows
            );
        }
    }
}

fn print_capture(path: &Path, capture: &CaptureFile, mic: &Wav, sys: &Wav, analysis: &Analysis) {
    println!("capture timing  ({})", path.display());
    let a = &capture.alignment;
    println!(
        "  sys's first frame {} after mic's; misaligned {} by the end",
        ms(a.start_offset_ms),
        ms(a.end_offset_ms)
    );
    for (name, w) in [("mic", mic), ("sys", sys)] {
        let Some(st) = capture.stream(name) else {
            println!("  {name}  no timing reported");
            continue;
        };
        let gaps: u64 = st.intervals.iter().map(|i| i.gaps).sum();
        let gap_ms: f64 = st.intervals.iter().map(|i| i.gap_ms).sum();
        let jitter = st.intervals.iter().map(|i| i.max_jitter_ms).fold(0.0, f64::max);
        let length = if w.samples.len() as u64 == st.frames_written {
            "WAV length matches ✓".to_string()
        } else {
            format!(
                "⚠ WAV holds {} frames, sidecar wrote {} — rewritten after close?",
                w.samples.len(),
                st.frames_written
            )
        };
        println!(
            "  {name}  {} interval(s), {gaps} gap(s) {gap_ms:.1} ms, jitter ≤ {jitter:.3} ms   {length}",
            st.intervals.len()
        );
        if let Some(first) = st.intervals.first() {
            let late = first.first_arrival_ms - first.first_stamp_ms;
            let verdict = if first.stamp_from_arrival {
                "stamped from arrival (no timestamp on the buffer)"
            } else if (0.0..=500.0).contains(&late) {
                "stamp is host time ✓"
            } else {
                "⚠ stamp and arrival disagree — the stamp may not be host time"
            };
            println!("       first buffer arrived {late:+.1} ms after its stamp: {verdict}");
        }
    }
    let label = |d: &Option<timing::Device>| {
        d.as_ref()
            .map(|d| timing::transport_label(d.transport.as_deref()).to_string())
            .unwrap_or_else(|| "?".into())
    };
    let devices = &capture.capture_timing.devices;
    let same_clock = match (devices.input.as_ref(), devices.output.as_ref()) {
        (Some(i), Some(o)) => match (i.clock_domain, o.clock_domain) {
            (Some(x), Some(y)) if x != 0 && x == y => "yes",
            (Some(_), Some(_)) => "no, or unknown (0)",
            _ => "unknown",
        },
        _ => "unknown",
    };
    println!(
        "  devices  in {} ({})  out {} ({})   shared clock domain: {same_clock}",
        label(&devices.input),
        ms(a.input_latency_ms),
        label(&devices.output),
        ms(a.output_latency_ms),
    );
    if let (Some(lag), Some(offset)) = (analysis.summary.first_ms(), a.start_offset_ms) {
        let hal = match (a.output_latency_ms, a.input_latency_ms) {
            (Some(o), Some(i)) => format!("   (the HAL accounts for {:.1} ms of it)", o + i),
            _ => String::new(),
        };
        println!(
            "  → echo lag {lag:+.2} ms = start offset {offset:+.2} ms + output/acoustic path {:+.2} ms{hal}",
            lag - offset
        );
    }
    if !analysis.summary.steps.is_empty() {
        println!(
            "  → {} of {} step(s) sit on a discontinuity the capture recorded",
            analysis.snapped,
            analysis.summary.steps.len()
        );
    }
}

fn write_csv(path: &Path, points: &[LagPoint]) -> Result<(), String> {
    let mut s = String::from("t_s,lag_ms,prominence,polarity,ref_dbfs,sig_dbfs,valid\n");
    for p in points {
        s.push_str(&format!(
            "{:.3},{:.4},{:.2},{},{:.1},{:.1},{}\n",
            p.t_s, p.lag_ms, p.prominence, p.polarity, p.ref_dbfs, p.sig_dbfs, p.valid
        ));
    }
    std::fs::write(path, s).map_err(|e| format!("{}: {e}", path.display()))
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn cmd_delay(args: &[String]) -> Result<(), String> {
    let a = parse(args, &["--timing", "--window", "--hop", "--max-lag", "--csv", "--json"], &[])?;
    let (mic_path, sys_path) = a.two_paths("<mic.wav> <sys.wav>")?;
    let (mic, sys) = read_pair(&mic_path, &sys_path)?;
    let cfg = delay_config(&a)?;
    let timing_path = a.path("--timing");
    let capture = timing_path.as_deref().map(timing::read).transpose()?;
    let analysis = analyze(&mic, &sys, &cfg, capture.as_ref());
    print_streams(&mic_path, &mic, &sys_path, &sys);
    print_track("echo lag of the mic behind sys (GCC-PHAT)", cfg.window_s, &analysis.summary, cfg.min_prominence);
    if let (Some(path), Some(capture)) = (timing_path.as_deref(), capture.as_ref()) {
        print_capture(path, capture, &mic, &sys, &analysis);
    }
    if let Some(path) = a.path("--csv") {
        write_csv(&path, &analysis.points)?;
    }
    if let Some(path) = a.path("--json") {
        write_json(&path, &serde_json::json!({ "summary": analysis.summary, "points": analysis.points }))?;
    }
    Ok(())
}

fn cmd_autocorr(args: &[String]) -> Result<(), String> {
    let a = parse(args, &["--window", "--hop", "--max-lag", "--csv"], &[])?;
    let [path] = a.positional.as_slice() else {
        return Err(format!("expected <playback.wav>\n\n{USAGE}"));
    };
    let path = PathBuf::from(path);
    let mix = wav::read(&path)?;
    let mut cfg = AutocorrConfig::default();
    if let Some(v) = a.f64("--window")? {
        cfg.window_s = v;
    }
    if let Some(v) = a.f64("--hop")? {
        cfg.hop_s = v;
    }
    if let Some(v) = a.f64("--max-lag")? {
        cfg.max_lag_ms = v;
    }
    let points = autocorr_track(&mix.samples, mix.rate, &cfg);
    let summary = summarize(&points, &DelayConfig::default());
    println!("mix  {:.3} s @ {} Hz  {}", mix.duration_s(), mix.rate, path.display());
    print_track("echo lag within the mix (onset autocorrelation, cepstral refinement)", cfg.window_s, &summary, cfg.min_prominence);
    if let Some(csv) = a.path("--csv") {
        write_csv(&csv, &points)?;
    }
    Ok(())
}

struct CancelSettings {
    aec: AecConfig,
    delay_ms: Option<f64>,
}

fn cancel_settings(a: &Args) -> Result<CancelSettings, String> {
    let mut aec = AecConfig::default();
    if let Some(v) = a.f64("--tail")? {
        aec.tail_ms = v;
    }
    if let Some(v) = a.f64("--passes")? {
        aec.passes = v.max(1.0) as usize;
    }
    if let Some(v) = a.f64("--floor")? {
        aec.floor_db = v;
    }
    aec.suppress = !a.has("--no-suppress");
    Ok(CancelSettings { aec, delay_ms: a.f64("--delay")? })
}

fn run_cancel(
    mic: &Wav,
    sys: &Wav,
    analysis: &Analysis,
    settings: &CancelSettings,
) -> Result<aec::AecOutput, String> {
    let map = match settings.delay_ms {
        Some(ms) => DelayMap::constant(ms),
        None => DelayMap::from_summary(&analysis.summary)
            .ok_or("no echo found to align on — pass --delay MS to cancel anyway")?,
    };
    let reference = aec::align_reference(&sys.samples, mic.samples.len(), mic.rate, &map, settings.aec.margin_ms);
    Ok(aec::cancel(&mic.samples, &reference, mic.rate, &settings.aec))
}

fn print_aec(r: &aec::AecReport) {
    let db = |v: Option<f64>| v.map(|v| format!("{v:.1} dB")).unwrap_or_else(|| "—".into());
    println!("echo cancellation  ({} partitions of the filter, {} blocks)", r.partitions, r.blocks);
    println!("  echo in the mic, against sys:      {}", db(r.erl_db));
    println!(
        "  echo removed where it dominates:   median {} linear, {} suppressed; best 10% {}   ({} blocks of 16 ms)",
        db(r.erle_linear_db),
        db(r.erle_db),
        db(r.erle_p90_db),
        r.echo_blocks
    );
    println!(
        "  the mic alone, level change:       {}   ({} blocks; ~0 dB means untouched)",
        db(r.near_change_db),
        r.near_blocks
    );
}

fn cmd_cancel(args: &[String]) -> Result<(), String> {
    let a = parse(
        args,
        &[
            "--out", "--timing", "--delay", "--tail", "--passes", "--floor", "--linear-out", "--echo-out",
            "--report",
        ],
        &["--no-suppress"],
    )?;
    let (mic_path, sys_path) = a.two_paths("<mic.wav> <sys.wav>")?;
    let out_path = a.path("--out").ok_or("--out FILE is required")?;
    for p in [Some(&out_path), a.path("--linear-out").as_ref(), a.path("--echo-out").as_ref()].into_iter().flatten() {
        refuse_app_data(p)?;
    }
    let (mic, sys) = read_pair(&mic_path, &sys_path)?;
    let cfg = DelayConfig::default();
    let capture = a.path("--timing").as_deref().map(timing::read).transpose()?;
    let analysis = analyze(&mic, &sys, &cfg, capture.as_ref());
    let settings = cancel_settings(&a)?;
    let out = run_cancel(&mic, &sys, &analysis, &settings)?;
    wav::write_pcm16(&out_path, mic.rate, &out.cleaned)?;
    if let Some(p) = a.path("--linear-out") {
        wav::write_pcm16(&p, mic.rate, &out.linear)?;
    }
    if let Some(p) = a.path("--echo-out") {
        wav::write_pcm16(&p, mic.rate, &out.echo)?;
    }
    print_track("echo lag of the mic behind sys (GCC-PHAT)", cfg.window_s, &analysis.summary, cfg.min_prominence);
    print_aec(&out.report);
    println!("wrote {}", out_path.display());
    if let Some(p) = a.path("--report") {
        write_json(&p, &serde_json::json!({ "delay": analysis.summary, "aec": out.report }))?;
    }
    Ok(())
}

fn cmd_session(args: &[String]) -> Result<(), String> {
    let a = parse(args, &["--out"], &["--no-cancel"])?;
    let [dir] = a.positional.as_slice() else {
        return Err(format!("expected <session-dir>\n\n{USAGE}"));
    };
    let dir = std::fs::canonicalize(dir).map_err(|e| format!("{dir}: {e}"))?;
    let (mic_path, sys_path) = (dir.join("mic.wav"), dir.join("sys.wav"));
    if !mic_path.exists() || !sys_path.exists() {
        let hint = if dir.join("playback.wav").exists() {
            "only playback.wav is here (a synced take, or one recorded with keep_audio off) — \
             try `echo-probe autocorr playback.wav`"
        } else {
            "record the take with keep_audio on (Settings → Recording → Audio retention)"
        };
        return Err(format!("{}: needs mic.wav and sys.wav; {hint}", dir.display()));
    }
    // recordings/<note>/<session>/, or recordings/<note>/ for a legacy flat take.
    let name = |p: &Path| p.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
    let parent = dir.parent().unwrap_or(&dir).to_path_buf();
    let (note_id, session_id, app_data) = if name(&parent) == "recordings" {
        (name(&dir), None, parent.parent().map(Path::to_path_buf))
    } else {
        let app = parent.parent().and_then(Path::parent).map(Path::to_path_buf);
        (name(&parent), Some(name(&dir)), app)
    };
    let timing_path = match (&app_data, &session_id) {
        (Some(app), Some(sid)) => Some(app.join("diagnostics").join(&note_id).join(format!("capture-{sid}.json"))),
        _ => None,
    }
    .filter(|p| p.exists());
    let capture = timing_path.as_deref().map(timing::read).transpose()?;

    let (mic, sys) = read_pair(&mic_path, &sys_path)?;
    let cfg = DelayConfig::default();
    let analysis = analyze(&mic, &sys, &cfg, capture.as_ref());
    print_streams(&mic_path, &mic, &sys_path, &sys);
    print_track("echo lag of the mic behind sys (GCC-PHAT)", cfg.window_s, &analysis.summary, cfg.min_prominence);
    match (&timing_path, &capture) {
        (Some(path), Some(capture)) => print_capture(path, capture, &mic, &sys, &analysis),
        _ => println!(
            "capture timing   none found — the take predates the capture_timing diagnostic, \
             or its diagnostics folder was cleared"
        ),
    }

    let out_dir = a
        .path("--out")
        .unwrap_or_else(|| PathBuf::from(format!("echo-probe-{}", session_id.as_deref().unwrap_or(&note_id))));
    let (aec_path, linear_path) = (out_dir.join("mic-aec.wav"), out_dir.join("mic-aec-linear.wav"));
    refuse_app_data(&aec_path)?;
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let mut report = serde_json::json!({
        "session_dir": dir,
        "timing_file": timing_path,
        "mic_s": mic.duration_s(),
        "sys_s": sys.duration_s(),
        "delay": analysis.summary,
        "points": analysis.points,
    });
    if !a.has("--no-cancel") {
        let settings = CancelSettings { aec: AecConfig::default(), delay_ms: None };
        match run_cancel(&mic, &sys, &analysis, &settings) {
            Ok(out) => {
                wav::write_pcm16(&aec_path, mic.rate, &out.cleaned)?;
                wav::write_pcm16(&linear_path, mic.rate, &out.linear)?;
                print_aec(&out.report);
                println!("wrote {} (suppressed) and {} (linear only)", aec_path.display(), linear_path.display());
                report["aec"] = serde_json::to_value(&out.report).unwrap_or_default();
            }
            Err(e) => println!("echo cancellation skipped: {e}"),
        }
    }
    let report_path = out_dir.join("report.json");
    write_json(&report_path, &report)?;
    println!("wrote {}", report_path.display());
    Ok(())
}

/// Known-answer takes through the whole pipeline, to check a build before
/// trusting what it says about a real one.
fn cmd_selftest() -> Result<(), String> {
    let mut failed = 0;
    let cases = [
        ("inverted echo, two remote voices, 157 ms", ScenarioConfig { polarity: -1.0, ..Default::default() }, 157.0),
        (
            "420 ms, with a 12 ms sys gap at 50 s",
            ScenarioConfig { lag_ms: 420.0, gap: Some((50.0, 12.0)), dur_s: 120.0, ..Default::default() },
            420.0,
        ),
        ("soft-clipped speaker", ScenarioConfig { nonlinear: true, ..Default::default() }, 157.0),
    ];
    for (name, cfg, lag_ms) in cases {
        let s = scenario(&cfg);
        let mic = Wav { rate: s.rate, channels: 1, samples: s.mic.clone(), size_mismatch: false };
        let sys = Wav { rate: s.rate, channels: 1, samples: s.sys.clone(), size_mismatch: false };
        let dc = DelayConfig::default();
        let analysis = analyze(&mic, &sys, &dc, None);
        let settings = CancelSettings { aec: AecConfig::default(), delay_ms: None };
        let out = run_cancel(&mic, &sys, &analysis, &settings)?;
        let skip = |m: Vec<bool>| m.into_iter().enumerate().map(|(i, v)| v && i >= 1000).collect::<Vec<_>>();
        let far = skip(s.far_only());
        let erle = db_ratio(masked_energy(&s.echo, &far), masked_energy(&out.cleaned, &far)).unwrap_or(0.0);
        let found = analysis.summary.first_ms().unwrap_or(f64::NAN);
        let ok = (found - lag_ms).abs() < 0.5 && erle > 18.0;
        failed += usize::from(!ok);
        println!(
            "{} {name}: lag {found:+.2} ms (true {lag_ms:+.1}), steps {}, echo removed {erle:.1} dB",
            if ok { "ok  " } else { "FAIL" },
            analysis.summary.steps.len()
        );
    }
    if failed > 0 {
        return Err(format!("{failed} self-test case(s) failed"));
    }
    Ok(())
}

fn cmd_synth(args: &[String]) -> Result<(), String> {
    let a = parse(args, &["--lag", "--polarity", "--gap", "--seconds"], &[])?;
    let [dir] = a.positional.as_slice() else {
        return Err(format!("expected <dir>\n\n{USAGE}"));
    };
    let dir = PathBuf::from(dir);
    let mut cfg = ScenarioConfig::default();
    if let Some(v) = a.f64("--lag")? {
        cfg.lag_ms = v;
    }
    if let Some(v) = a.f64("--polarity")? {
        cfg.polarity = v.signum();
    }
    if let Some(v) = a.f64("--seconds")? {
        cfg.dur_s = v;
    }
    if let Some(gap) = a.values.get("--gap") {
        let (at, len) = gap.split_once(':').ok_or("--gap wants AT_S:MS, e.g. 50:12")?;
        let parse = |v: &str| v.parse::<f64>().map_err(|_| format!("--gap: not a number: {v}"));
        cfg.gap = Some((parse(at)?, parse(len)?));
    }
    refuse_app_data(&dir.join("mic.wav"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let s = scenario(&cfg);
    // What `build_playback_wav` makes: the two streams summed from index 0.
    let mix: Vec<f32> = s.mic.iter().zip(&s.sys).map(|(m, x)| (m + x) * 0.5).collect();
    wav::write_pcm16(&dir.join("mic.wav"), s.rate, &s.mic)?;
    wav::write_pcm16(&dir.join("sys.wav"), s.rate, &s.sys)?;
    wav::write_pcm16(&dir.join("playback.wav"), s.rate, &mix)?;
    println!(
        "wrote {} — {:.0} s, echo {:+.1} ms behind sys{}{}",
        dir.display(),
        cfg.dur_s,
        cfg.lag_ms,
        if cfg.polarity < 0.0 { ", inverted" } else { "" },
        cfg.gap.map(|(at, ms)| format!(", sys loses {ms} ms at {at} s")).unwrap_or_default()
    );
    Ok(())
}
