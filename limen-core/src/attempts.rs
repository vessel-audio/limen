//! Trying a model request again when it fails for now, each try bounded
//! by its own time and all of them by a budget, as a provider's settings
//! ask.

use std::future::Future;
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::time::Instant;

use crate::backend::Transient;

/// How long each try of a request may take, how many more tries a failure
/// that may pass gets, and how long all of them may take together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Attempts {
    pub(crate) timeout: Duration,
    pub(crate) retries: u32,
    pub(crate) budget: Duration,
}

impl Attempts {
    /// `retries` more tries of `timeout` each, with no budget beyond them.
    pub(crate) fn each(timeout: Duration, retries: u32) -> Self {
        Self {
            timeout,
            retries,
            budget: timeout.saturating_mul(retries.saturating_add(1)),
        }
    }

    /// What `request` answered. A timeout or a [`Transient`] failure is
    /// tried again, at most `retries` more times and only while the budget
    /// lasts, each try cut short to what is left of it; any other failure,
    /// such as a rejected key, ends at once. `what` names the request in a
    /// timeout's error.
    pub(crate) async fn run<T, Request, Answer>(&self, what: &str, request: Request) -> Result<T>
    where
        Request: Fn() -> Answer,
        Answer: Future<Output = Result<T>>,
    {
        let started = Instant::now();
        let mut failure = anyhow!("{what} was not tried");
        for _ in 0..=self.retries {
            let left = self.budget.saturating_sub(started.elapsed());
            if left.is_zero() {
                break;
            }
            let timeout = self.timeout.min(left);
            failure = match tokio::time::timeout(timeout, request()).await {
                Ok(Ok(answer)) => return Ok(answer),
                Ok(Err(error)) if error.chain().any(|cause| cause.is::<Transient>()) => error,
                Ok(Err(error)) => return Err(error),
                Err(_) => anyhow!("{what} timed out after {timeout:?}"),
            };
        }
        Err(failure)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use anyhow::Result;

    use super::Attempts;
    use crate::backend::Transient;

    /// A request that fails `failures` times with what `fail` gives, then
    /// answers with its try's number; counts its tries in `tries`.
    fn failing(
        tries: &AtomicU32,
        failures: u32,
        fail: fn(u32) -> anyhow::Error,
    ) -> impl Fn() -> std::future::Ready<Result<u32>> + '_ {
        move || {
            let count = tries.fetch_add(1, Ordering::SeqCst) + 1;
            std::future::ready(match count > failures {
                true => Ok(count),
                false => Err(fail(count)),
            })
        }
    }

    fn transient(count: u32) -> anyhow::Error {
        anyhow::Error::new(Transient(format!("try {count} could not connect")))
    }

    #[tokio::test]
    async fn a_failure_that_may_pass_is_tried_again_until_the_retries_run_out() {
        let attempts = Attempts::each(Duration::from_secs(1), 2);
        let (second, never) = (AtomicU32::new(0), AtomicU32::new(0));

        let answered = attempts
            .run("the request", failing(&second, 1, transient))
            .await
            .unwrap();
        let failed = attempts
            .run("the request", failing(&never, 9, transient))
            .await
            .unwrap_err()
            .to_string();

        assert_eq!(
            (answered, failed, never.load(Ordering::SeqCst)),
            (2, "try 3 could not connect".to_string(), 3)
        );
    }

    #[tokio::test]
    async fn a_refused_request_is_never_repeated() {
        let tries = AtomicU32::new(0);
        let refused = |count| anyhow::anyhow!("try {count}: the service rejected the API key");

        let error = Attempts::each(Duration::from_secs(1), 5)
            .run("the request", failing(&tries, 9, refused))
            .await
            .unwrap_err();

        assert_eq!(
            (error.to_string(), tries.load(Ordering::SeqCst)),
            ("try 1: the service rejected the API key".to_string(), 1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_budget_bounds_every_try_together() {
        let tries = AtomicU32::new(0);
        let attempts = Attempts {
            timeout: Duration::from_secs(10),
            retries: 5,
            budget: Duration::from_secs(15),
        };

        let error = attempts
            .run("polishing", || {
                tries.fetch_add(1, Ordering::SeqCst);
                async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    Result::<()>::Err(anyhow::anyhow!("never answers"))
                }
            })
            .await
            .unwrap_err();

        // The first try takes 10 s; the second gets the 5 s left; none follows.
        assert_eq!(
            (error.to_string(), tries.load(Ordering::SeqCst)),
            ("polishing timed out after 5s".to_string(), 2)
        );
    }
}
