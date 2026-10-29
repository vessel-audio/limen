//! Root GPUI view and connection/status presentation.

use gpui_kit::component::{
    StyledExt, TitleBar,
    button::{Button, ButtonVariants},
};
use gpui_kit::*;
use tokio::sync::mpsc;
use voice_input_core::Paths;

use crate::bridge::{BridgeCommand, BridgeEvent};
use crate::model::{Action, Connection, GuiState, Warning};
use crate::pages;

/// Root application state owned by the GPUI thread.
pub struct AppView {
    pub model: GuiState,
    pub commands: mpsc::Sender<BridgeCommand>,
    pub paths: Paths,
    pub page: Page,
    pub about: bool,
    pub confirm_clear: bool,
    pub offer_terminate: bool,
    pub shortcut_report: Option<Result<voice_input_core::Report, String>>,
}

/// Top-level navigation pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Dictation,
    History,
    Shortcuts,
}

impl AppView {
    pub fn new(paths: Paths, commands: mpsc::Sender<BridgeCommand>) -> Self {
        Self {
            model: GuiState::default(),
            commands,
            paths,
            page: Page::Dictation,
            about: false,
            confirm_clear: false,
            offer_terminate: false,
            shortcut_report: None,
        }
    }

    /// Apply one bridge event and return GUI-only side effects.
    pub fn apply_bridge_event(&mut self, event: BridgeEvent) -> Vec<Action> {
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

    fn banners(&self) -> Vec<Div> {
        let mut banners = Vec::new();
        match &self.model.connection {
            Connection::Reconnecting { attempt, reason } => {
                banners.push(div().child(format!("Reconnecting (attempt {attempt})… · {reason}")))
            }
            Connection::ProtocolMismatch { message } => {
                let mut banner = div()
                    .v_flex()
                    .gap_2()
                    .child(format!("Background service is outdated: {message}"));
                if self.offer_terminate {
                    banner = banner.child(
                        Button::new("terminate-daemon")
                            .outline()
                            .label("Terminate after confirmation")
                            .on_click({
                                let commands = self.commands.clone();
                                move |_, _, _| {
                                    let _ = commands.try_send(BridgeCommand::TerminateDaemon);
                                }
                            }),
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
            banners.push(div().child(format!("Daemon version {running}; GUI expects {expected}")));
        }
        banners
    }

    fn connection_overlay(&self) -> Option<Div> {
        let (message, log) = match &self.model.connection {
            Connection::Connecting | Connection::Spawning => {
                ("Starting background service…".to_string(), None)
            }
            Connection::NotRunning => ("Background service is not running".to_string(), None),
            Connection::SpawnFailed { log_tail } => (
                "Background service failed to start".to_string(),
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
                .bg(rgb(0x202020))
                .child(message)
                .children(log.map(|log| div().child(log)))
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
                    "{} · {} · polish {} · shortcut {}",
                    config.input_method,
                    config.provider_model,
                    config.polish_enabled,
                    self.model
                        .snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.shortcut.backend.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                )
            })
            .unwrap_or_else(|| "Disconnected".to_string());
        div()
            .h_flex()
            .justify_between()
            .p_2()
            .border_t_1()
            .child(config)
            .child("Closing the window keeps dictation running")
    }
}

impl AppView {
    fn snapshot_config(&self) -> Option<voice_input_core::ConfigSummary> {
        self.model
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.state.config.clone())
    }
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let page = match self.page {
            Page::Dictation => pages::dictation(self, cx),
            Page::History => pages::history(self, cx),
            Page::Shortcuts => pages::shortcuts(self, cx),
        };
        div()
            .v_flex()
            .size_full()
            .child(self.title_bar(cx))
            .children(self.banners())
            .child(page)
            .child(self.footer())
            .children(self.about.then(|| {
                div()
                    .absolute()
                    .inset_0()
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .bg(rgb(0x202020))
                    .child(pages::about(self, cx))
            }))
            .children(self.connection_overlay())
    }
}
