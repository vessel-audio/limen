//! The resident daemon: translate I/O into events, apply pure dictation
//! transitions, and interpret the resulting effects.
//!
//! Actor model: the dictation loop in [`run`] is an actor that alone owns the
//! `Dictation` state. It handles one message at a time from its mailboxes —
//! IPC commands, session actor reports, text input outcomes, and the finalize
//! and hold deadlines — and never awaits slow work itself. Effects become messages to
//! the actors that own the slow resources:
//! - the session actor (`session`) owns microphone capture and the ASR session;
//! - the text input worker owns the input method and commits final text.
//!
//! Their outcomes come back as events, so the loop stays responsive to IPC
//! even while pw-record is being reaped or a take is being finalized.

use std::collections::VecDeque;
use std::pin::Pin;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

use crate::config::{Config, Paths};
use crate::dictation::{self, Dictation, Effect, Event, InputOutcome, Notification};
use crate::input_method::InputMethod;
use crate::ipc;
use crate::notify::notify;
use crate::polish::Polisher;
use crate::session::{SessionCommand, SessionHandle};
use crate::shortcut::HoldEdge;

/// Give up on a finalize after this long without a terminal event.
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(10);

type Deadline = Pin<Box<tokio::time::Sleep>>;

/// State of the dictation actor: business state plus the addresses of the
/// actors that perform its effects.
struct DictationActor {
    dictation: Dictation,
    session: SessionHandle,
    /// Mailbox of the text input worker.
    input_tx: mpsc::Sender<String>,
    finalize_deadline: Option<Deadline>,
    /// Stops a hold recording whose release never arrived.
    hold_deadline: Option<Deadline>,
    /// When the running hold recording started; how long it lasted decides
    /// whether its release keeps the take.
    hold_started: Option<Instant>,
    /// When the hold key went down, to log how long it was held.
    hold_pressed: Option<Instant>,
    /// IPC caller of a shortcut start, answered once the session actor reports
    /// whether the take started.
    start_caller: Option<oneshot::Sender<String>>,
    /// Shows a desktop notification: `(summary, body)`.
    notify: fn(&str, &str),
}

/// Whether the message being handled came from an IPC caller awaiting a reply.
enum ReplyTarget {
    NotRequested,
    Requested(oneshot::Sender<String>),
}

/// The dictation actor's mailboxes; its loop ends when one of them closes.
struct Mailboxes {
    commands: mpsc::Receiver<ipc::Command>,
    input_reports: mpsc::UnboundedReceiver<InputOutcome>,
    session_reports: mpsc::UnboundedReceiver<Event>,
}

impl DictationActor {
    fn new(session: SessionHandle, input_tx: mpsc::Sender<String>, notify: fn(&str, &str)) -> Self {
        Self {
            dictation: Dictation::default(),
            session,
            input_tx,
            finalize_deadline: None,
            hold_deadline: None,
            hold_started: None,
            hold_pressed: None,
            start_caller: None,
            notify,
        }
    }

    /// Handle one message: apply its event and drain any follow-up events
    /// produced by its effects. Never awaits; slow effects are messages to
    /// other actors.
    fn dispatch(&mut self, event: Event, mut reply: ReplyTarget) {
        let mut events = VecDeque::from([event]);
        while let Some(event) = events.pop_front() {
            let transition = dictation::transition(self.dictation, event);
            self.dictation = transition.state;
            // Only the message itself can come from an IPC caller.
            let caller = std::mem::replace(&mut reply, ReplyTarget::NotRequested);
            match (transition.reply, caller) {
                (Some(message), ReplyTarget::Requested(caller)) => {
                    let _ = caller.send(message);
                }
                (Some(message), ReplyTarget::NotRequested) => match self.start_caller.take() {
                    Some(caller) => {
                        let _ = caller.send(message);
                    }
                    None => tracing::warn!(message, "transition reply had no IPC target"),
                },
                // Only a shortcut start defers its answer: the core replies once
                // the session actor reports whether the take started.
                (None, ReplyTarget::Requested(caller)) => {
                    self.start_caller = Some(caller);
                }
                (None, ReplyTarget::NotRequested) => {}
            }

            for effect in transition.effects {
                match effect {
                    Effect::StartSession => self.session.tell(SessionCommand::Start),
                    Effect::StopSession => self.session.tell(SessionCommand::Stop),
                    Effect::TeardownSession => self.session.tell(SessionCommand::Teardown),
                    Effect::QueueInput(text) => match self.input_tx.try_send(text) {
                        Ok(()) => events.push_back(Event::InputQueued),
                        Err(error) => {
                            tracing::warn!(error = %error, "text input queue full or dead");
                            events.push_back(Event::InputDropped);
                        }
                    },
                    Effect::ArmFinalizeDeadline => {
                        self.finalize_deadline =
                            Some(Box::pin(tokio::time::sleep(FINALIZE_TIMEOUT)));
                    }
                    Effect::DisarmFinalizeDeadline => {
                        self.finalize_deadline = None;
                    }
                    Effect::ArmHoldDeadline => {
                        self.hold_started = Some(Instant::now());
                        self.hold_deadline =
                            Some(Box::pin(tokio::time::sleep(dictation::MAX_HOLD)));
                    }
                    Effect::DisarmHoldDeadline => {
                        self.hold_started = None;
                        self.hold_deadline = None;
                    }
                    Effect::Notify(Notification::Info(message)) => {
                        (self.notify)("voice-input", &message);
                    }
                    Effect::Notify(Notification::Error(message)) => {
                        (self.notify)("voice-input error", &message);
                    }
                }
            }
        }
    }

    /// Dispatch an edge of the hold-to-talk key. Nobody waits for the reply.
    fn hold(&mut self, edge: HoldEdge) {
        let now = Instant::now();
        let event = match edge {
            HoldEdge::Press => {
                // Key repeat must not restart the measurement.
                self.hold_pressed.get_or_insert(now);
                Event::HoldPressed
            }
            HoldEdge::Release => {
                let effective = effective_hold(self.hold_started, now);
                let held = self
                    .hold_pressed
                    .take()
                    .map(|pressed| now.saturating_duration_since(pressed));
                tracing::info!(?effective, ?held, "hold key released");
                Event::HoldReleased { effective }
            }
        };
        self.dispatch(event, ReplyTarget::NotRequested);
    }

    /// Message loop: handle one message at a time until a mailbox closes.
    async fn run(mut self, mut mailboxes: Mailboxes) -> Result<()> {
        loop {
            tokio::select! {
                cmd = mailboxes.commands.recv() => {
                    let Some(cmd) = cmd else { anyhow::bail!("ipc accept loop died") };
                    match cmd {
                        ipc::Command::Status(reply) => {
                            let _ = reply.send(self.dictation.label().to_string());
                        }
                        ipc::Command::Toggle(reply) => {
                            self.dispatch(Event::Toggle, ReplyTarget::Requested(reply));
                        }
                        ipc::Command::Hold(edge, reply) => {
                            // Answered at once: an edge never waits for the take to start.
                            let _ = reply.send(ipc::v1::Reply::Ok);
                            self.hold(edge);
                        }
                    }
                }

                outcome = mailboxes.input_reports.recv() => {
                    match outcome {
                        Some(outcome) => {
                            self.dispatch(Event::InputFinished(outcome), ReplyTarget::NotRequested);
                        }
                        None => anyhow::bail!("text input worker stopped unexpectedly"),
                    }
                }

                event = mailboxes.session_reports.recv() => {
                    match event {
                        Some(event) => self.dispatch(event, ReplyTarget::NotRequested),
                        None => anyhow::bail!("session actor stopped unexpectedly"),
                    }
                }

                _ = async { self.finalize_deadline.as_mut().unwrap().await },
                        if self.finalize_deadline.is_some() => {
                    tracing::warn!("finalize timed out");
                    self.dispatch(Event::FinalizeTimedOut, ReplyTarget::NotRequested);
                }

                _ = async { self.hold_deadline.as_mut().unwrap().await },
                        if self.hold_deadline.is_some() => {
                    tracing::warn!("hold exceeded its maximum duration");
                    self.dispatch(Event::HoldTimedOut, ReplyTarget::NotRequested);
                }
            }
        }
    }
}

/// How long the hold recording has lasted when the key is released at
/// `released`; zero when no hold recording is running.
fn effective_hold(started: Option<Instant>, released: Instant) -> Duration {
    started.map_or(Duration::ZERO, |started| {
        released.saturating_duration_since(started)
    })
}

pub async fn run(paths: Paths, cfg: Config) -> Result<()> {
    let listener = ipc::bind(&paths)?;
    let (cmd_tx, commands) = mpsc::channel::<ipc::Command>(8);
    ipc::spawn_accept_loop(listener, cmd_tx);

    // Text input worker: an actor that owns the input method and commits one
    // queued text at a time, so slow polishing or text input never stalls the
    // dictation loop.
    let (input_tx, mut input_rx) = mpsc::channel::<String>(16);
    let (input_report_tx, input_reports) = mpsc::unbounded_channel::<InputOutcome>();
    let backend = crate::input_method::default_backend(cfg.input_method.backend);
    let polisher = Polisher::from_config(&cfg.polish)?;
    tracing::info!(?backend, "input method backend");
    tokio::spawn(async move {
        let mut input_method = InputMethod::new(backend);
        while let Some(text) = input_rx.recv().await {
            let original = text;
            // Cloud work is optional; queued originals take priority.
            let corrected = polisher
                .polish_when_idle(original.clone(), input_rx.is_empty())
                .await;
            let text = input_method
                .compatible_text(&original, &corrected)
                .to_string();
            let outcome = match input_method.commit_text(&text).await {
                Ok(()) => InputOutcome::Succeeded,
                Err(error) => {
                    tracing::warn!(error = %format!("{error:#}"), "text input failed");
                    InputOutcome::Failed(format!("{error:#}"))
                }
            };
            if input_report_tx.send(outcome).is_err() {
                break;
            }
        }
    });

    // The session actor holds the only sender, so this mailbox closes exactly
    // when the actor stops.
    let (session_report_tx, session_reports) = mpsc::unbounded_channel::<Event>();
    let actor = DictationActor::new(
        SessionHandle::spawn(cfg, session_report_tx),
        input_tx,
        notify,
    );

    notify("voice-input", "daemon started");
    tracing::info!(socket = %paths.socket.display(), "daemon listening");

    actor
        .run(Mailboxes {
            commands,
            input_reports,
            session_reports,
        })
        .await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::{mpsc, oneshot};
    use tokio::time::Instant;

    use super::{DictationActor, Mailboxes, ReplyTarget, effective_hold};
    use crate::dictation::{Event, InputOutcome, MAX_HOLD};
    use crate::ipc::{self, v1::Reply};
    use crate::session::{SessionCommand, SessionHandle};
    use crate::shortcut::HoldEdge;

    const TAP: Duration = Duration::from_millis(100);
    const KEPT: Duration = Duration::from_secs(1);

    /// A dictation actor wired to channels the test holds.
    struct Harness {
        actor: DictationActor,
        /// Everything the actor told the session actor.
        session: mpsc::UnboundedReceiver<SessionCommand>,
        _input: mpsc::Receiver<String>,
    }

    fn harness() -> Harness {
        let (mailbox, session) = mpsc::unbounded_channel();
        let (input_tx, input) = mpsc::channel(16);
        Harness {
            actor: DictationActor::new(SessionHandle::from_mailbox(mailbox), input_tx, |_, _| {}),
            session,
            _input: input,
        }
    }

    /// The senders that keep a running actor's mailboxes open.
    struct Senders {
        commands: mpsc::Sender<ipc::Command>,
        _input_reports: mpsc::UnboundedSender<InputOutcome>,
        session_reports: mpsc::UnboundedSender<Event>,
    }

    fn mailboxes() -> (Mailboxes, Senders) {
        let (commands_tx, commands) = mpsc::channel(8);
        let (input_reports_tx, input_reports) = mpsc::unbounded_channel();
        let (session_reports_tx, session_reports) = mpsc::unbounded_channel();
        (
            Mailboxes {
                commands,
                input_reports,
                session_reports,
            },
            Senders {
                commands: commands_tx,
                _input_reports: input_reports_tx,
                session_reports: session_reports_tx,
            },
        )
    }

    /// Run the actor's message loop in the background.
    fn spawn_loop(actor: DictationActor) -> Senders {
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
    async fn status(senders: &Senders) -> String {
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

    #[tokio::test]
    async fn hold_edges_never_park_as_the_start_caller() {
        let mut harness = harness();
        harness.actor.hold(HoldEdge::Press);
        assert!(harness.actor.start_caller.is_none());
        harness
            .actor
            .dispatch(Event::SessionStarted, ReplyTarget::NotRequested);
        harness.actor.hold(HoldEdge::Release);
        assert!(harness.actor.start_caller.is_none());
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
}
