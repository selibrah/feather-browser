//! Platform-specific integration. Everything here presents the same API on every target;
//! only macOS currently has a real implementation.

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{ask_save_path, ContentBlocker};

#[cfg(not(target_os = "macos"))]
mod unsupported;
#[cfg(not(target_os = "macos"))]
pub use unsupported::{ask_save_path, ContentBlocker};
