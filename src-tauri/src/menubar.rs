//! Menu-bar mode (#21): a tray icon that can start and stop a recording, a
//! window that hides to it instead of quitting, and a configurable global
//! hotkey that does the same from anywhere.
//!
//! Three triggers, one rule. Tray "Start recording" and the hotkey both call
//! [`toggle_recording`], which branches on whether the main window is on
//! screen:
//!
//! - **Window visible** → emit [`TOGGLE_EVENT`] and let the frontend act. With
//!   a note open the hotkey *is* that note's Record button, so the behaviour
//!   has to be decided where the route is known. See `resolveHotkeyAction` in
//!   `src/lib/hotkey.ts`.
//! - **Window hidden** → run the headless path here: create a note, start the
//!   pipeline, and report the outcome through Notification Center, because
//!   there is no window for a toast to appear in. Recording is entirely
//!   backend-driven, so nothing about it needs a webview.
//!
//! The one piece of state that isn't derivable is which note a *headless*
//! recording belongs to ([`MenuBar::headless_note`]): the final
//! `Phase::Idle` carries no note id (the post-stop chain emits it with
//! `None`), so the note has to be remembered from the start of the recording
//! in order to name it in the "saved to …" notification.

use parking_lot::Mutex;
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::GlobalShortcutExt;
use tauri_plugin_notification::NotificationExt;

use crate::recording::Phase;
use crate::{db, AppState};

/// ⌃⌘R. Mirrored in `src/lib/hotkey.ts`; change both.
///
/// Picked by elimination, because a global hotkey outranks the focused app:
///
/// - **Not ⌥-based.** On the Norwegian layout most of Humla's users type on,
///   Option *is* a typing modifier (⌥7 = `|`, ⌥8 = `[`, ⌥⇧8 = `{`). A Carbon
///   hotkey swallows the keystroke system-wide, so an Option combination can
///   take a character away from the keyboard — and ⌥⌘H is Hide Others besides.
///   Adding Control suppresses Option's composition entirely, which is why
///   every candidate here has it.
/// - **Not ⌃⌥.** That is VoiceOver's own modifier, and a shipped default
///   shouldn't collide with a screen reader's whole command set.
/// - **Not ⌘⇧R or ⌥⌘R.** Both are bound *locally* all over the place (hard
///   reload; Safari's Reload Page From Origin), and a global hotkey would win
///   over them everywhere.
/// - **⌃⌘ is quiet.** macOS spends that space on F, Space, D and Q (Lock
///   Screen) — R is free — and `R` keeps the mnemonic that in-app ⌘R already
///   uses for recording the open note.
///
/// Written in the order [`accelFromEvent`](../../src/lib/hotkey.ts) produces,
/// so re-recording this exact combination is a no-op rather than a re-register.
pub const DEFAULT_RECORD_HOTKEY: &str = "Command+Control+KeyR";

/// Settings row holding the accelerator. An empty value means "no hotkey";
/// an absent row means "never configured", which resolves to the default.
pub const HOTKEY_SETTING: &str = "record_hotkey";
/// Settings row for hide-to-tray. Off by default, so quit still quits until
/// the user opts in.
pub const CLOSE_TO_TRAY_SETTING: &str = "close_to_tray";

/// Emitted to the webview when a tray or hotkey trigger arrives while the
/// window is on screen, so the frontend can apply the on-screen rule.
pub const TOGGLE_EVENT: &str = "menubar://toggle-record";

const ID_START: &str = "menubar-start";
const ID_STOP: &str = "menubar-stop";
const ID_OPEN: &str = "menubar-open";
const ID_QUIT: &str = "menubar-quit";

/// Live menu-bar handles, managed on the app so [`on_phase`] can reach them
/// from the recording pipeline.
pub struct MenuBar {
    tray: TrayIcon,
    start: MenuItem<tauri::Wry>,
    stop: MenuItem<tauri::Wry>,
    /// Note id of a recording started with no window on screen, cleared when
    /// that recording finishes. `None` for anything the user started in-app.
    headless_note: Mutex<Option<String>>,
    /// The glyph the tray is showing. Tracked so a phase change that doesn't
    /// change the glyph doesn't re-upload an identical image on every status
    /// event.
    showing_glyph: Mutex<TrayGlyph>,
    /// The accelerator registered right now (`""` = none). The settings row is
    /// the source of truth; this is what we'd have to unregister to change it.
    registered: Mutex<String>,
    /// The last phase [`on_phase`] saw. `emit_status` is the single funnel every
    /// phase change goes through, so this is what the pipeline is doing — and
    /// it's what lets the tray items, the hotkey and the frontend all answer
    /// "start, stop, or neither?" from the same rule instead of three.
    phase: Mutex<Phase>,
}

/// Which of the three glyphs the tray shows.
///
/// Three, not two: a held take must not look like a live one. The glyph is a
/// privacy signal before it is a progress signal, so the question it has to
/// answer at a glance is "is my microphone being read *right now*" — and
/// "recording, paused" is exactly where a single red icon lies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayGlyph {
    /// The bee. Nothing has the microphone.
    Idle,
    /// Red disc, stop square. Being captured now.
    Recording,
    /// Red disc, pause bars. A take still owns the microphone — resuming is one
    /// keystroke away — but nothing is being captured.
    Paused,
}

/// What the tray should look like in a given phase. Split out so the rule is
/// testable without a running app.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TrayMenuState {
    pub start_enabled: bool,
    pub stop_enabled: bool,
    pub glyph: TrayGlyph,
}

pub(crate) fn tray_menu_state(phase: Phase) -> TrayMenuState {
    match phase {
        Phase::Idle => {
            TrayMenuState { start_enabled: true, stop_enabled: false, glyph: TrayGlyph::Idle }
        }
        Phase::Recording => {
            TrayMenuState { start_enabled: false, stop_enabled: true, glyph: TrayGlyph::Recording }
        }
        Phase::Paused => {
            TrayMenuState { start_enabled: false, stop_enabled: true, glyph: TrayGlyph::Paused }
        }
        // The mic is coming live but the session isn't stoppable yet —
        // `recording_stop` would race the one the pipeline is still setting up.
        // Shown as recording regardless: erring toward "your mic is on" is the
        // only safe direction for this indicator to be wrong in.
        Phase::Starting => {
            TrayMenuState { start_enabled: false, stop_enabled: false, glyph: TrayGlyph::Recording }
        }
        // Busy, but nothing is listening: an import replays a file and the
        // post-stop chain works on audio already captured.
        Phase::Importing | Phase::Stopping | Phase::Diarizing => {
            TrayMenuState { start_enabled: false, stop_enabled: false, glyph: TrayGlyph::Idle }
        }
    }
}

// ---------------------------------------------------------------------------
// Icon
// ---------------------------------------------------------------------------

/// Pixel size of the generated tray glyph. tray-icon renders it at 18pt tall,
/// so this is comfortably past 2× for a Retina menu bar.
const ICON_PX: u32 = 44;
/// Recording red. `--color-record`'s dark-mode value, which is the one that
/// stays legible against a light *and* a dark menu bar.
const RECORD_RGB: (u8, u8, u8) = (0xe5, 0x54, 0x4b);

/// The recording disc and the two white marks that go inside it, as fractions
/// of the box. Both marks are the same optical height so the tray doesn't
/// appear to resize when a recording is paused; the pause gap is deliberately
/// wider than either bar, which is what stops the two bars merging into one at
/// the 18pt this is rendered at.
const DISC_RADIUS: f32 = 0.47;
const STOP_HALF: f32 = 0.17;
const STOP_RADIUS: f32 = 0.045;
const PAUSE_HALF_H: f32 = 0.17;
const PAUSE_BAR_HALF_W: f32 = 0.05;
const PAUSE_BAR_CENTRE: f32 = 0.105;
const PAUSE_RADIUS: f32 = 0.022;

/// Proportions of the glyph, as fractions of its box: a bumblebee — a rounded
/// body with two antennae — matching the mark the app itself uses (see
/// `icons/bee.svg`). The values are optically corrected for the 18pt the menu
/// bar renders at, which is the only size this is ever seen at: strokes that
/// look right in the app's icon come out sub-pixel here.
const BODY_CX: f32 = 0.5;
/// The body is the *upper* half of an ellipse centred on its own flat bottom
/// edge — a dome, not a disc.
const BODY_BOTTOM: f32 = 0.90;
const BODY_RX: f32 = 0.44;
const BODY_RY: f32 = 0.43;
/// Each antenna is a stroke from the top of the body out to the upper corner.
const ANTENNA_INNER: (f32, f32) = (0.42, 0.50);
const ANTENNA_OUTER: (f32, f32) = (0.24, 0.13);
const ANTENNA_HALF_WIDTH: f32 = 0.042;

/// The bee glyph as straight (non-premultiplied) RGBA.
///
/// Drawn rather than shipped as a PNG so there is no second asset to keep in
/// sync with the app's own mark, and no image decoder feature to enable.
/// Coverage is 3×3 supersampled, which is what keeps the dome's edge and the
/// antennae from looking notched at this size.
pub(crate) fn bee_glyph_rgba(size: u32, rgb: (u8, u8, u8)) -> Vec<u8> {
    let s = size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut hits = 0u32;
            for sy in 0..3 {
                for sx in 0..3 {
                    let px = (x as f32 + (sx as f32 + 0.5) / 3.0) / s;
                    let py = (y as f32 + (sy as f32 + 0.5) / 3.0) / s;
                    if covers(px, py) {
                        hits += 1;
                    }
                }
            }
            out.extend_from_slice(&[rgb.0, rgb.1, rgb.2, (hits * 255 / 9) as u8]);
        }
    }
    out
}

/// Is this point (in 0..1 glyph space) inside the bee?
fn covers(px: f32, py: f32) -> bool {
    // Body: the top half of an ellipse sitting on `BODY_BOTTOM`.
    let dx = (px - BODY_CX) / BODY_RX;
    let dy = (py - BODY_BOTTOM) / BODY_RY;
    if py <= BODY_BOTTOM && dx * dx + dy * dy <= 1.0 {
        return true;
    }
    // Antennae: one stroke and its mirror image.
    let mirrored = (1.0 - px, py);
    on_antenna(px, py) || on_antenna(mirrored.0, mirrored.1)
}

/// Distance from a point to the antenna segment, as a round-capped stroke.
fn on_antenna(px: f32, py: f32) -> bool {
    let (ax, ay) = ANTENNA_OUTER;
    let (bx, by) = ANTENNA_INNER;
    let (vx, vy) = (bx - ax, by - ay);
    let (wx, wy) = (px - ax, py - ay);
    let t = ((wx * vx + wy * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
    let (cx, cy) = (ax + vx * t, ay + vy * t);
    let (ddx, ddy) = (px - cx, py - cy);
    ddx * ddx + ddy * ddy <= ANTENNA_HALF_WIDTH * ANTENNA_HALF_WIDTH
}



/// A filled red disc with a white mark in it — the recording and paused
/// glyphs, which differ only in `inner`.
///
/// Deliberately a different *shape* from the idle bee, not the same shape in a
/// different colour: the state survives being read by someone who can't tell
/// the two colours apart, and the mark carries meaning a recoloured bee can't.
///
/// Two colours in one image, so unlike the bee these can't be template icons —
/// which is the point. The white mark is opaque rather than punched through to
/// the wallpaper, so it holds its contrast over any desktop.
fn disc_glyph_rgba(size: u32, inner: fn(f32, f32) -> bool) -> Vec<u8> {
    let s = size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut disc = 0u32;
            let mut stop = 0u32;
            for sy in 0..3 {
                for sx in 0..3 {
                    let px = (x as f32 + (sx as f32 + 0.5) / 3.0) / s;
                    let py = (y as f32 + (sy as f32 + 0.5) / 3.0) / s;
                    let (dx, dy) = (px - 0.5, py - 0.5);
                    if dx * dx + dy * dy > DISC_RADIUS * DISC_RADIUS {
                        continue;
                    }
                    disc += 1;
                    if inner(dx.abs(), dy.abs()) {
                        stop += 1;
                    }
                }
            }
            if disc == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            // The mark lies wholly inside the disc, so its coverage is also the
            // white/red mix for this pixel — that blend is what keeps its
            // corners from stair-stepping at 18pt.
            let t = stop as f32 / 9.0;
            let mix = |c: u8| (c as f32 * (1.0 - t) + 255.0 * t) as u8;
            out.extend_from_slice(&[
                mix(RECORD_RGB.0),
                mix(RECORD_RGB.1),
                mix(RECORD_RGB.2),
                (disc * 255 / 9) as u8,
            ]);
        }
    }
    out
}

/// Rounded-rect test for a mark centred on the glyph, given a point's distance
/// from the centre on each axis. `offset_x` shifts it sideways, which is how one
/// predicate draws both pause bars: taking `ax` as an absolute value already
/// mirrors it.
fn in_rounded_rect(
    ax: f32,
    ay: f32,
    offset_x: f32,
    half_w: f32,
    half_h: f32,
    radius: f32,
) -> bool {
    let ox = ((ax - offset_x).abs() - (half_w - radius)).max(0.0);
    let oy = (ay - (half_h - radius)).max(0.0);
    ox * ox + oy * oy <= radius * radius
}

fn in_stop_square(ax: f32, ay: f32) -> bool {
    in_rounded_rect(ax, ay, 0.0, STOP_HALF, STOP_HALF, STOP_RADIUS)
}

fn in_pause_bars(ax: f32, ay: f32) -> bool {
    in_rounded_rect(ax, ay, PAUSE_BAR_CENTRE, PAUSE_BAR_HALF_W, PAUSE_HALF_H, PAUSE_RADIUS)
}

/// The image for a glyph, paired with whether macOS should treat it as a
/// template. Returned together so the two can never get out of step — a red
/// disc drawn as a template would come out flat black.
fn icon_for(glyph: TrayGlyph) -> (Image<'static>, bool) {
    let rgba = match glyph {
        TrayGlyph::Idle => bee_glyph_rgba(ICON_PX, (0, 0, 0)),
        TrayGlyph::Recording => disc_glyph_rgba(ICON_PX, in_stop_square),
        TrayGlyph::Paused => disc_glyph_rgba(ICON_PX, in_pause_bars),
    };
    (
        Image::new_owned(rgba, ICON_PX, ICON_PX),
        glyph == TrayGlyph::Idle,
    )
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Build the tray and register the stored hotkey. Called from Tauri `setup`.
pub fn install(app: &AppHandle) -> tauri::Result<()> {
    let start = MenuItem::with_id(app, ID_START, "Start recording", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, ID_STOP, "Stop recording", false, None::<&str>)?;
    let open = MenuItem::with_id(app, ID_OPEN, "Open Humla", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, ID_QUIT, "Quit Humla", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &start,
            &stop,
            &PredefinedMenuItem::separator(app)?,
            &open,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let tray = TrayIconBuilder::with_id("humla-tray")
        .icon(icon_for(TrayGlyph::Idle).0)
        // Pure black plus this flag: a macOS template image is read through its
        // alpha channel only, and the system tints the glyph for the bar.
        .icon_as_template(true)
        .tooltip("Humla")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id().as_ref() {
            // Start and Stop share one entry point: the menu's enabled state
            // already encodes which of the two is meaningful, so the toggle
            // can never resolve to the wrong one.
            ID_START | ID_STOP => toggle_recording(app),
            ID_OPEN => show_main_window(app),
            ID_QUIT => app.exit(0),
            _ => {}
        })
        .build(app)?;

    app.manage(MenuBar {
        tray,
        start,
        stop,
        headless_note: Mutex::new(None),
        showing_glyph: Mutex::new(TrayGlyph::Idle),
        registered: Mutex::new(String::new()),
        phase: Mutex::new(Phase::Idle),
    });

    // Registering here rather than through the plugin builder's
    // `with_shortcut`: the accelerator lives in the database, which isn't open
    // until setup runs.
    let stored = stored_hotkey(app);
    if let Err(e) = register_hotkey(app, &stored) {
        eprintln!("[menubar] could not register {stored:?}: {e}");
    }
    Ok(())
}

/// The configured accelerator: the stored row, or the default when no row was
/// ever written. An explicitly empty row means the user turned it off.
pub fn stored_hotkey(app: &AppHandle) -> String {
    let Some(state) = app.try_state::<AppState>() else {
        return DEFAULT_RECORD_HOTKEY.to_string();
    };
    let conn = state.db.lock();
    match db::get_setting(&conn, HOTKEY_SETTING) {
        Ok(Some(v)) => v,
        Ok(None) => DEFAULT_RECORD_HOTKEY.to_string(),
        // A failed read is NOT the same as an unset one, and must not be
        // collapsed into it: a user who turned the hotkey off has `""` stored,
        // and answering the failure with the default would re-arm a global
        // shortcut they explicitly declined. Register nothing instead.
        Err(e) => {
            eprintln!("[menubar] could not read {HOTKEY_SETTING}: {e}");
            String::new()
        }
    }
}

fn close_to_tray_enabled(app: &AppHandle) -> bool {
    let Some(state) = app.try_state::<AppState>() else { return false };
    let conn = state.db.lock();
    db::get_setting(&conn, CLOSE_TO_TRAY_SETTING)
        .ok()
        .flatten()
        .as_deref()
        == Some("true")
}

// ---------------------------------------------------------------------------
// Hotkey
// ---------------------------------------------------------------------------

/// Reject an accelerator we shouldn't hand to the OS. Parsing is the plugin's
/// job; the extra rule here is that a global hotkey needs a real modifier —
/// bare `H` (or `Shift+H`, which is just how you type a capital H) would
/// swallow that key in every other app on the Mac.
pub(crate) fn validate_accel(accel: &str) -> Result<(), String> {
    let tokens: Vec<&str> = accel.split('+').map(str::trim).collect();
    if tokens.iter().any(|t| t.is_empty()) {
        return Err(format!("{accel:?} isn't a valid shortcut"));
    }
    let has_real_modifier = tokens.iter().any(|t| {
        matches!(
            t.to_ascii_uppercase().as_str(),
            "ALT"
                | "OPTION"
                | "CONTROL"
                | "CTRL"
                | "COMMAND"
                | "CMD"
                | "SUPER"
                | "COMMANDORCONTROL"
                | "COMMANDORCTRL"
                | "CMDORCTRL"
                | "CMDORCONTROL"
        )
    });
    if !has_real_modifier {
        return Err("A global shortcut needs ⌘, ⌃ or ⌥ — otherwise it would swallow that key in every app.".into());
    }
    Ok(())
}

/// Unregister whatever is registered and register `accel` instead. An empty
/// `accel` just clears. Does not persist — see [`set_hotkey`].
fn register_hotkey(app: &AppHandle, accel: &str) -> Result<(), String> {
    let Some(mb) = app.try_state::<MenuBar>() else {
        return Err("menu bar not installed".into());
    };
    let shortcuts = app.global_shortcut();
    let accel = accel.trim();
    let previous = mb.registered.lock().clone();
    if accel == previous {
        return Ok(());
    }

    // The new shortcut is registered BEFORE the old one is dropped, so that
    // every way this can fail — a malformed accelerator, or a combination
    // another app already owns — leaves the working shortcut working. The other
    // order looks tidier and is a trap: the caller reports the failure and
    // keeps the old value on screen, but nothing is listening for it any more.
    if !accel.is_empty() {
        validate_accel(accel)?;
        shortcuts
            .on_shortcut(accel, |app, _shortcut, event| {
                // Fires on both press and release; acting on both would toggle
                // twice per keypress.
                if event.state == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                    toggle_recording(app);
                }
            })
            .map_err(|e| format!("{e}"))?;
    }
    *mb.registered.lock() = accel.to_string();
    if !previous.is_empty() {
        // Best-effort: a shortcut that failed to register isn't there to remove,
        // and a stuck unregister must not undo the switch we just made.
        let _ = shortcuts.unregister(previous.as_str());
    }
    Ok(())
}

/// Register `accel`, then persist it. Registration first, deliberately: if the
/// combination is taken by another app, the stored setting still describes a
/// shortcut that actually works.
pub fn set_hotkey(app: &AppHandle, accel: &str) -> Result<(), String> {
    let accel = accel.trim();
    register_hotkey(app, accel)?;
    let Some(state) = app.try_state::<AppState>() else {
        return Err("app state unavailable".into());
    };
    let conn = state.db.lock();
    db::set_setting(&conn, HOTKEY_SETTING, accel).map_err(|e| format!("{e}"))
}

// ---------------------------------------------------------------------------
// Window lifecycle
// ---------------------------------------------------------------------------

/// Is there a window on screen to hand the trigger to?
///
/// A **minimized** window counts as absent, deliberately: `is_visible()` is
/// false while miniaturized, and that is the answer we want. Nothing is on
/// screen, so there is no context for the hotkey to honour — and the headless
/// path is the one that reports back through Notification Center, which is the
/// only feedback a user looking at another app is going to get.
fn main_window_visible(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

/// Bring the window back and put the Dock icon with it.
pub fn show_main_window(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// Handle the window's close button. Returns true when the close was absorbed
/// into a hide, in which case the caller must prevent it.
pub fn hide_on_close(app: &AppHandle) -> bool {
    if !close_to_tray_enabled(app) {
        return false;
    }
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    // Accessory drops the Dock icon and the app menu, which is the point: the
    // tray becomes the whole surface. Quit stays reachable from it.
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
    true
}

// ---------------------------------------------------------------------------
// Triggers
// ---------------------------------------------------------------------------

/// Start or stop a recording from the tray or the hotkey.
pub fn toggle_recording(app: &AppHandle) {
    if main_window_visible(app) {
        // The window knows which note is open; it decides. See the module doc.
        let _ = app.emit(TOGGLE_EVENT, ());
        return;
    }
    // The same rule the tray items are drawn from, so a hotkey press can't do
    // what a greyed-out menu item refuses to. Deciding from `note_id.is_some()`
    // instead would call every mid-transition phase "idle" — `recording_stop`
    // clears the id before the post-stop chain runs — and a second recording
    // started on top of a diarize in flight is the shape that used to hand the
    // finishing take's notification to the new one.
    let Some(mb) = app.try_state::<MenuBar>() else { return };
    let want = tray_menu_state(*mb.phase.lock());
    if !want.start_enabled && !want.stop_enabled {
        return;
    }
    let stopping = want.stop_enabled;

    let app = app.clone();
    // `async_runtime::spawn`, not `tokio::spawn`: this can be reached from the
    // tray handler on the main thread during startup, before tokio is attached
    // (see the setup caveat in CLAUDE.md).
    tauri::async_runtime::spawn(async move {
        if stopping {
            headless_stop(&app).await;
        } else {
            headless_start(&app).await;
        }
    });
}

async fn headless_start(app: &AppHandle) {
    let note = match create_headless_note(app) {
        Ok(note) => note,
        Err(e) => {
            notify(app, "Couldn't start recording", &e);
            return;
        }
    };
    // Remember the note before starting: a start that fails still needs to be
    // named, and a start that succeeds won't come back through here.
    *app.state::<MenuBar>().headless_note.lock() = Some(note.0.clone());
    // The window is closed, so refresh whenever it comes back.
    let _ = app.emit("notes_changed", ());

    let state = app.state::<AppState>();
    if let Err(e) = crate::commands::recording_start(app.clone(), state, note.0.clone()).await {
        app.state::<MenuBar>().headless_note.lock().take();
        // `recording_start` backs out of Phase::Starting on the paths that
        // reach it, but the early refusals (already recording, permissions,
        // provider prereqs) return before ever emitting one. Re-assert Idle so
        // the tray can't be left mid-transition by a failure it reported.
        crate::commands::emit_status(app, None, Phase::Idle);
        // A silent headless failure is the worst thing this feature can do:
        // revoked mic permission or a missing key/model normally surfaces as an
        // in-app toast, and there is no app on screen to show one.
        notify(app, "Couldn't start recording", &e);
    }
}

async fn headless_stop(app: &AppHandle) {
    let state = app.state::<AppState>();
    if let Err(e) = crate::commands::recording_stop(app.clone(), state).await {
        notify(app, "Couldn't stop recording", &e);
    }
    // The "saved to …" notification waits for Phase::Idle — the transcript
    // isn't written until the post-stop chain finishes.
}

/// Create the note a headless recording records into: the user's defaults,
/// in the active workspace, titled with the start time.
///
/// The title is what makes the note findable — both in the sidebar and in the
/// notification that names it. #90 (titles from the summary) is expected to
/// replace it; [`is_generated_title`] is the seam for recognising a title as
/// ours to overwrite.
fn create_headless_note(app: &AppHandle) -> Result<(String, String), String> {
    let state = app.state::<AppState>();
    let title = headless_note_title(chrono::Local::now());
    let id = {
        let conn = state.db.lock();
        let language = db::get_setting(&conn, "language")
            .map_err(|e| format!("{e}"))?
            .unwrap_or_else(|| crate::commands::DEFAULT_LANGUAGE.to_string());
        let preset = db::get_setting(&conn, "default_summary_preset")
            .map_err(|e| format!("{e}"))?
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "meeting".to_string());
        let workspace = crate::commands::cloud::active_workspace(&conn);
        let note =
            db::create_note(&conn, &language, &preset, &workspace).map_err(|e| format!("{e}"))?;
        db::update_note(
            &conn,
            &note.id,
            &db::NotePatch { title: Some(title.clone()), ..Default::default() },
        )
        .map_err(|e| format!("{e}"))?;
        note.id
    }; // drop the db guard before pinging sync (see the SyncObserver contract)
    state.sync.note_upserted(&id);
    Ok((id, title))
}

/// `"Recording 19 Aug 14:32"` — the start time, since it's the only thing we
/// know about a recording nobody typed a title for.
pub(crate) fn headless_note_title(now: chrono::DateTime<chrono::Local>) -> String {
    format!("Recording {}", now.format("%-d %b %H:%M"))
}

/// Whether a title is one [`headless_note_title`] produced, and so may be
/// replaced by a better one. Kept next to the generator (and pinned by a test)
/// so the two can't drift apart.
///
/// Nothing calls it yet: it exists for #90 (titles derived from the summary),
/// whose rule is "a generated timestamp is fair game, a human title is not".
/// That question has exactly one correct answer and it belongs here, beside the
/// format it has to recognise — not in whatever file #90 lands in.
#[allow(dead_code)]
pub fn is_generated_title(title: &str) -> bool {
    let Some(rest) = title.trim().strip_prefix("Recording ") else { return false };
    chrono::NaiveDateTime::parse_from_str(
        // The format carries no year, so parse against a fixed one — we only
        // care whether the shape matches.
        &format!("2000 {rest}"),
        "%Y %-d %b %H:%M",
    )
    .is_ok()
}

// ---------------------------------------------------------------------------
// Phase changes
// ---------------------------------------------------------------------------

/// Reflect a recording phase in the tray, and close the loop on a headless
/// recording once its post-stop chain has finished.
pub fn on_phase(app: &AppHandle, phase: Phase) {
    let Some(mb) = app.try_state::<MenuBar>() else { return };
    *mb.phase.lock() = phase;
    let want = tray_menu_state(phase);
    let _ = mb.start.set_enabled(want.start_enabled);
    let _ = mb.stop.set_enabled(want.stop_enabled);

    let mut showing = mb.showing_glyph.lock();
    if *showing != want.glyph {
        *showing = want.glyph;
        let (icon, template) = icon_for(want.glyph);
        // Reported rather than swallowed. `TrayIconBuilder::icon` drops a
        // conversion failure on the floor (`try_into().ok()`), which presents as
        // a status item that occupies space and draws nothing — hours of
        // "is the tray even there?" with no line in the log to go on.
        if let Err(e) = mb
            .tray
            .set_icon(Some(icon))
            .and_then(|()| mb.tray.set_icon_as_template(template))
        {
            eprintln!("[menubar] could not swap the tray icon: {e}");
        }
    }
    drop(showing);

    // Idle is emitted with no note id, so the headless note has to come from
    // what was stashed at the start — which makes it worth being sure this Idle
    // is the *end* of that recording. A capture in flight means it isn't: the
    // note we'd name is one still being recorded into.
    if phase == Phase::Idle && !capture_in_flight(app) {
        let claimed = mb.headless_note.lock().take();
        if let Some(note_id) = claimed {
            let title = note_title(app, &note_id);
            notify(app, "Recording saved", &format!("Saved to \"{title}\""));
        }
    }
}

/// Is a capture occupying the single recording slot right now?
fn capture_in_flight(app: &AppHandle) -> bool {
    app.try_state::<AppState>()
        .map(|s| s.recording.lock().note_id.is_some())
        .unwrap_or(false)
}

fn note_title(app: &AppHandle, note_id: &str) -> String {
    let fallback = "Untitled note".to_string();
    let Some(state) = app.try_state::<AppState>() else { return fallback };
    let conn = state.db.lock();
    match db::get_note(&conn, note_id) {
        Ok(n) if !n.title.trim().is_empty() => n.title,
        _ => fallback,
    }
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        eprintln!("[menubar] notification failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn only_idle_offers_start_and_only_a_live_take_offers_stop() {
        assert_eq!(
            tray_menu_state(Phase::Idle),
            TrayMenuState { start_enabled: true, stop_enabled: false, glyph: TrayGlyph::Idle }
        );
        for phase in [Phase::Recording, Phase::Paused] {
            let s = tray_menu_state(phase);
            assert!(!s.start_enabled && s.stop_enabled, "{phase:?} should offer only stop");
        }
        for phase in [Phase::Starting, Phase::Stopping, Phase::Diarizing, Phase::Importing] {
            let s = tray_menu_state(phase);
            assert!(!s.start_enabled && !s.stop_enabled, "{phase:?} should offer neither");
        }
    }

    // The glyph answers "is my microphone being read right now", so every phase
    // that keeps working on audio already captured — and an import, which
    // listens to nothing at all — has to leave it alone.
    #[test]
    fn the_glyph_tracks_the_microphone_not_busyness() {
        let glyph = |p| tray_menu_state(p).glyph;
        assert_eq!(glyph(Phase::Starting), TrayGlyph::Recording);
        assert_eq!(glyph(Phase::Recording), TrayGlyph::Recording);
        assert_eq!(glyph(Phase::Importing), TrayGlyph::Idle);
        assert_eq!(glyph(Phase::Stopping), TrayGlyph::Idle);
        assert_eq!(glyph(Phase::Diarizing), TrayGlyph::Idle);
        assert_eq!(glyph(Phase::Idle), TrayGlyph::Idle);
    }

    // A held take must not look like a live one: the whole point of the third
    // glyph is that "recording" and "paused" are different answers to "is my
    // microphone being read".
    #[test]
    fn a_paused_take_looks_like_neither_recording_nor_idle() {
        let paused = tray_menu_state(Phase::Paused).glyph;
        assert_eq!(paused, TrayGlyph::Paused);
        assert_ne!(paused, TrayGlyph::Recording);
        assert_ne!(paused, TrayGlyph::Idle);
        // And it is drawn differently, not just labelled differently.
        assert_ne!(
            disc_glyph_rgba(44, in_pause_bars),
            disc_glyph_rgba(44, in_stop_square)
        );
    }

    // Only the bee is a template image; a red disc drawn as one comes out flat
    // black, which is the failure this pairing exists to prevent.
    #[test]
    fn only_the_idle_glyph_is_a_template() {
        assert!(icon_for(TrayGlyph::Idle).1);
        assert!(!icon_for(TrayGlyph::Recording).1);
        assert!(!icon_for(TrayGlyph::Paused).1);
    }

    #[test]
    fn accepts_the_default_hotkey() {
        assert!(validate_accel(DEFAULT_RECORD_HOTKEY).is_ok());
        assert!(validate_accel("Command+Shift+KeyR").is_ok());
        assert!(validate_accel("Control+Alt+Space").is_ok());
    }

    #[test]
    fn refuses_a_shortcut_that_would_swallow_a_key_everywhere() {
        assert!(validate_accel("KeyH").is_err());
        assert!(validate_accel("Shift+KeyH").is_err());
    }

    #[test]
    fn refuses_a_malformed_accelerator() {
        assert!(validate_accel("Alt++KeyH").is_err());
        assert!(validate_accel("Alt+").is_err());
        assert!(validate_accel("").is_err());
    }

    #[test]
    fn titles_a_headless_note_with_its_start_time() {
        let at = chrono::Local.with_ymd_and_hms(2026, 8, 19, 14, 32, 0).unwrap();
        assert_eq!(headless_note_title(at), "Recording 19 Aug 14:32");
        let single_digit = chrono::Local.with_ymd_and_hms(2026, 8, 3, 9, 5, 0).unwrap();
        assert_eq!(headless_note_title(single_digit), "Recording 3 Aug 09:05");
    }

    // #90 will want to replace a generated title with one derived from the
    // summary, and must never touch a title a human wrote.
    #[test]
    fn recognises_its_own_titles_and_nobody_elses() {
        let at = chrono::Local.with_ymd_and_hms(2026, 8, 19, 14, 32, 0).unwrap();
        assert!(is_generated_title(&headless_note_title(at)));
        assert!(is_generated_title("Recording 3 Aug 09:05"));
        assert!(!is_generated_title(""));
        assert!(!is_generated_title("Recording"));
        assert!(!is_generated_title("Recording kickoff with Hege"));
        assert!(!is_generated_title("Standup 19 Aug 14:32"));
    }

    const PX: u32 = 44;
    const RED: (u8, u8, u8, u8) = (RECORD_RGB.0, RECORD_RGB.1, RECORD_RGB.2, 255);
    const WHITE: (u8, u8, u8, u8) = (255, 255, 255, 255);

    /// Sample a glyph at a fractional position in its box.
    fn sample(rgba: &[u8], x: f32, y: f32) -> (u8, u8, u8, u8) {
        let xi = (x * PX as f32) as usize;
        let yi = (y * PX as f32) as usize;
        let i = (yi * PX as usize + xi) * 4;
        (rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3])
    }

    #[test]
    fn draws_a_red_disc_with_a_stop_square_in_it() {
        let rgba = disc_glyph_rgba(PX, in_stop_square);
        assert_eq!(rgba.len() as u32, PX * PX * 4);
        // White square through the middle — the affordance, not a dot.
        assert_eq!(sample(&rgba, 0.5, 0.5), WHITE);
        // Red disc around it, on all four sides, so the square reads as centred.
        assert_eq!(sample(&rgba, 0.5, 0.25), RED);
        assert_eq!(sample(&rgba, 0.5, 0.75), RED);
        assert_eq!(sample(&rgba, 0.25, 0.5), RED);
        assert_eq!(sample(&rgba, 0.75, 0.5), RED);
        // A disc, so the corners are empty.
        assert_eq!(sample(&rgba, 0.03, 0.03).3, 0);
        assert_eq!(sample(&rgba, 0.97, 0.97).3, 0);
    }

    #[test]
    fn draws_two_pause_bars_with_daylight_between_them() {
        let rgba = disc_glyph_rgba(PX, in_pause_bars);
        // A bar either side of centre …
        assert_eq!(sample(&rgba, 0.5 - PAUSE_BAR_CENTRE, 0.5), WHITE);
        assert_eq!(sample(&rgba, 0.5 + PAUSE_BAR_CENTRE, 0.5), WHITE);
        // … and red between them, which is the whole difference from the stop
        // square: two marks, not one.
        assert_eq!(sample(&rgba, 0.5, 0.5), RED);
        // Bars, not dots: still white near the top and bottom of their run.
        let near_end = PAUSE_HALF_H * 0.8;
        assert_eq!(sample(&rgba, 0.5 - PAUSE_BAR_CENTRE, 0.5 - near_end), WHITE);
        assert_eq!(sample(&rgba, 0.5 + PAUSE_BAR_CENTRE, 0.5 + near_end), WHITE);
        // But not beyond them — the disc shows through above and below.
        assert_eq!(sample(&rgba, 0.5 - PAUSE_BAR_CENTRE, 0.5 - PAUSE_HALF_H - 0.05), RED);
        assert_eq!(sample(&rgba, 0.03, 0.03).3, 0);
    }

    // The two marks have to occupy the same optical height, or pausing looks
    // like the tray resized itself.
    #[test]
    fn the_two_marks_are_the_same_height() {
        assert_eq!(STOP_HALF, PAUSE_HALF_H);
    }

    #[test]
    fn draws_a_bee_that_fits_its_box() {
        let px = 44u32;
        let rgba = bee_glyph_rgba(px, (1, 2, 3));
        assert_eq!(rgba.len() as u32, px * px * 4);

        let alpha = |x: f32, y: f32| {
            let xi = (x * px as f32) as u32;
            let yi = (y * px as f32) as u32;
            rgba[((yi * px + xi) * 4 + 3) as usize]
        };
        // Solid through the middle of the body, empty in the bottom corners:
        // a dome sitting on its own flat edge, not a box and not a full disc.
        assert_eq!(alpha(0.5, 0.7), 255);
        assert_eq!(alpha(0.02, 0.88), 0);
        assert_eq!(alpha(0.98, 0.88), 0);
        // Nothing below the body's flat bottom.
        assert_eq!(alpha(0.5, 0.97), 0);
        // Two antennae up top, and clear sky between them — the gap is what
        // makes it read as a bee rather than a blob.
        assert_eq!(alpha(0.22, 0.13), 255);
        assert_eq!(alpha(0.78, 0.13), 255);
        assert_eq!(alpha(0.5, 0.13), 0);
        // Colour is carried on every pixel; only alpha shapes the glyph.
        assert_eq!(&rgba[0..3], &[1, 2, 3]);
    }
}
