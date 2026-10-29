//! Root GPUI view and connection/status presentation.

use gpui_kit::component::{
    ActiveTheme, StyledExt, TitleBar,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;
use std::time::Instant;
use tokio::sync::mpsc;

use voice_input_core::{Backend, DictationState, Paths};

use crate::bridge::{BridgeCommand, BridgeEvent};
use crate::model::{Action, Connection, GuiState, Msg, Warning};
use crate::pages;
use crate::recorder::RecorderState;
use gpui_kit::component::input::{InputEvent, InputState};

/// Root application state owned by the GPUI thread.
pub struct AppView {
    pub model: GuiState,
    pub commands: mpsc::Sender<BridgeCommand>,
    pub paths: Paths,
    pub page: Page,
    pub selected_backend: Backend,
    pub backend_selected: bool,
    pub recorder: RecorderState,
    pub toggle_input: Entity<InputState>,
    pub hold_input: Entity<InputState>,
    pub toggle_touched: bool,
    pub hold_touched: bool,
    pub toggle_focused: bool,
    pub hold_focused: bool,
    _subscriptions: Vec<Subscription>,
    pub about: bool,
    pub confirm_clear: bool,
    pub confirm_shutdown: Option<ShutdownAction>,
    pub toast: Option<(String, Instant)>,
    pub offer_terminate: bool,
    pub level_history: Vec<f32>,
    pub silent_since: Option<Instant>,
    pub shortcut_report: Option<Result<voice_input_core::Report, crate::bridge::ShortcutFailure>>,
}

/// Top-level navigation pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Dictation,
    History,
    Shortcuts,
}

/// Daemon-stop actions that require an explicit confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownAction {
    Stop,
    Terminate,
}

impl AppView {
    pub fn new(
        paths: Paths,
        commands: mpsc::Sender<BridgeCommand>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let toggle_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Super+Z")
                .default_value("Super+Z")
        });
        let hold_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Super+X")
                .default_value("Super+X")
        });
        let subscriptions = vec![
            cx.subscribe_in(&toggle_input, window, |this, _, event, _, _| match event {
                InputEvent::Change => this.toggle_touched = true,
                InputEvent::Focus => this.toggle_focused = true,
                InputEvent::Blur => this.toggle_focused = false,
                _ => {}
            }),
            cx.subscribe_in(&hold_input, window, |this, _, event, _, _| match event {
                InputEvent::Change => this.hold_touched = true,
                InputEvent::Focus => this.hold_focused = true,
                InputEvent::Blur => this.hold_focused = false,
                _ => {}
            }),
        ];
        Self {
            model: GuiState::default(),
            commands,
            paths,
            page: Page::Dictation,
            selected_backend: Backend::Portal,
            backend_selected: false,
            recorder: RecorderState::Idle { current: None },
            toggle_input,
            hold_input,
            toggle_touched: false,
            hold_touched: false,
            toggle_focused: false,
            hold_focused: false,
            _subscriptions: subscriptions,
            about: false,
            confirm_clear: false,
            confirm_shutdown: None,
            toast: None,
            offer_terminate: false,
            level_history: Vec::new(),
            silent_since: None,
            shortcut_report: None,
        }
    }

    /// Apply one bridge event and return GUI-only side effects.
    pub fn apply_bridge_event(&mut self, event: BridgeEvent) -> Vec<Action> {
        let level_event = matches!(event, BridgeEvent::Msg(Msg::Level(_)));
        if matches!(event, BridgeEvent::Msg(Msg::DaemonReplaced)) {
            self.toggle_touched = false;
            self.hold_touched = false;
            self.backend_selected = false;
        }
        if let BridgeEvent::Msg(Msg::Snapshot(snapshot)) = &event
            && !self.backend_selected
        {
            self.selected_backend = snapshot.shortcut.backend;
        }
        let actions = self.apply_event(event);
        self.update_level_tracking(level_event);
        actions
    }

    fn apply_event(&mut self, event: BridgeEvent) -> Vec<Action> {
        match event {
            BridgeEvent::Msg(msg) => {
                let (next, actions) = std::mem::take(&mut self.model).apply(msg);
                self.model = next;
                actions
            }
            BridgeEvent::OfferTerminate => {
                self.offer_terminate = true;
                Vec::new()
            }
            BridgeEvent::ShortcutReport(report) => {
                self.shortcut_report = Some(report);
                Vec::new()
            }
            BridgeEvent::Toast(_) => Vec::new(),
        }
    }

    fn update_level_tracking(&mut self, level_event: bool) {
        let recording = self.model.snapshot.as_ref().is_some_and(|snapshot| {
            matches!(
                snapshot.state.dictation.state,
                DictationState::Recording { .. }
            )
        });
        if !recording {
            self.model.level = voice_input_core::Level::SILENCE;
            self.level_history.clear();
            self.silent_since = None;
            return;
        }
        if level_event {
            self.level_history.push(self.model.level.rms);
            if self.level_history.len() > 24 {
                self.level_history.remove(0);
            }
        }
        if self.model.level.dbfs < -50.0 {
            self.silent_since.get_or_insert_with(Instant::now);
        } else {
            self.silent_since = None;
        }
    }

    /// Whether silence has stayed below the hint threshold for three seconds.
    pub fn no_sound_detected(&self) -> bool {
        self.silent_since
            .is_some_and(|since| since.elapsed() >= std::time::Duration::from_secs(3))
    }

    fn title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let button = |id: &'static str, label: String, page: Page| {
            Button::new(id)
                .ghost()
                .label(label)
                .on_click(cx.listener(move |this, _, _, _| this.page = page))
        };
        TitleBar::new().child(
            div()
                .h_flex()
                .items_center()
                .justify_between()
                .w_full()
                .child(
                    div()
                        .h_flex()
                        .gap_2()
                        .items_center()
                        .child(rust_i18n::t!("app.title").to_string())
                        .child(button(
                            "dictation-page",
                            rust_i18n::t!("nav.dictation").to_string(),
                            Page::Dictation,
                        ))
                        .child(button(
                            "history-page",
                            rust_i18n::t!("nav.history").to_string(),
                            Page::History,
                        ))
                        .child(button(
                            "shortcuts-page",
                            rust_i18n::t!("nav.shortcuts").to_string(),
                            Page::Shortcuts,
                        )),
                )
                .child(
                    Button::new("about")
                        .ghost()
                        .label("⋮")
                        .on_click(cx.listener(|this, _, _, _| this.about = true)),
                ),
        )
    }

    fn banners(&self, cx: &mut Context<Self>) -> Vec<Div> {
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
    }

    fn connection_overlay(&self, cx: &mut Context<Self>) -> Option<Div> {
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
                .gap_3()
                .bg(cx.theme().colors.background)
                .child(message)
                .children(log.map(|log| div().child(log)))
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
        )
    }

    fn footer(&self) -> Div {
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
            .p_2()
            .border_t_1()
            .child(config)
            .child(rust_i18n::t!("connection.closing_hint").to_string())
    }
}

impl AppView {
    fn shutdown_overlay(&self, cx: &mut Context<Self>) -> Option<Div> {
        let action = self.confirm_shutdown?;
        let (message, command) = match action {
            ShutdownAction::Stop => (
                rust_i18n::t!("connection.stop_confirm").to_string(),
                BridgeCommand::ShutdownDaemon,
            ),
            ShutdownAction::Terminate => (
                rust_i18n::t!("connection.terminate_confirm").to_string(),
                BridgeCommand::TerminateDaemon,
            ),
        };
        Some(
            div()
                .absolute()
                .inset_0()
                .v_flex()
                .items_center()
                .justify_center()
                .gap_3()
                .bg(cx.theme().colors.background)
                .child(message)
                .child(
                    div()
                        .h_flex()
                        .gap_2()
                        .child(
                            Button::new("confirm-shutdown")
                                .primary()
                                .label(rust_i18n::t!("action.confirm").to_string())
                                .on_click(cx.listener(move |this, _, _, _| {
                                    let _ = this.commands.try_send(command.clone());
                                    this.confirm_shutdown = None;
                                })),
                        )
                        .child(
                            Button::new("cancel-shutdown")
                                .ghost()
                                .label(rust_i18n::t!("action.cancel").to_string())
                                .on_click(cx.listener(|this, _, _, _| {
                                    this.confirm_shutdown = None;
                                })),
                        ),
                ),
        )
    }

    /// Show a GUI-owned toast for three seconds.
    pub fn show_toast(&mut self) {
        self.toast = Some((rust_i18n::t!("toast.copied").to_string(), Instant::now()));
    }

    fn toast_overlay(&self) -> Option<Div> {
        let (message, at) = self.toast.as_ref()?;
        if at.elapsed() >= std::time::Duration::from_secs(3) {
            return None;
        }
        Some(div().absolute().bottom_4().right_4().child(message.clone()))
    }

    fn snapshot_config(&self) -> Option<voice_input_core::ConfigSummary> {
        self.model
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.state.config.clone())
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = match self.page {
            Page::Dictation => pages::dictation(self, cx),
            Page::History => pages::history(self, cx),
            Page::Shortcuts => pages::shortcuts(self, cx, window),
        };
        div()
            .v_flex()
            .size_full()
            .child(self.title_bar(cx))
            .children(self.banners(cx))
            .child(page)
            .child(self.footer())
            .children(self.about.then(|| {
                div()
                    .absolute()
                    .inset_0()
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .bg(cx.theme().colors.background)
                    .child(pages::about(self, cx))
            }))
            .children(self.shutdown_overlay(cx))
            .children(self.toast_overlay())
            .children(self.connection_overlay(cx))
    }
}
