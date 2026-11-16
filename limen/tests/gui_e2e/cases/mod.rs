//! The cases, grouped by the part of the window they exercise.

mod dictation;
mod lifecycle;
mod models;
mod navigation;
mod network;
mod settings;
mod shortcuts;
mod window;

use anyhow::Result;
use limen_core::v1;

use crate::Case;
use crate::desktop::Key;
use crate::session::Session;

macro_rules! cases {
    ($($path:path),* $(,)?) => {
        &[$(Case { name: stringify!($path), run: $path }),*]
    };
}

pub const ALL: &[Case] = cases![
    navigation::the_sidebar_toggle_shows_a_tooltip,
    navigation::collapsed_tabs_name_themselves_in_tooltips,
    window::dragging_the_title_bar_moves_the_window,
    window::double_clicking_the_title_bar_toggles_maximize,
    window::closing_the_window_keeps_the_daemon_running,
    window::a_second_launch_keeps_the_open_window,
    dictation::start_and_stop_copy_the_transcript,
    models::an_asr_provider_is_saved_and_applied,
    shortcuts::recording_a_gnome_shortcut_with_the_keyboard,
    settings::appearance_follows_the_desktops_dark_preference,
    settings::the_interface_starts_in_the_system_language,
    settings::the_font_select_applies_a_family,
    settings::lightweight_mode_skips_motion,
    network::a_manual_proxy_is_saved_and_applied,
    lifecycle::stopping_asks_first_and_start_brings_it_back,
    lifecycle::restarting_replaces_the_daemon,
    lifecycle::a_failed_start_offers_the_log,
];

/// How many times the daemon was sent `request`.
pub fn reloads(session: &Session, request: &v1::Request) -> Result<usize> {
    Ok(session
        .requests()?
        .iter()
        .filter(|sent| *sent == request)
        .count())
}

/// Replace a text field's contents by typing, as a user does.
pub fn retype(session: &Session, id: &str, text: &str) -> Result<()> {
    session.click(id)?;
    session.chord(&[Key::CONTROL, Key::char('a')])?;
    session.x11().type_text(text)
}
