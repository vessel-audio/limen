//! User-interface preferences and the settings subpages.

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Sizable, StyledExt,
    button::{Button, ButtonVariants},
    scroll::ScrollbarMode,
    searchable_list::SearchableListItem,
    select::Select,
    switch::Switch,
};
use gpui_kit::*;

use crate::app::{AppView, Page, ShutdownAction};
use crate::bridge::BridgeCommand;

/// Settings subpages shown below the sidebar's Settings item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPage {
    General,
    About,
}

/// Application color-scheme preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppearancePreference {
    System,
    Light,
    Dark,
}

/// Whether decorative motion is skipped to save rendering work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightweightMode {
    Off,
    On,
}

/// One entry of the interface-font picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontChoice {
    /// The platform's UI font, resolved by the theme.
    System,
    Family(SharedString),
}

impl FontChoice {
    pub const SYSTEM_FAMILY: &'static str = ".SystemUIFont";

    /// The system entry followed by every installed family, sorted.
    pub fn all(cx: &App) -> Vec<Self> {
        let mut families = cx.text_system().all_font_names();
        families.sort_unstable();
        families.dedup();
        std::iter::once(Self::System)
            .chain(
                families
                    .into_iter()
                    .filter(|family| !family.starts_with('.'))
                    .map(|family| Self::Family(family.into())),
            )
            .collect()
    }
}

impl SearchableListItem for FontChoice {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        match self {
            Self::System => rust_i18n::t!("settings.follow_system").into(),
            Self::Family(family) => family.clone(),
        }
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let family: SharedString = self.value().clone();
        div().font_family(family).child(self.title())
    }

    fn value(&self) -> &SharedString {
        static SYSTEM: SharedString = SharedString::new_static(FontChoice::SYSTEM_FAMILY);
        match self {
            Self::System => &SYSTEM,
            Self::Family(family) => family,
        }
    }
}

/// Application language preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanguagePreference {
    System,
    English,
    Chinese,
}

pub fn general(view: &AppView, cx: &mut Context<AppView>) -> Stateful<Div> {
    let appearance = [
        (
            "appearance-system",
            rust_i18n::t!("settings.follow_system").to_string(),
            AppearancePreference::System,
        ),
        (
            "appearance-light",
            rust_i18n::t!("settings.light").to_string(),
            AppearancePreference::Light,
        ),
        (
            "appearance-dark",
            rust_i18n::t!("settings.dark").to_string(),
            AppearancePreference::Dark,
        ),
    ]
    .map(|(id, label, appearance)| {
        choice(id, label, view.appearance == appearance).on_click(cx.listener(
            move |this, _, window, cx| {
                this.appearance = appearance;
                this.apply_appearance(window, cx);
            },
        ))
    });
    let language = [
        (
            "language-system",
            rust_i18n::t!("settings.system_language").to_string(),
            LanguagePreference::System,
        ),
        (
            "language-english",
            "English".to_string(),
            LanguagePreference::English,
        ),
        (
            "language-chinese",
            "简体中文".to_string(),
            LanguagePreference::Chinese,
        ),
    ]
    .map(|(id, label, language)| {
        choice(id, label, view.language == language).on_click(cx.listener(move |this, _, _, cx| {
            this.language = language;
            match language {
                LanguagePreference::System => crate::locale::initialize(),
                LanguagePreference::English => crate::locale::Locale::En.initialize(),
                LanguagePreference::Chinese => crate::locale::Locale::ZhCn.initialize(),
            }
            cx.notify();
        }))
    });
    let scroll = &view.settings_scroll;
    let content = div()
        .v_flex()
        .gap_5()
        .p_8()
        .on_scroll(scroll.on_scroll())
        .child(header(view, cx))
        .child(
            div()
                .v_flex()
                .rounded_xl()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.18))
                .children(
                    [
                        setting(
                            "settings.appearance",
                            div().h_flex().gap_2().children(appearance),
                            cx,
                        ),
                        setting(
                            "settings.language",
                            div().h_flex().gap_2().children(language),
                            cx,
                        ),
                        setting(
                            "settings.font_size",
                            div()
                                .h_flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    Button::new("font-decrease")
                                        .small()
                                        .outline()
                                        .label("−")
                                        .disabled(view.font_size <= 12.0)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.font_size = (this.font_size - 1.0).max(12.0);
                                            this.apply_typography(cx);
                                        })),
                                )
                                .child(
                                    div()
                                        .w(px(64.))
                                        .text_center()
                                        .child(format!("{} px", view.font_size)),
                                )
                                .child(
                                    Button::new("font-increase")
                                        .small()
                                        .outline()
                                        .label("+")
                                        .disabled(view.font_size >= 22.0)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.font_size = (this.font_size + 1.0).min(22.0);
                                            this.apply_typography(cx);
                                        })),
                                ),
                            cx,
                        ),
                        setting(
                            "settings.interface_font",
                            div().w(px(220.)).child(
                                Select::new(&view.font_select)
                                    .menu_max_h(px(320.))
                                    .search_placeholder(
                                        rust_i18n::t!("settings.search_font").to_string(),
                                    ),
                            ),
                            cx,
                        ),
                        setting(
                            "settings.lightweight",
                            Switch::new("lightweight")
                                .checked(view.lightweight == LightweightMode::On)
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.lightweight = match checked {
                                        true => LightweightMode::On,
                                        false => LightweightMode::Off,
                                    };
                                    cx.set_reduce_motion(*checked);
                                    cx.notify();
                                })),
                            cx,
                        ),
                        setting(
                            "settings.service",
                            div()
                                .h_flex()
                                .gap_2()
                                .child(
                                    Button::new("restart-daemon")
                                        .small()
                                        .outline()
                                        .label(rust_i18n::t!("action.restart_daemon").to_string())
                                        .on_click({
                                            let commands = view.commands.clone();
                                            move |_, _, _| {
                                                let _ =
                                                    commands.try_send(BridgeCommand::RestartDaemon);
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("stop-daemon")
                                        .small()
                                        .outline()
                                        .label(rust_i18n::t!("action.stop_daemon").to_string())
                                        .on_click(cx.listener(|this, _, _, _| {
                                            this.confirm_shutdown = Some(ShutdownAction::Stop);
                                        })),
                                ),
                            cx,
                        ),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, row)| match index {
                        0 => row,
                        _ => row.border_t_1(),
                    }),
                ),
        );
    div()
        .id("settings-general")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .track_scroll(scroll.handle())
        .child(content)
        .child(scroll.scrollbar(ScrollbarMode::Always))
}

pub fn about(view: &AppView, cx: &mut Context<AppView>) -> Stateful<Div> {
    let card = div()
        .v_flex()
        .items_center()
        .gap_4()
        .rounded_xl()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted.opacity(0.18))
        .px_16()
        .py_12()
        .child(
            Icon::new(IconName::Mic)
                .size(px(36.))
                .text_color(cx.theme().primary),
        )
        .child(
            div()
                .text_2xl()
                .font_family("serif")
                .child(rust_i18n::t!("app.title").to_string()),
        )
        .child(div().text_color(cx.theme().muted_foreground).child(format!(
            "{} {}",
            rust_i18n::t!("about.version"),
            env!("CARGO_PKG_VERSION")
        )));
    div()
        .id("settings-about")
        .v_flex()
        .flex_1()
        .gap_5()
        .p_8()
        .child(header(view, cx))
        .child(
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .child(card),
        )
}

/// The settings title above tabs for its subpages.
fn header(view: &AppView, cx: &mut Context<AppView>) -> Div {
    div()
        .v_flex()
        .gap_4()
        .child(crate::pages::title(rust_i18n::t!("settings.title")))
        .child(
            div().h_flex().gap_1().children(
                [
                    (
                        "settings-tab-general",
                        rust_i18n::t!("settings.general"),
                        SettingsPage::General,
                    ),
                    (
                        "settings-tab-about",
                        rust_i18n::t!("about.menu"),
                        SettingsPage::About,
                    ),
                ]
                .map(|(id, label, page)| {
                    let tab = div()
                        .id(id)
                        .px(px(14.))
                        .py_1p5()
                        .rounded_md()
                        .text_sm()
                        .cursor_pointer()
                        .child(label.to_string())
                        .on_click(cx.listener(move |this, _, _, _| {
                            this.page = Page::Settings(page);
                        }));
                    match view.page == Page::Settings(page) {
                        true => tab
                            .bg(cx.theme().accent)
                            .text_color(cx.theme().foreground)
                            .font_medium(),
                        false => tab
                            .text_color(cx.theme().muted_foreground)
                            .hover(|style| style.bg(cx.theme().accent.opacity(0.5))),
                    }
                }),
            ),
        )
}

fn choice(id: &'static str, label: String, selected: bool) -> Button {
    let button = Button::new(id).small().label(label);
    match selected {
        true => button.primary(),
        false => button.outline(),
    }
}

/// One settings row: the title and description under `key` beside a control.
fn setting(key: &str, control: impl IntoElement, cx: &mut Context<AppView>) -> Div {
    let hint = format!("{key}_hint");
    div()
        .h_flex()
        .flex_wrap()
        .items_center()
        .justify_between()
        .gap_x_6()
        .gap_y_3()
        .px_5()
        .py_4()
        .border_color(cx.theme().border)
        .child(
            div()
                .v_flex()
                .flex_1()
                .min_w(px(160.))
                .gap_0p5()
                .child(
                    div()
                        .text_sm()
                        .font_medium()
                        .child(rust_i18n::t!(key).to_string()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(rust_i18n::t!(&hint).to_string()),
                ),
        )
        .child(div().flex_shrink_0().child(control))
}
