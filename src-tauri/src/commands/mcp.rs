//! The one command the MCP integration needs from the app (#172): where the
//! server binary actually is.
//!
//! Everything else about the integration is a plain setting (`mcp_enabled`, read
//! by the server process itself) — but the client config snippet Settings offers
//! has to name a real path, and that path differs between a bundled `.app` and a
//! `cargo`/`tauri dev` build. Computing it in the frontend would mean the frontend
//! knowing how macOS lays out a bundle.

use super::err;

/// Absolute path to the `humla-mcp` executable, resolved next to the running
/// binary — `Humla.app/Contents/MacOS/humla-mcp` in a bundle, and `target/debug/
/// humla-mcp` under `tauri dev`, which is what makes the snippet in Settings
/// copyable during development too.
///
/// Returns the path whether or not the file exists. A missing binary is a build
/// problem, and a snippet that silently disappears is harder to report than one
/// that names a path the user can check.
#[tauri::command]
pub fn mcp_server_path() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(err)?;
    let dir = exe.parent().ok_or_else(|| "no parent directory".to_string())?;
    dir.join("humla-mcp")
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| "non-utf8 path".to_string())
}
