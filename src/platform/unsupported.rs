//! Content-rule blocking is a WebKit feature. On other platforms the browser still runs;
//! it just falls back to navigation-level filtering via `TrafficFilter`.

use log::warn;
use std::sync::mpsc::Sender;
use wry::WebView;

use crate::app::AppEvent;
use crate::network::ContentRules;

#[derive(Clone, Default)]
pub struct ContentBlocker;

impl ContentBlocker {
    pub fn new() -> Self {
        Self
    }

    pub fn is_ready(&self) -> bool {
        false
    }

    pub fn rule_count(&self) -> usize {
        0
    }

    pub fn apply_to(&self, _webview: &WebView) {}

    pub fn start(&self, _rules: ContentRules, _notify: Sender<AppEvent>) {
        warn!("Subresource content blocking is only implemented for WebKit (macOS).");
    }
}

/// No native save panel here, so downloads go straight to ~/Downloads without asking.
// ponytail: a GTK/Win32 file dialog is a whole dependency for a platform this browser
// does not block on yet. Add one when there is a non-macOS user.
pub fn ask_save_path(suggested_name: &str) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(std::path::PathBuf::from(home).join("Downloads").join(suggested_name))
}
