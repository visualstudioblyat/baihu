use super::Provider;
use async_trait::async_trait;
use dashmap::DashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Cached provider response with TTL.
struct CachedResponse {
    content: String,
    created_at: Instant,
}

const CACHE_TTL_SECS: u64 = 60;

/// Provider wrapper with retry + fallback behavior + response caching.
pub struct ReliableProvider {
    providers: Vec<(String, Box<dyn Provider>)>,
    max_retries: u32,
    base_backoff_ms: u64,
    cache: Arc<DashMap<u64, CachedResponse>>,
}

impl ReliableProvider {
    pub fn new(
        providers: Vec<(String, Box<dyn Provider>)>,
        max_retries: u32,
        base_backoff_ms: u64,
    ) -> Self {
        Self {
            providers,
            max_retries,
            base_backoff_ms: base_backoff_ms.max(50),
            cache: Arc::new(DashMap::new()),
        }
    }

    fn cache_key(message: &str, model: &str) -> u64 {
        let mut hasher = DefaultHasher::new();
        message.hash(&mut hasher);
        model.hash(&mut hasher);
        hasher.finish()
    }
}

#[async_trait]
impl Provider for ReliableProvider {
    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        // Check cache first
        let key = Self::cache_key(message, model);
        if let Some(entry) = self.cache.get(&key) {
            if entry.created_at.elapsed().as_secs() < CACHE_TTL_SECS {
                return Ok(entry.content.clone());
            }
            drop(entry);
            self.cache.remove(&key);
        }

        let mut failures = Vec::new();

        for (provider_name, provider) in &self.providers {
            let mut backoff_ms = self.base_backoff_ms;

            for attempt in 0..=self.max_retries {
                match provider
                    .chat_with_system(system_prompt, message, model, temperature)
                    .await
                {
                    Ok(resp) => {
                        if attempt > 0 {
                            tracing::info!(
                                provider = provider_name,
                                attempt,
                                "Provider recovered after retries"
                            );
                        }
                        // Cache the successful response
                        self.cache.insert(
                            key,
                            CachedResponse {
                                content: resp.clone(),
                                created_at: Instant::now(),
                            },
                        );
                        return Ok(resp);
                    }
                    Err(e) => {
                        failures.push(format!(
                            "{provider_name} attempt {}/{}: {e}",
                            attempt + 1,
                            self.max_retries + 1
                        ));

                        if attempt < self.max_retries {
                            tracing::warn!(
                                provider = provider_name,
                                attempt = attempt + 1,
                                max_retries = self.max_retries,
                                "Provider call failed, retrying"
                            );
                            let jittered = apply_jitter(backoff_ms);
                            tokio::time::sleep(Duration::from_millis(jittered)).await;
                            backoff_ms = (backoff_ms.saturating_mul(2)).min(10_000);
                        }
                    }
                }
            }

            tracing::warn!(provider = provider_name, "Switching to fallback provider");
        }

        anyhow::bail!("All providers failed. Attempts:\n{}", failures.join("\n"))
    }
}

/// Adds +/-25% jitter to a backoff value to prevent thundering herd.
/// Uses UUID v4 (OS CSPRNG) for random bytes.
fn apply_jitter(base_ms: u64) -> u64 {
    let random_bytes = uuid::Uuid::new_v4();
    let raw = u32::from_le_bytes([
        random_bytes.as_bytes()[0],
        random_bytes.as_bytes()[1],
        random_bytes.as_bytes()[2],
        random_bytes.as_bytes()[3],
    ]);
    // Map raw u32 to [0.75, 1.25] range
    let factor = 0.75 + (f64::from(raw) / f64::from(u32::MAX)) * 0.5;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let result = (base_ms as f64 * factor) as u64;
    result.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn cache_key_deterministic() {
        let k1 = ReliableProvider::cache_key("hello", "gpt-4");
        let k2 = ReliableProvider::cache_key("hello", "gpt-4");
        assert_eq!(k1, k2);
    }

    #[test]
    fn cache_key_varies_by_model() {
        let k1 = ReliableProvider::cache_key("hello", "gpt-4");
        let k2 = ReliableProvider::cache_key("hello", "gpt-3.5");
        assert_ne!(k1, k2);
    }

    #[tokio::test]
    async fn cache_returns_same_response() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response: "cached_result",
                    error: "boom",
                }),
            )],
            0,
            1,
        );

        let r1 = provider.chat("hello", "test", 0.0).await.unwrap();
        let r2 = provider.chat("hello", "test", 0.0).await.unwrap();
        assert_eq!(r1, "cached_result");
        assert_eq!(r2, "cached_result");
        // Second call should hit cache, so only 1 actual provider call
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn jitter_within_bounds() {
        for _ in 0..100 {
            let result = apply_jitter(1000);
            assert!(result >= 750, "Jitter too low: {result}");
            assert!(result <= 1250, "Jitter too high: {result}");
        }
    }

    #[test]
    fn jitter_not_deterministic() {
        let results: std::collections::HashSet<u64> = (0..20).map(|_| apply_jitter(1000)).collect();
        assert!(results.len() > 1, "Jitter should produce varying values");
    }

    #[test]
    fn jitter_minimum_one() {
        assert!(apply_jitter(0) >= 1);
        assert!(apply_jitter(1) >= 1);
    }

    struct MockProvider {
        calls: Arc<AtomicUsize>,
        fail_until_attempt: usize,
        response: &'static str,
        error: &'static str,
    }

    #[async_trait]
    impl Provider for MockProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            let attempt = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.fail_until_attempt {
                anyhow::bail!(self.error);
            }
            Ok(self.response.to_string())
        }
    }

    #[tokio::test]
    async fn succeeds_without_retry() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 0,
                    response: "ok",
                    error: "boom",
                }),
            )],
            2,
            1,
        );

        let result = provider.chat("hello", "test", 0.0).await.unwrap();
        assert_eq!(result, "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn retries_then_recovers() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = ReliableProvider::new(
            vec![(
                "primary".into(),
                Box::new(MockProvider {
                    calls: Arc::clone(&calls),
                    fail_until_attempt: 1,
                    response: "recovered",
                    error: "temporary",
                }),
            )],
            2,
            1,
        );

        let result = provider.chat("hello", "test", 0.0).await.unwrap();
        assert_eq!(result, "recovered");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn falls_back_after_retries_exhausted() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let fallback_calls = Arc::new(AtomicUsize::new(0));

        let provider = ReliableProvider::new(
            vec![
                (
                    "primary".into(),
                    Box::new(MockProvider {
                        calls: Arc::clone(&primary_calls),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "primary down",
                    }),
                ),
                (
                    "fallback".into(),
                    Box::new(MockProvider {
                        calls: Arc::clone(&fallback_calls),
                        fail_until_attempt: 0,
                        response: "from fallback",
                        error: "fallback down",
                    }),
                ),
            ],
            1,
            1,
        );

        let result = provider.chat("hello", "test", 0.0).await.unwrap();
        assert_eq!(result, "from fallback");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 2);
        assert_eq!(fallback_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn returns_aggregated_error_when_all_providers_fail() {
        let provider = ReliableProvider::new(
            vec![
                (
                    "p1".into(),
                    Box::new(MockProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "p1 error",
                    }),
                ),
                (
                    "p2".into(),
                    Box::new(MockProvider {
                        calls: Arc::new(AtomicUsize::new(0)),
                        fail_until_attempt: usize::MAX,
                        response: "never",
                        error: "p2 error",
                    }),
                ),
            ],
            0,
            1,
        );

        let err = provider
            .chat("hello", "test", 0.0)
            .await
            .expect_err("all providers should fail");
        let msg = err.to_string();
        assert!(msg.contains("All providers failed"));
        assert!(msg.contains("p1 attempt 1/1"));
        assert!(msg.contains("p2 attempt 1/1"));
    }
}
