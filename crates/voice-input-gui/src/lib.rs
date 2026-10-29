//! Optional GPUI control panel for the voice-input daemon.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use gpui_kit::*;
use voice_input_core::Paths;

/// Run the control panel on the calling thread.
pub fn run(_paths: Paths) -> Result<()> {
    struct Window;

    impl Render for Window {
        fn render(&mut self, _: &mut gpui_kit::Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .items_center()
                .justify_center()
                .child("voice-input")
        }
    }

    let opened = Rc::new(RefCell::new(None));
    let result = Rc::clone(&opened);
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        *result.borrow_mut() = gpui_kit::open_window(
            WindowOptions {
                app_id: Some("io.github.Emin017.VoiceInput".to_string()),
                ..WindowOptions::default()
            },
            cx,
            |_, cx| cx.new(|_| Window),
        )
        .err();
    });
    match opened.borrow_mut().take() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
