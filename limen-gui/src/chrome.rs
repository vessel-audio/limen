//! Window chrome: brand, connection banners, and status footer.

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Icon, Sizable, StyledExt, TitleBar, button::ButtonVariants,
};
use gpui_kit::*;

use crate::a11y;
use crate::app::{AppView, Page, ShutdownAction};
use crate::bridge::BridgeCommand;
use crate::model::{Connection, Warning};
use crate::motion;
use crate::settings::SettingsPage;
use crate::sidebar::SidebarMode;

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
        let pending = matches!(
            self.model.connection,
            Connection::Connecting | Connection::Spawning | Connection::Reconnecting { .. }
        );
        let dot = div().size(px(6.)).rounded_full().bg(color);
        let (section, page_key) = match self.page {
            Page::Dictation => ("nav.section", "nav.dictation"),
            Page::History => ("nav.section", "nav.history"),
            Page::Shortcuts => ("nav.section", "nav.shortcuts"),
            Page::Settings(SettingsPage::General) => ("settings.title", "settings.general"),
            Page::Settings(SettingsPage::Network) => ("settings.title", "settings.network"),
            Page::Settings(SettingsPage::About) => ("settings.title", "about.menu"),
            Page::Providers => ("nav.models", "nav.providers"),
            Page::Usage => ("nav.models", "nav.usage"),
        };
        let page = rust_i18n::t!(page_key).to_string();
        // The brand mark shares the sidebar's centre line and its label starts
        // in the navigation label column, so the two read as one column; the
        // brand area follows the sidebar's width so the page controls start
        // above the content panel.
        TitleBar::new()
            .h(px(48.))
            .pl_0()
            .border_b_0()
            .bg(cx.theme().muted)
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .w_full()
                    .pr_2()
                    .child(
                        div()
                            .h_flex()
                            .flex_shrink_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .items_center()
                            .gap(px(6.))
                            .pl(px(18.))
                            .child(
                                // A fixed mark: collapsing clips and fades the
                                // name, never squeezes the logo.
                                div()
                                    .flex_shrink_0()
                                    .size(px(24.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        Icon::empty()
                                            .path(crate::assets::MARK)
                                            .size(px(22.))
                                            .text_color(cx.theme().primary),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_sm()
                                    .font_semibold()
                                    .child(rust_i18n::t!("app.title").to_string())
                                    .with_spring(
                                        "brand-name",
                                        SpringAnimation::new(motion::SPRING)
                                            .to(self.sidebar_mode.label_opacity()),
                                        |name, opacity| name.opacity(opacity.clamp(0., 1.)),
                                    ),
                            )
                            .with_spring(
                                "brand-width",
                                SpringAnimation::new(motion::SPRING)
                                    .to(self.sidebar_mode.width(self.sidebar_width)),
                                |brand, width| brand.w(width),
                            ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            // Keep the press from reaching the title bar, which
                            // would otherwise start a window drag.
                            .child(
                                div()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child(
                                        a11y::button("toggle-sidebar")
                                            .ghost()
                                            .small()
                                            .icon(match self.sidebar_mode {
                                                SidebarMode::Expanded => IconName::PanelLeftClose,
                                                SidebarMode::Collapsed => IconName::PanelLeftOpen,
                                            })
                                            .tooltip(
                                                rust_i18n::t!("nav.toggle_sidebar").to_string(),
                                            )
                                            .accessibility_label(
                                                rust_i18n::t!("nav.toggle_sidebar").to_string(),
                                            )
                                            .on_click(cx.listener(|this, _, _, _| {
                                                this.sidebar_mode = match this.sidebar_mode {
                                                    SidebarMode::Expanded => SidebarMode::Collapsed,
                                                    SidebarMode::Collapsed => SidebarMode::Expanded,
                                                };
                                            })),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(rust_i18n::t!(section).to_string()),
                            )
                            .child(div().text_color(cx.theme().border).child("/"))
                            .child(
                                div()
                                    .id("current-page")
                                    .accessibility_id("current-page")
                                    .role(Role::Heading)
                                    .aria_label(page.clone())
                                    .min_w_0()
                                    .truncate()
                                    .font_medium()
                                    .child(page),
                            ),
                    )
                    .child(
                        a11y::status("connection-status", status.clone())
                            .h_flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(match pending {
                                true => dot
                                    .with_animation("status-pending", motion::pulse(), |dot, t| {
                                        dot.opacity(t)
                                    })
                                    .into_any_element(),
                                false => dot.into_any_element(),
                            })
                            .child(status),
                    ),
            )
    }

    pub(crate) fn banners(&self, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let banner = |id: &'static str, text: String| a11y::status(id, text.clone()).child(text);
        let mut banners = Vec::new();
        match &self.model.connection {
            Connection::Reconnecting { attempt, reason } => banners.push(banner(
                "reconnecting-banner",
                format!(
                    "{} {attempt}… · {reason}",
                    rust_i18n::t!("connection.reconnecting")
                ),
            )),
            Connection::ProtocolMismatch { message } => {
                let mut banner = banner(
                    "outdated-banner",
                    format!("{}: {message}", rust_i18n::t!("connection.outdated")),
                )
                .v_flex()
                .gap_2();
                if self.offer_terminate {
                    banner = banner.child(
                        a11y::button("terminate-daemon")
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
            banners.push(banner(
                "version-banner",
                format!(
                    "{} {running}; {} {expected}",
                    rust_i18n::t!("connection.daemon_version"),
                    rust_i18n::t!("connection.gui_expects")
                ),
            ));
        }
        banners
            .into_iter()
            .map(|banner| {
                banner
                    .mx_8()
                    .mt_6()
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

    pub(crate) fn connection_overlay(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
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
        let overlay = div()
            .id("connection-overlay")
            .accessibility_id("connection-overlay")
            .role(Role::Dialog)
            .aria_label(message.clone());
        let overlay = match log.clone() {
            Some(log) => overlay.aria_description(log),
            None => overlay,
        };
        Some(
            overlay
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
                            a11y::button("open-startup-log")
                                .outline()
                                .label(rust_i18n::t!("action.open_log").to_string())
                                .on_click({
                                    let log = self.paths.log_file.clone();
                                    move |_, _, cx| cx.open_with_system(&log)
                                }),
                        )
                        .child(
                            a11y::button("start-daemon")
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
            .px_8()
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
