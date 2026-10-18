//! Log files players can attach to a bug report.
//!
//! Never log API keys, screenshots, questions, answers or window titles: logs get shared.

use std::path::PathBuf;

use cue_core::Config;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

/// Daily files, so this is a week of logs.
const KEEP_FILES: usize = 7;

/// Cue's own messages from `info` up; libraries only from `warn`, so their chatter doesn't
/// bury them. GPUI warns about every duplicate system font on macOS.
const DEFAULT_FILTER: &str = "warn,cue=info,cue_core=info,gpui_apple::text_system=error";

/// Sends logs to daily files in [`Config::log_dir`] and, in debug builds, to stderr too.
/// `CUE_LOG` overrides the filter, e.g. `CUE_LOG=debug`. Returns the log directory if
/// file logging works.
pub fn init() -> Option<PathBuf> {
    let filter =
        EnvFilter::try_from_env("CUE_LOG").unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let dir = Config::log_dir();
    // The appender writes synchronously, so a panic's message reaches the file even
    // when the process aborts right after.
    let file = dir.as_ref().and_then(|dir| {
        Builder::new()
            .rotation(Rotation::DAILY)
            .filename_prefix("cue")
            .filename_suffix("log")
            .max_log_files(KEEP_FILES)
            .build(dir)
            .ok()
    });
    let file_works = file.is_some();
    let file_layer = file.map(|appender| fmt::layer().with_ansi(false).with_writer(appender));
    let stderr_layer = cfg!(debug_assertions).then(|| fmt::layer().with_writer(std::io::stderr));
    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .init();

    log_panics();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        "Cue starting"
    );
    if !file_works {
        tracing::warn!(dir = ?dir, "couldn't open the log directory; logging to stderr only");
    }
    dir.filter(|_| file_works)
}

fn log_panics() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("panic: {info}\n{backtrace}");
        default_hook(info);
    }));
}
