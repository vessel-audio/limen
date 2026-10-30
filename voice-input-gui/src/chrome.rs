//! Window chrome: brand, navigation, connection banners, and status footer.

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Icon, StyledExt, TitleBar,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;

use crate::app::{AppView, Page, ShutdownAction};
use crate::bridge::BridgeCommand;
use crate::model::{Connection, Warning};
use crate::settings::SettingsPage;

impl AppView {
    pub(crate) fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (status, color) = match self.model.connection {
            Connection::Connected => (
                rust_i18n::t!("connection.online").to_string(),
                cx.theme().success,
            ),
            Connection::NotRunning
            | Connection::Connecting
            | Connection::Spawning
            | Connection::Reconnecting { .. }
            | Connection::SpawnFailed { .. }
            | Connection::ProtocolMismatch { .. } => (
                rust_i18n::t!("connection.offline").to_string(),
                cx.theme().muted_foreground,
            ),
        };
        TitleBar::new().child(
            div()
                .h_flex()
                .items_center()
                .justify_between()
                .w_full()
                .px_4()
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .size(px(30.))
                                .rounded_lg()
                                .bg(cx.theme().primary)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    Icon::new(IconName::Mic)
                                        .size(px(18.))
                                        .text_color(cx.theme().primary_foreground),
                                ),
                        )
                        .child(
                            div()
                                .text_lg()
                                .font_semibold()
                                .child(rust_i18n::t!("app.title").to_string()),
                        ),
                )
                .child(
                    div()
                        .h_flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(div().size(px(7.)).rounded_full().bg(color))
                        .child(status),
                ),
        )
    }

    pub(crate) fn sidebar(&self, cx: &mut Context<Self>) -> Div {
        div()
            .v_flex()
            .w(px(208.))
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .gap_1()
            .p_3()
            .bg(cx.theme().muted.opacity(0.22))
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.navigation_item(
                "dictation-page",
                IconName::Mic,
                rust_i18n::t!("nav.dictation").to_string(),
                Page::Dictation,
                cx,
            ))
            .child(self.navigation_item(
                "history-page",
                IconName::Clock,
                rust_i18n::t!("nav.history").to_string(),
                Page::History,
                cx,
            ))
            .child(self.navigation_item(
                "shortcuts-page",
                IconName::Keyboard,
                rust_i18n::t!("nav.shortcuts").to_string(),
                Page::Shortcuts,
                cx,
            ))
            .child(div().flex_1())
            .children(self.settings_open.then(|| {
                div()
                    .v_flex()
                    .pl_4()
                    .child(self.navigation_item(
                        "settings-general",
                        IconName::Settings2,
                        rust_i18n::t!("settings.general").to_string(),
                        Page::Settings(SettingsPage::General),
                        cx,
                    ))
                    .child(self.navigation_item(
                        "settings-about",
                        IconName::Info,
                        rust_i18n::t!("about.menu").to_string(),
                        Page::Settings(SettingsPage::About),
                        cx,
                    ))
            }))
            .child({
                let button = Button::new("settings")
                    .icon(Icon::new(IconName::Settings2))
                    .label(rust_i18n::t!("settings.title").to_string())
                    .w_full();
                match matches!(self.page, Page::Settings(_)) {
                    true => button.outline(),
                    false => button.ghost(),
                }
                .on_click(cx.listener(|this, _, _, _| {
                    this.settings_open = !this.settings_open;
                    if this.settings_open {
                        this.page = Page::Settings(SettingsPage::General);
                    }
                }))
            })
    }

    fn navigation_item(
        &self,
        id: &'static str,
        icon: IconName,
        label: String,
        page: Page,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.page == page;
        let item = div()
            .id(id)
            .h_flex()
            .w_full()
            .items_center()
            .gap_3()
            .rounded_md()
            .px_3()
            .py_2()
            .cursor_pointer()
            .text_color(match selected {
                true => cx.theme().primary,
                false => cx.theme().muted_foreground,
            })
            .on_click(cx.listener(move |this, _, _, _| this.page = page))
            .child(Icon::new(icon).size(px(18.)))
            .child(label);
        match selected {
            true => item.bg(cx.theme().primary.opacity(0.12)),
            false => item.hover(|style| style.bg(cx.theme().muted)),
        }
    }

    pub(crate) fn banners(&self, cx: &mut Context<Self>) -> Vec<Div> {
        let mut banners = Vec::new();
        match &self.model.connection {
            Connection::Reconnecting { attempt, reason } => banners.push(div().child(format!(
                "{} {attempt}… · {reason}",
                rust_i18n::t!("connection.reconnecting")
            ))),
            Connection::ProtocolMismatch { message } => {
                let mut banner = div().v_flex().gap_2().child(format!(
                    "{}: {message}",
                    rust_i18n::t!("connection.outdated")
                ));
                if self.offer_terminate {
                    banner = banner.child(
                        Button::new("terminate-daemon")
                            .outline()
                            .label(rust_i18n::t!("action.terminate_daemon").to_string())
                            .on_click(cx.listener(|this, _, _, _| {
                                this.confirm_shutdown = Some(ShutdownAction::Terminate);
                            })),
                    );
                }
                banners.push(banner);
            }
            Connection::NotRunning
            | Connection::Connecting
            | Connection::Spawning
            | Connection::Connected
            | Connection::SpawnFailed { .. } => {}
        }
        if let Some(Warning::DaemonVersion { running, expected }) = &self.model.warning {
            banners.push(div().child(format!(
                "{} {running}; {} {expected}",
                rust_i18n::t!("connection.daemon_version"),
                rust_i18n::t!("connection.gui_expects")
            )));
        }
        banners
            .into_iter()
            .map(|banner| {
                banner
                    .mx_6()
                    .mt_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted.opacity(0.3))
                    .px_4()
                    .py_3()
                    .text_sm()
            })
            .collect()
    }

    pub(crate) fn connection_overlay(&self, cx: &mut Context<Self>) -> Option<Div> {
        let (message, log) = match &self.model.connection {
            Connection::Connecting | Connection::Spawning => {
                (rust_i18n::t!("connection.starting").to_string(), None)
            }
            Connection::NotRunning => (rust_i18n::t!("connection.not_running").to_string(), None),
            Connection::SpawnFailed { log_tail } => (
                rust_i18n::t!("connection.failed_start").to_string(),
                Some(log_tail.clone()),
            ),
            Connection::Connected
            | Connection::Reconnecting { .. }
            | Connection::ProtocolMismatch { .. } => return None,
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .v_flex()
                .items_center()
                .justify_center()
                .bg(cx.theme().colors.background)
                .p_6()
                .child(
                    div()
                        .v_flex()
                        .w_full()
                        .max_w(px(520.))
                        .items_center()
                        .gap_4()
                        .rounded_xl()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().muted.opacity(0.18))
                        .p_8()
                        .child(
                            div()
                                .size(px(48.))
                                .rounded_full()
                                .bg(cx.theme().primary.opacity(0.12))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(
                                    Icon::new(IconName::AudioWaveform)
                                        .size(px(24.))
                                        .text_color(cx.theme().primary),
                                ),
                        )
                        .child(div().text_xl().font_semibold().child(message))
                        .children(log.map(|log| {
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(log)
                        }))
                        .child(
                            Button::new("open-startup-log")
                                .outline()
                                .label(rust_i18n::t!("action.open_log").to_string())
                                .on_click({
                                    let log = self.paths.log_file.clone();
                                    move |_, _, cx| cx.open_with_system(&log)
                                }),
                        )
                        .child(
                            Button::new("start-daemon")
                                .primary()
                                .label(rust_i18n::t!("action.start_daemon").to_string())
                                .on_click({
                                    let commands = self.commands.clone();
                                    move |_, _, _| {
                                        let _ = commands.try_send(BridgeCommand::SpawnDaemon);
                                    }
                                }),
                        ),
                ),
        )
    }

    pub(crate) fn footer(&self, cx: &mut Context<Self>) -> Div {
        let config = self
            .snapshot_config()
            .map(|config| {
                format!(
                    "{} · {} · {} {} · {} {}",
                    config.input_method,
                    config.provider_model,
                    rust_i18n::t!("footer.polish"),
                    config.polish_enabled,
                    rust_i18n::t!("footer.shortcut"),
                    self.model
                        .snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.shortcut.backend.to_string())
                        .unwrap_or_else(|| rust_i18n::t!("about.unknown").to_string())
                )
            })
            .unwrap_or_else(|| rust_i18n::t!("connection.disconnected").to_string());
        div()
            .h_flex()
            .justify_between()
            .gap_4()
            .px_6()
            .py_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(div().flex_1().min_w_0().text_ellipsis().child(config))
            .child(
                div()
                    .flex_shrink_0()
                    .child(rust_i18n::t!("connection.closing_hint").to_string()),
            )
    }
}
