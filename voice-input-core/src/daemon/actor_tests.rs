use super::input::{InputJob, InputReport};
use super::projection::Completion;
use super::snapshot::HistoryOutcome;
use super::tests::{Senders, harness, mailboxes, spawn_loop, status};
use super::{DictationActor, ReplyTarget};
use crate::dictation::{Event, InputOutcome};
use crate::ipc::{
    self,
    v1::{Delivery, Reply},
};
use tokio::sync::oneshot;

async fn wait_for_status(senders: &Senders, expected: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while status(senders).await != expected {}
    })
    .await
    .unwrap();
}

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
            delivery: Delivery::Input,
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
            delivery: Delivery::Input,
            text: "polished".to_string(),
            outcome: InputOutcome::Succeeded,
        })
        .unwrap();
    wait_for_status(&senders, "finalizing").await;
    assert!(matches!(
        harness._snapshots.borrow().history[0].outcome,
        HistoryOutcome::Committed
    ));
    assert_eq!(
        harness._snapshots.borrow().history[0].text.as_deref(),
        Some("polished")
    );
}

#[tokio::test]
async fn unpolled_reports_keep_the_outstanding_limit_in_force() {
    let mut harness = harness();
    let (mailboxes, senders) = mailboxes();
    start_recording(&mut harness.actor);
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::NotRequested);
    for index in 0..17 {
        harness.actor.dispatch(
            Event::Transcript(format!("text-{index}")),
            ReplyTarget::NotRequested,
        );
        let job = harness._input.recv().await.unwrap();
        senders
            ._input_reports
            .send(InputReport {
                entry: job.entry,
                delivery: job.delivery,
                text: job.text,
                outcome: InputOutcome::Succeeded,
            })
            .unwrap();
    }
    harness.actor.dispatch(
        Event::Transcript("dropped".to_string()),
        ReplyTarget::NotRequested,
    );

    let snapshot = harness._snapshots.borrow().clone();
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
    assert!(harness._input.try_recv().is_err());

    tokio::spawn(harness.actor.run(mailboxes));
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while harness._snapshots.borrow().history[..17]
            .iter()
            .any(|entry| matches!(entry.outcome, HistoryOutcome::Pending))
        {
            status(&senders).await;
        }
    })
    .await
    .unwrap();
    senders
        .session_reports
        .send(Event::Transcript("admitted".to_string()))
        .unwrap();
    wait_for_status(&senders, "finalizing").await;
    let admitted = harness._input.recv().await.unwrap();
    assert_eq!(admitted.entry, Some(19));
    assert!(matches!(
        harness._snapshots.borrow().history[18].outcome,
        HistoryOutcome::Pending
    ));
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
            delivery: Delivery::Input,
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

#[test]
fn intents_validate_state_and_return_stable_take_ids() {
    let mut harness = harness();
    assert_eq!(
        harness.actor.stop(Delivery::Keep),
        Reply::Error {
            message: "not recording".to_string(),
        }
    );
    assert_eq!(harness.actor.start(), Reply::Accepted { take: 1 });
    assert_eq!(
        harness.actor.start(),
        Reply::Error {
            message: "already recording".to_string(),
        }
    );
    start_recording(&mut harness.actor);
    assert_eq!(
        harness.actor.start(),
        Reply::Error {
            message: "already recording".to_string(),
        }
    );
    assert_eq!(
        harness.actor.stop(Delivery::Keep),
        Reply::Accepted { take: 1 }
    );
    assert_eq!(harness.actor.start(), Reply::Accepted { take: 2 });
    assert_eq!(harness.actor.start(), Reply::Accepted { take: 2 });
    harness.actor.dispatch(
        Event::Transcript("first".to_string()),
        ReplyTarget::NotRequested,
    );
    harness
        .actor
        .dispatch(Event::ProviderCompleted, ReplyTarget::NotRequested);
    assert_eq!(
        harness.actor.stop(Delivery::Input),
        Reply::Error {
            message: "not recording".to_string(),
        }
    );
}

#[tokio::test]
async fn intents_answer_immediately_without_using_the_legacy_start_caller() {
    let mut harness = harness();
    let (reply, answer) = oneshot::channel();
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::Requested(reply));
    assert_eq!(
        harness.actor.start(),
        Reply::Error {
            message: "already recording".to_string(),
        }
    );
    assert_eq!(
        harness.actor.stop(Delivery::Keep),
        Reply::Error {
            message: "not recording".to_string(),
        }
    );
    assert!(harness.actor.start_caller.is_some());
    harness
        .actor
        .dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
    assert_eq!(answer.await.unwrap(), "recording");
}

#[tokio::test]
async fn keep_does_not_leak_into_the_next_shortcut_stopped_take() {
    let mut harness = harness();
    assert_eq!(harness.actor.start(), Reply::Accepted { take: 1 });
    harness
        .actor
        .dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
    assert_eq!(
        harness.actor.stop(Delivery::Keep),
        Reply::Accepted { take: 1 }
    );
    assert_eq!(harness.actor.start(), Reply::Accepted { take: 2 });
    harness.actor.dispatch(
        Event::Transcript("first".to_string()),
        ReplyTarget::NotRequested,
    );
    assert_eq!(
        harness._input.recv().await,
        Some(InputJob {
            entry: Some(1),
            delivery: Delivery::Keep,
            text: "first".to_string(),
        })
    );
    harness
        .actor
        .dispatch(Event::ProviderCompleted, ReplyTarget::NotRequested);

    let senders = spawn_loop(harness.actor);
    senders
        ._input_reports
        .send(InputReport {
            entry: Some(1),
            delivery: Delivery::Keep,
            text: "first kept".to_string(),
            outcome: InputOutcome::Succeeded,
        })
        .unwrap();
    wait_for_status(&senders, "idle").await;
    senders.session_reports.send(Event::SessionStarted).unwrap();
    wait_for_status(&senders, "recording").await;
    senders
        .activations
        .send(crate::shortcut::ActivationReceipt {
            activation: crate::shortcut::Activation::Toggle,
            received_at: tokio::time::Instant::now(),
        })
        .unwrap();
    wait_for_status(&senders, "finalizing").await;
    senders
        .session_reports
        .send(Event::Transcript("second".to_string()))
        .unwrap();
    wait_for_status(&senders, "finalizing").await;
    assert_eq!(
        harness._input.recv().await,
        Some(InputJob {
            entry: Some(2),
            delivery: Delivery::Input,
            text: "second".to_string(),
        })
    );
    senders
        ._input_reports
        .send(InputReport {
            entry: Some(2),
            delivery: Delivery::Input,
            text: "second committed".to_string(),
            outcome: InputOutcome::Succeeded,
        })
        .unwrap();
    senders
        .session_reports
        .send(Event::ProviderCompleted)
        .unwrap();
    wait_for_status(&senders, "idle").await;
    assert!(matches!(
        harness._snapshots.borrow().history[0].outcome,
        HistoryOutcome::Kept
    ));
    assert!(matches!(
        harness._snapshots.borrow().history[1].outcome,
        HistoryOutcome::Committed
    ));
}

#[tokio::test]
async fn clear_history_removes_completed_entries_and_replies_ok() {
    let mut harness = harness();
    start_recording(&mut harness.actor);
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::NotRequested);
    harness.actor.dispatch(
        Event::Transcript("first".to_string()),
        ReplyTarget::NotRequested,
    );
    harness
        .actor
        .projection
        .complete(Some(1), "first".to_string(), Completion::Committed);
    harness.actor.projection.queue("pending", 2);
    let senders = spawn_loop(harness.actor);
    let (reply, answer) = oneshot::channel();
    senders
        .commands
        .send(ipc::Command::ClearHistory(reply))
        .await
        .unwrap();

    assert_eq!(answer.await.unwrap(), Reply::Ok);
    let history = &harness._snapshots.borrow().history;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, 2);
    assert!(matches!(history[0].outcome, HistoryOutcome::Pending));
}
