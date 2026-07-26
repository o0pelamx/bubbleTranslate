//! Opt-in stage tracing, enabled with `BUBBLETRANSLATE_DEBUG=1`.
//!
//! The pipeline crosses three threads and two macOS APIs, so when a selection
//! produces no bubble the useful question is *which stage dropped it* — the
//! gesture filter, the capture, or the provider chain. Each stage logs one
//! line so that question is answerable without a debugger.

use std::sync::OnceLock;

static ENABLED: OnceLock<bool> = OnceLock::new();

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("BUBBLETRANSLATE_DEBUG").is_some())
}

#[macro_export]
macro_rules! trace {
    ($($arg:tt)*) => {
        if $crate::trace::enabled() {
            eprintln!("[bubbleTranslate] {}", format!($($arg)*));
        }
    };
}
