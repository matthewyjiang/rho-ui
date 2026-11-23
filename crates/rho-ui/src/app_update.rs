//! Updating rho-ui itself. A release build looks for a newer release at
//! start and then every [`CHECK_EVERY`]; the sidebar's footer and Settings
//! › About offer to install it ([`render_offer`]), then to restart into it.
//! Installing runs the release's own install script
//! ([`rho_ui_core::release`]).

#[cfg(not(target_family = "wasm"))]
use std::time::Duration;

use gpui_kit::component::{
    alert::Alert, button::Button, h_flex, spinner::Spinner, v_flex, ActiveTheme as _, IconName,
    Sizable as _,
};
#[cfg(not(target_family = "wasm"))]
use gpui_kit::Task;
use gpui_kit::{
    div, AnyElement, App, Context, Entity, IntoElement, ParentElement as _, SharedString,
    Styled as _,
};
#[cfg(not(target_family = "wasm"))]
use rho_ui_core::release::{self, Installation};

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
    /// `None` in a build that doesn't update itself, or one the install
    /// scripts didn't put in place.
    #[cfg(not(target_family = "wasm"))]
    updater: Option<Updater>,
}

/// The installation an update replaces, and the work on it.
#[cfg(not(target_family = "wasm"))]
struct Updater {
    /// Found at start: on Linux, once an update renames the new program
    /// over the old, the running one's path names a deleted file.
    installation: Installation,
    /// Every [`CHECK_EVERY`], for as long as rho-ui runs.
    _checks: Task<()>,
    /// The install under way, if one is.
    install: Option<Task<()>>,
}

impl AppUpdate {
    /// Starts checking for a newer release in a release build.
    pub(crate) fn new(cx: &mut Context<'_, Self>) -> Self {
        #[cfg(target_family = "wasm")]
        let _ = cx;
        Self {
            offer: None,
            #[cfg(not(target_family = "wasm"))]
            updater: Self::updater(cx),
        }
    }

    /// What to offer, if anything.
    pub(crate) fn offer(&self) -> Option<&AppOffer> {
        self.offer.as_ref()
    }

    /// Starts the checks, if this build updates itself and the install
    /// scripts put it in place. Says why not on stderr, where a launch from
    /// a terminal shows it.
    #[cfg(not(target_family = "wasm"))]
    #[expect(clippy::print_stderr, reason = "the desktop app has no log")]
    fn updater(cx: &mut Context<'_, Self>) -> Option<Updater> {
        if !UPDATES {
            return None;
        }
        // Without symlinks, as macOS's `rho-ui` on PATH is one.
        let exe = std::env::current_exe().and_then(|exe| rho_ui_core::platform::canonicalize(&exe));
        let Some(installation) = exe.as_deref().ok().and_then(Installation::of) else {
            eprintln!(
                "rho-ui: not updating itself: {exe:?} isn't laid out as the install scripts lay it out"
            );
            return None;
        };
        Some(Updater {
            installation,
            _checks: Self::check(cx),
            install: None,
        })
    }

    /// Checks now and every [`CHECK_EVERY`] after. A newer release replaces
    /// an offer to install an older one, or one that failed to; a failed
    /// check, as when GitHub is out of reach, leaves the offer as it was,
    /// and says why on stderr.
    #[cfg(not(target_family = "wasm"))]
    #[expect(clippy::print_stderr, reason = "the desktop app has no log")]
    fn check(cx: &mut Context<'_, Self>) -> Task<()> {
        cx.spawn(async move |this, cx| loop {
            match release::newer_release(env!("CARGO_PKG_VERSION")).await {
                Ok(newer) => {
                    let updated = this.update(cx, |this, cx| {
                        if matches!(
                            this.offer,
                            None | Some(AppOffer::Available { .. } | AppOffer::Failed { .. })
                        ) {
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
                Err(error) => eprintln!("rho-ui: {error}"),
            }
            cx.background_executor().timer(CHECK_EVERY).await;
        })
    }

    /// Installs the release on offer over this rho-ui.
    #[cfg(not(target_family = "wasm"))]
    fn install(&mut self, cx: &mut Context<'_, Self>) {
        let latest = match &self.offer {
            Some(AppOffer::Available { latest } | AppOffer::Failed { latest, .. }) => {
                latest.clone()
            }
            _ => return,
        };
        let Some(updater) = self.updater.as_mut() else {
            return;
        };
        let installation = updater.installation.clone();
        self.offer = Some(AppOffer::Installing {
            latest: latest.clone(),
        });
        cx.notify();
        updater.install = Some(cx.spawn(async move |this, cx| {
            let installed = release::install_release(&latest, &installation).await;
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

    /// Quits, and starts the program installed with the arguments this one
    /// started with. On Unix it starts in this one's folder and environment,
    /// so relative arguments and `RHO_HOME` mean what they did; GPUI's own
    /// restart would reopen a macOS app as the Finder does, without them.
    /// On Windows, GPUI's restart does, with the arguments from `main`.
    #[cfg(not(target_family = "wasm"))]
    #[cfg_attr(
        unix,
        expect(clippy::print_stderr, reason = "the desktop app has no log")
    )]
    fn restart(&mut self, cx: &mut Context<'_, Self>) {
        let Some(updater) = &self.updater else {
            return;
        };
        #[cfg(unix)]
        match release::relaunch(&updater.installation, std::env::args_os().skip(1)) {
            Ok(()) => cx.quit(),
            Err(error) => eprintln!("rho-ui: couldn't restart into the update: {error}"),
        }
        #[cfg(windows)]
        {
            cx.set_restart_path(updater.installation.exe().to_path_buf());
            cx.restart();
        }
    }

    /// A page in a browser never updates itself.
    #[cfg(target_family = "wasm")]
    #[expect(
        clippy::unused_self,
        reason = "the views call the desktop's, which uses it"
    )]
    fn install(&mut self, _: &mut Context<'_, Self>) {}

    /// A page in a browser never updates itself.
    #[cfg(target_family = "wasm")]
    #[expect(
        clippy::unused_self,
        reason = "the views call the desktop's, which uses it"
    )]
    fn restart(&mut self, _: &mut Context<'_, Self>) {}
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
                    .on_click(move |_, _, cx| update.update(cx, AppUpdate::restart)),
            )
        }
    }
}
