//! The macOS main menu: the application, File, Edit, View, Window, and Help
//! menus every Mac app shows while it has a window, with the standard key
//! equivalents.

use gpui_kit::component::input;
use gpui_kit::*;
use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;
use objc2_foundation::NSString;
use rust_i18n::t;

actions!(
    limen,
    [
        /// Show the standard About panel, filled from the bundle's Info.plist.
        About,
        /// Hide Limen.
        Hide,
        /// Hide every other app.
        HideOthers,
        /// Show every hidden app.
        ShowAll,
        /// Quit Limen, stopping the daemon it hosts.
        Quit,
        /// Close the key window as its close button would, which keeps Limen
        /// in the menu bar.
        CloseWindow,
        /// Enter or leave full screen.
        ToggleFullScreen,
        /// Minimize the active window to the Dock.
        Minimize,
        /// Zoom the active window.
        Zoom,
        /// Bring every Limen window to the front.
        BringAllToFront,
        /// Open the project's page.
        OpenHelp,
    ]
);

/// Where Limen Help leads.
const HELP_URL: &str = "https://github.com/Emin017/limen#readme";

/// Install the main menu and the app-wide handlers of its items. Text
/// editing items act on the focused input and are disabled without one.
pub(crate) fn install(cx: &mut App) {
    cx.on_action(|_: &About, _| {
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).orderFrontStandardAboutPanel(None)
        }
    });
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &CloseWindow, _| {
        if let Some(window) = MainThreadMarker::new()
            .and_then(|mtm| NSApplication::sharedApplication(mtm).keyWindow())
        {
            window.performClose(None)
        }
    });
    cx.on_action(|_: &ToggleFullScreen, cx| {
        with_active_window(cx, |window| window.toggle_fullscreen())
    });
    cx.on_action(|_: &Minimize, cx| with_active_window(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| with_active_window(cx, |window| window.zoom_window()));
    cx.on_action(|_: &BringAllToFront, _| {
        if let Some(mtm) = MainThreadMarker::new() {
            NSApplication::sharedApplication(mtm).arrangeInFront(None)
        }
    });
    cx.on_action(|_: &OpenHelp, cx| cx.open_url(HELP_URL));
    cx.bind_keys([
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    let window_title = t!("app_menu.window");
    let help_title = t!("app_menu.help");
    cx.set_menus([
        // AppKit titles the first menu with the bundle name.
        Menu::new("Limen").items([
            MenuItem::action(t!("app_menu.about"), About),
            MenuItem::separator(),
            MenuItem::os_submenu(t!("app_menu.services"), SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(t!("app_menu.hide"), Hide),
            MenuItem::action(t!("app_menu.hide_others"), HideOthers),
            MenuItem::action(t!("app_menu.show_all"), ShowAll),
            MenuItem::separator(),
            MenuItem::action(t!("status_item.quit"), Quit),
        ]),
        Menu::new(t!("app_menu.file")).items([MenuItem::action(t!("app_menu.close"), CloseWindow)]),
        Menu::new(t!("app_menu.edit")).items([
            MenuItem::action(t!("app_menu.undo"), input::Undo),
            MenuItem::action(t!("app_menu.redo"), input::Redo),
            MenuItem::separator(),
            MenuItem::action(t!("app_menu.cut"), input::Cut),
            MenuItem::action(t!("app_menu.copy"), input::Copy),
            MenuItem::action(t!("app_menu.paste"), input::Paste),
            MenuItem::action(t!("app_menu.select_all"), input::SelectAll),
        ]),
        Menu::new(t!("app_menu.view")).items([MenuItem::action(
            t!("app_menu.full_screen"),
            ToggleFullScreen,
        )]),
        Menu::new(window_title.clone()).items([
            MenuItem::action(t!("app_menu.minimize"), Minimize),
            MenuItem::action(t!("app_menu.zoom"), Zoom),
            MenuItem::separator(),
            MenuItem::action(t!("app_menu.bring_all_to_front"), BringAllToFront),
        ]),
        Menu::new(help_title.clone())
            .items([MenuItem::action(t!("app_menu.limen_help"), OpenHelp)]),
    ]);
    // GPUI hands AppKit only a Window menu titled in English, and no Help
    // menu; AppKit lists the windows in one and adds the search field to
    // the other.
    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        let submenu = |title: &str| {
            let main = app.mainMenu()?;
            let title = NSString::from_str(title);
            (0..main.numberOfItems())
                .filter_map(|index| main.itemAtIndex(index))
                .find(|item| item.title().isEqualToString(&title))
                .and_then(|item| item.submenu())
        };
        app.setWindowsMenu(submenu(&window_title).as_deref());
        app.setHelpMenu(submenu(&help_title).as_deref());
    }
}

/// Run `act` on the active window, if Limen has one.
fn with_active_window(cx: &mut App, act: impl FnOnce(&mut Window)) {
    if let Some(window) = cx.active_window() {
        let _ = window.update(cx, |_, window, _| act(window));
    }
}
