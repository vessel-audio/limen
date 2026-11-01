//! Root GPUI view and connection/status presentation.

use gpui_kit::component::{ActiveTheme, StyledExt, Theme, ThemeMode, button::ButtonVariants};
use gpui_kit::*;
use std::time::Instant;
use tokio::sync::mpsc;

use limen_core::{Backend, DictationState, Paths, ProviderUrlChange};

use crate::a11y;
use crate::bridge::{BridgeCommand, BridgeEvent};
use crate::model::{Action, GuiState, Msg, Toast};
use crate::pages::{self, ProviderForm};
use crate::recorder::RecorderState;
use crate::settings::{
    AppearancePreference, FontChoice, LanguagePreference, LightweightMode, SettingsPage,
};
use crate::smooth_scroll::SmoothScroll;
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::searchable_list::SearchableVec;
use gpui_kit::component::select::{SelectEvent, SelectState};

/// Recording levels kept for the live waveform.
pub const LEVEL_BARS: usize = 64;

/// Root application state owned by the GPUI thread.
pub struct AppView {
    pub model: GuiState,
    pub commands: mpsc::Sender<BridgeCommand>,
    pub paths: Paths,
    pub page: Page,
    pub selected_backend: Backend,
    pub backend_selected: bool,
    pub recorder: RecorderState,
    pub recorder_focus: FocusHandle,
    pub toggle_input: Entity<InputState>,
    pub hold_input: Entity<InputState>,
    pub toggle_touched: bool,
    pub hold_touched: bool,
    pub toggle_focused: bool,
    pub hold_focused: bool,
    pub provider: ProviderForm,
    _subscriptions: Vec<Subscription>,
    pub sidebar_mode: crate::sidebar::SidebarMode,
    pub appearance: AppearancePreference,
    pub language: LanguagePreference,
    pub font_size: f32,
    pub font_family: SharedString,
    pub font_select: Entity<SelectState<SearchableVec<FontChoice>>>,
    pub lightweight: LightweightMode,
    pub confirm_clear: bool,
    pub confirm_shutdown: Option<ShutdownAction>,
    pub toast: Option<(String, Instant)>,
    pub offer_terminate: bool,
    pub level_history: Vec<f32>,
    pub silent_since: Option<Instant>,
    pub shortcut_report: Option<Result<limen_core::Report, crate::bridge::ShortcutFailure>>,
    pub settings_scroll: SmoothScroll,
    pub shortcuts_scroll: SmoothScroll,
    pub history_scroll: SmoothScroll,
    pub requests_scroll: SmoothScroll,
    /// The failed request whose full error is shown.
    pub expanded_request: Option<crate::pages::RequestKey>,
}

/// Top-level navigation pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Dictation,
    History,
    Shortcuts,
    Settings(SettingsPage),
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
        let (provider, provider_subscriptions) = ProviderForm::new(window, cx);
        let font_select = cx.new(|cx| {
            SelectState::new(
                SearchableVec::new(FontChoice::all(cx)),
                Some(gpui_kit::component::IndexPath::new(0)),
                window,
                cx,
            )
            .searchable(true)
        });
        let subscriptions = vec![
            cx.subscribe_in(&toggle_input, window, |this, _, event, _, _| match event {
                InputEvent::Change => this.toggle_touched = true,
                InputEvent::Focus => this.toggle_focused = true,
                InputEvent::Blur => this.toggle_focused = false,
                _ => {}
            }),
            cx.subscribe_in(
                &font_select,
                window,
                |this, _, event: &SelectEvent<SearchableVec<FontChoice>>, _, cx| match event {
                    SelectEvent::Confirm(Some(family)) => {
                        this.font_family = family.clone();
                        this.apply_typography(cx);
                    }
                    SelectEvent::Confirm(None) => {}
                },
            ),
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
            recorder_focus: cx.focus_handle(),
            toggle_input,
            hold_input,
            toggle_touched: false,
            hold_touched: false,
            toggle_focused: false,
            hold_focused: false,
            provider,
            _subscriptions: subscriptions
                .into_iter()
                .chain(provider_subscriptions)
                .collect(),
            sidebar_mode: crate::sidebar::SidebarMode::Expanded,
            appearance: AppearancePreference::System,
            language: LanguagePreference::System,
            font_size: 16.0,
            font_family: FontChoice::SYSTEM_FAMILY.into(),
            font_select,
            lightweight: LightweightMode::Off,
            confirm_clear: false,
            confirm_shutdown: None,
            toast: None,
            offer_terminate: false,
            level_history: Vec::new(),
            silent_since: None,
            shortcut_report: None,
            expanded_request: None,
            settings_scroll: SmoothScroll::default(),
            shortcuts_scroll: SmoothScroll::default(),
            history_scroll: SmoothScroll::default(),
            requests_scroll: SmoothScroll::default(),
        }
    }

    pub(crate) fn apply_appearance(&self, window: &mut Window, cx: &mut Context<Self>) {
        match self.appearance {
            AppearancePreference::System => Theme::sync_system_appearance(Some(window), cx),
            AppearancePreference::Light => Theme::change(ThemeMode::Light, Some(window), cx),
            AppearancePreference::Dark => Theme::change(ThemeMode::Dark, Some(window), cx),
        }
        crate::palette::apply(cx);
        self.apply_typography(cx);
    }

    /// Re-apply font choices, which a theme reload may reset.
    pub(crate) fn apply_typography(&self, cx: &mut App) {
        let (family, size) = (self.font_family.clone(), px(self.font_size));
        Theme::update(cx, |theme| {
            theme.font_family = family;
            theme.font_size = size;
        });
    }

    /// Apply one bridge event and return GUI-only side effects.
    pub fn apply_bridge_event(&mut self, event: BridgeEvent) -> Vec<Action> {
        let level_event = matches!(event, BridgeEvent::Msg(Msg::Level(_)));
        if matches!(event, BridgeEvent::Msg(Msg::DaemonReplaced)) {
            self.toggle_touched = false;
            self.hold_touched = false;
            self.provider.error = None;
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
            BridgeEvent::ProviderUrl(Ok(change)) => {
                self.provider.finish(None);
                vec![Action::Toast(match change {
                    ProviderUrlChange::Applied => Toast::ProviderApplied,
                    ProviderUrlChange::Saved => Toast::ProviderSaved,
                })]
            }
            BridgeEvent::ProviderUrl(Err(message)) => {
                self.provider.finish(Some(message));
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
            self.model.level = limen_core::Level::SILENCE;
            self.level_history.clear();
            self.silent_since = None;
            return;
        }
        if level_event {
            self.level_history.push(self.model.level.rms);
            if self.level_history.len() > LEVEL_BARS {
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
}

impl AppView {
    fn shutdown_overlay(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
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
                .id("shutdown-confirmation")
                .accessibility_id("shutdown-confirmation")
                .role(Role::AlertDialog)
                .aria_label(message.clone())
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
                            a11y::button("confirm-shutdown")
                                .primary()
                                .label(rust_i18n::t!("action.confirm").to_string())
                                .on_click(cx.listener(move |this, _, _, _| {
                                    let _ = this.commands.try_send(command.clone());
                                    this.confirm_shutdown = None;
                                })),
                        )
                        .child(
                            a11y::button("cancel-shutdown")
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
    pub fn show_toast(&mut self, toast: Toast) {
        let key = match toast {
            Toast::CopiedToClipboard => "toast.copied",
            Toast::ProviderApplied => "toast.provider_applied",
            Toast::ProviderSaved => "toast.provider_saved",
        };
        self.toast = Some((rust_i18n::t!(key).to_string(), Instant::now()));
    }

    fn toast_overlay(&self, cx: &mut Context<Self>) -> Option<Div> {
        let (message, at) = self.toast.as_ref()?;
        if at.elapsed() >= std::time::Duration::from_secs(3) {
            return None;
        }
        Some(
            div()
                .absolute()
                .bottom_16()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    a11y::status("toast", message.clone())
                        .relative()
                        .rounded_full()
                        .bg(cx.theme().foreground)
                        .text_color(cx.theme().background)
                        .text_sm()
                        .px_4()
                        .py_2()
                        .shadow_md()
                        .child(message.clone())
                        .with_animation(
                            SharedString::from(format!("toast-{at:?}")),
                            crate::motion::enter(),
                            |toast, t| toast.opacity(t).top(px(8. * (1. - t))),
                        ),
                ),
        )
    }

    pub(crate) fn snapshot_config(&self) -> Option<limen_core::ConfigSummary> {
        self.model
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.state.config.clone())
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if matches!(self.appearance, AppearancePreference::System) {
            let mode = ThemeMode::from(window.appearance());
            if cx.theme().mode != mode {
                Theme::change(mode, Some(window), cx);
                crate::palette::apply(cx);
                self.apply_typography(cx);
            }
        }
        for scroll in [
            &self.settings_scroll,
            &self.shortcuts_scroll,
            &self.history_scroll,
            &self.requests_scroll,
        ] {
            scroll.step(window);
        }
        let page = match self.page {
            Page::Dictation => div()
                .v_flex()
                .flex_1()
                .min_h_0()
                .child(pages::dictation(self, cx, window)),
            Page::History => pages::history(self, cx),
            Page::Shortcuts => div()
                .v_flex()
                .flex_1()
                .min_h_0()
                .child(pages::shortcuts(self, cx, window)),
            Page::Settings(page) => match page {
                SettingsPage::General => div()
                    .v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(crate::settings::general(self, cx)),
                SettingsPage::About => div()
                    .v_flex()
                    .flex_1()
                    .min_h_0()
                    .child(crate::settings::about(self, cx)),
            },
        };
        div()
            .v_flex()
            .size_full()
            .bg(cx.theme().muted)
            .child(self.title_bar(cx))
            .child(
                div()
                    .h_flex()
                    .flex_1()
                    .min_h_0()
                    .pr_2()
                    .pb_2()
                    .child(self.sidebar(cx))
                    .child(
                        div()
                            .v_flex()
                            .h_full()
                            .min_h_0()
                            .flex_1()
                            .min_w_0()
                            .rounded_xl()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().background)
                            .overflow_hidden()
                            .children(self.banners(cx))
                            .child(
                                div()
                                    .relative()
                                    .v_flex()
                                    .flex_1()
                                    .min_h_0()
                                    .child(page)
                                    .with_animation(
                                        SharedString::from(format!("page-{:?}", self.page)),
                                        crate::motion::enter(),
                                        |page, t| page.opacity(t).top(px(8. * (1. - t))),
                                    ),
                            )
                            .child(self.footer(cx)),
                    ),
            )
            .children(self.shutdown_overlay(cx))
            .children(self.toast_overlay(cx))
            .children(self.connection_overlay(cx))
    }
}
