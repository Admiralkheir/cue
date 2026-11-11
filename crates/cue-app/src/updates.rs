//! Checking for and installing new releases, shown in the settings window.

use cue_core::update::{self, Release};
use gpui_kit::{App, AppContext as _};

use crate::app_state::AppState;

/// Where the update check stands, in `AppState::update`.
#[derive(Debug, Clone, Default)]
pub enum UpdateStatus {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Installing(Release),
    Restarting(Release),
    Failed {
        /// Kept when installing failed, so the user can try again.
        release: Option<Release>,
        error: String,
    },
}

impl UpdateStatus {
    pub fn busy(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::Installing(_) | Self::Restarting(_)
        )
    }
}

/// Cleans up after the last update and, if the user wants it, checks for the next one.
pub fn on_startup(cx: &mut App) {
    cx.background_spawn(async {
        let removed = update::clean_up();
        if removed > 0 {
            tracing::info!(removed, "cleaned up after the last update");
        }
    })
    .detach();

    let settings = cx.global::<AppState>().config.updates;
    if settings.checks_on_startup() {
        check(settings.install_automatically, cx);
    }
}

/// Asks GitHub for a newer release; with `install`, installs it and restarts into it.
pub fn check(install_if_newer: bool, cx: &mut App) {
    if cx.global::<AppState>().update.busy() {
        return;
    }
    set_status(UpdateStatus::Checking, cx);
    let task = cx.background_spawn(async { update::check() });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| match result {
            Ok(Some(release)) => {
                tracing::info!(version = %release.version, "a new version is available");
                if install_if_newer && update::install_blocker().is_none() {
                    install(release, cx);
                } else {
                    set_status(UpdateStatus::Available(release), cx);
                }
            }
            Ok(None) => {
                tracing::info!("Cue is up to date");
                set_status(UpdateStatus::UpToDate, cx);
            }
            Err(err) => {
                tracing::warn!("couldn't check for updates: {err}");
                set_status(
                    UpdateStatus::Failed {
                        release: None,
                        error: format!("Couldn't check for updates: {err}"),
                    },
                    cx,
                );
            }
        });
    })
    .detach();
}

/// Downloads and installs `release`, then restarts Cue into it.
pub fn install(release: Release, cx: &mut App) {
    if matches!(
        cx.global::<AppState>().update,
        UpdateStatus::Installing(_) | UpdateStatus::Restarting(_)
    ) {
        return;
    }
    set_status(UpdateStatus::Installing(release.clone()), cx);
    tracing::info!(version = %release.version, "installing the update");
    let task = {
        let release = release.clone();
        cx.background_spawn(async move { update::install(&release) })
    };
    cx.spawn(async move |cx| {
        let installed = task.await;
        cx.update(|cx| {
            let installed = match installed {
                Ok(installed) => installed,
                Err(err) => {
                    tracing::error!("couldn't install the update: {err}");
                    set_status(
                        UpdateStatus::Failed {
                            error: format!("Couldn't install Cue {}: {err}", release.version),
                            release: Some(release),
                        },
                        cx,
                    );
                    return;
                }
            };
            tracing::info!(version = %release.version, "update installed; restarting");
            match installed.relaunch() {
                Ok(()) => {
                    set_status(UpdateStatus::Restarting(release), cx);
                    cx.quit();
                }
                Err(err) => {
                    tracing::error!("couldn't restart into the update: {err}");
                    set_status(
                        UpdateStatus::Failed {
                            error: format!(
                                "Cue {} is installed but couldn't restart ({err}). It runs \
                                 the next time you open Cue.",
                                release.version
                            ),
                            release: None,
                        },
                        cx,
                    );
                }
            }
        });
    })
    .detach();
}

fn set_status(status: UpdateStatus, cx: &mut App) {
    cx.global_mut::<AppState>().update = status;
    cx.refresh_windows();
}
