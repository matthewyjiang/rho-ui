//! Updating rho-ui itself. A release build looks for a newer release at
//! start and then every [`CHECK_EVERY`]; the sidebar's footer and Settings
//! › About offer to install it ([`render_offer`]), then to restart into it.
//! Installing runs the release's own install script
//! ([`rho_ui_core::release`]).

use std::path::PathBuf;
#[cfg(not(target_family = "wasm"))]
use std::time::Duration;

use gpui_kit::component::{
    alert::Alert, button::Button, h_flex, spinner::Spinner, v_flex, ActiveTheme as _, IconName,
    Sizable as _,
};
use gpui_kit::{
    div, AnyElement, App, Context, Entity, IntoElement, ParentElement as _, SharedString,
    Styled as _, Task,
};

use crate::rho_install::last_lines;

/// How long a release build waits between checks. Each is one request to
/// github.com, not its API, and releases come days apart.
#[cfg(not(target_family = "wasm"))]
const CHECK_EVERY: Duration = Duration::from_hours(1);

/// Whether this build updates itself: only the release builds, which
/// `.github/workflows/release.yml` makes with `RHO_UI_RELEASE` set, and
/// which the install scripts put where an update puts the next one. A
/// build from source never replaces itself with a release.
#[cfg(not(target_family = "wasm"))]
const UPDATES: bool = option_env!("RHO_UI_RELEASE").is_some();

/// What the views offer about a newer rho-ui.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    target_family = "wasm",
    expect(dead_code, reason = "a page in a browser never updates itself")
)]
pub(crate) enum AppOffer {
    /// Release `latest` is out: install it.
    Available { latest: SharedString },
    /// `latest` is installing.
    Installing { latest: SharedString },
    /// Installing `latest` failed: try again. Why, in the script's words.
    Failed {
        latest: SharedString,
        why: SharedString,
    },
    /// `latest` is installed: restart into it.
    Installed { latest: SharedString },
}

/// rho-ui's own updates.
pub(crate) struct AppUpdate {
    offer: Option<AppOffer>,
    /// This rho-ui's program as it was at start, which an update replaces
    /// and a restart runs; `None` in a build that doesn't update itself.
    /// Kept from the start because on Linux, once an update renames the new
    /// program over it, the running one's path names a deleted file.
    exe: Option<PathBuf>,
    /// The checks, or the install under way; replacing it drops the other.
    #[cfg_attr(
        target_family = "wasm",
        expect(dead_code, reason = "a page in a browser never updates itself")
    )]
    task: Option<Task<()>>,
}

impl AppUpdate {
    /// Starts checking for a newer release in a release build.
    pub(crate) fn new(cx: &mut Context<'_, Self>) -> Self {
        #[cfg(not(target_family = "wasm"))]
        if UPDATES {
            return Self {
                offer: None,
                // A symlink, as macOS's `rho-ui` on PATH is, to the program.
                exe: std::env::current_exe()
                    .and_then(|exe| rho_ui_core::platform::canonicalize(&exe))
                    .ok(),
                task: Some(Self::check(cx)),
            };
        }
        #[cfg(target_family = "wasm")]
        let _ = cx;
        Self {
            offer: None,
            exe: None,
            task: None,
        }
    }

    /// What to offer, if anything.
    pub(crate) fn offer(&self) -> Option<&AppOffer> {
        self.offer.as_ref()
    }

    /// Checks now and every [`CHECK_EVERY`] after. A failed check, as when
    /// GitHub is out of reach, leaves the offer as it was.
    #[cfg(not(target_family = "wasm"))]
    fn check(cx: &mut Context<'_, Self>) -> Task<()> {
        cx.spawn(async move |this, cx| loop {
            if let Ok(newer) = rho_ui_core::release::newer_release(env!("CARGO_PKG_VERSION")).await
            {
                let updated = this.update(cx, |this, cx| {
                    if matches!(this.offer, None | Some(AppOffer::Available { .. })) {
                        this.offer = newer.map(|latest| AppOffer::Available {
                            latest: latest.into(),
                        });
                        cx.notify();
                    }
                });
                if updated.is_err() {
                    break;
                }
            }
            cx.background_executor().timer(CHECK_EVERY).await;
        })
    }

    /// Installs the release on offer over this rho-ui.
    fn install(&mut self, cx: &mut Context<'_, Self>) {
        let latest = match &self.offer {
            Some(AppOffer::Available { latest } | AppOffer::Failed { latest, .. }) => {
                latest.clone()
            }
            _ => return,
        };
        #[cfg(not(target_family = "wasm"))]
        {
            let Some(exe) = self.exe.clone() else {
                return;
            };
            self.offer = Some(AppOffer::Installing {
                latest: latest.clone(),
            });
            cx.notify();
            self.task = Some(cx.spawn(async move |this, cx| {
                let installed = rho_ui_core::release::install_release(&latest, &exe).await;
                let _ = this.update(cx, |this, cx| {
                    this.offer = Some(match installed {
                        Ok(()) => AppOffer::Installed { latest },
                        Err(error) => AppOffer::Failed {
                            latest,
                            why: error.to_string().into(),
                        },
                    });
                    cx.notify();
                });
            }));
        }
        #[cfg(target_family = "wasm")]
        let _ = (latest, cx);
    }
}

/// Quits and starts the program installed, `exe`, with the arguments this
/// one started with (`main`). On macOS GPUI opens the app bundle, whose
/// path the update kept.
fn restart(exe: Option<PathBuf>, cx: &mut App) {
    if let Some(exe) = exe.filter(|_| !cfg!(target_os = "macos")) {
        cx.set_restart_path(exe);
    }
    cx.restart();
}

/// What `offer` says, and the button that acts on it: the sidebar's footer
/// and the About page draw it.
pub(crate) fn render_offer(update: &Entity<AppUpdate>, offer: &AppOffer, cx: &App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    let line = |text: String| div().text_xs().text_color(muted).child(text);
    let button = |id: &'static str, label: &'static str, icon: IconName| {
        Button::new(id).outline().xsmall().icon(icon).label(label)
    };
    let install = {
        let update = update.clone();
        move |_: &gpui_kit::ClickEvent, _: &mut gpui_kit::Window, cx: &mut App| {
            update.update(cx, AppUpdate::install);
        }
    };
    let row = |text: String, button: Button| {
        h_flex()
            .gap_2()
            .justify_between()
            .child(div().flex_1().min_w_0().child(line(text)))
            .child(button)
            .into_any_element()
    };
    match offer {
        AppOffer::Available { latest } => row(
            format!("rho-ui v{latest} available"),
            button("update-rho-ui", "Update", IconName::ArrowDown).on_click(install),
        ),
        AppOffer::Installing { latest } => h_flex()
            .gap_1()
            .child(Spinner::new().xsmall())
            .child(line(format!("Updating rho-ui to v{latest}…")))
            .into_any_element(),
        AppOffer::Failed { why, .. } => {
            v_flex()
                .gap_1()
                .child(
                    Alert::error("rho-ui-update-failed", last_lines(why))
                        .title("Couldn't update rho-ui")
                        .small(),
                )
                .child(h_flex().justify_end().child(
                    button("retry-rho-ui", "Try again", IconName::RotateCw).on_click(install),
                ))
                .into_any_element()
        }
        AppOffer::Installed { latest } => {
            let update = update.clone();
            row(
                format!("rho-ui v{latest} is installed"),
                button("restart-rho-ui", "Restart", IconName::RotateCw)
                    .on_click(move |_, _, cx| restart(update.read(cx).exe.clone(), cx)),
            )
        }
    }
}
