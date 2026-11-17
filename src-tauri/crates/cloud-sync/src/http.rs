//! The one place this crate builds HTTP clients; see the app's `http` module
//! for why each one turns proxies off and installs ring.

pub(crate) fn client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder().no_proxy().build().expect("HTTP client")
}
