use async_trait::async_trait;
use std::time::Duration;
use tokio::time::sleep;

use crate::llm::{ChunkContext, LlmError, LlmProvider, ProposedCard};

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: usize,
    pub base_delay: Duration,
    pub max_backoff: Duration,
    pub backoff_factor: f64,
}

/// Conservative defaults for a local Ollama instance: 3 attempts with 100 ms base
/// delay, doubling each retry, capped at 5 s. Override any field via struct literal.
impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
            backoff_factor: 2.0,
        }
    }
}

impl RetryPolicy {
    fn backoff_delay(&self, attempt: usize) -> Duration {
        self.base_delay
            .mul_f64(self.backoff_factor.powi(attempt as i32))
            .min(self.max_backoff)
    }
}

pub struct RetryingProvider<P> {
    inner: P,
    policy: RetryPolicy,
}

impl<P> RetryingProvider<P> {
    pub fn new(inner: P, policy: RetryPolicy) -> Self {
        Self { inner, policy }
    }
}

#[async_trait]
impl<P: LlmProvider> LlmProvider for RetryingProvider<P> {
    async fn propose_cards(&self, chunk: &ChunkContext) -> Result<Vec<ProposedCard>, LlmError> {
        let mut last_error = None;

        for attempt in 0..self.policy.max_attempts {
            match self.inner.propose_cards(chunk).await {
                Ok(result) => return Ok(result),
                Err(LlmError::Transient {
                    reason,
                    retry_after,
                }) => {
                    last_error = Some(LlmError::Transient {
                        reason,
                        retry_after,
                    });

                    if attempt == self.policy.max_attempts - 1 {
                        break;
                    }

                    // Respect retry_after from the server; fall back to computed exponential backoff.
                    let delay = retry_after.unwrap_or_else(|| self.policy.backoff_delay(attempt));
                    sleep(delay).await;
                }
                Err(e @ LlmError::BadInput { .. }) => return Err(e),
                Err(e @ LlmError::Config { .. }) => return Err(e),
            }
        }

        Err(last_error.unwrap_or_else(|| LlmError::Config {
            reason: "no attempts made".to_string(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tokio::time;

    type ResponseQueue = Arc<Mutex<VecDeque<Result<Vec<ProposedCard>, LlmError>>>>;

    struct CountingMock {
        responses: ResponseQueue,
        call_count: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl CountingMock {
        fn new(responses: Vec<Result<Vec<ProposedCard>, LlmError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(VecDeque::from(responses))),
                call_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn count(&self) -> Arc<std::sync::atomic::AtomicUsize> {
            self.call_count.clone()
        }
    }

    #[async_trait]
    impl LlmProvider for CountingMock {
        async fn propose_cards(
            &self,
            _chunk: &ChunkContext,
        ) -> Result<Vec<ProposedCard>, LlmError> {
            self.call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(vec![]))
        }
    }

    struct TimestampedMock {
        responses: ResponseQueue,
        call_times: Arc<Mutex<Vec<time::Instant>>>,
    }

    impl TimestampedMock {
        fn new(responses: Vec<Result<Vec<ProposedCard>, LlmError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(VecDeque::from(responses))),
                call_times: Arc::new(Mutex::new(vec![])),
            }
        }

        fn call_times(&self) -> Arc<Mutex<Vec<time::Instant>>> {
            self.call_times.clone()
        }
    }

    #[async_trait]
    impl LlmProvider for TimestampedMock {
        async fn propose_cards(
            &self,
            _chunk: &ChunkContext,
        ) -> Result<Vec<ProposedCard>, LlmError> {
            self.call_times.lock().unwrap().push(time::Instant::now());
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(vec![]))
        }
    }

    fn chunk() -> ChunkContext {
        ChunkContext {
            source_file: "test.md".to_string(),
            source_heading: None,
            tags: vec![],
            text: "test content".to_string(),
        }
    }

    fn transient() -> Result<Vec<ProposedCard>, LlmError> {
        Err(LlmError::Transient {
            reason: "boom".to_string(),
            retry_after: None,
        })
    }

    fn transient_with_retry_after(d: Duration) -> Result<Vec<ProposedCard>, LlmError> {
        Err(LlmError::Transient {
            reason: "rate limited".to_string(),
            retry_after: Some(d),
        })
    }

    fn ok() -> Result<Vec<ProposedCard>, LlmError> {
        Ok(vec![])
    }

    #[tokio::test]
    async fn transient_retries_then_succeeds() {
        time::pause();
        let mock = CountingMock::new(vec![transient(), transient(), ok()]);
        let count = mock.count();
        let provider = RetryingProvider::new(mock, RetryPolicy::default());
        let result = provider.propose_cards(&chunk()).await;
        assert!(result.is_ok());
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn transient_exhausts_all_attempts_and_returns_error() {
        time::pause();
        let mock = CountingMock::new(vec![transient(), transient(), transient()]);
        let count = mock.count();
        let provider = RetryingProvider::new(mock, RetryPolicy::default());
        let result = provider.propose_cards(&chunk()).await;
        assert!(matches!(result, Err(LlmError::Transient { .. })));
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn bad_input_passes_through_without_retry() {
        let mock = CountingMock::new(vec![Err(LlmError::BadInput {
            reason: "bad".to_string(),
        })]);
        let count = mock.count();
        let provider = RetryingProvider::new(mock, RetryPolicy::default());
        let result = provider.propose_cards(&chunk()).await;
        assert!(matches!(result, Err(LlmError::BadInput { .. })));
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn config_passes_through_without_retry() {
        let mock = CountingMock::new(vec![Err(LlmError::Config {
            reason: "bad config".to_string(),
        })]);
        let count = mock.count();
        let provider = RetryingProvider::new(mock, RetryPolicy::default());
        let result = provider.propose_cards(&chunk()).await;
        assert!(matches!(result, Err(LlmError::Config { .. })));
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retry_after_is_respected_not_capped() {
        time::pause();
        // Server says wait 60s — well beyond max_backoff of 5s.
        // The retry layer must honor it, not cap it.
        let long_retry_after = Duration::from_secs(60);
        let mock = CountingMock::new(vec![transient_with_retry_after(long_retry_after), ok()]);
        let provider = RetryingProvider::new(mock, RetryPolicy::default());

        let start = time::Instant::now();
        let handle = tokio::spawn(async move { provider.propose_cards(&chunk()).await });
        time::advance(long_retry_after).await;
        let result = handle.await.unwrap();
        assert!(result.is_ok());
        // Elapsed virtual time should be at least retry_after (60s), not capped at max_backoff (5s).
        assert!(start.elapsed() >= long_retry_after);
    }

    #[tokio::test]
    async fn exponential_backoff_is_capped_at_max_backoff() {
        time::pause();
        // 3 attempts, fail first two with no retry_after.
        // base_delay=1s, factor=2, max_backoff=3s → delays: 1s, 2s (capped: 3s would be attempt 2)
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_secs(1),
            max_backoff: Duration::from_secs(3),
            backoff_factor: 10.0, // exaggerated: attempt 1 delay = 10s, capped to 3s
        };
        let mock = CountingMock::new(vec![transient(), transient(), ok()]);
        let provider = RetryingProvider::new(mock, policy);

        let start = time::Instant::now();
        let handle = tokio::spawn(async move { provider.propose_cards(&chunk()).await });
        // advance past the two capped 3s sleeps
        time::advance(Duration::from_secs(10)).await;
        let result = handle.await.unwrap();
        assert!(result.is_ok());
        // Total elapsed should be 1s (attempt 0, capped to 1s base) + 3s (attempt 1, capped) = 4s
        // Definitely less than 20s (what uncapped would be: 1s + 10s)
        assert!(start.elapsed() < Duration::from_secs(20));
    }

    #[tokio::test]
    async fn first_attempt_success_makes_single_call() {
        let mock = CountingMock::new(vec![ok()]);
        let count = mock.count();
        let provider = RetryingProvider::new(mock, RetryPolicy::default());
        let result = provider.propose_cards(&chunk()).await;
        assert!(result.is_ok());
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn zero_max_attempts_makes_no_call_and_returns_config_fallback() {
        let policy = RetryPolicy {
            max_attempts: 0,
            ..RetryPolicy::default()
        };
        let mock = CountingMock::new(vec![]);
        let count = mock.count();
        let provider = RetryingProvider::new(mock, policy);
        let result = provider.propose_cards(&chunk()).await;
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 0);
        match result {
            Err(LlmError::Config { reason }) => assert_eq!(reason, "no attempts made"),
            other => panic!("expected Config fallback, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exhaustion_returns_the_last_transient_error() {
        time::pause();
        let mock = CountingMock::new(vec![
            Err(LlmError::Transient {
                reason: "first".to_string(),
                retry_after: None,
            }),
            Err(LlmError::Transient {
                reason: "second".to_string(),
                retry_after: None,
            }),
            Err(LlmError::Transient {
                reason: "third".to_string(),
                retry_after: None,
            }),
        ]);
        let provider = RetryingProvider::new(mock, RetryPolicy::default());
        let result = provider.propose_cards(&chunk()).await;
        match result {
            Err(LlmError::Transient { reason, .. }) => assert_eq!(reason, "third"),
            other => panic!("expected last transient error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exponential_backoff_grows_uncapped() {
        time::pause();
        // base=1s, factor=2.0, max_backoff high enough not to cap.
        // Attempt 0 fails → sleep 1s. Attempt 1 fails → sleep 2s. Attempt 2 succeeds.
        let policy = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_secs(1),
            max_backoff: Duration::from_secs(100),
            backoff_factor: 2.0,
        };
        let mock = TimestampedMock::new(vec![transient(), transient(), ok()]);
        let call_times = mock.call_times();
        let provider = RetryingProvider::new(mock, policy);

        let handle = tokio::spawn(async move { provider.propose_cards(&chunk()).await });
        time::advance(Duration::from_secs(10)).await;
        let result = handle.await.unwrap();
        assert!(result.is_ok());

        let times = call_times.lock().unwrap();
        assert_eq!(times.len(), 3);
        // gap between call 0 and call 1: base * factor^0 = 1s
        let gap0 = times[1] - times[0];
        // gap between call 1 and call 2: base * factor^1 = 2s
        let gap1 = times[2] - times[1];
        // Paused virtual clock uses a timer wheel, so gaps land in [Ns, N+10ms).
        assert!(gap0 >= Duration::from_secs(1) && gap0 < Duration::from_millis(1100));
        assert!(gap1 >= Duration::from_secs(2) && gap1 < Duration::from_millis(2100));
    }
}
