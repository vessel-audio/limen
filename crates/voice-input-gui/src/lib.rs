//! Optional GPUI control panel for the voice-input daemon.

mod locale;
mod model;
mod recorder;

pub use model::{
    Action, Connection, DictationButton, DisabledReason, GuiState, Msg, Toast, Warning,
};
pub use recorder::{
    Action as RecorderAction, Event as RecorderEvent, RecorderError, RecorderState,
};

rust_i18n::i18n!("locales");

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gpui_kit::*;
use voice_input_core::Paths;

/// Run the control panel on the calling thread.
pub fn run(_paths: Paths) -> Result<()> {
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
                cx.new(|_| Window {
                    model: model.clone(),
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
    match opened.borrow_mut().take() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
