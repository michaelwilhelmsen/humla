//! What a `humla://` link asks the backend to do (#91). A recording link starts
//! or stops a recording through the menu bar's headless path; any other link
//! only brings the window forward, and the frontend reads it
//! (`src/lib/deepLink.ts`).

use std::time::Duration;

use parking_lot::Mutex;
use tauri::{AppHandle, Manager, Url};

use crate::menubar;
use crate::recording::Phase;

/// Act on the links macOS handed Humla.
pub(crate) fn open(app: &AppHandle, urls: Vec<Url>) {
    let mut focus_window = false;
    for url in &urls {
        match parse(url) {
            Some(link) => hold_or_follow(app, link),
            None => focus_window = true,
        }
    }
    if focus_window {
        menubar::show_main_window(app);
    }
}

/// Recording links held until the frontend listens for `recording_status`: a
/// link that cold-launches Humla would otherwise start a recording the window
/// that opens never hears about. `None` once it listens.
pub(crate) struct Held(Mutex<Option<Vec<RecordLink>>>);

impl Default for Held {
    fn default() -> Self {
        Held(Mutex::new(Some(Vec::new())))
    }
}

/// How long a held link waits for the frontend before it is followed anyway.
const HOLD_LIMIT: Duration = Duration::from_secs(10);

fn hold_or_follow(app: &AppHandle, link: RecordLink) {
    if let Some(held) = app.try_state::<Held>() {
        if let Some(queue) = held.0.lock().as_mut() {
            queue.push(link);
            return;
        }
    }
    follow(app, link);
}

/// Follow the held links, and every later one as it arrives.
pub(crate) fn release(app: &AppHandle) {
    let Some(held) = app.try_state::<Held>() else { return };
    let links = held.0.lock().take();
    for link in links.into_iter().flatten() {
        follow(app, link);
    }
}

/// [`release`] after [`HOLD_LIMIT`], for a frontend that never says it listens.
pub(crate) fn release_after_limit(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(HOLD_LIMIT).await;
        release(&app);
    });
}

fn follow(app: &AppHandle, link: RecordLink) {
    let Some(phase) = menubar::current_phase(app) else {
        menubar::notify(
            app,
            "Couldn't follow the link",
            "Humla's menu bar isn't running, and recording links need it.",
        );
        return;
    };
    let app = app.clone();
    // `async_runtime::spawn`: the plugin's listener runs on the main thread,
    // where no tokio runtime is attached.
    match decide(link, phase) {
        Outcome::Refuse(refusal) => refusal.notify(&app),
        Outcome::Start { name } => {
            // Another trigger can have taken the start since `phase` was read.
            if !menubar::claim_start(&app) {
                Refusal::AlreadyRecording.notify(&app);
                return;
            }
            let title = title_from_name(name.as_deref(), chrono::Local::now());
            tauri::async_runtime::spawn(async move {
                if menubar::headless_start(&app, &title).await {
                    menubar::notify(
                        &app,
                        "Recording started",
                        &format!("A link started \"{title}\". Stop it from the menu bar."),
                    );
                }
            });
        }
        Outcome::Stop => {
            menubar::announce_when_saved(&app);
            tauri::async_runtime::spawn(async move { menubar::headless_stop(&app).await });
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecordLink {
    Start { name: Option<String> },
    Stop,
}

/// Reads the route as `parseDeepLink` (src/lib/deepLink.ts) does; change both.
pub(crate) fn parse(url: &Url) -> Option<RecordLink> {
    if url.scheme() != "humla" {
        return None;
    }
    let route = format!("{}{}", url.host_str().unwrap_or(""), url.path());
    match route.trim_matches('/').to_ascii_lowercase().as_str() {
        "record/start" => Some(RecordLink::Start {
            name: url.query_pairs().find(|(k, _)| k == "name").map(|(_, v)| v.into_owned()),
        }),
        "record/stop" => Some(RecordLink::Stop),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Start { name: Option<String> },
    Stop,
    Refuse(Refusal),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    AlreadyRecording,
    StillSaving,
    Importing,
    NotRecording,
    StillStarting,
    AlreadyStopping,
}

impl Refusal {
    fn notify(self, app: &AppHandle) {
        let (title, body) = match self {
            Refusal::AlreadyRecording => {
                ("Already recording", "The link didn't start a second recording.")
            }
            Refusal::StillSaving => (
                "Couldn't start recording",
                "Humla is still saving the last recording. Try the link again once it's saved.",
            ),
            Refusal::Importing => (
                "Couldn't start recording",
                "Humla is importing a file. Try the link again once it's done.",
            ),
            Refusal::NotRecording => ("Not recording", "There's no recording for the link to stop."),
            Refusal::StillStarting => (
                "Couldn't stop recording",
                "The recording is still starting. Try the link again in a moment.",
            ),
            Refusal::AlreadyStopping => ("Already stopped", "The recording is already being saved."),
        };
        menubar::notify(app, title, body);
    }
}

/// What a link does in `phase`. A link starts only where the tray's Start item
/// is enabled and stops only where Stop is (`menubar::tray_menu_state`).
pub(crate) fn decide(link: RecordLink, phase: Phase) -> Outcome {
    use Phase::*;
    match (link, phase) {
        (RecordLink::Start { name }, Idle) => Outcome::Start { name },
        (RecordLink::Start { .. }, Starting | Recording | Paused) => {
            Outcome::Refuse(Refusal::AlreadyRecording)
        }
        (RecordLink::Start { .. }, Stopping | Diarizing) => Outcome::Refuse(Refusal::StillSaving),
        (RecordLink::Start { .. }, Importing) => Outcome::Refuse(Refusal::Importing),
        (RecordLink::Stop, Recording | Paused) => Outcome::Stop,
        (RecordLink::Stop, Idle | Importing) => Outcome::Refuse(Refusal::NotRecording),
        (RecordLink::Stop, Starting) => Outcome::Refuse(Refusal::StillStarting),
        (RecordLink::Stop, Stopping | Diarizing) => Outcome::Refuse(Refusal::AlreadyStopping),
    }
}

/// The title of the note a start link records into.
pub(crate) fn title_from_name(name: Option<&str>, now: chrono::DateTime<chrono::Local>) -> String {
    let spaced: String = name
        .unwrap_or("")
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let title = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return menubar::headless_note_title(now);
    }
    crate::commands::truncate_chars(&title, MAX_TITLE_CHARS)
}

const MAX_TITLE_CHARS: usize = 200;

#[cfg(test)]
mod tests {
    use super::*;

    fn link(raw: &str) -> Option<RecordLink> {
        parse(&Url::parse(raw).unwrap())
    }

    #[test]
    fn a_start_link_starts_a_recording() {
        assert_eq!(link("humla://record/start"), Some(RecordLink::Start { name: None }));
    }

    #[test]
    fn a_start_link_carries_its_name_decoded() {
        let named = |name: &str| Some(RecordLink::Start { name: Some(name.to_string()) });
        assert_eq!(link("humla://record/start?name=Daily%20Standup"), named("Daily Standup"));
        assert_eq!(link("humla://record/start?name=Daily+Standup"), named("Daily Standup"));
        assert_eq!(link("humla://record/start?name=1%3A1%20%C3%98yvind"), named("1:1 Øyvind"));
    }

    #[test]
    fn a_stop_link_stops_the_recording() {
        assert_eq!(link("humla://record/stop"), Some(RecordLink::Stop));
        assert_eq!(link("humla://record/stop?name=Ignored"), Some(RecordLink::Stop));
    }

    // The spellings `parseDeepLink` (src/lib/deepLink.ts) accepts for its routes.
    #[test]
    fn a_route_reads_the_same_however_the_link_spells_it() {
        for raw in ["humla:///record/stop", "humla:record/stop", "HUMLA://Record/Stop/"] {
            assert_eq!(link(raw), Some(RecordLink::Stop), "{raw}");
        }
    }

    #[test]
    fn any_other_link_is_not_a_recording_link() {
        for raw in [
            "humla://new-workspace?source=web",
            "humla://record",
            "humla://record/pause",
            "humla://",
            "https://record/start",
        ] {
            assert_eq!(link(raw), None, "{raw}");
        }
    }

    const START: RecordLink = RecordLink::Start { name: None };

    #[test]
    fn a_start_link_starts_a_recording_when_humla_is_idle() {
        assert_eq!(decide(START, Phase::Idle), Outcome::Start { name: None });
        let named = RecordLink::Start { name: Some("Daily Standup".into()) };
        assert_eq!(
            decide(named, Phase::Idle),
            Outcome::Start { name: Some("Daily Standup".into()) }
        );
    }

    // A second session would steal the slot from the one the user can see.
    #[test]
    fn a_start_link_never_starts_a_second_recording() {
        for phase in [Phase::Starting, Phase::Recording, Phase::Paused] {
            assert_eq!(decide(START, phase), Outcome::Refuse(Refusal::AlreadyRecording), "{phase:?}");
        }
    }

    // The capture slot stays taken until the last recording's post-stop chain
    // has finished (#182), so a start then has to wait — and say so.
    #[test]
    fn a_start_link_waits_for_the_last_recording_to_be_saved() {
        for phase in [Phase::Stopping, Phase::Diarizing] {
            assert_eq!(decide(START, phase), Outcome::Refuse(Refusal::StillSaving), "{phase:?}");
        }
    }

    // An import holds the same single capture slot as a recording.
    #[test]
    fn a_start_link_waits_for_an_import_to_finish() {
        assert_eq!(decide(START, Phase::Importing), Outcome::Refuse(Refusal::Importing));
    }

    #[test]
    fn a_stop_link_stops_a_live_or_paused_take() {
        for phase in [Phase::Recording, Phase::Paused] {
            assert_eq!(decide(RecordLink::Stop, phase), Outcome::Stop, "{phase:?}");
        }
    }

    // A stop link can't cancel an import: that isn't a recording.
    #[test]
    fn a_stop_link_with_nothing_to_stop_says_so() {
        for phase in [Phase::Idle, Phase::Importing] {
            assert_eq!(
                decide(RecordLink::Stop, phase),
                Outcome::Refuse(Refusal::NotRecording),
                "{phase:?}"
            );
        }
    }

    // `recording_stop` would race the start the pipeline is still setting up.
    #[test]
    fn a_stop_link_waits_for_the_recording_to_finish_starting() {
        assert_eq!(
            decide(RecordLink::Stop, Phase::Starting),
            Outcome::Refuse(Refusal::StillStarting)
        );
    }

    #[test]
    fn a_stop_link_after_the_stop_says_it_is_already_being_saved() {
        for phase in [Phase::Stopping, Phase::Diarizing] {
            assert_eq!(
                decide(RecordLink::Stop, phase),
                Outcome::Refuse(Refusal::AlreadyStopping),
                "{phase:?}"
            );
        }
    }

    // A link can't do what a greyed-out tray item refuses to, nor refuse what
    // an enabled one offers.
    #[test]
    fn a_link_does_exactly_what_the_tray_offers() {
        use crate::menubar::tray_menu_state;
        use Phase::*;
        for phase in [Idle, Starting, Recording, Paused, Stopping, Diarizing, Importing] {
            let tray = tray_menu_state(phase);
            let starts = matches!(decide(START, phase), Outcome::Start { .. });
            let stops = decide(RecordLink::Stop, phase) == Outcome::Stop;
            assert_eq!(starts, tray.start_enabled, "start in {phase:?}");
            assert_eq!(stops, tray.stop_enabled, "stop in {phase:?}");
        }
    }

    fn at() -> chrono::DateTime<chrono::Local> {
        use chrono::TimeZone;
        chrono::Local.with_ymd_and_hms(2026, 11, 10, 9, 30, 0).unwrap()
    }

    #[test]
    fn a_named_link_titles_its_note_with_the_name() {
        assert_eq!(title_from_name(Some("Daily Standup"), at()), "Daily Standup");
    }

    #[test]
    fn an_unnamed_link_titles_its_note_with_the_start_time() {
        for name in [None, Some(""), Some("  \n\t ")] {
            assert_eq!(title_from_name(name, at()), "Recording 10 Nov 09:30", "{name:?}");
        }
    }

    // A title is one line, and a link's name can come from anywhere.
    #[test]
    fn a_name_is_flattened_to_one_clean_line() {
        assert_eq!(title_from_name(Some("  Daily \n Standup\t"), at()), "Daily Standup");
        assert_eq!(title_from_name(Some("Daily\u{0}\u{7}Standup"), at()), "Daily Standup");
    }

    #[test]
    fn a_long_name_is_cut_to_a_title_length() {
        let long = "Ø".repeat(500);
        assert_eq!(title_from_name(Some(&long), at()), "Ø".repeat(200));
        let ends_in_space = format!("{} tail", "a".repeat(199));
        assert_eq!(title_from_name(Some(&ends_in_space), at()), "a".repeat(199));
    }

    // #90's titler may replace only a title Humla made up.
    #[test]
    fn the_automatic_titler_keeps_a_name_and_replaces_the_start_time() {
        use crate::menubar::is_replaceable_title;
        assert!(!is_replaceable_title(&title_from_name(Some("Daily Standup"), at())));
        assert!(is_replaceable_title(&title_from_name(None, at())));
    }
}
