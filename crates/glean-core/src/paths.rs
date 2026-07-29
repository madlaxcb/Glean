//! Default on-disk locations (local-first).

use std::path::PathBuf;

/// Portable mode data dir: if a `data` folder sits next to the running exe,
/// use it for the DB + config (dev plan §6.1 portable directory mode).
/// Returns the dir path only when it already exists.
fn portable_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let data = dir.join("data");
    if data.is_dir() {
        Some(data)
    } else {
        None
    }
}

/// `%APPDATA%\Glean\glean.db` on Windows; `~/.local/share/Glean/glean.db` elsewhere.
/// Portable mode (`./data` next to the exe) takes precedence.
pub fn default_db_path() -> PathBuf {
    if let Some(p) = portable_dir() {
        return p.join("glean.db");
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        return PathBuf::from(appdata).join("Glean").join("glean.db");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("Glean")
            .join("glean.db");
    }
    PathBuf::from("glean.db")
}

/// `config.json` next to the DB file.
pub fn default_config_path() -> PathBuf {
    default_db_path()
        .parent()
        .map(|p| p.join("config.json"))
        .unwrap_or_else(|| PathBuf::from("config.json"))
}
