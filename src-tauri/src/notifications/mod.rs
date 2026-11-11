//! Notifications Humla posts through macOS's UserNotifications: whether it
//! may, posting one with buttons, taking one back, and what the user clicked.

mod center;
mod permission;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2_user_notifications::UNNotificationDefaultActionIdentifier;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;

use center::Center;
pub(crate) use permission::Authorization;

/// The event a click on a notification is emitted as, carrying a [`Response`].
/// A click that launches Humla can arrive before the window listens, so only
/// a listener added in `setup` is sure to hear that one.
pub(crate) const RESPONSE_EVENT: &str = "notification_response";

/// What the user clicked on a notification Humla posted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Response {
    /// The id the notification was posted with.
    pub id: String,
    /// The button's id, or `None` for a click on the notification itself.
    pub action: Option<String>,
}

impl Response {
    /// The response to a click on notification `id`, from the identifier macOS
    /// reports for what was clicked.
    fn from_click(id: &str, action_identifier: &str) -> Response {
        // SAFETY: a constant string UserNotifications exports.
        let body = unsafe { UNNotificationDefaultActionIdentifier }.to_string();
        let action = (action_identifier != body).then(|| action_identifier.to_owned());
        Response { id: id.to_owned(), action }
    }
}

/// A button on a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Action {
    pub id: String,
    pub title: String,
}

/// macOS's current answer.
pub(crate) async fn status() -> Authorization {
    match Center::get() {
        Some(center) => center.authorization().await,
        None => Authorization::Unknown,
    }
}

/// Asks macOS, which shows its prompt only while nothing has been answered,
/// and waits for the answer.
pub(crate) async fn request() {
    if let Some(center) = Center::get() {
        center.request().await;
    }
}

/// Makes clicks reach the app. Runs in `setup`, since macOS hands a click that
/// launched Humla to whatever delegate is set as launching finishes.
pub(crate) fn install(app: &AppHandle) {
    if let Some(center) = Center::get() {
        center.install(app.clone());
    }
}

/// Posts notification `id` with a button per action. Asks macOS first when
/// nothing has been answered yet, and drops it when notifications are off.
pub(crate) fn notify(app: &AppHandle, id: &str, title: &str, body: &str, actions: &[Action]) {
    WAITING.lock().ask(id);
    let app = app.clone();
    let notification = Notification {
        id: id.to_owned(),
        title: title.to_owned(),
        body: body.to_owned(),
        actions: actions.to_vec(),
    };
    tauri::async_runtime::spawn(async move { deliver(&app, notification).await });
}

/// Takes notification `id` off the screen and out of Notification Center.
pub(crate) fn dismiss(id: &str) {
    WAITING.lock().dismiss(id);
    if let Some(center) = Center::get() {
        center.remove(id);
    }
}

/// A notification on its way to the user.
struct Notification {
    id: String,
    title: String,
    body: String,
    actions: Vec<Action>,
}

/// Held while deciding a route, so a second notification waits for the answer
/// to a prompt the first one raised rather than raising its own.
static DECIDING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static CATEGORIES: parking_lot::Mutex<Categories> =
    parking_lot::Mutex::new(Categories(Vec::new()));
static WAITING: parking_lot::Mutex<Waiting> = parking_lot::Mutex::new(Waiting(BTreeMap::new()));
static SAID_PLAIN: AtomicBool = AtomicBool::new(false);
static SAID_OFF: AtomicBool = AtomicBool::new(false);

async fn deliver(app: &AppHandle, notification: Notification) {
    let center = Center::get();
    let chosen = {
        let _deciding = DECIDING.lock().await;
        let chosen = route(center.is_some(), status().await);
        if chosen == Route::AskFirst {
            request().await;
            route(center.is_some(), status().await)
        } else {
            chosen
        }
    };
    let category = match (chosen, center) {
        (Route::Post, Some(center)) => registered_category(center, &notification.actions).await,
        _ => None,
    };
    // A `dismiss` can come while this one waits on macOS.
    let wanted = WAITING.lock().take(&notification.id);
    if !wanted {
        return;
    }
    match chosen {
        Route::Post => {
            let Some(center) = center else { return };
            if let Err(e) = center.post(&notification, category.as_deref()).await {
                eprintln!("[notifications] couldn't post: {e}");
            }
        }
        Route::Plain => {
            say_once(
                &SAID_PLAIN,
                "not inside an app bundle, so notifications are plain banners without buttons",
            );
            let builder = app.notification().builder();
            if let Err(e) = builder.title(&notification.title).body(&notification.body).show() {
                eprintln!("[notifications] couldn't post: {e}");
            }
        }
        Route::AskFirst => {
            eprintln!("[notifications] macOS's prompt went unanswered, so nothing was posted");
        }
        Route::Drop => say_once(&SAID_OFF, "notifications are off for Humla, so none are posted"),
    }
}

/// The category a notification with `actions` posts under, once macOS holds it.
async fn registered_category(center: Center, actions: &[Action]) -> Option<String> {
    let category = category(actions)?;
    // Registered under the lock, so two new categories can't reach macOS out
    // of order and the later set leave one out.
    {
        let mut known = CATEGORIES.lock();
        if let Some(all) = known.add(category.clone()) {
            center.register(&all);
        }
    }
    center.registered().await;
    Some(category.id)
}

/// Reports a click on notification `id` to the app.
fn clicked(app: &AppHandle, id: &str, action_identifier: &str) {
    let response = Response::from_click(id, action_identifier);
    eprintln!("[notifications] clicked {} (action {:?})", response.id, response.action);
    if let Err(e) = app.emit(RESPONSE_EVENT, &response) {
        eprintln!("[notifications] couldn't report a click: {e}");
    }
}

fn say_once(said: &AtomicBool, message: &str) {
    if !said.swap(true, Ordering::Relaxed) {
        eprintln!("[notifications] {message}");
    }
}

/// A set of buttons, registered with macOS under one id.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Category {
    id: String,
    actions: Vec<Action>,
}

/// The category a notification with `actions` posts under; none without buttons.
fn category(actions: &[Action]) -> Option<Category> {
    if actions.is_empty() {
        return None;
    }
    let buttons: Vec<(&str, &str)> =
        actions.iter().map(|a| (a.id.as_str(), a.title.as_str())).collect();
    let id = serde_json::to_string(&buttons).ok()?;
    Some(Category { id, actions: actions.to_vec() })
}

/// The categories handed to macOS by this process.
#[derive(Default)]
struct Categories(Vec<Category>);

impl Categories {
    /// Records `category`. When it is new, returns every category to hand
    /// macOS, since registering replaces the whole set.
    fn add(&mut self, category: Category) -> Option<Vec<Category>> {
        if self.0.iter().any(|known| known.id == category.id) {
            return None;
        }
        self.0.push(category);
        Some(self.0.clone())
    }
}

/// Notifications asked for that haven't gone out yet, by id.
#[derive(Default)]
struct Waiting(BTreeMap<String, usize>);

impl Waiting {
    fn ask(&mut self, id: &str) {
        *self.0.entry(id.to_owned()).or_default() += 1;
    }

    /// Whether a notification asked for as `id` still goes out.
    fn take(&mut self, id: &str) -> bool {
        let Some(count) = self.0.get_mut(id) else { return false };
        *count -= 1;
        if *count == 0 {
            self.0.remove(id);
        }
        true
    }

    fn dismiss(&mut self, id: &str) {
        self.0.remove(id);
    }
}

/// How a notification reaches the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    /// Through UserNotifications.
    Post,
    /// Nothing has been answered yet: ask macOS, then post.
    AskFirst,
    /// The plugin's plain banner, with no buttons and no clicks.
    Plain,
    /// Notifications are off for Humla.
    Drop,
}

fn route(bundled: bool, authorization: Authorization) -> Route {
    if !bundled {
        return Route::Plain;
    }
    match authorization {
        Authorization::Granted | Authorization::Unknown => Route::Post,
        Authorization::NotDetermined => Route::AskFirst,
        Authorization::Denied => Route::Drop,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_on_the_notification_itself_carries_no_action() {
        // SAFETY: a constant string UserNotifications exports.
        let body = unsafe { UNNotificationDefaultActionIdentifier }.to_string();
        assert_eq!(Response::from_click("n-1", &body), Response { id: "n-1".into(), action: None });
    }

    #[test]
    fn a_button_click_carries_the_buttons_id() {
        assert_eq!(
            Response::from_click("n-1", "open"),
            Response { id: "n-1".into(), action: Some("open".into()) }
        );
    }

    fn button(id: &str, title: &str) -> Action {
        Action { id: id.into(), title: title.into() }
    }

    #[test]
    fn a_notification_without_buttons_has_no_category() {
        assert_eq!(category(&[]), None);
    }

    #[test]
    fn the_same_buttons_post_under_one_category() {
        let first = category(&[button("open", "Open")]).expect("buttons need a category");
        let again = category(&[button("open", "Open")]).expect("buttons need a category");
        assert_eq!(first.id, again.id);
    }

    #[test]
    fn renaming_a_button_makes_another_category() {
        let open = category(&[button("open", "Open")]).expect("buttons need a category");
        let renamed = category(&[button("open", "Open note")]).expect("buttons need a category");
        assert_ne!(open.id, renamed.id);
    }

    #[test]
    fn a_category_is_handed_to_macos_once() {
        let open = category(&[button("open", "Open")]).expect("buttons need a category");
        let mut known = Categories::default();
        assert_eq!(known.add(open.clone()), Some(vec![open.clone()]));
        assert_eq!(known.add(open), None);
    }

    #[test]
    fn a_new_category_is_handed_over_with_every_earlier_one() {
        let open = category(&[button("open", "Open")]).expect("buttons need a category");
        let later = category(&[button("later", "Later")]).expect("buttons need a category");
        let mut known = Categories::default();
        known.add(open.clone());
        assert_eq!(known.add(later.clone()), Some(vec![open, later]));
    }

    #[test]
    fn outside_an_app_bundle_only_the_plain_banner_is_posted() {
        use Authorization::*;
        for answer in [Granted, Denied, NotDetermined, Unknown] {
            assert_eq!(route(false, answer), Route::Plain, "{answer:?}");
        }
    }

    #[test]
    fn an_app_allowed_to_notify_posts() {
        assert_eq!(route(true, Authorization::Granted), Route::Post);
    }

    #[test]
    fn an_app_macos_never_asked_asks_before_it_posts() {
        assert_eq!(route(true, Authorization::NotDetermined), Route::AskFirst);
    }

    #[test]
    fn notifications_turned_off_are_dropped() {
        assert_eq!(route(true, Authorization::Denied), Route::Drop);
    }

    #[test]
    fn an_answer_macos_didnt_give_in_time_still_posts() {
        assert_eq!(route(true, Authorization::Unknown), Route::Post);
    }

    #[test]
    fn a_notification_dismissed_before_it_goes_out_never_does() {
        let mut waiting = Waiting::default();
        waiting.ask("n-1");
        waiting.dismiss("n-1");
        assert!(!waiting.take("n-1"));
    }

    #[test]
    fn a_notification_nobody_dismissed_goes_out() {
        let mut waiting = Waiting::default();
        waiting.ask("n-1");
        assert!(waiting.take("n-1"));
    }

    #[test]
    fn every_notification_posted_under_one_id_goes_out() {
        let mut waiting = Waiting::default();
        waiting.ask("n-1");
        waiting.ask("n-1");
        assert!(waiting.take("n-1"));
        assert!(waiting.take("n-1"));
        assert!(!waiting.take("n-1"));
    }
}
