//! General and About settings: the subpages, appearance, language, text
//! size, the interface font, lightweight mode, and the page's scrollbar. The
//! Linux desktop suite keeps what needs a real desktop: that its dark
//! preference reaches the window and is painted.

use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::{ActiveTheme, ThemeMode};
use gpui_kit::{ScrollDelta, TestAppContext, point, px};
use rust_i18n::t;

use super::{Daemon, Ui};
use crate::locale::Locale;
use crate::settings::{FontChoice, LightweightMode};

/// The checked state of each option of a segmented control, in order.
fn checked<const N: usize>(ui: &mut Ui, options: [&str; N]) -> [Option<bool>; N] {
    options.map(|id| ui.find(id).checked())
}

/// The subpages in the sidebar switch between the General controls and the
/// About card, each naming itself in the title bar.
#[gpui_kit::test]
fn settings_tabs_switch_between_general_and_about(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    ui.click("settings-page");
    let general = (
        ui.find("settings-tab-general").selected(),
        ui.label("current-page"),
        ui.try_find("appearance-dark").is_some(),
        ui.try_find("about-version").is_some(),
    );
    ui.click("settings-tab-about");
    let about = (
        ui.find("settings-tab-about").selected(),
        ui.label("current-page"),
        ui.try_find("appearance-dark").is_some(),
        ui.find("about-version").value().map(str::to_string),
    );
    ui.click("settings-tab-general");
    assert_eq!(
        (general, about, ui.label("current-page")),
        (
            (Some(true), t!("settings.general").into(), true, false),
            (
                Some(true),
                t!("about.menu").into(),
                false,
                Some(format!(
                    "{} {}",
                    t!("about.version"),
                    env!("CARGO_PKG_VERSION")
                )),
            ),
            t!("settings.general").into(),
        )
    );
}

/// Light and Dark override the system's scheme, and Follow system, the
/// default, goes back to it. The test window's scheme is light.
#[gpui_kit::test]
fn appearance_follows_the_choice_and_the_system(cx: &mut TestAppContext) {
    const OPTIONS: [&str; 3] = ["appearance-system", "appearance-light", "appearance-dark"];
    let mut ui = Ui::connected(cx, Daemon::default());
    ui.click("settings-page");
    let observe = |ui: &mut Ui| (ui.read(|_, cx| cx.theme().mode), checked(ui, OPTIONS));
    let opened = observe(&mut ui);
    let chosen = [
        "appearance-dark",
        "appearance-light",
        "appearance-dark",
        "appearance-system",
    ]
    .map(|choice| {
        ui.click(choice);
        observe(&mut ui)
    });
    assert_eq!(
        (opened, chosen),
        (
            (ThemeMode::Light, [Some(true), Some(false), Some(false)]),
            [
                (ThemeMode::Dark, [Some(false), Some(false), Some(true)]),
                (ThemeMode::Light, [Some(false), Some(true), Some(false)]),
                (ThemeMode::Dark, [Some(false), Some(false), Some(true)]),
                (ThemeMode::Light, [Some(true), Some(false), Some(false)]),
            ]
        )
    );
}

/// English and Chinese override the system language, and Follow system goes
/// back to the one the environment names.
#[gpui_kit::test]
fn language_choice_renames_the_interface(cx: &mut TestAppContext) {
    const OPTIONS: [&str; 3] = ["language-system", "language-english", "language-chinese"];
    let system = Locale::detect(|name| std::env::var(name).ok()).as_str();
    let mut ui = Ui::switching_language(cx, Daemon::default());
    ui.click("settings-page");
    let observe = |ui: &mut Ui| {
        (
            (ui.label("settings-page"), ui.label("dictation-page")),
            checked(ui, OPTIONS),
        )
    };
    let named = |locale: &str| -> (String, String) {
        (
            t!("settings.title", locale = locale).into(),
            t!("nav.dictation", locale = locale).into(),
        )
    };
    let opened = observe(&mut ui);
    let chosen = [
        "language-chinese",
        "language-system",
        "language-english",
        "language-system",
    ]
    .map(|choice| {
        ui.click(choice);
        observe(&mut ui)
    });
    assert_eq!(
        (opened, chosen),
        (
            (named("en"), [Some(true), Some(false), Some(false)]),
            [
                (named("zh-CN"), [Some(false), Some(false), Some(true)]),
                (named(system), [Some(true), Some(false), Some(false)]),
                (named("en"), [Some(false), Some(true), Some(false)]),
                (named(system), [Some(true), Some(false), Some(false)]),
            ]
        )
    );
}

/// The text size is one choice among scales: choosing one checks it alone,
/// 125% makes text taller, and 100% restores it.
#[gpui_kit::test]
fn the_text_scale_resizes_the_interface(cx: &mut TestAppContext) {
    const SCALES: [&str; 4] = [
        "font-scale-100",
        "font-scale-110",
        "font-scale-125",
        "font-scale-150",
    ];
    let mut ui = Ui::connected(cx, Daemon::default());
    ui.click("settings-page");
    let observe = |ui: &mut Ui| {
        (
            checked(ui, SCALES),
            ui.read(|_, cx| cx.theme().font_size),
            ui.find("page-title").bounds().size.height,
        )
    };
    let normal = observe(&mut ui);
    ui.click("font-scale-125");
    let larger = observe(&mut ui);
    ui.click("font-scale-100");
    let restored = observe(&mut ui);
    assert_eq!(
        (
            (normal.0, normal.1),
            (larger.0, larger.1),
            larger.2 > normal.2,
            restored,
        ),
        (
            ([Some(true), Some(false), Some(false), Some(false)], px(16.)),
            ([Some(false), Some(false), Some(true), Some(false)], px(20.)),
            true,
            normal,
        )
    );
}

/// The font select searches the installed families and applies the chosen
/// one; following the system restores the default. The test platform has no
/// fonts, so the select is given the families a desktop would list.
#[gpui_kit::test]
fn the_font_select_applies_a_family(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    let select = ui.read(|view, _| view.font_select.clone());
    let families = std::iter::once(FontChoice::System)
        .chain(["DejaVu Sans", "DejaVu Serif"].map(|family| FontChoice::Family(family.into())))
        .collect::<Vec<_>>();
    ui.act(|window, cx| {
        select.update(cx, |select, cx| {
            select.set_items(SearchableVec::new(families), window, cx)
        })
    });
    ui.click("settings-page");
    let applied = |ui: &mut Ui| {
        (
            ui.read(|view, cx| {
                (
                    view.font_family.to_string(),
                    cx.theme().font_family.to_string(),
                )
            }),
            ui.find("font-select").expanded(),
        )
    };
    let system = applied(&mut ui);
    let choose = |ui: &mut Ui, search: &str| {
        ui.click("font-select");
        let opened = ui.find("font-select").expanded();
        ui.input(search);
        ui.press("enter");
        (opened, applied(ui))
    };
    let serif = choose(&mut ui, "Serif");
    let followed = choose(&mut ui, &t!("settings.follow_system"));
    assert_eq!(
        (system.clone(), serif, followed),
        (
            system,
            (
                Some(true),
                (
                    ("DejaVu Serif".to_string(), "DejaVu Serif".to_string()),
                    Some(false)
                )
            ),
            (
                Some(true),
                (
                    (
                        FontChoice::SYSTEM_FAMILY.to_string(),
                        FontChoice::SYSTEM_FAMILY.to_string()
                    ),
                    Some(false)
                )
            ),
        )
    );
}

/// Lightweight mode turns on and off, and while on, the window reduces
/// motion; turning it off brings motion back.
#[gpui_kit::test]
fn lightweight_mode_skips_motion(cx: &mut TestAppContext) {
    const OPTIONS: [&str; 2] = ["lightweight-off", "lightweight-on"];
    let mut ui = Ui::connected(cx, Daemon::default());
    ui.click("settings-page");
    // The last rows lie below the window.
    ui.scroll(
        "settings-general",
        ScrollDelta::Pixels(point(px(0.), px(-2000.))),
    );
    let observe = |ui: &mut Ui| {
        (
            ui.read(|view, cx| (view.lightweight, cx.reduce_motion())),
            checked(ui, OPTIONS),
        )
    };
    // The tests' window starts with motion reduced; going off must end that.
    let chosen = ["lightweight-off", "lightweight-on", "lightweight-off"].map(|choice| {
        ui.click(choice);
        observe(&mut ui)
    });
    assert_eq!(
        chosen,
        [
            ((LightweightMode::Off, false), [Some(true), Some(false)]),
            ((LightweightMode::On, true), [Some(false), Some(true)]),
            ((LightweightMode::Off, false), [Some(true), Some(false)]),
        ]
    );
}

/// Dragging the always-visible scrollbar scrolls General settings to the
/// service controls at the bottom, which larger text pushes below the view.
#[gpui_kit::test]
fn the_scrollbar_drags_the_settings_page(cx: &mut TestAppContext) {
    let mut ui = Ui::connected(cx, Daemon::default());
    ui.click("settings-page");
    ui.click("font-scale-150");
    let view = ui.find("settings-general").bounds();
    let (title, stop) = (
        ui.find("page-title").bounds(),
        ui.find("stop-daemon").bounds(),
    );
    // The thumb starts at the top of the track, on its right edge.
    let thumb = point(view.right() - px(7.), view.top() + px(12.));
    ui.drag(thumb, point(thumb.x, view.bottom() - px(4.)));
    let (scrolled_title, scrolled_stop) = (
        ui.find("page-title").bounds(),
        ui.find("stop-daemon").bounds(),
    );
    assert_eq!(
        (
            stop.bottom() > view.bottom(),
            scrolled_title.top() + px(50.) < title.top(),
            scrolled_stop.bottom() <= view.bottom(),
        ),
        (true, true, true),
        "the view is {view:?}; the title was at {title:?} then {scrolled_title:?}, \
         the stop button at {stop:?} then {scrolled_stop:?}"
    );
}
