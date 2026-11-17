//! The one place the app builds HTTP clients.

/// A client builder that never goes through a proxy and does TLS with ring.
///
/// reqwest reads the macOS proxy settings but not their bypass list, so a
/// local model server or a LAN webhook would otherwise be sent to the proxy.
/// reqwest is built without a crypto provider of its own; the first one
/// installed in the process is used for every client.
pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder().no_proxy()
}

pub(crate) fn client() -> reqwest::Client {
    client_builder().build().expect("HTTP client")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    // A client built anywhere else would pick up the macOS proxy, and panic for
    // want of a crypto provider if it came first.
    #[test]
    fn every_http_client_is_built_here() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        rust_files(&root.join("src"), &mut files);
        rust_files(&root.join("crates"), &mut files);
        let allowed = [root.join("src/http.rs"), root.join("crates/cloud-sync/src/http.rs")];
        let offenders: Vec<_> = files
            .iter()
            .filter(|f| !allowed.contains(f))
            .filter(|f| {
                let text = std::fs::read_to_string(f).unwrap();
                text.contains("Client::new(") || text.contains("Client::builder(")
            })
            .collect();
        assert!(offenders.is_empty(), "build clients through http::client(): {offenders:?}");
    }
}
