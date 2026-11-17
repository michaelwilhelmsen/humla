//! Finding the Copilot CLI, and downloading it when there is none. npm installs
//! `copilot` as a Node loader around a native binary in a per-platform package;
//! Humla runs that binary directly, so `node` is never needed. A Finder-
//! launched app sees only `/usr/bin:/bin:/usr/sbin:/sbin`, so the user's own
//! install is looked for in the usual roots and then on the login shell's PATH.
//! When none turns up, Humla downloads the version the SDK was built against,
//! checks it against a pinned sha512 and GitHub's code signature, and keeps it
//! under Application Support.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use base64::Engine as _;
use sha2::{Digest, Sha512};

/// The CLI `github-copilot-sdk` 1.0.27 was published against.
pub const VERSION: &str = "1.0.104";

/// GitHub's Apple team, which signs the CLI binary.
const GITHUB_TEAM_ID: &str = "VEKTX9H2N7";

/// The npm package for this Mac and its sha512, as the registry lists it.
fn pinned(arch: &str) -> Option<(&'static str, &'static str)> {
    match arch {
        "aarch64" => Some((
            "copilot-darwin-arm64",
            "sha512-CpnR+JyacZZyMf/jeKLCewQJNT1aHcCxwCSYKqM87Zn62YMB0fJJ7vqvvIzVYvoqHb72i9BszpO/iCuKSoma5g==",
        )),
        "x86_64" => Some((
            "copilot-darwin-x64",
            "sha512-pwI5YaVhC2ESsI9lZR+jLaXeb4zlCguWaU8fXZU+/+nD8JkW7mzoGF1xTPvD/JNRrDMpIc8YW38cUv2RpbehgA==",
        )),
        _ => None,
    }
}

fn npm_arch(arch: &str) -> &str {
    match arch {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    }
}

/// Where Humla keeps a CLI it downloaded.
pub fn managed_path(app_data: &Path) -> PathBuf {
    app_data.join("copilot").join(VERSION).join("copilot")
}

/// What the user's login shell knows: its PATH, and npm's global prefix, whose
/// `bin` npm doesn't always put on that PATH.
#[derive(Clone, Debug, PartialEq)]
pub struct LoginShell {
    pub path: String,
    pub npm_prefix: Option<String>,
}

const PATH_MARK: &str = "__HUMLA_PATH__";
const NPM_MARK: &str = "__HUMLA_NPM__";

impl LoginShell {
    /// Read the shell's answer between markers, ignoring whatever the
    /// profile printed around it.
    pub fn parse(out: &str) -> Option<Self> {
        let between = |mark: &str| {
            let (_, rest) = out.split_once(mark)?;
            rest.split_once(mark).map(|(v, _)| v.trim().to_string())
        };
        Some(LoginShell {
            path: between(PATH_MARK)?,
            npm_prefix: between(NPM_MARK).filter(|p| p.starts_with('/')),
        })
    }
}

/// Every place the CLI may be, in the order they are tried: an explicit
/// `COPILOT_CLI_PATH`, Humla's own download, the usual install roots, then the
/// login shell's PATH and npm prefix. A download comes before the user's own
/// install because its version is the one the SDK speaks.
pub fn candidates(
    env_override: Option<&Path>,
    managed: &Path,
    home: &Path,
    shell: Option<&LoginShell>,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    if let Some(p) = env_override {
        push(p.to_path_buf());
    }
    push(managed.to_path_buf());
    for dir in [
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".local/bin"),
        home.join(".npm-global/bin"),
        home.join(".volta/bin"),
        home.join("bin"),
    ] {
        push(dir.join("copilot"));
    }
    if let Some(shell) = shell {
        for dir in shell.path.split(':').filter(|d| d.starts_with('/')) {
            push(Path::new(dir).join("copilot"));
        }
        if let Some(prefix) = &shell.npm_prefix {
            push(Path::new(prefix).join("bin/copilot"));
        }
    }
    out
}

const MACH_O_MAGICS: [[u8; 4]; 3] = [
    [0xcf, 0xfa, 0xed, 0xfe], // 64-bit, little-endian
    [0xca, 0xfe, 0xba, 0xbe], // universal
    [0xfe, 0xed, 0xfa, 0xcf], // 64-bit, big-endian
];

/// The native binary behind `path`: the file itself when it is a Mach-O, the
/// platform package's binary when it is npm's Node loader, else nothing.
pub fn native_binary(path: &Path, arch: &str) -> Option<PathBuf> {
    let real = std::fs::canonicalize(path).ok()?;
    let mut head = [0u8; 4];
    std::fs::File::open(&real).ok()?.read_exact(&mut head).ok()?;
    if MACH_O_MAGICS.contains(&head) {
        return Some(real);
    }
    if !head.starts_with(b"#!") {
        return None;
    }
    // The loader is `<prefix>/lib/node_modules/@github/copilot/npm-loader.js`;
    // npm puts the platform package beneath it or, hoisted, beside it.
    let package = format!("copilot-darwin-{}", npm_arch(arch));
    let pkg_dir = real.parent()?;
    [
        pkg_dir.join("node_modules/@github").join(&package).join("copilot"),
        pkg_dir.parent()?.join(&package).join("copilot"),
    ]
    .into_iter()
    .find_map(|p| native_binary(&p, arch).filter(|_| p.is_file()))
}

/// The first candidate that resolves to a native binary.
pub fn find(candidates: &[PathBuf], arch: &str) -> Option<PathBuf> {
    candidates.iter().find_map(|c| native_binary(c, arch))
}

/// Check `bytes` against an npm `sha512-<base64>` integrity string.
pub fn verify_integrity(bytes: &[u8], integrity: &str) -> anyhow::Result<()> {
    let expected = integrity
        .strip_prefix("sha512-")
        .ok_or_else(|| anyhow!("unsupported integrity {integrity}"))?;
    let actual = base64::engine::general_purpose::STANDARD.encode(Sha512::digest(bytes));
    if actual != expected {
        bail!("the downloaded Copilot CLI doesn't match its checksum");
    }
    Ok(())
}

/// Unpack `package/copilot` from an npm tarball to `dest`, executable.
pub fn extract_binary(tgz: &[u8], dest: &Path) -> anyhow::Result<()> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tgz));
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() == Path::new("package/copilot") {
            let parent = dest.parent().context("destination has no parent")?;
            std::fs::create_dir_all(parent)?;
            let tmp = parent.join(".copilot.partial");
            let mut out = std::fs::File::create(&tmp)?;
            std::io::copy(&mut entry, &mut out)?;
            drop(out);
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
            std::fs::rename(&tmp, dest)?;
            return Ok(());
        }
    }
    bail!("the Copilot CLI package holds no binary")
}

// ---- the machine ------------------------------------------------------------

pub fn arch() -> &'static str {
    std::env::consts::ARCH
}

/// Ask the user's login shell once, with a 5 s limit. `None` on any failure.
pub async fn login_shell() -> Option<LoginShell> {
    let shell = std::env::var("SHELL").ok().filter(|s| s.starts_with('/'))?;
    let script = format!(
        "printf '{PATH_MARK}%s{PATH_MARK}' \"$PATH\"; \
         printf '{NPM_MARK}%s{NPM_MARK}' \"$(npm prefix -g 2>/dev/null)\""
    );
    let run = tokio::process::Command::new(shell)
        .args(["-ilc", &script])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), run).await.ok()?.ok()?;
    LoginShell::parse(&String::from_utf8_lossy(&out.stdout))
}

/// Look for the CLI everywhere, the login shell last since it costs a process.
pub async fn locate(app_data: &Path) -> Option<PathBuf> {
    let env_override = std::env::var_os("COPILOT_CLI_PATH").map(PathBuf::from);
    let managed = managed_path(app_data);
    let home = dirs::home_dir()?;
    let quick = candidates(env_override.as_deref(), &managed, &home, None);
    if let Some(found) = find(&quick, arch()) {
        return Some(found);
    }
    let shell = login_shell().await?;
    find(&candidates(None, &managed, &home, Some(&shell)), arch())
}

/// `copilot --version`'s version number.
pub async fn version(binary: &Path) -> Option<String> {
    let run = tokio::process::Command::new(binary)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), run).await.ok()?.ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(|w| w.trim_end_matches('.').to_string())
}

/// Download the pinned CLI to [`managed_path`], reporting the fraction done.
pub async fn download(
    app_data: &Path,
    mut on_progress: impl FnMut(f64) + Send,
) -> anyhow::Result<PathBuf> {
    use futures_util::StreamExt;
    let (package, integrity) =
        pinned(arch()).ok_or_else(|| anyhow!("Copilot isn't available for this Mac"))?;
    let url = format!("https://registry.npmjs.org/@github/{package}/-/{package}-{VERSION}.tgz");
    let r = crate::http::client()
        .get(&url)
        .header("User-Agent", super::auth::user_agent())
        .send()
        .await
        .context("couldn't reach the npm registry")?
        .error_for_status()
        .context("the npm registry refused the download")?;
    let total = r.content_length().unwrap_or(0);
    let mut bytes = Vec::with_capacity(total as usize);
    let mut stream = r.bytes_stream();
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk.context("the download was interrupted")?);
        if total > 0 {
            on_progress(bytes.len() as f64 / total as f64);
        }
    }
    verify_integrity(&bytes, integrity)?;
    let dest = managed_path(app_data);
    let unverified = dest.with_file_name("copilot.unverified");
    tokio::task::spawn_blocking({
        let unverified = unverified.clone();
        move || extract_binary(&bytes, &unverified)
    })
    .await??;
    verify_signature(&unverified).await?;
    std::fs::rename(&unverified, &dest)?;
    Ok(dest)
}

/// Refuse a binary GitHub didn't sign.
async fn verify_signature(binary: &Path) -> anyhow::Result<()> {
    let requirement = format!("=anchor apple generic and certificate leaf[subject.OU] = \"{GITHUB_TEAM_ID}\"");
    let status = tokio::process::Command::new("/usr/bin/codesign")
        .args(["--verify", "--strict", "-R"])
        .arg(&requirement)
        .arg(binary)
        .status()
        .await?;
    if !status.success() {
        let _ = std::fs::remove_file(binary);
        bail!("the downloaded Copilot CLI isn't signed by GitHub");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACH_O: &[u8] = &[0xcf, 0xfa, 0xed, 0xfe, 0, 0, 0, 0];

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn candidates_run_from_explicit_to_the_login_shell() {
        let home = Path::new("/Users/u");
        let managed = Path::new("/data/copilot/1.0.104/copilot");
        let got = candidates(
            Some(Path::new("/custom/copilot")),
            managed,
            home,
            Some(&LoginShell {
                path: "/Users/u/.nvm/versions/node/v22/bin:/usr/local/bin:relative/bin".into(),
                npm_prefix: Some("/Users/u/.hermes/node".into()),
            }),
        );
        assert_eq!(
            got,
            [
                "/custom/copilot",
                "/data/copilot/1.0.104/copilot",
                "/opt/homebrew/bin/copilot",
                "/usr/local/bin/copilot",
                "/Users/u/.local/bin/copilot",
                "/Users/u/.npm-global/bin/copilot",
                "/Users/u/.volta/bin/copilot",
                "/Users/u/bin/copilot",
                "/Users/u/.nvm/versions/node/v22/bin/copilot",
                "/Users/u/.hermes/node/bin/copilot",
            ]
            .map(PathBuf::from)
        );
    }

    /// Fetches ~120 MB from npm. Run with
    /// `cargo test --lib copilot_download_live -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn copilot_download_live() {
        let dir = tempfile::tempdir().unwrap();
        let mut last = 0.0;
        let bin = download(dir.path(), |f| last = f).await.unwrap();
        assert_eq!(bin, managed_path(dir.path()));
        assert!(last > 0.99);
        assert_eq!(version(&bin).await.as_deref(), Some(VERSION));
    }

    #[test]
    fn the_login_shell_answer_is_read_between_its_markers() {
        let out = "Last login: Tue\nwelcome!\n__HUMLA_PATH__/a:/b__HUMLA_PATH____HUMLA_NPM__/Users/u/.hermes/node__HUMLA_NPM__";
        assert_eq!(
            LoginShell::parse(out),
            Some(LoginShell { path: "/a:/b".into(), npm_prefix: Some("/Users/u/.hermes/node".into()) })
        );
        assert_eq!(
            LoginShell::parse("__HUMLA_PATH__/a__HUMLA_PATH____HUMLA_NPM____HUMLA_NPM__"),
            Some(LoginShell { path: "/a".into(), npm_prefix: None })
        );
        assert_eq!(LoginShell::parse("zsh: no such file"), None);
    }

    #[test]
    fn a_native_binary_resolves_to_itself() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin/copilot");
        write(&bin, MACH_O);
        assert_eq!(native_binary(&bin, "aarch64"), Some(bin.canonicalize().unwrap()));
    }

    #[test]
    fn the_npm_loader_resolves_to_its_platform_binary() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("lib/node_modules/@github/copilot");
        write(&pkg.join("npm-loader.js"), b"#!/usr/bin/env node\n");
        let native = pkg.join("node_modules/@github/copilot-darwin-arm64/copilot");
        write(&native, MACH_O);
        let link = dir.path().join("bin/copilot");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(pkg.join("npm-loader.js"), &link).unwrap();

        assert_eq!(native_binary(&link, "aarch64"), Some(native.canonicalize().unwrap()));
        assert_eq!(native_binary(&link, "x86_64"), None);
    }

    #[test]
    fn a_hoisted_platform_package_is_found_too() {
        let dir = tempfile::tempdir().unwrap();
        let modules = dir.path().join("lib/node_modules/@github");
        write(&modules.join("copilot/npm-loader.js"), b"#!/usr/bin/env node\n");
        let native = modules.join("copilot-darwin-x64/copilot");
        write(&native, MACH_O);
        assert_eq!(
            native_binary(&modules.join("copilot/npm-loader.js"), "x86_64"),
            Some(native.canonicalize().unwrap())
        );
    }

    #[test]
    fn something_else_called_copilot_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("a/copilot");
        write(&first, b"not a program");
        let second = dir.path().join("b/copilot");
        write(&second, MACH_O);
        let missing = dir.path().join("c/copilot");
        assert_eq!(
            find(&[missing, first, second.clone()], "aarch64"),
            Some(second.canonicalize().unwrap())
        );
    }

    #[test]
    fn integrity_accepts_the_right_bytes_and_refuses_others() {
        // sha512("hello"), as npm writes it.
        let hello = "sha512-m3HSJL1i83hdltRq0+o9czGb+8KJDKra4t/3JRlnPKcjI8PZm6XBHXx6zG4UuMXaDEZjR1wuXDre9G9zvN7AQw==";
        assert!(verify_integrity(b"hello", hello).is_ok());
        assert!(verify_integrity(b"hellO", hello).is_err());
        assert!(verify_integrity(b"hello", "sha1-abc").is_err());
    }

    #[test]
    fn the_binary_is_unpacked_executable_from_the_tarball() {
        let mut tar_bytes = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut tar_bytes, flate2::Compression::fast());
            let mut builder = tar::Builder::new(gz);
            for (name, body) in [("package/package.json", &b"{}"[..]), ("package/copilot", MACH_O)] {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, name, body).unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("copilot/1.0.104/copilot");

        extract_binary(&tar_bytes, &dest).unwrap();

        assert_eq!(std::fs::read(&dest).unwrap(), MACH_O);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777, 0o755);
        assert!(native_binary(&dest, "aarch64").is_some());
    }

    #[test]
    fn a_tarball_without_the_binary_is_refused() {
        let mut tar_bytes = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut tar_bytes, flate2::Compression::fast());
            let mut builder = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(2);
            header.set_cksum();
            builder.append_data(&mut header, "package/package.json", &b"{}"[..]).unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(extract_binary(&tar_bytes, &dir.path().join("copilot")).is_err());
    }
}
