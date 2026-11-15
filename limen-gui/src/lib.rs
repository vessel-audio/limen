//! Optional GPUI control panel for the Limen daemon.

mod a11y;
mod app;
#[cfg(target_os = "macos")]
mod app_menu;
mod assets;
mod bridge;
mod chrome;
mod instance;
mod locale;
#[cfg(target_os = "macos")]
mod menu_bar;
mod model;
mod motion;
mod pages;
mod palette;
mod recorder;
mod settings;
mod sidebar;
mod smooth_scroll;
#[cfg(target_os = "macos")]
mod status_item;

pub use bridge::{BridgeCommand, BridgeEvent, ModelRequest, ShortcutRequest};
pub use model::{
    Action, Connection, DictationButton, DisabledReason, GuiState, Msg, Toast, Warning,
};
pub use recorder::{
    Action as RecorderAction, Event as RecorderEvent, RecorderError, RecorderState,
};

rust_i18n::i18n!("locales");

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use gpui_kit::component::TitleBar;
use gpui_kit::*;
use limen_core::{DaemonHost, ModelBackend, Paths};

/// Run the control panel on the calling thread; `models` lists the models
/// a provider offers. On macOS the panel hosts the daemon it starts and
/// stays in the menu bar while its window is closed.
pub fn run(paths: Paths, models: Arc<dyn ModelBackend>) -> Result<()> {
    let _instance = match instance::acquire(&paths)? {
        Some(file) => file,
        None => return Ok(()),
    };
    locale::initialize();
    #[cfg(target_os = "macos")]
    let (host, pump) = {
        let (host, pump) = DaemonHost::embedded(models.clone());
        (host, menu_bar::Pump::new(pump))
    };
    #[cfg(not(target_os = "macos"))]
    let host = DaemonHost::Detached;
    let (commands, mut events, bridge_thread) = bridge::start(paths.clone(), models, host);
    let bridge = Rc::new(RefCell::new(Some(bridge_thread)));
    let quit_bridge = Rc::clone(&bridge);
    let opened = Rc::new(RefCell::new(None));
    let result = Rc::clone(&opened);
    let main_window = Rc::new(Cell::new(None));
    let application = gpui_kit::application().with_assets(assets::Assets);
    #[cfg(target_os = "macos")]
    {
        let main_window = Rc::clone(&main_window);
        application.on_reopen(move |cx| {
            if let Some(window) = main_window.get() {
                menu_bar::show(window, cx)
            }
        });
    }
    application.run(move |cx| {
        gpui_kit::init(cx);
        gpui_kit::component::theme::Theme::sync_system_appearance(None, cx);
        palette::apply(cx);
        #[cfg(target_os = "macos")]
        app_menu::install(cx);
        // Registered before anything can quit, so the daemon this process
        // hosts is stopped on every way out.
        #[cfg(target_os = "macos")]
        let quit_pump = pump.clone();
        cx.on_app_quit(move |_| {
            #[cfg(target_os = "macos")]
            quit_pump.close();
            stop_bridge(&quit_bridge);
            async {}
        })
        .detach();
        match gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: point(px(0.0), px(0.0)),
                    size: size(px(980.0), px(660.0)),
                })),
                window_min_size: Some(size(px(600.0), px(440.0))),
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
                                            cx.write_to_clipboard(ClipboardItem::new_string(text));
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
            Ok((window, view)) => {
                main_window.set(Some(window));
                #[cfg(target_os = "macos")]
                {
                    pump.start(cx);
                    menu_bar::attach(window, view, commands.clone(), cx);
                }
                #[cfg(not(target_os = "macos"))]
                drop(view);
            }
            Err(error) => {
                // On macOS quitting ends the process before `run` returns
                // the error, so it is logged here as well.
                tracing::error!(error = %format!("{error:#}"), "failed to open the control panel");
                *result.borrow_mut() = Some(error);
                cx.quit();
            }
        }
    });
    stop_bridge(&bridge);
    match opened.borrow_mut().take() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Stop the bridge, and with it the daemon this process hosts. Blocks; the
/// first call does the work. On macOS quitting ends the process before
/// [`run`] returns, so the app's quit handler is the call that counts.
fn stop_bridge(bridge: &RefCell<Option<bridge::BridgeThread>>) {
    let taken = bridge.borrow_mut().take();
    if let Some(bridge) = taken {
        bridge.stop();
    }
}
