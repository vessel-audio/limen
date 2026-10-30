//! User-interface preferences and the settings subpages.

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, StyledExt, Theme,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;

use crate::app::{AppView, ShutdownAction};
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
    div()
        .id("settings-general")
        .v_flex()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .gap_4()
        .p_4()
        .child(
            div()
                .text_xl()
                .font_family("serif")
                .child(rust_i18n::t!("settings.general").to_string()),
        )
        .child(card(
            rust_i18n::t!("settings.appearance").to_string(),
            div().h_flex().gap_2().children(appearance),
            cx,
        ))
        .child(card(
            rust_i18n::t!("settings.language").to_string(),
            div().h_flex().gap_2().children(language),
            cx,
        ))
        .child(card(
            rust_i18n::t!("settings.font_size").to_string(),
            div()
                .h_flex()
                .items_center()
                .gap_3()
                .child(
                    Button::new("font-decrease")
                        .outline()
                        .label("−")
                        .disabled(view.font_size <= 12.0)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.font_size = (this.font_size - 1.0).max(12.0);
                            let size = this.font_size;
                            Theme::update(cx, |theme| theme.font_size = px(size));
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
                        .outline()
                        .label("+")
                        .disabled(view.font_size >= 22.0)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.font_size = (this.font_size + 1.0).min(22.0);
                            let size = this.font_size;
                            Theme::update(cx, |theme| theme.font_size = px(size));
                        })),
                ),
            cx,
        ))
        .child(card(
            rust_i18n::t!("settings.service").to_string(),
            div()
                .h_flex()
                .gap_2()
                .child(
                    Button::new("restart-daemon")
                        .outline()
                        .label(rust_i18n::t!("action.restart_daemon").to_string())
                        .on_click({
                            let commands = view.commands.clone();
                            move |_, _, _| {
                                let _ = commands.try_send(BridgeCommand::RestartDaemon);
                            }
                        }),
                )
                .child(
                    Button::new("stop-daemon")
                        .outline()
                        .label(rust_i18n::t!("action.stop_daemon").to_string())
                        .on_click(cx.listener(|this, _, _, _| {
                            this.confirm_shutdown = Some(ShutdownAction::Stop);
                        })),
                ),
            cx,
        ))
}

pub fn about(_: &AppView, cx: &mut Context<AppView>) -> Stateful<Div> {
    div()
        .id("settings-about")
        .v_flex()
        .flex_1()
        .items_center()
        .justify_center()
        .p_6()
        .child(
            div()
                .v_flex()
                .items_center()
                .gap_4()
                .rounded_xl()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted.opacity(0.18))
                .p_8()
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
                ))),
        )
}

fn choice(id: &'static str, label: String, selected: bool) -> Button {
    let button = Button::new(id).label(label);
    match selected {
        true => button.primary(),
        false => button.outline(),
    }
}

fn card(title: String, control: Div, cx: &mut Context<AppView>) -> Div {
    div()
        .v_flex()
        .gap_2()
        .rounded_xl()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().muted.opacity(0.18))
        .p_3()
        .child(div().text_sm().font_semibold().child(title))
        .child(control)
}
