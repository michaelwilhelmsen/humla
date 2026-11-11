//! Every UserNotifications call, each a method on [`Center`].

use std::path::Path;
use std::ptr::NonNull;
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_foundation::{NSArray, NSError, NSSet, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationAction,
    UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSettings, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};
use parking_lot::Mutex;
use tauri::AppHandle;
use tokio::sync::oneshot;

use super::{Authorization, Category, Notification};

/// How long a request waits for the user to answer macOS's prompt.
const ANSWER_WAIT: Duration = Duration::from_secs(300);
/// How long any other call waits for macOS to reply.
const REPLY_WAIT: Duration = Duration::from_secs(2);

/// UserNotifications, which raises for a process outside an app bundle (a
/// `tauri dev` binary), so there is a `Center` only inside one.
#[derive(Clone, Copy)]
pub(super) struct Center(());

impl Center {
    pub(super) fn get() -> Option<Center> {
        let bundled = std::env::current_exe().is_ok_and(|exe| in_app_bundle(&exe));
        bundled.then_some(Center(()))
    }

    pub(super) async fn authorization(self) -> Authorization {
        let (reply, answer) = reply();
        // Scoped so nothing that isn't `Send` is held across the `.await`.
        {
            let handler = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
                // SAFETY: macOS hands the handler a live settings object.
                reply.send(unsafe { settings.as_ref() }.authorizationStatus().into());
            });
            center().getNotificationSettingsWithCompletionHandler(&handler);
        }
        within(REPLY_WAIT, answer).await.unwrap_or(Authorization::Unknown)
    }

    pub(super) async fn request(self) {
        let (reply, answer) = reply();
        {
            let handler = RcBlock::new(move |_granted: Bool, _error: *mut NSError| reply.send(()));
            center().requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &handler,
            );
        }
        within(ANSWER_WAIT, answer).await;
    }

    pub(super) fn install(self, app: AppHandle) {
        let delegate = Delegate::new(app);
        center().setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // The center holds its delegate weakly, so this one is kept for the
        // life of the process.
        std::mem::forget(delegate);
    }

    /// Replaces every category macOS holds for Humla with `categories`.
    pub(super) fn register(self, categories: &[Category]) {
        let natives: Vec<Retained<UNNotificationCategory>> =
            categories.iter().map(native).collect();
        center().setNotificationCategories(&NSSet::from_retained_slice(&natives));
    }

    /// Waits until macOS holds the categories registered so far: a notification
    /// posted under one it doesn't hold yet shows without its buttons.
    pub(super) async fn registered(self) {
        let (reply, answer) = reply();
        {
            let handler =
                RcBlock::new(move |_: NonNull<NSSet<UNNotificationCategory>>| reply.send(()));
            center().getNotificationCategoriesWithCompletionHandler(&handler);
        }
        within(REPLY_WAIT, answer).await;
    }

    pub(super) async fn post(
        self,
        notification: &Notification,
        category: Option<&str>,
    ) -> Result<(), String> {
        let (reply, answer) = reply();
        {
            let content = UNMutableNotificationContent::new();
            content.setTitle(&NSString::from_str(&notification.title));
            content.setBody(&NSString::from_str(&notification.body));
            if let Some(category) = category {
                content.setCategoryIdentifier(&NSString::from_str(category));
            }
            let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
                &NSString::from_str(&notification.id),
                &content,
                None,
            );
            let handler = RcBlock::new(move |error: *mut NSError| {
                // SAFETY: macOS hands the handler a live error, or none.
                let error = unsafe { error.as_ref() };
                reply.send(error.map(|e| e.localizedDescription().to_string()));
            });
            center().addNotificationRequest_withCompletionHandler(&request, Some(&handler));
        }
        match within(REPLY_WAIT, answer).await {
            Some(Some(failure)) => Err(failure),
            _ => Ok(()),
        }
    }

    pub(super) fn remove(self, id: &str) {
        let ids = NSArray::from_retained_slice(&[NSString::from_str(id)]);
        center().removePendingNotificationRequestsWithIdentifiers(&ids);
        center().removeDeliveredNotificationsWithIdentifiers(&ids);
    }
}

fn center() -> Retained<UNUserNotificationCenter> {
    UNUserNotificationCenter::currentNotificationCenter()
}

fn in_app_bundle(exe: &Path) -> bool {
    exe.to_string_lossy().contains(".app/Contents/MacOS/")
}

/// A completion handler's way back to the task waiting on it. Handlers are
/// `Fn` blocks, so the sender is taken out on the first call.
struct Reply<T>(Mutex<Option<oneshot::Sender<T>>>);

impl<T> Reply<T> {
    fn send(&self, value: T) {
        if let Some(tx) = self.0.lock().take() {
            let _ = tx.send(value);
        }
    }
}

fn reply<T>() -> (Reply<T>, oneshot::Receiver<T>) {
    let (tx, rx) = oneshot::channel();
    (Reply(Mutex::new(Some(tx))), rx)
}

async fn within<T>(wait: Duration, answer: oneshot::Receiver<T>) -> Option<T> {
    tokio::time::timeout(wait, answer).await.ok().and_then(Result::ok)
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Delegate` doesn't
    // implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "HumlaNotificationDelegate"]
    #[ivars = AppHandle]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        // Without this, macOS shows nothing while Humla is the active app.
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion_handler: &DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion_handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &DynBlock<dyn Fn()>,
        ) {
            let id = response.notification().request().identifier().to_string();
            super::clicked(self.ivars(), &id, &response.actionIdentifier().to_string());
            completion_handler.call(());
        }
    }
);

impl Delegate {
    fn new(app: AppHandle) -> Retained<Self> {
        let this = Self::alloc().set_ivars(app);
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}

fn native(category: &Category) -> Retained<UNNotificationCategory> {
    let actions: Vec<Retained<UNNotificationAction>> = category
        .actions
        .iter()
        .map(|action| {
            UNNotificationAction::actionWithIdentifier_title_options(
                &NSString::from_str(&action.id),
                &NSString::from_str(&action.title),
                UNNotificationActionOptions::empty(),
            )
        })
        .collect();
    UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
        &NSString::from_str(&category.id),
        &NSArray::from_retained_slice(&actions),
        &NSArray::<NSString>::new(),
        UNNotificationCategoryOptions::empty(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_bundled_app_asks_macos() {
        assert!(in_app_bundle(Path::new("/Applications/Humla.app/Contents/MacOS/humla")));
        assert!(!in_app_bundle(Path::new("/Users/me/humla/src-tauri/target/debug/humla")));
    }
}
