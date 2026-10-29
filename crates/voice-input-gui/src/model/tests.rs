use super::{Action, Connection, DictationButton, DisabledReason, GuiState, Msg, Toast, Warning};
use voice_input_core::{
    ConfigSummary, Control, DictationState, DictationStatus, HistoryEntry, HistoryOutcome,
    PerAction, SessionState, Shortcut, ShortcutStatus, StateSnapshot, WireSnapshot,
};

fn snapshot(state: DictationState, history: Vec<HistoryEntry>) -> WireSnapshot {
    WireSnapshot {
        seq: 1,
        state: StateSnapshot {
            take: 1,
            dictation: DictationStatus {
                state,
                since_ms: None,
            },
            queued: false,
            history,
            last_error: None,
            config: ConfigSummary {
                input_method: "fcitx5".to_string(),
                provider_model: "model".to_string(),
                provider_url: "http://127.0.0.1".to_string(),
                polish_enabled: false,
            },
        },
        shortcut: ShortcutStatus {
            backend: voice_input_core::Backend::None,
            session: SessionState::Disabled,
            preferred: PerAction {
                toggle: Shortcut::DEFAULT_TOGGLE,
                hold: Shortcut::DEFAULT_HOLD,
            },
            actual: PerAction::default(),
            last_error: None,
        },
    }
}

fn kept(id: u64, take: u64) -> HistoryEntry {
    HistoryEntry {
        id,
        take,
        at_ms: 1,
        original: "original".to_string(),
        text: Some("kept text".to_string()),
        outcome: HistoryOutcome::Kept,
        truncated: false,
    }
}

fn connected() -> GuiState {
    GuiState::default()
        .apply(Msg::Hello {
            protocol: 1,
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        })
        .0
}

#[test]
fn spawn_failure_and_reconnect_keep_their_distinct_states() {
    let (spawning, actions) = GuiState::default().apply(Msg::NotRunning);
    assert_eq!(spawning.connection, Connection::Spawning);
    assert_eq!(actions, [Action::SpawnDaemon]);
    let (failed, _) = spawning.apply(Msg::SpawnFailed {
        log_tail: "last 20 lines".to_string(),
    });
    assert_eq!(
        failed.connection,
        Connection::SpawnFailed {
            log_tail: "last 20 lines".to_string(),
        }
    );
    let (disconnected, actions) = failed.apply(Msg::Disconnected {
        reason: "socket closed".to_string(),
    });
    assert_eq!(
        disconnected.connection,
        Connection::Reconnecting {
            attempt: 1,
            reason: "socket closed".to_string(),
        }
    );
    assert_eq!(actions, [Action::Reconnect]);
    let (retry, _) = disconnected.apply(Msg::Disconnected {
        reason: "again".to_string(),
    });
    assert_eq!(
        retry.connection,
        Connection::Reconnecting {
            attempt: 2,
            reason: "again".to_string(),
        }
    );
}

#[test]
fn protocol_mismatch_is_fatal_but_version_difference_is_only_a_warning() {
    let (old, _) = GuiState::default().apply(Msg::DaemonError {
        message: "invalid request: unknown variant `subscribe`".to_string(),
    });
    assert_eq!(
        old.connection,
        Connection::ProtocolMismatch {
            message: "invalid request: unknown variant `subscribe`".to_string(),
        }
    );
    let (wrong_protocol, _) = GuiState::default().apply(Msg::Hello {
        protocol: 2,
        daemon_version: "0.1.0".to_string(),
    });
    assert!(matches!(
        wrong_protocol.connection,
        Connection::ProtocolMismatch { .. }
    ));
    let (warning, _) = GuiState::default().apply(Msg::Hello {
        protocol: 1,
        daemon_version: "0.0.9".to_string(),
    });
    assert_eq!(
        warning.warning,
        Some(Warning::DaemonVersion {
            running: "0.0.9".to_string(),
            expected: env!("CARGO_PKG_VERSION").to_string(),
        })
    );
    assert_eq!(warning.connection, Connection::Connected);
}

#[test]
fn only_this_guis_kept_take_is_copied_once_across_reconnects() {
    let (stopped, _) = connected().apply(Msg::StopAccepted { take: 7 });
    let snapshot = snapshot(
        DictationState::Idle,
        vec![kept(10, 7), kept(11, 8), kept(12, 7)],
    );
    let (copied, actions) = stopped.apply(Msg::Snapshot(Box::new(snapshot.clone())));
    assert_eq!(
        actions,
        [
            Action::CopyToClipboard("kept text".to_string()),
            Action::Toast(Toast::CopiedToClipboard),
        ]
    );
    assert!(copied.copied_entries.contains(&10));
    assert!(!copied.copied_entries.contains(&11));
    let (repeated, repeated_actions) = copied.apply(Msg::Snapshot(Box::new(snapshot.clone())));
    assert!(repeated_actions.is_empty());
    let (disconnected, _) = repeated.apply(Msg::Disconnected {
        reason: "reconnect".to_string(),
    });
    let (_reconnected, reconnected_actions) = disconnected.apply(Msg::Snapshot(Box::new(snapshot)));
    assert!(reconnected_actions.is_empty());
}

#[test]
fn a_new_daemon_does_not_reuse_the_previous_instances_copy_history() {
    let (stopped, _) = connected().apply(Msg::StopAccepted { take: 7 });
    let (copied, _) = stopped.apply(Msg::Snapshot(Box::new(snapshot(
        DictationState::Idle,
        vec![kept(10, 7)],
    ))));
    let (fresh, _) = copied.apply(Msg::NotRunning);
    assert!(fresh.copied_entries.is_empty());
    assert!(fresh.pending_copies.is_empty());
    assert!(fresh.snapshot.is_none());
}

#[test]
fn button_availability_matches_the_daemon_view() {
    let cases = [
        (DictationState::Idle, DictationButton::Start),
        (
            DictationState::Starting {
                control: Control::Toggle,
            },
            DictationButton::Disabled(DisabledReason::OpeningMicrophone),
        ),
        (
            DictationState::Recording {
                control: Control::Toggle,
            },
            DictationButton::Stop,
        ),
        (
            DictationState::Recording {
                control: Control::Hold,
            },
            DictationButton::Disabled(DisabledReason::ReleaseShortcut),
        ),
        (DictationState::Transcribing, DictationButton::RecordAnother),
        (DictationState::Committing, DictationButton::RecordAnother),
    ];
    for (view, expected) in cases {
        let state = GuiState {
            connection: Connection::Connected,
            snapshot: Some(snapshot(view, Vec::new())),
            ..GuiState::default()
        };
        assert_eq!(state.button(), expected, "{view:?}");
    }
    let mut queued = connected();
    queued.snapshot = Some(snapshot(DictationState::Transcribing, Vec::new()));
    queued.snapshot.as_mut().unwrap().state.queued = true;
    assert_eq!(queued.button(), DictationButton::Queued);
    assert_eq!(
        GuiState::default().button(),
        DictationButton::Disabled(DisabledReason::Offline)
    );
}

#[test]
fn command_errors_stay_verbatim_and_an_intentional_stop_does_not_spawn() {
    let (state, actions) = connected().apply(Msg::CommandError {
        message: "not recording".to_string(),
    });
    assert_eq!(state.command_error.as_deref(), Some("not recording"));
    assert!(actions.is_empty());
    let (stopped, actions) = state.apply(Msg::DaemonStopped);
    assert_eq!(stopped.connection, Connection::NotRunning);
    assert!(actions.is_empty());
}

#[test]
fn replacing_the_daemon_resets_ids_without_spawning_again() {
    let (stopped, _) = connected().apply(Msg::StopAccepted { take: 7 });
    let (copied, _) = stopped.apply(Msg::Snapshot(Box::new(snapshot(
        DictationState::Idle,
        vec![kept(10, 7)],
    ))));
    let (replaced, actions) = copied.apply(Msg::DaemonReplaced);
    assert!(actions.is_empty());
    assert_eq!(replaced.connection, Connection::Connecting);
    assert!(replaced.copied_entries.is_empty());
    assert!(replaced.pending_copies.is_empty());
    assert!(replaced.snapshot.is_none());
}
