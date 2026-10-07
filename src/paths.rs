//! Where FeatherBrowser keeps its data.
//!
//! One directory for everything persistent: the session database and the filter lists.
//! Not the working directory — which session you got used to depend on where the binary
//! was launched from.

use std::path::PathBuf;

/// `~/Library/Application Support/FeatherBrowser` on macOS, `$XDG_DATA_HOME/FeatherBrowser`
/// (or `~/.local/share/...`) elsewhere. Falls back to the working directory when there is
/// no home to put it in.
pub fn data_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);

    if cfg!(target_os = "macos") {
        if let Some(home) = home {
            return home.join("Library/Application Support/FeatherBrowser");
        }
    } else if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(data).join("FeatherBrowser");
    } else if let Some(home) = home {
        return home.join(".local/share/FeatherBrowser");
    }

    PathBuf::from(".")
}

/// The data directory, created if it does not exist. Falls back to the working directory
/// when it cannot be created, so a bad HOME costs you persistence across launches rather
/// than the browser.
pub fn ensure_data_dir() -> PathBuf {
    let dir = data_dir();
    match std::fs::create_dir_all(&dir) {
        Ok(()) => dir,
        Err(e) => {
            log::warn!("Could not create {}: {}. Falling back to the working directory.", dir.display(), e);
            PathBuf::from(".")
        }
    }
}
