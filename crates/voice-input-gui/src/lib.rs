//! Optional GPUI control panel for the voice-input daemon.

mod bridge;
mod locale;
mod model;
mod recorder;

pub use model::{
    Action, Connection, DictationButton, DisabledReason, GuiState, Msg, Toast, Warning,
};
pub use recorder::{
    Action as RecorderAction, Event as RecorderEvent, RecorderError, RecorderState,
};

pub use bridge::{BridgeCommand, BridgeEvent, ShortcutRequest};

rust_i18n::i18n!("locales");

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gpui_kit::*;
use voice_input_core::Paths;

/// Run the control panel on the calling thread.
pub fn run(paths: Paths) -> Result<()> {
    struct Window {
        model: model::GuiState,
    }

    impl Render for Window {
        fn render(&mut self, _: &mut gpui_kit::Window, _: &mut Context<Self>) -> impl IntoElement {
            let _ = self.model.button();
            div()
                .size_full()
                .items_center()
                .justify_center()
                .child("voice-input")
        }
    }

    locale::initialize();
    let (model, _) = model::GuiState::default().apply(model::Msg::Connecting);
    let (commands, mut events, bridge_thread) = bridge::start(paths);
    let opened = Rc::new(RefCell::new(None));
    let result = Rc::clone(&opened);
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        match gpui_kit::open_window(
            WindowOptions {
                app_id: Some("io.github.Emin017.VoiceInput".to_string()),
                ..WindowOptions::default()
            },
            cx,
            |_, cx| {
                cx.new(|cx| {
                    let window_commands = commands.clone();
                    cx.spawn(async move |this, cx| {
                        while let Some(event) = events.recv().await {
                            let _ = this.update(cx, |window: &mut Window, cx| {
                                let (next, actions) = match event {
                                    bridge::BridgeEvent::Msg(msg) => {
                                        std::mem::take(&mut window.model).apply(msg)
                                    }
                                    bridge::BridgeEvent::OfferTerminate
                                    | bridge::BridgeEvent::ShortcutReport(_)
                                    | bridge::BridgeEvent::Toast(_) => {
                                        (window.model.clone(), Vec::new())
                                    }
                                };
                                window.model = next;
                                for action in actions {
                                    match action {
                                        model::Action::CopyToClipboard(text) => {
                                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                                        }
                                        model::Action::SpawnDaemon => {
                                            let _ = window_commands
                                                .try_send(bridge::BridgeCommand::SpawnDaemon);
                                        }
                                        model::Action::Reconnect | model::Action::Toast(_) => {}
                                    }
                                }
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                    Window {
                        model: model.clone(),
                    }
                })
            },
        ) {
            Ok(_) => {}
            Err(error) => {
                *result.borrow_mut() = Some(error);
                cx.quit();
            }
        }
    });
    let _ = bridge_thread.join();
    match opened.borrow_mut().take() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
