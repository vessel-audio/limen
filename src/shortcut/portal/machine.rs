//! The decisions of the shortcut actor as a pure state machine: which
//! generation runs, which call it makes next, what the status says, what a
//! signal triggers, and when to set up again.
//!
//! A generation starts only once the previous one's task has ended, so at
//! most one holds a portal connection. Reports of any other generation than
//! the running one are dropped.

use std::time::Duration;

use super::connection::{Call, Generation, Report};
use crate::config::ShortcutConfig;
use crate::shortcut::status::{PerAction, SessionState, ShortcutStatus};
use crate::shortcut::{Action, Activation, Backend, HoldEdge};

/// The first wait before setting up again after a failure; it doubles with
/// each failure in a row.
pub(super) const FIRST_RETRY: Duration = Duration::from_secs(1);

/// The longest wait between two attempts.
pub(super) const LAST_RETRY: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Input {
    /// Apply this `[shortcut]` section.
    Rebind(ShortcutConfig),
    Report(Generation, Report),
    /// The task of a generation has ended.
    Ended(Generation),
    /// The wait after a failure is over.
    RetryDue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Output {
    /// Run a new generation on a new connection.
    Connect(Generation),
    Call(Generation, Call),
    /// Close the generation and end its task.
    Stop(Generation),
    Publish(ShortcutStatus),
    /// The rebinds asked for so far have taken effect: their state is
    /// published.
    Rebound,
    Activate(Activation),
    /// Tell the user about a failure.
    Notify(String),
    RetryAfter(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Connecting,
    Registering,
    CreatingSession,
    Binding,
    Bound,
}

/// What follows once a stopping generation has ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Then {
    /// Set up for the config, or settle when its backend is not the portal.
    Start,
    Retry,
    /// Stay without a session.
    Rest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// No generation runs, and none is due.
    Idle,
    /// A failed generation has ended; another is due after the wait.
    Waiting,
    Running {
        generation: Generation,
        step: Step,
    },
    Stopping {
        generation: Generation,
        then: Then,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Machine {
    config: ShortcutConfig,
    /// The last generation started.
    last: Generation,
    stage: Stage,
    status: ShortcutStatus,
    retry: Duration,
    /// The last failure notified: one that repeats is notified once.
    notified: Option<String>,
    /// A rebind waits for its state to be published.
    rebinding: bool,
}

impl Default for Machine {
    fn default() -> Self {
        let config = ShortcutConfig::default();
        Self {
            config,
            last: Generation(0),
            stage: Stage::Idle,
            status: status(&config, SessionState::Disabled),
            retry: FIRST_RETRY,
            notified: None,
            rebinding: false,
        }
    }
}

/// The status for `config` in `session`, before anything is bound.
fn status(config: &ShortcutConfig, session: SessionState) -> ShortcutStatus {
    ShortcutStatus {
        backend: config.backend,
        session,
        preferred: PerAction {
            toggle: config.toggle,
            hold: config.hold,
        },
        actual: PerAction::default(),
        last_error: None,
    }
}

impl Machine {
    pub(super) fn status(&self) -> &ShortcutStatus {
        &self.status
    }

    pub(super) fn handle(&mut self, input: Input) -> Vec<Output> {
        match input {
            Input::Rebind(config) => {
                self.config = config;
                self.rebinding = true;
                self.retry = FIRST_RETRY;
                self.status.last_error = None;
                match self.stage {
                    Stage::Running { generation, .. } => {
                        self.stage = Stage::Stopping {
                            generation,
                            then: Then::Start,
                        };
                        vec![Output::Stop(generation)]
                    }
                    Stage::Stopping { generation, .. } => {
                        self.stage = Stage::Stopping {
                            generation,
                            then: Then::Start,
                        };
                        Vec::new()
                    }
                    Stage::Idle | Stage::Waiting => self.start(),
                }
            }
            Input::Report(generation, report) => match self.stage {
                Stage::Running {
                    generation: running,
                    step,
                } if running == generation => self.report(generation, step, report),
                Stage::Running { .. } | Stage::Stopping { .. } | Stage::Idle | Stage::Waiting => {
                    Vec::new()
                }
            },
            Input::Ended(generation) => match self.stage {
                Stage::Stopping {
                    generation: stopping,
                    then,
                } if stopping == generation => match then {
                    Then::Start => self.start(),
                    Then::Retry => self.wait(),
                    Then::Rest => {
                        self.stage = Stage::Idle;
                        Vec::new()
                    }
                },
                // Its task died before it reported a failure.
                Stage::Running {
                    generation: running,
                    ..
                } if running == generation => {
                    let mut outputs = self.failed("the portal connection ended".to_string());
                    outputs.extend(self.wait());
                    outputs
                }
                Stage::Running { .. } | Stage::Stopping { .. } | Stage::Idle | Stage::Waiting => {
                    Vec::new()
                }
            },
            Input::RetryDue => match self.stage {
                Stage::Waiting => self.start(),
                Stage::Idle | Stage::Running { .. } | Stage::Stopping { .. } => Vec::new(),
            },
        }
    }

    fn report(&mut self, generation: Generation, step: Step, report: Report) -> Vec<Output> {
        match (step, report) {
            (Step::Connecting, Report::Connected) => {
                self.advance(generation, Step::Registering, Call::Register)
            }
            // The portal knows the app only after Register: no session before.
            (Step::Registering, Report::Registered) => {
                self.advance(generation, Step::CreatingSession, Call::CreateSession)
            }
            (Step::CreatingSession, Report::SessionCreated) => {
                let preferred = self.status.preferred;
                self.advance(generation, Step::Binding, Call::Bind(preferred))
            }
            (Step::Binding, Report::Bound(actual)) => {
                self.stage = Stage::Running {
                    generation,
                    step: Step::Bound,
                };
                self.retry = FIRST_RETRY;
                self.notified = None;
                self.status.session = SessionState::Bound;
                self.status.actual = actual;
                self.status.last_error = None;
                vec![Output::Publish(self.status.clone())]
            }
            (_, Report::Cancelled) => {
                self.stage = Stage::Stopping {
                    generation,
                    then: Then::Rest,
                };
                self.status.session = SessionState::Cancelled;
                vec![
                    Output::Stop(generation),
                    Output::Publish(self.status.clone()),
                ]
            }
            (_, Report::Failed(error)) => {
                self.stage = Stage::Stopping {
                    generation,
                    then: Then::Retry,
                };
                [vec![Output::Stop(generation)], self.failed(error)].concat()
            }
            // The session went with the portal: set up again at once.
            (_, Report::PortalRestarted) => {
                self.stage = Stage::Stopping {
                    generation,
                    then: Then::Start,
                };
                vec![Output::Stop(generation)]
            }
            (Step::Bound, Report::Activated(id)) => match Action::from_id(&id) {
                Some(Action::Toggle) => vec![Output::Activate(Activation::Toggle)],
                Some(Action::Hold) => vec![Output::Activate(Activation::Hold(HoldEdge::Press))],
                None => Vec::new(),
            },
            (Step::Bound, Report::Deactivated(id)) => match Action::from_id(&id) {
                Some(Action::Hold) => {
                    vec![Output::Activate(Activation::Hold(HoldEdge::Release))]
                }
                Some(Action::Toggle) | None => Vec::new(),
            },
            (Step::Bound, Report::Changed(actual)) => {
                self.status.actual = actual;
                vec![Output::Publish(self.status.clone())]
            }
            // Out of order, e.g. a signal before the bind finished.
            (
                _,
                Report::Connected
                | Report::Registered
                | Report::SessionCreated
                | Report::Bound(_)
                | Report::Activated(_)
                | Report::Deactivated(_)
                | Report::Changed(_),
            ) => Vec::new(),
        }
    }

    fn advance(&mut self, generation: Generation, step: Step, call: Call) -> Vec<Output> {
        self.stage = Stage::Running { generation, step };
        vec![Output::Call(generation, call)]
    }

    /// Set up for the config, or settle when its backend is not the portal.
    fn start(&mut self) -> Vec<Output> {
        let mut outputs = match self.config.backend {
            Backend::Portal => {
                let generation = Generation(self.last.0 + 1);
                self.last = generation;
                self.stage = Stage::Running {
                    generation,
                    step: Step::Connecting,
                };
                self.status = ShortcutStatus {
                    last_error: self.status.last_error.take(),
                    ..status(&self.config, SessionState::Binding)
                };
                vec![
                    Output::Connect(generation),
                    Output::Publish(self.status.clone()),
                ]
            }
            Backend::GnomeKeybinding | Backend::None => {
                self.stage = Stage::Idle;
                self.status = status(&self.config, SessionState::Disabled);
                vec![Output::Publish(self.status.clone())]
            }
        };
        if std::mem::take(&mut self.rebinding) {
            outputs.push(Output::Rebound);
        }
        outputs
    }

    /// Wait before the next attempt, longer after each failure in a row.
    fn wait(&mut self) -> Vec<Output> {
        self.stage = Stage::Waiting;
        let wait = self.retry;
        self.retry = (self.retry * 2).min(LAST_RETRY);
        vec![Output::RetryAfter(wait)]
    }

    /// Publish `error`, and notify it unless it is the one notified last.
    fn failed(&mut self, error: String) -> Vec<Output> {
        self.status.session = SessionState::Failed;
        self.status.last_error = Some(error.clone());
        let publish = Output::Publish(self.status.clone());
        match self.notified.as_ref() == Some(&error) {
            true => vec![publish],
            false => {
                self.notified = Some(error.clone());
                vec![publish, Output::Notify(error)]
            }
        }
    }
}

#[cfg(test)]
mod tests;
