//! Optional GPUI control panel for the Limen daemon.

mod a11y;
mod app;
mod assets;
mod bridge;
mod chrome;
mod instance;
mod locale;
mod model;
mod motion;
mod pages;
mod palette;
mod recorder;
mod settings;
mod sidebar;
mod smooth_scroll;

pub use bridge::{BridgeCommand, BridgeEvent, ShortcutRequest};
pub use model::{
    Action, Connection, DictationButton, DisabledReason, GuiState, Msg, Toast, Warning,
};
pub use recorder::{
    Action as RecorderAction, Event as RecorderEvent, RecorderError, RecorderState,
};

rust_i18n::i18n!("locales");

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gpui_kit::component::TitleBar;
use gpui_kit::*;
use limen_core::{ModelBackend, Paths};

/// Run the control panel on the calling thread; `models` lists the models
/// a provider offers.
pub fn run(paths: Paths, models: Arc<dyn ModelBackend>) -> Result<()> {
    let _instance = match instance::acquire(&paths)? {
        Some(file) => file,
        None => return Ok(()),
    };
    locale::initialize();
    let (commands, mut events, bridge_thread) = bridge::start(paths.clone(), models);
    let bridge_commands = commands.clone();
    let opened = Rc::new(RefCell::new(None));
    let result = Rc::clone(&opened);
    gpui_kit::application()
        .with_assets(assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            gpui_kit::component::theme::Theme::sync_system_appearance(None, cx);
            palette::apply(cx);
            match gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds {
                        origin: point(px(0.0), px(0.0)),
                        size: size(px(820.0), px(580.0)),
                    })),
                    window_min_size: Some(size(px(560.0), px(420.0))),
                    app_id: Some("io.github.Emin017.Limen".to_string()),
                    ..TitleBar::window_options()
                },
                cx,
                |window, cx| {
                    cx.new(|cx| {
                        let app = app::AppView::new(paths.clone(), commands.clone(), window, cx);
                        cx.spawn(async move |this, cx| {
                            while let Some(event) = events.recv().await {
                                let _ = this.update(cx, |window: &mut app::AppView, cx| {
                                    for action in window.apply_bridge_event(event) {
                                        match action {
                                            Action::CopyToClipboard(text) => {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    text,
                                                ));
                                            }
                                            Action::Reconnect => {}
                                            Action::Toast(toast) => window.show_toast(toast),
                                        }
                                    }
                                    cx.notify();
                                });
                            }
                        })
                        .detach();
                        cx.spawn(async move |this, cx| {
                            loop {
                                cx.background_executor()
                                    .timer(Duration::from_millis(200))
                                    .await;
                                let _ = this.update(cx, |_: &mut app::AppView, cx| cx.notify());
                            }
                        })
                        .detach();
                        app
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
    let _ = bridge_commands.blocking_send(BridgeCommand::Quit);
    let _ = bridge_thread.join();
    match opened.borrow_mut().take() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
