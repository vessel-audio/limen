//! The cases, grouped by the part of the window they exercise.

mod dictation;
mod history;
mod lifecycle;
mod models;
mod navigation;
mod settings;
mod shortcuts;
mod window;

use anyhow::Result;
use limen_core::{HistoryEntry, HistoryOutcome, RequestEntry, RequestOutcome};

use crate::Case;
use crate::desktop::Key;
use crate::session::Session;

macro_rules! cases {
    ($($path:path),* $(,)?) => {
        &[$(Case { name: stringify!($path), run: $path }),*]
    };
}

pub const ALL: &[Case] = cases![
    navigation::sidebar_switches_pages,
    navigation::sidebar_collapses_to_icons_and_back,
    navigation::title_bar_buttons_show_tooltips,
    navigation::settings_button_opens_general_settings,
    navigation::the_keyboard_reaches_and_presses_controls,
    window::dragging_the_title_bar_moves_the_window,
    window::double_clicking_the_title_bar_toggles_maximize,
    window::closing_the_window_keeps_the_daemon_running,
    window::a_second_launch_keeps_the_open_window,
    dictation::start_and_stop_copy_the_transcript,
    dictation::failed_requests_expand_to_the_full_error,
    dictation::the_request_list_scrolls_with_the_wheel,
    dictation::recording_another_take_queues_it,
    models::the_dictation_button_opens_the_asr_page,
    models::an_asr_provider_is_saved_and_applied,
    models::a_vendor_is_chosen_from_its_card,
    models::an_invalid_asr_server_is_reported,
    models::polishing_is_set_up_with_a_local_server,
    models::the_switch_shows_the_providers,
    models::a_saved_key_is_removed,
    models::a_url_credential_is_rejected,
    models::edits_made_while_saving_survive,
    models::openai_polishing_chooses_its_chat_api,
    models::the_model_is_detected_from_the_typed_server,
    models::the_models_are_detected_on_request,
    models::a_model_is_tested_before_it_is_saved,
    models::advanced_settings_belong_to_their_provider,
    history::copy_puts_a_transcript_on_the_clipboard,
    history::clearing_history_asks_for_confirmation,
    history::the_history_scrolls_with_the_wheel,
    shortcuts::the_backend_selector_swaps_the_form,
    shortcuts::copying_the_toggle_command,
    shortcuts::applying_the_portal_backend_writes_the_config,
    shortcuts::an_unparsable_portal_shortcut_is_reported,
    shortcuts::editing_portal_shortcuts_opens_gnome_settings,
    shortcuts::retrying_a_failed_portal_session,
    shortcuts::recording_a_gnome_shortcut_with_the_keyboard,
    shortcuts::shifted_symbols_need_manual_entry,
    shortcuts::setting_and_clearing_a_gnome_shortcut_by_hand,
    settings::settings_tabs_switch_between_general_and_about,
    settings::appearance_follows_the_choice_and_the_system,
    settings::language_choice_renames_the_interface,
    settings::the_text_scale_resizes_the_interface,
    settings::the_font_select_applies_a_family,
    settings::lightweight_mode_skips_motion,
    settings::the_scrollbar_drags_the_settings_page,
    lifecycle::stopping_asks_first_and_start_brings_it_back,
    lifecycle::restarting_replaces_the_daemon,
    lifecycle::a_failed_start_offers_the_log,
    lifecycle::an_outdated_daemon_can_be_terminated,
    lifecycle::a_mismatched_daemon_version_is_named,
];

/// A fixed point in time so cases render the same clock everywhere.
const EPOCH_MS: u64 = 1_767_225_600_000;

/// A transcript the daemon kept in history without typing it.
pub fn kept(id: u64, text: &str) -> HistoryEntry {
    HistoryEntry {
        id,
        take: id,
        at_ms: EPOCH_MS + id * 60_000,
        original: text.to_string(),
        text: Some(text.to_string()),
        outcome: HistoryOutcome::Kept,
        truncated: false,
    }
}

/// A transcription request that failed with `error`.
pub fn failed_request(take: u64, error: &str) -> RequestEntry {
    RequestEntry {
        take,
        started_ms: EPOCH_MS + take * 60_000,
        duration_ms: 1_200,
        outcome: RequestOutcome::Failed {
            error: error.to_string(),
        },
    }
}

/// Replace a text field's contents by typing, as a user does.
pub fn retype(session: &Session, id: &str, text: &str) -> Result<()> {
    session.click(id)?;
    session.chord(&[Key::CONTROL, Key::char('a')])?;
    session.x11().type_text(text)
}
