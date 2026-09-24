//! Keeps the speakers' echo out of the mic's voices (#196).
//!
//! When a call plays through the laptop's speakers the mic records every
//! remote voice a second time, and the mic diarize hears that echo as people.
//! Two things keep it out. The system stream is the far-end signal, so the
//! echo is cancelled out of the mic before it is diarized; and echo can only
//! sound while the system stream does, so a mic voice that is almost never
//! heard while the system stream is silent is dropped as echo.

use crate::diarize::Segment;
use echo_probe::aec::{self, AecConfig, AecReport};
use echo_probe::delay::{self, DelayConfig, DelayMap, LagPoint, TrackSummary};

/// Every stream the sidecar writes is 16 kHz mono.
const RATE: u32 = 16_000;

/// Samples in one of the check's 10 ms frames.
const FRAME: usize = 160;

/// A window's lag counts only when this many of the valid windows either side
/// of it, up to [`NEIGHBOURS`] away, find the same one.
const AGREEING: usize = 2;
const NEIGHBOURS: usize = 3;

/// The check reads the mic in 10 ms frames.
const FRAME_MS: u64 = 10;

/// A voice heard in sys silence less than half as often as the take is, is
/// echo. Measured: echo voices 0.00–0.36, the user 0.67–3.55.
const ECHO_RATIO: f64 = 0.5;

/// A voice that lost this much of its energy to the linear canceller is echo.
/// Measured: the user 0.9–3.8 dB (the top of it double talk), echo voices
/// 5.1–26 dB.
const ECHO_REMOVED_DB: f64 = 4.5;

/// Below this much sys silence in a take, the share a voice is measured
/// against is too thin to judge by. Test audio had about 8 s.
const MIN_SYS_SILENCE_MS: u64 = 5_000;

/// Sys sounds in a frame within this many dB of its 95th-percentile frame...
const SYS_ACTIVE_BELOW_P95_DB: f64 = 40.0;
/// ...and for 150 ms either side of one.
const SYS_HANG_FRAMES: usize = 15;

/// One take's echo analysis.
pub struct TakeAnalysis {
    /// Where the echo lands in the mic, window by window, reduced to a level.
    pub lag: TrackSummary,
    /// `None` when no window showed an echo — headphones, or muted speakers —
    /// so there is nothing to cancel and no voice to suspect.
    pub echo: Option<TakeEcho>,
}

impl TakeAnalysis {
    /// `segments` — the take's mic voices — without the ones that read as
    /// echo, and what the diarize dump records of it.
    pub fn check(&self, segments: Vec<Segment>) -> (Vec<Segment>, EchoReport) {
        let check = match &self.echo {
            Some(echo) if !segments.is_empty() => Some(check_voices(&segments, &echo.evidence())),
            _ => None,
        };
        let kept = match &check {
            Some(c) => c.keep(segments),
            None => segments,
        };
        (kept, self.report(check))
    }

    /// What the diarize dump records of this take, with `check`'s verdicts.
    pub fn report(&self, check: Option<VoiceCheck>) -> EchoReport {
        EchoReport {
            lag: self.lag.clone(),
            cancel: self.echo.as_ref().map(|e| e.cancel.clone()),
            check,
        }
    }
}

/// What the diarize dump keeps of one take's echo, so the thresholds can be
/// revisited on real meetings. Text only.
#[derive(Clone, Debug, serde::Serialize)]
pub struct EchoReport {
    /// Where the echo lands in the mic: level, spread, polarity, drift, steps.
    pub lag: TrackSummary,
    /// How much echo the canceller removed.
    pub cancel: Option<AecReport>,
    /// `None` when there was no echo to check against, or no voice to check.
    pub check: Option<VoiceCheck>,
}

pub struct TakeEcho {
    /// Per 10 ms frame of the mic: the system stream sounds there.
    sys_active: Vec<bool>,
    /// Per 10 ms frame: the mic's energy, and the linear canceller's output's.
    mic_energy: Vec<f64>,
    linear_energy: Vec<f64>,
    /// The mic after the canceller and its residual suppressor.
    cleaned: Vec<f32>,
    cancel: AecReport,
}

impl TakeEcho {
    pub fn evidence(&self) -> Evidence<'_> {
        Evidence {
            sys_active: &self.sys_active,
            energy: Some((&self.mic_energy, &self.linear_energy)),
        }
    }

    /// The mic with the echo cancelled out of it — the diarize input. Taken
    /// rather than lent, so it can be let go of as soon as it is written.
    pub fn take_cleaned(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.cleaned)
    }
}

/// What the check reads, per 10 ms frame of the mic.
pub struct Evidence<'a> {
    /// The system stream sounds here, once shifted by the take's lag.
    pub sys_active: &'a [bool],
    /// The mic's energy here and the linear canceller's output's, when the
    /// canceller ran.
    pub energy: Option<(&'a [f64], &'a [f64])>,
}

/// One mic voice as the check read it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct VoiceVerdict {
    pub speaker_id: String,
    pub seconds: f64,
    /// Its share of frames in sys silence over the take's. `None` when sys is
    /// never silent.
    pub ratio: Option<f64>,
    /// How much of its energy the linear canceller removed.
    pub energy_removed_db: Option<f64>,
    pub dropped: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct VoiceCheck {
    /// How long sys is silent in the take — what a voice is measured against.
    pub sys_silent_s: f64,
    /// Too little of that to measure against, so no voice was judged.
    pub skipped: bool,
    pub voices: Vec<VoiceVerdict>,
    /// Every voice read as echo, so all of them were kept: a take always has
    /// someone at the mic, and one merged voice is what this protects.
    pub kept_all: bool,
}

impl VoiceCheck {
    /// `segments` without the voices the check dropped.
    pub fn keep(&self, segments: Vec<Segment>) -> Vec<Segment> {
        let dropped: std::collections::HashSet<&str> = self
            .voices
            .iter()
            .filter(|v| v.dropped)
            .map(|v| v.speaker_id.as_str())
            .collect();
        segments.into_iter().filter(|s| !dropped.contains(s.speaker_id.as_str())).collect()
    }
}

/// Which of `segments`' voices are echo.
///
/// A person talks independently of sys, or in its pauses, so the share of
/// their frames in sys silence is about the take's or more. An echo voice can
/// only sound while sys does, so its share is near nothing.
pub fn check_voices(segments: &[Segment], evidence: &Evidence) -> VoiceCheck {
    let frames = evidence.sys_active.len();
    let silent = evidence.sys_active.iter().filter(|a| !**a).count();
    let base = silent as f64 / frames.max(1) as f64;
    let skipped = (silent as u64) * FRAME_MS < MIN_SYS_SILENCE_MS;
    let mut by_voice: std::collections::BTreeMap<&str, Vec<&Segment>> = Default::default();
    for s in segments {
        by_voice.entry(&s.speaker_id).or_default().push(s);
    }
    let mut voices: Vec<VoiceVerdict> = by_voice
        .into_iter()
        .map(|(id, segs)| {
            let on = voice_frames(&segs, frames);
            let heard = on.iter().filter(|v| **v).count().max(1);
            let in_silence = on
                .iter()
                .zip(evidence.sys_active)
                .filter(|(v, a)| **v && !**a)
                .count();
            let ratio = (base > 0.0).then(|| in_silence as f64 / heard as f64 / base);
            let energy_removed_db =
                evidence.energy.and_then(|(mic, linear)| energy_removed(&on, mic, linear));
            // The energy only ever adds to what the ratio finds: on a mic the
            // canceller already cleaned, what is left of an echo voice is where
            // it removed little.
            let echo = ratio.is_some_and(|r| r < ECHO_RATIO)
                || energy_removed_db.is_some_and(|db| db >= ECHO_REMOVED_DB);
            VoiceVerdict {
                speaker_id: id.to_string(),
                seconds: heard as f64 * FRAME_MS as f64 / 1000.0,
                ratio,
                energy_removed_db,
                dropped: !skipped && echo,
            }
        })
        .collect();
    voices.sort_by(|a, b| b.seconds.total_cmp(&a.seconds));
    let kept_all = !voices.is_empty() && voices.iter().all(|v| v.dropped);
    if kept_all {
        voices.iter_mut().for_each(|v| v.dropped = false);
    }
    VoiceCheck {
        sys_silent_s: (silent as u64 * FRAME_MS) as f64 / 1000.0,
        skipped,
        voices,
        kept_all,
    }
}

/// One take that had a system stream, inside a mic stream that joins several
/// takes end to end: where it starts and ends there, in ms.
pub struct TakeSpan<'a> {
    pub start_ms: u64,
    pub end_ms: u64,
    pub evidence: Evidence<'a>,
}

/// The check over voices diarized from takes joined end to end. Each take
/// judges the voices heard inside it against its own sys silence, so a voice
/// that is echo in one take and a person in another is dropped only where it
/// was echo. A take with no system stream isn't in `takes` and is never judged.
pub fn check_joined(segments: &[Segment], takes: &[TakeSpan]) -> (Vec<Segment>, Vec<VoiceCheck>) {
    let mut cuts: Vec<(u64, u64, String)> = Vec::new();
    let mut checks = Vec::with_capacity(takes.len());
    for take in takes {
        let (start, end) = (take.start_ms, take.end_ms);
        let local: Vec<Segment> = segments
            .iter()
            .filter(|s| s.start_ms < end && s.end_ms > start)
            .map(|s| Segment {
                start_ms: s.start_ms.max(start) - start,
                end_ms: s.end_ms.min(end) - start,
                speaker_id: s.speaker_id.clone(),
            })
            .collect();
        let check = check_voices(&local, &take.evidence);
        cuts.extend(check.voices.iter().filter(|v| v.dropped).map(|v| (start, end, v.speaker_id.clone())));
        checks.push(check);
    }
    let mut kept = Vec::with_capacity(segments.len());
    for s in segments {
        let mut pieces = vec![(s.start_ms, s.end_ms)];
        for (cut_start, cut_end, id) in &cuts {
            if *id != s.speaker_id {
                continue;
            }
            pieces = pieces
                .into_iter()
                .flat_map(|(a, b)| [(a, b.min(*cut_start)), (a.max(*cut_end), b)])
                .filter(|(a, b)| b > a)
                .collect();
        }
        kept.extend(pieces.into_iter().map(|(start_ms, end_ms)| Segment {
            start_ms,
            end_ms,
            speaker_id: s.speaker_id.clone(),
        }));
    }
    (kept, checks)
}

/// How much of the mic's energy over the frames `on` marks the linear
/// canceller removed, in dB.
fn energy_removed(on: &[bool], mic: &[f64], linear: &[f64]) -> Option<f64> {
    let (mut before, mut after) = (0.0, 0.0);
    for ((&v, &m), &l) in on.iter().zip(mic).zip(linear) {
        if v {
            before += m;
            after += l;
        }
    }
    (before > 0.0).then(|| 10.0 * (before / after.max(1e-20)).log10())
}

/// The 10 ms frames any of `segments` covers, out of `frames`.
fn voice_frames(segments: &[&Segment], frames: usize) -> Vec<bool> {
    let mut on = vec![false; frames];
    for s in segments {
        let first = (s.start_ms / FRAME_MS) as usize;
        let last = (s.end_ms.div_ceil(FRAME_MS) as usize).min(frames);
        for v in on.iter_mut().take(last).skip(first) {
            *v = true;
        }
    }
    on
}

/// The lag of `mic` behind `sys`, the echo cancelled out of `mic`, and what
/// the check needs from all three.
pub fn analyze_take(mic: Vec<f32>, sys: Vec<f32>) -> TakeAnalysis {
    let cfg = DelayConfig::default();
    let mut points = delay::lag_track(&mic, &sys, RATE, &cfg);
    keep_agreeing(&mut points, cfg.step_ms);
    let mut lag = delay::summarize(&points, &cfg);
    if lag.valid == 0 {
        return TakeAnalysis { lag, echo: None };
    }
    delay::refine_steps(&mic, &sys, RATE, &mut lag, &cfg);
    let Some(map) = DelayMap::from_summary(&lag) else {
        return TakeAnalysis { lag, echo: None };
    };
    let sys_active = at_mic(&sys_activity(&sys), mic.len().div_ceil(FRAME), &map);
    let aec_cfg = AecConfig::default();
    let reference = aec::align_reference(&sys, mic.len(), RATE, &map, aec_cfg.margin_ms);
    drop(sys);
    let aec::AecOutput { linear, cleaned, report, .. } = aec::cancel(&mic, &reference, RATE, &aec_cfg);
    drop(reference);
    let echo = TakeEcho {
        sys_active,
        mic_energy: frame_energy(&mic),
        linear_energy: frame_energy(&linear),
        cleaned,
        cancel: report,
    };
    TakeAnalysis { lag, echo: Some(echo) }
}

/// Per 10 ms frame of `sys`: it sounds there. Taken from its own energy rather
/// than from a diarize of it, because how an engine pads its segments decides
/// how much silence is left to measure against.
fn sys_activity(sys: &[f32]) -> Vec<bool> {
    let energy = frame_energy(sys);
    let mut sorted = energy.clone();
    sorted.sort_by(f64::total_cmp);
    let p95 = sorted.get(sorted.len() * 95 / 100).copied().unwrap_or(0.0);
    let floor = p95 * 10f64.powf(-SYS_ACTIVE_BELOW_P95_DB / 10.0);
    let loud: Vec<bool> = energy.iter().map(|&e| e > floor).collect();
    (0..loud.len())
        .map(|f| loud[f.saturating_sub(SYS_HANG_FRAMES)..(f + SYS_HANG_FRAMES + 1).min(loud.len())].contains(&true))
        .collect()
}

/// Sum of squares per 10 ms frame, the last one partial.
fn frame_energy(x: &[f32]) -> Vec<f64> {
    x.chunks(FRAME).map(|c| c.iter().map(|&v| v as f64 * v as f64).sum()).collect()
}

/// `sys_active` carried to the `frames` 10 ms frames of the mic: each reads
/// the sys frame its echo would come from, per `map`.
fn at_mic(sys_active: &[bool], frames: usize, map: &DelayMap) -> Vec<bool> {
    (0..frames)
        .map(|f| {
            let centre_ms = (f as u64 * FRAME_MS + FRAME_MS / 2) as f64;
            let from_ms = centre_ms - map.lag_ms_at(centre_ms / 1000.0);
            from_ms >= 0.0 && sys_active.get((from_ms / FRAME_MS as f64) as usize) == Some(&true)
        })
        .collect()
}

/// Voiced speech gives GCC-PHAT sharp peaks at chance lags, often over the
/// prominence bar, while an echo's lag holds from window to window. So a valid
/// window whose neighbours don't find its lag is not evidence of an echo.
fn keep_agreeing(points: &mut [LagPoint], tolerance_ms: f64) {
    let valid: Vec<usize> = (0..points.len()).filter(|&i| points[i].valid).collect();
    let lone: Vec<usize> = (0..valid.len())
        .filter(|&p| {
            let lag = points[valid[p]].lag_ms;
            let near = p.saturating_sub(NEIGHBOURS)..(p + NEIGHBOURS + 1).min(valid.len());
            near.filter(|&q| q != p)
                .filter(|&q| (points[valid[q]].lag_ms - lag).abs() <= tolerance_ms)
                .count()
                < AGREEING
        })
        .collect();
    for p in lone {
        points[valid[p]].valid = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use echo_probe::synth::{masked_energy, scenario, Scenario, ScenarioConfig};

    /// A synthetic take whose voices are levelled as speech is, since the check
    /// tells the system stream's speech from its silence by energy.
    fn take(cfg: ScenarioConfig) -> Scenario {
        scenario(&ScenarioConfig { levelled: true, ..cfg })
    }

    #[test]
    fn finds_the_takes_own_lag_behind_an_inverting_echo() {
        let s = take(ScenarioConfig { lag_ms: 157.3, polarity: -1.0, ..Default::default() });
        let analysis = analyze_take(s.mic, s.sys);
        let lag = analysis.lag.median_ms.expect("a lag");
        assert!((lag - 157.3).abs() < 0.5, "lag {lag}");
        assert!(analysis.echo.is_some());
    }

    #[test]
    fn cancels_the_echo_out_of_the_mic_and_leaves_the_user_alone() {
        let s = take(ScenarioConfig { lag_ms: 323.0, polarity: -1.0, ..Default::default() });
        let mut analysis = analyze_take(s.mic.clone(), s.sys.clone());
        let cleaned = analysis.echo.as_mut().expect("an echo").take_cleaned();
        assert_eq!(cleaned.len(), s.mic.len());
        // From 10 s on: the first pass converges, and a score over the opening
        // would measure that rather than the filter.
        let from_10s = |m: Vec<bool>| -> Vec<bool> {
            m.into_iter().enumerate().map(|(i, v)| v && i >= 1000).collect()
        };
        let (far, near) = (from_10s(s.far_only()), from_10s(s.near_only()));
        let db = |num: f64, den: f64| 10.0 * (num / den).log10();
        let removed = db(masked_energy(&s.echo, &far), masked_energy(&cleaned, &far));
        let user = db(masked_energy(&cleaned, &near), masked_energy(&s.mic, &near));
        assert!(removed > 25.0, "echo removed {removed:.1} dB");
        assert!(user.abs() < 0.5, "the user alone {user:+.2} dB");
        let report = analysis.check(Vec::new()).1.cancel.expect("the canceller's report");
        assert!(report.erle_linear_db.unwrap() > 20.0, "{report:?}");
    }

    #[test]
    fn a_take_with_no_echo_has_nothing_to_check() {
        // The user alone on the mic, the remote voices alone on sys: nothing
        // correlates the two, as on headphones.
        for seed in [1, 2, 3, 7] {
            let s = take(ScenarioConfig { seed, ..Default::default() });
            let mic: Vec<f32> = s.mic.iter().zip(&s.echo).map(|(m, e)| m - e).collect();
            let analysis = analyze_take(mic, s.sys);
            assert_eq!(analysis.lag.valid, 0, "seed {seed}: {:?}", analysis.lag);
            assert!(analysis.echo.is_none());
        }
    }

    fn seg(start_s: f64, end_s: f64, id: &str) -> Segment {
        Segment {
            start_ms: (start_s * 1000.0) as u64,
            end_ms: (end_s * 1000.0) as u64,
            speaker_id: id.to_string(),
        }
    }

    /// Per 10 ms frame of a `len_s` take: sys sounds inside `spans`.
    fn active(len_s: f64, spans: &[(f64, f64)]) -> Vec<bool> {
        (0..(len_s * 100.0) as usize)
            .map(|f| {
                let t = f as f64 / 100.0;
                spans.iter().any(|&(a, b)| t >= a && t < b)
            })
            .collect()
    }

    fn ids(segments: &[Segment]) -> Vec<&str> {
        let mut out: Vec<&str> = segments.iter().map(|s| s.speaker_id.as_str()).collect();
        out.sort();
        out.dedup();
        out
    }

    #[test]
    fn drops_the_echo_voice_and_keeps_both_room_voices() {
        // Sys sounds for 40 of 60 s. Two people in the room talk in its pauses
        // and over it; the echo only ever sounds while it does.
        let sys_active = active(60.0, &[(0.0, 20.0), (30.0, 50.0)]);
        let segments = vec![
            seg(1.0, 4.0, "echo"),
            seg(5.0, 8.0, "anna"),
            seg(20.5, 25.0, "anna"),
            seg(31.0, 45.0, "echo"),
            seg(35.0, 40.0, "bjorn"),
            seg(52.0, 58.0, "bjorn"),
        ];
        let check = check_voices(&segments, &Evidence { sys_active: &sys_active, energy: None });
        let dropped: Vec<&str> =
            check.voices.iter().filter(|v| v.dropped).map(|v| v.speaker_id.as_str()).collect();
        assert_eq!(dropped, vec!["echo"]);
        assert_eq!(ids(&check.keep(segments)), vec!["anna", "bjorn"]);
    }

    #[test]
    fn keeps_every_voice_when_all_of_them_read_as_echo() {
        // Community-1 merges the user and the echo into one voice, mostly
        // echo. No check can split it, and dropping it would leave nobody.
        let sys_active = active(60.0, &[(0.0, 50.0)]);
        let segments = vec![seg(0.0, 48.0, "merged"), seg(50.5, 51.0, "merged"), seg(10.0, 20.0, "echo")];
        let check = check_voices(&segments, &Evidence { sys_active: &sys_active, energy: None });
        assert!(check.voices.iter().all(|v| v.ratio.unwrap() < 0.5), "{check:?}");
        assert!(check.voices.iter().all(|v| !v.dropped), "{check:?}");
        assert!(check.kept_all);
        assert_eq!(check.keep(segments).len(), 3);
    }

    #[test]
    fn judges_nothing_when_sys_is_silent_for_under_five_seconds() {
        // 4 s of silence in a minute: a share too thin to measure a voice
        // against, however clearly one reads as echo.
        let sys_active = active(60.0, &[(0.0, 30.0), (34.0, 60.0)]);
        let segments = vec![seg(1.0, 20.0, "echo"), seg(30.0, 34.0, "user"), seg(40.0, 45.0, "user")];
        let check = check_voices(&segments, &Evidence { sys_active: &sys_active, energy: None });
        assert!(check.skipped);
        assert!((check.sys_silent_s - 4.0).abs() < 0.05, "{}", check.sys_silent_s);
        assert!(check.voices.iter().all(|v| !v.dropped), "{check:?}");
        assert_eq!(check.keep(segments.clone()).len(), 3);

        let sys_active = active(60.0, &[(0.0, 30.0), (36.0, 60.0)]);
        let check = check_voices(&segments, &Evidence { sys_active: &sys_active, energy: None });
        assert!(!check.skipped);
        assert!(check.voices.iter().any(|v| v.dropped), "{check:?}");
    }

    #[test]
    fn energy_the_canceller_took_catches_what_the_ratio_missed_and_never_saves_a_flagged_voice() {
        // Per voice, how much of the mic's energy the linear canceller took:
        // the user little, a 1 s echo fragment that happens to sit in a sys
        // pause a lot, and an echo voice the cancelled mic under-reads.
        let sys_active = active(60.0, &[(0.0, 20.0), (30.0, 50.0)]);
        let segments = vec![
            seg(16.0, 19.0, "user"),
            seg(20.5, 28.0, "user"),
            seg(1.0, 15.0, "echo"),
            seg(51.0, 52.0, "fragment"),
        ];
        let mic = vec![1.0; 6000];
        let mut linear = mic.clone();
        for (id, removed_db) in [("user", 1.0), ("fragment", 5.2), ("echo", 3.0)] {
            for s in segments.iter().filter(|s| s.speaker_id == id) {
                for f in (s.start_ms / 10) as usize..(s.end_ms / 10) as usize {
                    linear[f] = 10f64.powf(-removed_db / 10.0);
                }
            }
        }
        let check = check_voices(
            &segments,
            &Evidence { sys_active: &sys_active, energy: Some((&mic, &linear)) },
        );
        let verdict = |id: &str| check.voices.iter().find(|v| v.speaker_id == id).unwrap();
        assert!(!verdict("user").dropped, "{check:?}");
        assert!((verdict("user").energy_removed_db.unwrap() - 1.0).abs() < 0.05);
        assert!(verdict("fragment").ratio.unwrap() > 1.0 && verdict("fragment").dropped, "{check:?}");
        assert!(verdict("echo").dropped, "3 dB doesn't overrule a ratio of {:?}", verdict("echo").ratio);
    }

    /// A segment per run of `mask`'s 10 ms frames, named by `name(run)`.
    fn runs(mask: &[bool], name: impl Fn(usize) -> String) -> Vec<Segment> {
        let mut out = Vec::new();
        let mut start = None;
        for (f, &on) in mask.iter().chain(std::iter::once(&false)).enumerate() {
            match (on, start) {
                (true, None) => start = Some(f),
                (false, Some(s)) => {
                    out.push(Segment {
                        start_ms: s as u64 * 10,
                        end_ms: f as u64 * 10,
                        speaker_id: name(out.len()),
                    });
                    start = None;
                }
                _ => {}
            }
        }
        out
    }

    /// A diarizer that hears the synthetic take exactly as it was made: the
    /// user where they spoke, and the remote side's echo as two voices.
    fn truth(s: &Scenario) -> Vec<Segment> {
        let mut segments = runs(&s.near_active, |_| "user".into());
        segments.extend(runs(&s.echo_active, |i| format!("remote-{}", i % 2 + 1)));
        segments
    }

    #[test]
    fn reads_the_echo_voices_off_a_synthetic_take_and_keeps_the_user() {
        let s = take(ScenarioConfig { lag_ms: 323.0, polarity: -1.0, ..Default::default() });
        let segments = truth(&s);
        let analysis = analyze_take(s.mic.clone(), s.sys.clone());
        let echo = analysis.echo.expect("an echo");
        let check = check_voices(&segments, &echo.evidence());
        let verdict = |id: &str| check.voices.iter().find(|v| v.speaker_id == id).unwrap();
        assert!(!verdict("user").dropped && verdict("user").ratio.unwrap() > 1.0, "{check:?}");
        assert!(verdict("remote-1").dropped && verdict("remote-2").dropped, "{check:?}");
        // The canceller's second signal reads the same take the same way.
        assert!(verdict("user").energy_removed_db.unwrap() < ECHO_REMOVED_DB, "{check:?}");
        for id in ["remote-1", "remote-2"] {
            assert!(verdict(id).energy_removed_db.unwrap() >= ECHO_REMOVED_DB, "{check:?}");
        }
    }

    #[test]
    fn a_joined_stream_drops_a_voice_only_inside_the_take_it_echoed_in() {
        // Take 1 (0–60 s) was a call on the speakers: "hege" was remote, and
        // her voice reached the mic only while sys sounded. Take 2 (60–120 s)
        // was in person — she sat at the laptop — and has no system stream,
        // so it is not checked at all. One segment runs across the join.
        let sys_active = active(60.0, &[(0.0, 20.0), (30.0, 50.0)]);
        let segments = vec![
            seg(1.0, 15.0, "hege"),
            seg(21.0, 29.0, "user"),
            seg(32.0, 48.0, "hege"),
            seg(52.0, 58.5, "user"),
            seg(59.0, 70.0, "hege"),
            seg(80.0, 100.0, "hege"),
            seg(101.0, 110.0, "user"),
        ];
        let takes = [TakeSpan {
            start_ms: 0,
            end_ms: 60_000,
            evidence: Evidence { sys_active: &sys_active, energy: None },
        }];
        let (kept, checks) = check_joined(&segments, &takes);
        assert_eq!(checks.len(), 1);
        assert!(checks[0].voices.iter().any(|v| v.speaker_id == "hege" && v.dropped), "{checks:?}");
        let hege: Vec<(u64, u64)> = kept
            .iter()
            .filter(|s| s.speaker_id == "hege")
            .map(|s| (s.start_ms, s.end_ms))
            .collect();
        assert_eq!(hege, vec![(60_000, 70_000), (80_000, 100_000)]);
        assert_eq!(kept.iter().filter(|s| s.speaker_id == "user").count(), 3);
    }
}
