//! Trying a model request again when it fails, each try bounded by its own
//! time, as a provider's settings ask.

use std::future::Future;
use std::time::Duration;

use anyhow::{Result, anyhow};

/// How long each try of a request may take, and how many more tries a
/// failed one gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Attempts {
    pub(crate) timeout: Duration,
    pub(crate) retries: u32,
}

impl Attempts {
    /// What `request` answered, tried again while it fails or outlasts the
    /// timeout, at most `retries` more times; the last failure otherwise.
    /// `what` names the request in a timeout's error.
    pub(crate) async fn run<T, Request, Answer>(&self, what: &str, request: Request) -> Result<T>
    where
        Request: Fn() -> Answer,
        Answer: Future<Output = Result<T>>,
    {
        let mut failure = anyhow!("{what} was not tried");
        for _ in 0..=self.retries {
            failure = match tokio::time::timeout(self.timeout, request()).await {
                Ok(Ok(answer)) => return Ok(answer),
                Ok(Err(error)) => error,
                Err(_) => anyhow!("{what} timed out after {:?}", self.timeout),
            };
        }
        Err(failure)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use anyhow::bail;

    use super::Attempts;

    #[tokio::test]
    async fn a_failed_request_is_tried_again_until_its_retries_run_out() {
        let tries = AtomicU32::new(0);
        let attempts = Attempts {
            timeout: Duration::from_secs(1),
            retries: 2,
        };
        let succeeds_on = |success: u32| {
            let tries = &tries;
            move || async move {
                match tries.fetch_add(1, Ordering::SeqCst) + 1 {
                    count if count == success => Ok(count),
                    count => bail!("try {count} failed"),
                }
            }
        };

        let second = attempts.run("the request", succeeds_on(2)).await.unwrap();
        tries.store(0, Ordering::SeqCst);
        let never = attempts
            .run("the request", succeeds_on(9))
            .await
            .unwrap_err()
            .to_string();

        assert_eq!(
            (second, never, tries.load(Ordering::SeqCst)),
            (2, "try 3 failed".to_string(), 3)
        );
    }

    #[tokio::test]
    async fn each_try_is_bounded_by_the_timeout() {
        let attempts = Attempts {
            timeout: Duration::from_millis(20),
            retries: 0,
        };

        let error = attempts
            .run("polishing", || async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(())
            })
            .await
            .unwrap_err();

        assert_eq!(error.to_string(), "polishing timed out after 20ms");
    }
}
