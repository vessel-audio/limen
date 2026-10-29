use super::{Completion, ERROR_CAP, HISTORY_BUDGET, HISTORY_LEN, Projection};
use crate::config::Config;
use crate::daemon::snapshot::{
    ConfigSummary, ErrorEntry, FIELD_CAP, HistoryEntry, HistoryOutcome, MAX_PUSH_BYTES, capped,
};
use crate::dictation::{Control, DictationState, DictationView};

fn config() -> Config {
    toml::from_str(
        r#"
        [provider]
        url = "http://127.0.0.1:8000/v1/audio/transcriptions"
        model = "test-model"
        "#,
    )
    .unwrap()
}

fn projection() -> Projection {
    Projection::new(&config()).0
}

fn view(state: DictationState) -> DictationView {
    DictationView {
        state,
        queued: false,
    }
}

fn starting() -> DictationView {
    view(DictationState::Starting {
        control: Control::Toggle,
    })
}

fn recording() -> DictationView {
    view(DictationState::Recording {
        control: Control::Toggle,
    })
}

fn entry(id: u64, outcome: HistoryOutcome) -> HistoryEntry {
    HistoryEntry {
        id,
        take: 1,
        at_ms: id,
        original: format!("original-{id}"),
        text: Some(format!("text-{id}")),
        outcome,
        truncated: false,
    }
}

#[test]
fn take_ids_errors_and_timing_change_only_at_their_boundaries() {
    let (mut projection, mut snapshots) = Projection::new(&config());
    projection.record_error("previous failure".to_string(), 1);
    projection.publish(starting(), 2);
    assert_eq!(projection.snapshot().take, 1);
    assert_eq!(projection.snapshot().last_error, None);
    assert!(snapshots.has_changed().unwrap());
    assert_eq!(
        snapshots.borrow_and_update().clone(),
        projection.snapshot().clone()
    );

    projection.publish(recording(), 3);
    projection.publish(recording(), 4);
    assert_eq!(projection.snapshot().dictation.since_ms, Some(3));
    projection.publish(view(DictationState::Transcribing), 5);
    projection.publish(view(DictationState::Committing), 6);
    assert_eq!(projection.snapshot().dictation.since_ms, Some(5));

    projection.publish(view(DictationState::Idle), 7);
    projection.record_error("start failed".to_string(), 8);
    projection.publish(view(DictationState::Idle), 8);
    assert_eq!(
        projection.snapshot().last_error,
        Some(ErrorEntry {
            at_ms: 8,
            message: "start failed".to_string(),
        })
    );
    projection.publish(starting(), 9);
    assert_eq!(projection.snapshot().take, 2);
    assert_eq!(projection.snapshot().last_error, None);

    snapshots.borrow_and_update();
    projection.publish(starting(), 10);
    assert!(!snapshots.has_changed().unwrap());
}

#[test]
fn history_completes_in_queue_order_and_clear_preserves_pending() {
    let mut projection = projection();
    projection.publish(starting(), 1);
    let first = projection.queue("  original\ntext  ", 2);
    projection.complete(
        first,
        " polished\ntext\n".to_string(),
        Completion::Committed,
    );
    let pending = projection.queue("pending", 3);
    projection.clear_history();
    let next = projection.queue("next", 4);

    assert_eq!(first, Some(1));
    assert_eq!(pending, Some(2));
    assert_eq!(next, Some(3));
    assert_eq!(
        projection.snapshot().history,
        vec![
            HistoryEntry {
                id: 2,
                take: 1,
                at_ms: 3,
                original: "pending".to_string(),
                text: None,
                outcome: HistoryOutcome::Pending,
                truncated: false,
            },
            HistoryEntry {
                id: 3,
                take: 1,
                at_ms: 4,
                original: "next".to_string(),
                text: None,
                outcome: HistoryOutcome::Pending,
                truncated: false,
            },
        ]
    );
}

#[test]
fn a_completed_entry_contains_the_exact_selected_text() {
    let mut projection = projection();
    projection.publish(starting(), 1);
    let id = projection.queue("original", 2);
    projection.complete(
        id,
        " polished\n".to_string(),
        Completion::CommitFailed("input\nfailed".to_string()),
    );
    assert_eq!(
        projection.snapshot().history,
        vec![HistoryEntry {
            id: 1,
            take: 1,
            at_ms: 2,
            original: "original".to_string(),
            text: Some("polished".to_string()),
            outcome: HistoryOutcome::CommitFailed {
                error: "input failed".to_string(),
            },
            truncated: false,
        }]
    );
}

#[test]
fn blank_transcripts_and_unknown_reports_create_no_history() {
    let mut projection = projection();
    assert_eq!(projection.queue(" \n\t ", 1), None);
    projection.complete(None, String::new(), Completion::Committed);
    assert_eq!(projection.snapshot().history, Vec::new());
}

#[cfg(debug_assertions)]
#[test]
#[should_panic]
fn an_unknown_history_id_is_a_programming_error() {
    projection().complete(Some(999), "text".to_string(), Completion::Committed);
}

#[test]
fn completed_entries_are_bounded_without_evicting_pending() {
    let mut projection = projection();
    projection.publish(starting(), 1);
    for id in 1..=55 {
        projection.queue(&format!("original-{id}"), id);
        projection.complete(Some(id), format!("text-{id}"), Completion::Committed);
    }
    assert_eq!(projection.snapshot().history.len(), HISTORY_LEN);
    assert_eq!(
        projection.snapshot().history,
        (6..=55)
            .map(|id| entry(id, HistoryOutcome::Committed))
            .collect::<Vec<_>>()
    );

    let mut bounded = Projection::new(&config()).0;
    bounded.publish(starting(), 1);
    bounded.queue("pending", 1);
    let large = "x".repeat(FIELD_CAP - 100);
    for id in 2..=7 {
        bounded.queue(&large, id);
        bounded.complete(
            Some(id),
            large.clone(),
            Completion::CommitFailed(large.clone()),
        );
    }
    assert!(bounded.snapshot().history[0].outcome == HistoryOutcome::Pending);
    assert_eq!(
        bounded
            .snapshot()
            .history
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        vec![1, 3, 4, 5, 6, 7]
    );
}

#[test]
fn stored_fields_are_sanitized_and_capped_at_character_boundaries() {
    let mut projection = projection();
    projection.publish(starting(), 1);
    let id = projection.queue(&format!("{}\n", "é".repeat(FIELD_CAP)), 2);
    projection.complete(
        id,
        "x".repeat(FIELD_CAP + 1),
        Completion::CommitFailed("y".repeat(FIELD_CAP + 1)),
    );
    let entry = &projection.snapshot().history[0];
    assert_eq!(entry.original.len(), FIELD_CAP - 1);
    assert_eq!(entry.text.as_deref().unwrap().len(), FIELD_CAP);
    assert!(entry.original.ends_with('…'));
    assert!(entry.truncated);
    assert_eq!(
        capped("aéz", 3),
        ("…".to_string(), true),
        "the ellipsis itself stays within the byte cap"
    );
    assert_eq!(capped("hello", ERROR_CAP), ("hello".to_string(), false));
}

#[test]
fn the_worst_bounded_snapshot_fits_the_push_limit() {
    let mut projection = projection();
    projection.publish(starting(), 1);
    for _ in 0..17 {
        projection.queue(&"\"".repeat(FIELD_CAP), 2);
    }
    let field = "\"".repeat(HISTORY_BUDGET / (HISTORY_LEN * 3));
    for _ in 0..HISTORY_LEN {
        let id = projection.queue(&field, 3);
        projection.complete(id, field.clone(), Completion::CommitFailed(field.clone()));
    }
    projection.record_error("\"".repeat(ERROR_CAP + 1), 4);
    projection.snapshot.config = ConfigSummary {
        input_method: capped(&"\"".repeat(300), 256).0,
        provider_model: capped(&"\"".repeat(300), 256).0,
        provider_url: capped(&"\"".repeat(300), 256).0,
        polish_enabled: true,
    };

    let serialized = serde_json::to_vec(projection.snapshot()).unwrap();
    assert!(
        serialized.len() <= MAX_PUSH_BYTES,
        "{} bytes",
        serialized.len()
    );
}

#[test]
fn config_summary_has_no_keys_and_caps_display_fields() {
    let mut config = config();
    config.provider.model = "m".repeat(300);
    config.provider.url = format!("http://127.0.0.1/{}", "u".repeat(300));
    let summary = ConfigSummary::from_config(&config);
    assert_eq!(summary.provider_model.len(), 256);
    assert_eq!(summary.provider_url.len(), 256);
    assert!(!summary.provider_url.contains("API_KEY"));
}
