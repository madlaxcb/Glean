//! Default on-disk locations (local-first).

use std::path::PathBuf;

/// `%APPDATA%\Glean\glean.db` on Windows; `~/.local/share/Glean/glean.db` elsewhere.
pub fn default_db_path() -> PathBuf {
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
