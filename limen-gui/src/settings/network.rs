//! The Network settings page: which proxy requests to model services go
//! through, and who signs in to it.

use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme, Disableable, StyledExt, button::ButtonVariants, scroll::ScrollbarMode,
};
use gpui_kit::*;
use limen_core::{ApiKey, ConfigChange, KeyChange, Network, ProxyMode, ProxyPassword};

use super::{Segment, SettingsPage, card, header, segmented, setting};
use crate::a11y;
use crate::app::{AppView, Page};
use crate::bridge::{BridgeCommand, NetworkRequest};
use crate::model::{Action, Toast};

/// The page's fields, and the requests that load and save them.
pub struct NetworkForm {
    mode: ProxyMode,
    url: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    bypass: Entity<InputState>,
    /// Settings that arrived and fill the fields on the next frame.
    arrived: Option<(Network, ProxyPassword)>,
    /// The fields hold edits not saved yet.
    touched: bool,
    /// The settings must be loaded again when the page shows.
    stale: bool,
    /// The page showed on the last frame.
    visible: bool,
    /// Identifies load and save requests, so only the latest reply applies.
    next_request: u64,
    loading: Option<u64>,
    saving: Option<u64>,
    error: Option<String>,
}

impl NetworkForm {
    /// The page's fields, and the subscriptions that track the user's edits.
    pub fn new(window: &mut Window, cx: &mut Context<AppView>) -> (Self, Vec<Subscription>) {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("http://127.0.0.1:7890"));
        let username = cx.new(|cx| {
            InputState::new(window, cx).placeholder(rust_i18n::t!("network.username").to_string())
        });
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(rust_i18n::t!("network.password").to_string())
        });
        let bypass = cx.new(|cx| InputState::new(window, cx));
        let subscriptions = [&url, &username, &password, &bypass]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(
                    input,
                    window,
                    |this, _, event: &InputEvent, _, cx| match event {
                        // The password field follows whether a username is typed.
                        InputEvent::Change => {
                            this.network.touched = true;
                            cx.notify();
                        }
                        InputEvent::PressEnter { .. } => save(this, cx),
                        _ => {}
                    },
                )
            })
            .collect();
        (
            Self {
                mode: ProxyMode::System,
                url,
                username,
                password,
                bypass,
                arrived: None,
                touched: false,
                stale: true,
                visible: false,
                next_request: 0,
                loading: None,
                saving: None,
                error: None,
            },
            subscriptions,
        )
    }

    /// Record the settings the load `id` read, unless a later one was sent.
    pub fn loaded(&mut self, id: u64, result: Result<(Network, ProxyPassword), String>) {
        if self.loading == Some(id) {
            self.loading = None;
            match result {
                Ok(settings) => self.arrived = Some(settings),
                Err(message) => self.error = Some(message),
            }
        }
    }

    /// Record how the save `id` ended; saved settings load again unless
    /// they were edited since.
    pub fn saved(&mut self, id: u64, result: Result<ConfigChange, String>) -> Vec<Action> {
        if self.saving != Some(id) {
            return Vec::new();
        }
        self.saving = None;
        match result {
            Ok(change) => {
                self.error = None;
                self.stale = !self.touched;
                vec![Action::Toast(match change {
                    ConfigChange::Applied => Toast::NetworkApplied,
                    ConfigChange::Saved => Toast::NetworkSaved,
                })]
            }
            Err(message) => {
                self.error = Some(message);
                // What was refused is still unsaved.
                self.touched = true;
                Vec::new()
            }
        }
    }
}

/// Load the settings when the page shows, and fill the fields with what
/// arrived. Edits not saved yet are kept.
pub fn follow(view: &mut AppView, window: &mut Window, cx: &mut Context<AppView>) {
    let visible = view.page == Page::Settings(SettingsPage::Network);
    let commands = view.commands.clone();
    let form = &mut view.network;
    if visible && !form.visible && !form.touched {
        form.stale = true;
    }
    form.visible = visible;
    if visible && form.stale && form.saving.is_none() && form.loading.is_none() {
        form.next_request += 1;
        let id = form.next_request;
        if commands
            .try_send(BridgeCommand::Network(NetworkRequest::Load { id }))
            .is_ok()
        {
            form.loading = Some(id);
            form.stale = false;
        }
    }
    let Some((network, password)) = form.arrived.take() else {
        return;
    };
    form.mode = network.mode;
    form.touched = false;
    let placeholder = match password {
        ProxyPassword::Saved => rust_i18n::t!("network.password_saved").to_string(),
        ProxyPassword::Missing => rust_i18n::t!("network.password").to_string(),
    };
    for (input, value) in [
        (&form.url, network.url),
        (&form.username, network.username),
        (&form.bypass, network.bypass),
        (&form.password, String::new()),
    ] {
        input.update(cx, |input, cx| input.set_value(value, window, cx));
    }
    form.password.update(cx, |input, cx| {
        input.set_placeholder(placeholder, window, cx)
    });
}

/// Send the fields to be saved. An empty password field keeps the saved
/// password.
fn save(view: &mut AppView, cx: &mut Context<AppView>) {
    let form = &mut view.network;
    if !form.touched || form.saving.is_some() {
        return;
    }
    let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
    let network = Network {
        mode: form.mode,
        url: value(&form.url),
        username: value(&form.username),
        bypass: value(&form.bypass),
    };
    let password = match value(&form.password).trim() {
        "" => KeyChange::Keep,
        typed => KeyChange::Set(ApiKey::new(typed.to_string())),
    };
    form.next_request += 1;
    let id = form.next_request;
    match view
        .commands
        .try_send(BridgeCommand::Network(NetworkRequest::Save {
            id,
            network,
            password,
        })) {
        Ok(()) => {
            form.saving = Some(id);
            // Only edits after this point are unsaved when the reply arrives.
            form.touched = false;
            form.error = None;
        }
        Err(error) => form.error = Some(error.to_string()),
    }
}

/// The Network page: the proxy mode, and a manual proxy's server, login,
/// and the hosts it is not used for.
pub fn network(view: &AppView, cx: &mut Context<AppView>) -> Div {
    let form = &view.network;
    let mode = segmented(
        "proxy-mode",
        rust_i18n::t!("network.proxy").to_string(),
        rems(6.5),
        vec![
            Segment {
                id: "proxy-system".into(),
                label: rust_i18n::t!("settings.follow_system").to_string(),
                value: ProxyMode::System,
            },
            Segment {
                id: "proxy-none".into(),
                label: rust_i18n::t!("network.direct").to_string(),
                value: ProxyMode::None,
            },
            Segment {
                id: "proxy-manual".into(),
                label: rust_i18n::t!("network.manual").to_string(),
                value: ProxyMode::Manual,
            },
        ],
        form.mode,
        |this, mode, _, cx| {
            this.network.mode = mode;
            this.network.touched = true;
            cx.notify();
        },
        cx,
    );
    let input = |input: &Entity<InputState>, id: &'static str, key: &str| {
        Input::new(input)
            .accessibility_id(id)
            .aria_label(rust_i18n::t!(key).to_string())
    };
    // A manual proxy's fields belong to the Manual choice, so they sit
    // under it, indented along a guide, rather than beside it as settings
    // of their own.
    let manual = match form.mode {
        ProxyMode::Manual => {
            // The core keeps no password for a proxy without a username.
            let anonymous = form.username.read(cx).value().trim().is_empty();
            let login = div()
                .h_flex()
                .gap_2()
                .child(div().flex_1().child(input(
                    &form.username,
                    "proxy-username",
                    "network.username",
                )))
                .child(
                    div().flex_1().child(
                        input(&form.password, "proxy-password", "network.password")
                            .mask_toggle()
                            .disabled(anonymous),
                    ),
                );
            let fields = [
                (
                    "network.server",
                    input(&form.url, "proxy-url", "network.server").into_any_element(),
                ),
                ("network.login", login.into_any_element()),
                (
                    "network.bypass",
                    input(&form.bypass, "proxy-bypass", "network.bypass").into_any_element(),
                ),
            ]
            .map(|(key, control)| {
                div()
                    .h_flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .py_1p5()
                    .child(div().text_sm().child(rust_i18n::t!(key).to_string()))
                    .child(div().w(px(300.)).flex_shrink_0().child(control))
            });
            Some(
                div()
                    .v_flex()
                    .mx_5()
                    .mb_4()
                    .pl_4()
                    .border_l_2()
                    .border_color(cx.theme().border)
                    .children(fields),
            )
        }
        ProxyMode::System | ProxyMode::None => None,
    };
    let error = form.error.clone().map(|error| {
        a11y::alert("network-error", error.clone())
            .text_sm()
            .text_color(cx.theme().danger)
            .child(error)
    });
    let content = div()
        .v_flex()
        .gap_5()
        .p_8()
        .on_scroll(view.network_scroll.on_scroll())
        .child(header(view, cx))
        .child(card(
            [div()
                .v_flex()
                .child(setting("network.proxy", mode, cx))
                .children(manual)],
            cx,
        ))
        .children(error)
        .child(
            div().h_flex().justify_end().child(
                a11y::button("save-network")
                    .primary()
                    .label(rust_i18n::t!("models.save").to_string())
                    .disabled(!form.touched || form.saving.is_some())
                    .loading(form.saving.is_some())
                    .on_click(cx.listener(|this, _, _, cx| save(this, cx))),
            ),
        );
    view.network_scroll
        .container(
            div().id("settings-network").child(content),
            ScrollbarMode::Always,
        )
        .flex_1()
}
