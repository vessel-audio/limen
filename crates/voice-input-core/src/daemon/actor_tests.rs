use super::input::{InputJob, InputReport};
use super::snapshot::HistoryOutcome;
use super::tests::{harness, spawn_loop, status};
use super::{DictationActor, ReplyTarget};
use crate::dictation::{Event, InputOutcome};
use tokio::sync::oneshot;

fn start_recording(actor: &mut DictationActor) {
    actor.dispatch(Event::Toggle, ReplyTarget::NotRequested);
    actor.dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
}

#[test]
fn complete_dispatches_publish_only_the_final_view() {
    let mut harness = harness();
    let snapshots = &mut harness._snapshots;
    snapshots.borrow_and_update();

    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::NotRequested);
    assert!(snapshots.has_changed().unwrap());
    assert_eq!(snapshots.borrow_and_update().take, 1);
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::NotRequested);
    assert!(!snapshots.has_changed().unwrap());

    start_recording(&mut harness.actor);
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::NotRequested);
    snapshots.borrow_and_update();
    harness
        .actor
        .dispatch(Event::ProviderCompleted, ReplyTarget::NotRequested);
    let snapshot = snapshots.borrow_and_update();
    assert!(matches!(
        snapshot.dictation.state,
        crate::dictation::DictationState::Idle
    ));
    assert_eq!(snapshot.take, 1);
}

#[test]
fn errors_clear_only_when_the_next_take_actually_starts() {
    let mut harness = harness();
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::NotRequested);
    harness.actor.dispatch(
        Event::SessionStartFailed("offline".to_string()),
        ReplyTarget::NotRequested,
    );
    assert_eq!(
        harness
            ._snapshots
            .borrow()
            .last_error
            .as_ref()
            .unwrap()
            .message,
        "offline"
    );
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::NotRequested);
    assert_eq!(harness._snapshots.borrow().take, 2);
    assert_eq!(harness._snapshots.borrow().last_error, None);
}

#[tokio::test]
async fn input_reports_complete_their_pending_history_entry() {
    let mut harness = harness();
    start_recording(&mut harness.actor);
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::NotRequested);
    harness.actor.dispatch(
        Event::Transcript("original".to_string()),
        ReplyTarget::NotRequested,
    );
    assert_eq!(
        harness._input.recv().await,
        Some(InputJob {
            entry: Some(1),
            text: "original".to_string(),
        })
    );
    assert!(matches!(
        harness._snapshots.borrow().history[0].outcome,
        HistoryOutcome::Pending
    ));

    let senders = spawn_loop(harness.actor);
    senders
        ._input_reports
        .send(InputReport {
            entry: Some(1),
            text: "polished".to_string(),
            outcome: InputOutcome::Succeeded,
        })
        .unwrap();
    assert_eq!(status(&senders).await, "finalizing");
    assert!(matches!(
        harness._snapshots.borrow().history[0].outcome,
        HistoryOutcome::Committed
    ));
    assert_eq!(
        harness._snapshots.borrow().history[0].text.as_deref(),
        Some("polished")
    );
}

#[test]
fn the_eighteenth_outstanding_job_is_dropped_without_becoming_pending() {
    let mut harness = harness();
    start_recording(&mut harness.actor);
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::NotRequested);
    for index in 0..18 {
        harness.actor.dispatch(
            Event::Transcript(format!("text-{index}")),
            ReplyTarget::NotRequested,
        );
    }

    let snapshot = harness._snapshots.borrow();
    assert_eq!(snapshot.history.len(), 18);
    assert!(
        snapshot.history[..17]
            .iter()
            .all(|entry| matches!(entry.outcome, HistoryOutcome::Pending))
    );
    assert!(matches!(
        snapshot.history[17].outcome,
        HistoryOutcome::Dropped
    ));
    assert_eq!(harness.actor.outstanding_input, 17);
    assert_eq!(
        std::iter::from_fn(|| harness._input.try_recv().ok()).count(),
        17
    );
}

#[test]
fn whitespace_jobs_have_no_history_entry() {
    let mut harness = harness();
    start_recording(&mut harness.actor);
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::NotRequested);
    harness.actor.dispatch(
        Event::Transcript(" \n\t ".to_string()),
        ReplyTarget::NotRequested,
    );
    assert!(harness._snapshots.borrow().history.is_empty());
    assert_eq!(
        harness._input.try_recv(),
        Ok(InputJob {
            entry: None,
            text: " \n\t ".to_string(),
        })
    );
}

#[test]
fn a_parked_legacy_toggle_caller_is_not_used_by_unrequested_events() {
    let mut harness = harness();
    let (reply, answer) = oneshot::channel();
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::Requested(reply));
    start_recording(&mut harness.actor);
    assert_eq!(answer.blocking_recv().unwrap(), "recording");
}
