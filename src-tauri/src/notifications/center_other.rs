//! Outside macOS there is no UserNotifications, so there is never a [`Center`]
//! and every notification takes the plugin's plain banner.

use std::convert::Infallible;

use tauri::AppHandle;

use super::{Authorization, Category, Notification};

#[derive(Clone, Copy)]
pub(super) struct Center(Infallible);

impl Center {
    pub(super) fn get() -> Option<Center> {
        None
    }

    pub(super) async fn authorization(self) -> Authorization {
        match self.0 {}
    }

    pub(super) async fn request(self) {
        match self.0 {}
    }

    pub(super) fn install(self, _app: AppHandle) {
        match self.0 {}
    }

    pub(super) fn register(self, _categories: &[Category]) {
        match self.0 {}
    }

    pub(super) async fn registered(self) {
        match self.0 {}
    }

    pub(super) async fn post(
        self,
        _notification: &Notification,
        _category: Option<&str>,
    ) -> Result<(), String> {
        match self.0 {}
    }

    pub(super) fn remove(self, _id: &str) {
        match self.0 {}
    }
}
