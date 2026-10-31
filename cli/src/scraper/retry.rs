//! Bounded retries with exponential backoff, on an injected clock.
//!
//! The spec's failure table needs two behaviors: transient failures
//! (network, timeout, 5xx) retry a bounded number of times and then fail
//! the run, while structure drift and credential problems fail immediately
//! — no hammering a portal that is refusing, no lockout-inducing loops.
//! The clock is injected so every backoff test observes the exact delays
//! without waiting for them.

/// The retry budget for one step: attempt at most `max_attempts` times,
/// sleeping `base_delay * 2^(attempt-1)` between failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay: std::time::Duration,
}

impl RetryPolicy {
    /// The budget portal logins get: a bad password must surface fast, so
    /// only a couple of attempts — this is not a lockout generator.
    #[must_use]
    pub(crate) fn login_default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: std::time::Duration::from_secs(2),
        }
    }

    /// The budget notice discovery and document fetches get: portals drop
    /// connections; a few patient attempts ride that out.
    #[must_use]
    pub(crate) fn fetch_default() -> Self {
        Self {
            max_attempts: 4,
            base_delay: std::time::Duration::from_secs(1),
        }
    }
}

/// The sleep seam, injected so tests never wait. Production sleeps on the
/// Tokio timer; tests record the delays and continue.
#[async_trait::async_trait]
pub(crate) trait RetryClock: Send + Sync {
    async fn sleep(&self, delay: std::time::Duration);
}

/// The production clock.
#[derive(Debug, Default)]
pub(crate) struct TokioClock;

#[async_trait::async_trait]
impl RetryClock for TokioClock {
    async fn sleep(&self, delay: std::time::Duration) {
        tokio::time::sleep(delay).await;
    }
}

/// The delay before the retry after `failed_attempt` (0-based): full jitter
/// is not worth its unpredictability for a court portal — a fixed doubling
/// sequence keeps the load pattern legible in the logs.
#[must_use]
pub(crate) fn backoff_delay(policy: &RetryPolicy, failed_attempt: u32) -> std::time::Duration {
    // Saturate the shift well before u64 seconds overflow; a run will hit
    // its attempt cap long before a delay gets large.
    let doubling = failed_attempt.min(10);
    policy.base_delay.saturating_mul(1u32 << doubling)
}

/// The outcome of exhausting a step's retry budget: how many attempts ran
/// and the error the last one failed with.
#[derive(Debug, thiserror::Error)]
#[error("failed after {attempts} attempt(s): {last}")]
pub(crate) struct RetryFailure<E> {
    pub attempts: u32,
    pub last: E,
}

/// Run `step` under `policy`, retrying while `is_retryable` holds and
/// sleeping `backoff_delay` between attempts on the injected `clock`.
///
/// # Errors
///
/// The last attempt's error wrapped in [`RetryFailure`] once the budget is
/// exhausted.
pub(crate) async fn with_retries<T, E, F, Fut, R>(
    policy: &RetryPolicy,
    clock: &dyn RetryClock,
    is_retryable: R,
    mut step: F,
) -> Result<T, RetryFailure<E>>
where
    // `FnMut`, not `Fn`: the download step re-borrows its sink mutably on
    // every attempt, and the returned future borrows from the closure.
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    R: Fn(&E) -> bool,
{
    let mut attempt = 0u32;
    loop {
        let outcome = step().await;
        match outcome {
            Ok(value) => return Ok(value),
            Err(error) => {
                attempt += 1;
                if attempt >= policy.max_attempts || !is_retryable(&error) {
                    return Err(RetryFailure {
                        attempts: attempt,
                        last: error,
                    });
                }
                clock.sleep(backoff_delay(policy, attempt - 1)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{backoff_delay, with_retries, RetryClock, RetryPolicy, TokioClock};
    use std::sync::Mutex;

    /// Records every delay instead of sleeping, so tests assert the exact
    /// backoff sequence without waiting for it.
    #[derive(Default)]
    struct StubClock {
        delays: Mutex<Vec<std::time::Duration>>,
    }

    #[async_trait::async_trait]
    impl RetryClock for StubClock {
        async fn sleep(&self, delay: std::time::Duration) {
            self.delays.lock().expect("uncontended").push(delay);
        }
    }

    impl StubClock {
        fn delays(&self) -> Vec<std::time::Duration> {
            self.delays.lock().expect("uncontended").clone()
        }
    }

    fn instant_policy(max_attempts: u32) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            base_delay: std::time::Duration::from_millis(250),
        }
    }

    fn transient(step: &str) -> String {
        format!("transient at {step}")
    }

    #[test]
    fn backoff_doubles_and_saturates() {
        let policy = instant_policy(20);
        assert_eq!(
            backoff_delay(&policy, 0),
            std::time::Duration::from_millis(250)
        );
        assert_eq!(
            backoff_delay(&policy, 1),
            std::time::Duration::from_millis(500)
        );
        assert_eq!(
            backoff_delay(&policy, 2),
            std::time::Duration::from_millis(1000)
        );
        let huge = RetryPolicy {
            max_attempts: u32::MAX,
            base_delay: std::time::Duration::from_secs(u64::from(u32::MAX)),
        };
        assert_eq!(
            backoff_delay(&huge, 100),
            std::time::Duration::from_secs(u64::from(u32::MAX))
        );
    }

    #[tokio::test]
    async fn success_on_the_first_attempt_never_sleeps() {
        let clock = StubClock::default();
        let outcome = with_retries(
            &instant_policy(3),
            &clock,
            |_unit: &()| true,
            || async { Ok::<_, ()>(7) },
        )
        .await;
        assert_eq!(outcome.expect("first attempt succeeds"), 7);
        assert!(clock.delays().is_empty());
    }

    #[tokio::test]
    async fn transient_failures_retry_until_success() {
        let clock = StubClock::default();
        let attempts = Mutex::new(0u32);
        let outcome = with_retries(
            &instant_policy(4),
            &clock,
            |error: &String| error.starts_with("transient"),
            || async {
                let mut count = attempts.lock().expect("uncontended");
                *count += 1;
                if *count < 3 {
                    Err(transient("notices"))
                } else {
                    Ok(*count)
                }
            },
        )
        .await;
        assert_eq!(outcome.expect("recovered on attempt 3"), 3);
        assert_eq!(
            clock.delays(),
            vec![
                std::time::Duration::from_millis(250),
                std::time::Duration::from_millis(500),
            ]
        );
    }

    #[tokio::test]
    async fn the_budget_exhausts_with_the_last_error() {
        let clock = StubClock::default();
        let attempts = Mutex::new(0u32);
        let outcome = with_retries(
            &instant_policy(3),
            &clock,
            |error: &String| error.starts_with("transient"),
            || async {
                let mut count = attempts.lock().expect("uncontended");
                *count += 1;
                Err::<(), _>(transient("download"))
            },
        )
        .await;
        let failure = outcome.expect_err("budget exhausted");
        assert_eq!(failure.attempts, 3);
        assert_eq!(failure.last, transient("download"));
        assert_eq!(
            clock.delays(),
            vec![
                std::time::Duration::from_millis(250),
                std::time::Duration::from_millis(500),
            ]
        );
    }

    #[tokio::test]
    async fn a_non_retryable_error_fails_immediately() {
        let clock = StubClock::default();
        let attempts = Mutex::new(0u32);
        let outcome = with_retries(
            &instant_policy(5),
            &clock,
            |error: &String| error.starts_with("transient"),
            || async {
                let mut count = attempts.lock().expect("uncontended");
                *count += 1;
                Err::<(), _>("structure drift".to_owned())
            },
        )
        .await;
        let failure = outcome.expect_err("hard stop");
        assert_eq!(failure.attempts, 1, "no retry for a hard-stop error");
        assert!(
            clock.delays().is_empty(),
            "no backoff for a hard-stop error"
        );
    }

    #[tokio::test]
    async fn the_production_clock_actually_sleeps() {
        // Guard the TokioClock wiring with a real (tiny) sleep so a broken
        // await surfaces here rather than only in live runs.
        TokioClock.sleep(std::time::Duration::from_millis(1)).await;
    }
}
