//! The macOS menu bar: the app menu, File, Edit and Window, as every Mac
//! app has. Each item names an action; GPUI shows the shortcut the keymap
//! binds to it, enables it while something on screen handles it, and gives
//! a key to the window before the menu, so a terminal keeps its keys.

use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::{App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType};

use crate::{NewSession, NewSessionIn, OpenSettings, Quit, ToggleRightPanel, ToggleSidebar};

gpui_kit::actions!(
    rho_ui,
    [
        /// Hide the app.
        Hide,
        /// Hide every other app.
        HideOthers,
        /// Show every app again.
        ShowAll,
        /// Minimize the window to the Dock.
        Minimize,
        /// Zoom the window to fill the screen, or back.
        Zoom
    ]
);

/// Sets the menu bar, with the window commands the system leaves to apps
/// and their keys. Run after the app's key bindings are in, as the menu
/// reads its shortcuts from them once.
pub(crate) fn install(cx: &mut App) {
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.minimize_window());
        }
    });
    cx.on_action(|_: &Zoom, cx| {
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, _| window.zoom_window());
        }
    });
    cx.bind_keys([
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("cmd-alt-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    cx.set_menus([
        // macOS titles the first menu with the app's name, whatever it says.
        Menu::new("Rho").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Rho", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Rho", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Session", NewSession),
            MenuItem::action("New Session in…", NewSessionIn),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Toggle Right Panel", ToggleRightPanel),
        ]),
        // Named "Window", GPUI makes it the system's Window menu, which
        // lists the app's windows below these.
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
    ]);
}
