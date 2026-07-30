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

/// Base data directory shared by DB, config and cache. Portable mode
/// (`./data` next to the exe) takes precedence; then `%APPDATA%\Glean`
/// (Windows) or `~/.local/share/Glean`. Returns `None` only when neither
/// `APPDATA` nor `HOME` is set.
fn data_base_dir() -> Option<PathBuf> {
    if let Some(p) = portable_dir() {
        return Some(p);
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        return Some(PathBuf::from(appdata).join("Glean"));
    }
    if let Ok(home) = std::env::var("HOME") {
        return Some(
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("Glean"),
        );
    }
    None
}

/// `%APPDATA%\Glean\glean.db` on Windows; `~/.local/share/Glean/glean.db` elsewhere.
/// Portable mode (`./data` next to the exe) takes precedence.
pub fn default_db_path() -> PathBuf {
    data_base_dir()
        .map(|b| b.join("glean.db"))
        .unwrap_or_else(|| PathBuf::from("glean.db"))
}

/// Disk cache dir for entry bodies (dev plan §2.5): `<data_dir>/cache/entries/`.
/// Returns `None` when no base data dir can be resolved.
pub fn cache_entries_dir() -> Option<PathBuf> {
    data_base_dir().map(|b| b.join("cache").join("entries"))
}

/// Disk cache dir for downloaded images (dev plan §2.5.2): `<data_dir>/cache/images/`.
/// Returns `None` when no base data dir can be resolved.
pub fn cache_images_dir() -> Option<PathBuf> {
    data_base_dir().map(|b| b.join("cache").join("images"))
}

/// Disk cache dir for favicons: `<data_dir>/cache/favicons/`.
pub fn cache_favicons_dir() -> Option<PathBuf> {
    data_base_dir().map(|b| b.join("cache").join("favicons"))
}

/// `config.json` next to the DB file.
pub fn default_config_path() -> PathBuf {
    default_db_path()
        .parent()
        .map(|p| p.join("config.json"))
        .unwrap_or_else(|| PathBuf::from("config.json"))
}

/// Clear all cache subdirectories (entries, images, favicons).
/// Returns the number of files removed.
pub fn clear_all_cache() -> u64 {
    let dirs = [
        cache_entries_dir(),
        cache_images_dir(),
        cache_favicons_dir(),
    ];
    let mut removed = 0u64;
    for dir in dirs.iter().flatten() {
        if !dir.exists() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.path().is_file() {
                    if std::fs::remove_file(entry.path()).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
    }
    removed
}
