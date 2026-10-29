//! Dictation actor tests.

use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use super::input::{InputJob, InputReport};
use super::projection::Projection;
use super::snapshot::StateSnapshot;
use super::{DictationActor, Mailboxes, ReplyTarget, effective_hold};
use crate::config::ShortcutConfig;
use crate::dictation::{Event, MAX_HOLD};
use crate::ipc::{self, v1::Reply};
use crate::session::{SessionCommand, SessionHandle};
use crate::shortcut::portal::{Bus, ShortcutHandle};
use crate::shortcut::status::{PerAction, ShortcutStatus};
use crate::shortcut::{Activation, ActivationReceipt, Backend, HoldEdge};

const TAP: Duration = Duration::from_millis(100);
const KEPT: Duration = Duration::from_secs(1);

/// A dictation actor wired to channels the test holds.
pub(super) struct Harness {
    pub(super) actor: DictationActor,
    /// Everything the actor told the session actor.
    pub(super) session: mpsc::UnboundedReceiver<SessionCommand>,
    pub(super) _input: mpsc::UnboundedReceiver<InputJob>,
    pub(super) _snapshots: watch::Receiver<StateSnapshot>,
}

fn test_projection() -> (Projection, watch::Receiver<StateSnapshot>) {
    let config = toml::from_str(
        r#"
        [provider]
        url = "http://127.0.0.1:8000/v1/audio/transcriptions"
        model = "test-model"
        "#,
    )
    .unwrap();
    Projection::new(&config)
}

pub(super) fn harness() -> Harness {
    let (mailbox, session) = mpsc::unbounded_channel();
    let (input_tx, input) = mpsc::unbounded_channel();
    let (projection, snapshots) = test_projection();
    Harness {
        actor: DictationActor::new(
            SessionHandle::from_mailbox(mailbox),
            input_tx,
            |_, _| {},
            PathBuf::from("/nonexistent/config.toml"),
            ShortcutHandle::detached(),
            projection,
        ),
        session,
        _input: input,
        _snapshots: snapshots,
    }
}

/// The senders that keep a running actor's mailboxes open.
pub(super) struct Senders {
    pub(super) commands: mpsc::Sender<ipc::Command>,
    pub(super) _input_reports: mpsc::UnboundedSender<InputReport>,
    pub(super) session_reports: mpsc::UnboundedSender<Event>,
    pub(super) activations: mpsc::UnboundedSender<ActivationReceipt>,
}

pub(super) fn mailboxes() -> (Mailboxes, Senders) {
    let (commands_tx, commands) = mpsc::channel(8);
    let (input_reports_tx, input_reports) = mpsc::unbounded_channel();
    let (session_reports_tx, session_reports) = mpsc::unbounded_channel();
    let (activations_tx, activations) = mpsc::unbounded_channel();
    (
        Mailboxes {
            commands,
            input_reports,
            session_reports,
            activations,
        },
        Senders {
            commands: commands_tx,
            _input_reports: input_reports_tx,
            session_reports: session_reports_tx,
            activations: activations_tx,
        },
    )
}

/// Run the actor's message loop in the background.
pub(super) fn spawn_loop(actor: DictationActor) -> Senders {
    let (mailboxes, senders) = mailboxes();
    tokio::spawn(actor.run(mailboxes));
    senders
}

/// Send a hold key edge over IPC to a running loop and return its answer.
async fn ipc_hold(senders: &Senders, edge: HoldEdge) -> Reply {
    let (reply, answer) = oneshot::channel();
    senders
        .commands
        .send(ipc::Command::Hold(edge, reply))
        .await
        .unwrap();
    answer.await.unwrap()
}

fn sent(session: &mut mpsc::UnboundedReceiver<SessionCommand>) -> Vec<SessionCommand> {
    std::iter::from_fn(|| session.try_recv().ok()).collect()
}

/// Ask a running loop for its status: once answered, the loop has handled
/// everything that was due before.
pub(super) async fn status(senders: &Senders) -> String {
    let (reply, answer) = oneshot::channel();
    senders
        .commands
        .send(ipc::Command::Status(reply))
        .await
        .unwrap();
    answer.await.unwrap()
}

/// Press the hold key and let its take start.
fn start_hold(actor: &mut DictationActor) {
    actor.hold(HoldEdge::Press);
    actor.dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
}

#[test]
fn effective_hold_counts_from_the_start_of_the_recording() {
    let started = Instant::now();
    let released = started + Duration::from_millis(450);
    assert_eq!(
        [
            effective_hold(Some(started), released),
            effective_hold(None, released)
        ],
        [Duration::from_millis(450), Duration::ZERO]
    );
}

#[tokio::test(start_paused = true)]
async fn accidental_tap_tears_the_take_down_without_stopping_it() {
    let mut harness = harness();
    start_hold(&mut harness.actor);
    tokio::time::advance(TAP).await;
    harness.actor.hold(HoldEdge::Release);
    assert_eq!(
        sent(&mut harness.session),
        [SessionCommand::Start, SessionCommand::Teardown]
    );
}

#[tokio::test(start_paused = true)]
async fn kept_hold_stops_the_take() {
    let mut harness = harness();
    start_hold(&mut harness.actor);
    tokio::time::advance(KEPT).await;
    harness.actor.hold(HoldEdge::Release);
    assert_eq!(
        sent(&mut harness.session),
        [SessionCommand::Start, SessionCommand::Stop]
    );
}

#[tokio::test(start_paused = true)]
async fn press_right_after_a_tap_starts_after_the_teardown() {
    let mut harness = harness();
    start_hold(&mut harness.actor);
    tokio::time::advance(TAP).await;
    harness.actor.hold(HoldEdge::Release);
    harness.actor.hold(HoldEdge::Press);
    assert_eq!(
        sent(&mut harness.session),
        [
            SessionCommand::Start,
            SessionCommand::Teardown,
            SessionCommand::Start
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn release_outside_a_hold_recording_sends_nothing() {
    let mut harness = harness();
    harness.actor.hold(HoldEdge::Release);
    assert_eq!(sent(&mut harness.session), []);

    let (reply, _answer) = oneshot::channel();
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::Requested(reply));
    harness
        .actor
        .dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
    tokio::time::advance(KEPT).await;
    harness.actor.hold(HoldEdge::Release);
    assert_eq!(sent(&mut harness.session), [SessionCommand::Start]);
}

#[tokio::test(start_paused = true)]
async fn hold_deadline_stops_a_take_whose_release_is_lost() {
    let mut harness = harness();
    start_hold(&mut harness.actor);
    let _senders = spawn_loop(harness.actor);
    let started = Instant::now();
    assert_eq!(
        [harness.session.recv().await, harness.session.recv().await],
        [Some(SessionCommand::Start), Some(SessionCommand::Stop)]
    );
    assert_eq!(started.elapsed(), MAX_HOLD);
}

#[tokio::test(start_paused = true)]
async fn disarmed_hold_deadline_never_fires() {
    let mut harness = harness();
    start_hold(&mut harness.actor);
    tokio::time::advance(KEPT).await;
    harness.actor.hold(HoldEdge::Release);
    let senders = spawn_loop(harness.actor);
    tokio::time::advance(MAX_HOLD * 2).await;
    assert_eq!(status(&senders).await, "finalizing");
    assert_eq!(
        sent(&mut harness.session),
        [SessionCommand::Start, SessionCommand::Stop]
    );
}

#[tokio::test(start_paused = true)]
async fn release_and_timeout_due_together_stop_the_take_once() {
    // The release is handled first: it disarms the deadline that is due.
    let mut released_first = harness();
    start_hold(&mut released_first.actor);
    tokio::time::advance(MAX_HOLD).await;
    released_first.actor.hold(HoldEdge::Release);
    let senders = spawn_loop(released_first.actor);
    assert_eq!(status(&senders).await, "finalizing");
    assert_eq!(
        sent(&mut released_first.session),
        [SessionCommand::Start, SessionCommand::Stop]
    );

    // The timeout is handled first: the release that follows is ignored.
    let mut timed_out_first = harness();
    start_hold(&mut timed_out_first.actor);
    tokio::time::advance(MAX_HOLD).await;
    timed_out_first
        .actor
        .dispatch(Event::HoldTimedOut, ReplyTarget::NotRequested);
    timed_out_first.actor.hold(HoldEdge::Release);
    assert_eq!(
        sent(&mut timed_out_first.session),
        [SessionCommand::Start, SessionCommand::Stop]
    );
}

#[tokio::test]
async fn hold_requests_leave_a_waiting_toggle_caller_its_own_answer() {
    let reports = [
        (Event::SessionStarted, "recording"),
        (
            Event::SessionStartFailed("offline".to_string()),
            "error: offline",
        ),
    ];
    for (report, toggle_answer) in reports {
        let mut harness = harness();
        let senders = spawn_loop(harness.actor);
        let (reply, answer) = oneshot::channel();
        senders
            .commands
            .send(ipc::Command::Toggle(reply))
            .await
            .unwrap();
        assert_eq!(harness.session.recv().await, Some(SessionCommand::Start));
        assert_eq!(
            [
                ipc_hold(&senders, HoldEdge::Press).await,
                ipc_hold(&senders, HoldEdge::Release).await
            ],
            [Reply::Ok, Reply::Ok]
        );
        senders.session_reports.send(report).unwrap();
        assert_eq!(answer.await.unwrap(), toggle_answer);
    }
}

#[tokio::test(start_paused = true)]
async fn release_and_timeout_ready_in_one_iteration_stop_the_take_once() {
    // select! polls ready branches in random order: repeat to see both.
    for _ in 0..16 {
        let mut harness = harness();
        start_hold(&mut harness.actor);
        let (mailboxes, senders) = mailboxes();
        let (reply, answer) = oneshot::channel();
        senders
            .commands
            .try_send(ipc::Command::Hold(HoldEdge::Release, reply))
            .unwrap();
        tokio::time::advance(MAX_HOLD).await;
        tokio::spawn(harness.actor.run(mailboxes));
        assert_eq!(answer.await.unwrap(), Reply::Ok);
        assert_eq!(status(&senders).await, "finalizing");
        assert_eq!(
            sent(&mut harness.session),
            [SessionCommand::Start, SessionCommand::Stop]
        );
    }
}

#[tokio::test]
async fn key_repeat_held_across_the_end_of_a_take_starts_nothing() {
    let mut harness = harness();
    let (start_reply, _start_answer) = oneshot::channel();
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::Requested(start_reply));
    harness
        .actor
        .dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
    let (stop_reply, _stop_answer) = oneshot::channel();
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::Requested(stop_reply));
    // Pressed while the take finalizes, still held once it has finished.
    harness.actor.hold(HoldEdge::Press);
    harness
        .actor
        .dispatch(Event::ProviderCompleted, ReplyTarget::NotRequested);
    harness.actor.hold(HoldEdge::Press);
    harness.actor.hold(HoldEdge::Release);
    assert_eq!(
        sent(&mut harness.session),
        [
            SessionCommand::Start,
            SessionCommand::Stop,
            SessionCommand::Teardown
        ]
    );

    harness.actor.hold(HoldEdge::Press);
    assert_eq!(sent(&mut harness.session), [SessionCommand::Start]);
}

#[tokio::test]
async fn a_shortcut_toggle_leaves_a_waiting_ipc_caller_its_own_answer() {
    let mut harness = harness();
    let (reply, answer) = oneshot::channel();
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::Requested(reply));
    // A shortcut toggle while the take starts is answered "starting", for nobody.
    harness
        .actor
        .dispatch(Event::Toggle, ReplyTarget::NotRequested);
    harness
        .actor
        .dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
    assert_eq!(answer.await.unwrap(), "recording");
    assert_eq!(sent(&mut harness.session), [SessionCommand::Start]);
}

#[tokio::test(start_paused = true)]
async fn shortcut_hold_edges_drive_a_hold_take() {
    let mut harness = harness();
    let senders = spawn_loop(harness.actor);
    senders
        .activations
        .send(ActivationReceipt {
            activation: Activation::Hold(HoldEdge::Press),
            received_at: Instant::now(),
        })
        .unwrap();
    assert_eq!(harness.session.recv().await, Some(SessionCommand::Start));
    senders.session_reports.send(Event::SessionStarted).unwrap();
    // The report and the status query travel through different mailboxes.
    while status(&senders).await != "recording" {
        tokio::task::yield_now().await;
    }
    tokio::time::advance(KEPT).await;
    senders
        .activations
        .send(ActivationReceipt {
            activation: Activation::Hold(HoldEdge::Release),
            received_at: Instant::now(),
        })
        .unwrap();
    assert_eq!(harness.session.recv().await, Some(SessionCommand::Stop));
}

#[tokio::test]
async fn shortcut_rebind_applies_only_a_valid_config() {
    let dir = std::env::temp_dir().join(format!("voice-input-rebind-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let config_file = dir.join("config.toml");
    let base =
        "[provider]\nurl = \"http://127.0.0.1:8000/v1/audio/transcriptions\"\nmodel = \"m\"\n";
    let (activations, _activations) = mpsc::unbounded_channel();
    let shortcuts = ShortcutHandle::spawn(
        ShortcutConfig::default(),
        Bus::Session,
        activations,
        |_, _| {},
    );
    let before = shortcuts.status();
    let (mailbox, _session) = mpsc::unbounded_channel();
    let (input_tx, _input) = mpsc::unbounded_channel();
    let (projection, _snapshots) = test_projection();
    let actor = DictationActor::new(
        SessionHandle::from_mailbox(mailbox),
        input_tx,
        |_, _| {},
        config_file.clone(),
        shortcuts,
        projection,
    );
    let senders = spawn_loop(actor);
    let rebind = || async {
        let (reply, answer) = oneshot::channel();
        senders
            .commands
            .send(ipc::Command::ShortcutRebind(reply))
            .await
            .unwrap();
        answer.await.unwrap()
    };
    let status = || async {
        let (reply, answer) = oneshot::channel();
        senders
            .commands
            .send(ipc::Command::ShortcutStatus(reply))
            .await
            .unwrap();
        answer.await.unwrap()
    };

    std::fs::write(&config_file, format!("{base}[audio]\nchunk_ms = 0\n")).unwrap();
    assert!(matches!(
        rebind().await,
        Reply::Error { message } if message.contains("audio.chunk_ms")
    ));
    assert_eq!(status().await, Reply::ShortcutStatus(before.clone()));

    std::fs::write(
        &config_file,
        format!("{base}[shortcut]\nbackend = \"gnome-keybinding\"\ntoggle = \"Ctrl+Alt+V\"\n"),
    )
    .unwrap();
    assert_eq!(rebind().await, Reply::Ok);
    assert_eq!(
        status().await,
        Reply::ShortcutStatus(ShortcutStatus {
            backend: Backend::GnomeKeybinding,
            preferred: PerAction {
                toggle: "Ctrl+Alt+V".parse().unwrap(),
                hold: before.preferred.hold,
            },
            ..before
        })
    );
}

#[tokio::test(start_paused = true)]
async fn release_queued_before_the_start_report_stays_a_tap() {
    let mut harness = harness();
    harness.actor.hold(HoldEdge::Press);
    tokio::time::advance(Duration::from_millis(100)).await;
    let (mailboxes, senders) = mailboxes();
    // The release is already queued when the start report is handled.
    senders
        .activations
        .send(ActivationReceipt {
            activation: Activation::Hold(HoldEdge::Release),
            received_at: Instant::now(),
        })
        .unwrap();
    harness
        .actor
        .dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
    // Scheduling delays the queued release beyond MIN_HOLD.
    tokio::time::advance(Duration::from_millis(400)).await;
    tokio::spawn(harness.actor.run(mailboxes));
    assert_eq!(
        [harness.session.recv().await, harness.session.recv().await],
        [Some(SessionCommand::Start), Some(SessionCommand::Teardown)]
    );
}

#[tokio::test(start_paused = true)]
async fn after_a_timeout_only_a_real_release_reenables_holding() {
    let mut harness = harness();
    start_hold(&mut harness.actor);
    let senders = spawn_loop(harness.actor);
    // The actual deadline branch stops the take.
    assert_eq!(
        [harness.session.recv().await, harness.session.recv().await],
        [Some(SessionCommand::Start), Some(SessionCommand::Stop)]
    );
    senders
        .session_reports
        .send(Event::ProviderCompleted)
        .unwrap();
    assert_eq!(harness.session.recv().await, Some(SessionCommand::Teardown));

    // A repeated or fresh press cannot be distinguished safely after the
    // release was lost, so it must not start another take.
    senders
        .activations
        .send(ActivationReceipt {
            activation: Activation::Hold(HoldEdge::Press),
            received_at: Instant::now(),
        })
        .unwrap();
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(sent(&mut harness.session), []);

    senders
        .activations
        .send(ActivationReceipt {
            activation: Activation::Hold(HoldEdge::Release),
            received_at: Instant::now(),
        })
        .unwrap();
    senders
        .activations
        .send(ActivationReceipt {
            activation: Activation::Hold(HoldEdge::Press),
            received_at: Instant::now(),
        })
        .unwrap();
    assert_eq!(harness.session.recv().await, Some(SessionCommand::Start));
}
