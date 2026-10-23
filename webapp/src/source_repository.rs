//! Navigator's product page and public source repository.

/// The product page linked by the platform name in every footer.
pub const NAVIGATOR_HREF: &str = "https://www.neonlaw.com/navigator";
/// The public source repository named in the footer.
pub const SOURCE_REPO: &str = "neon-law/navigator";
/// The public source repository's web address.
pub const SOURCE_HREF: &str = "https://github.com/neon-law/navigator";

#[cfg(feature = "server")]
mod cache {
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    pub(super) struct StarCache {
        pub(super) count: Option<u64>,
        checked_at: Option<Instant>,
    }

    impl StarCache {
        fn fresh(&self, now: Instant) -> bool {
            self.checked_at
                .is_some_and(|checked| now.duration_since(checked) < Duration::from_secs(3600))
        }

        fn record(&mut self, count: Option<u64>, now: Instant) {
            if count.is_some() {
                self.count = count;
            }
            self.checked_at = Some(now);
        }
    }

    #[derive(serde::Deserialize)]
    struct Repository {
        stargazers_count: u64,
    }

    static CACHE: OnceLock<tokio::sync::Mutex<StarCache>> = OnceLock::new();

    pub(super) fn known_stars() -> Option<u64> {
        CACHE.get()?.try_lock().ok()?.count
    }

    pub(super) async fn stars() -> Option<u64> {
        let mut cache = CACHE
            .get_or_init(|| tokio::sync::Mutex::new(StarCache::default()))
            .lock()
            .await;
        if !cache.fresh(Instant::now()) {
            // Anonymous access uses no deployment credential. Bound the complete
            // request and body read so GitHub cannot hold page rendering open.
            let result = tokio::time::timeout(Duration::from_secs(2), async {
                reqwest::Client::new()
                    .get("https://api.github.com/repos/neon-law/navigator")
                    .header(reqwest::header::USER_AGENT, "neon-law-navigator")
                    .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                    .send()
                    .await?
                    .error_for_status()?
                    .json::<Repository>()
                    .await
            })
            .await;
            let count = result
                .ok()
                .and_then(Result::ok)
                .map(|repo| repo.stargazers_count);
            // A failed refresh retains the last known count and backs off for
            // an hour, including during anonymous API rate limiting.
            cache.record(count, Instant::now());
        }
        cache.count
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn github_response_requires_a_numeric_star_count() {
            let repository = serde_json::from_str::<Repository>(r#"{"stargazers_count":0}"#)
                .expect("GitHub's numeric count is valid");
            assert_eq!(repository.stargazers_count, 0);
            assert!(serde_json::from_str::<Repository>(r#"{"message":"rate limited"}"#).is_err());
            assert!(
                serde_json::from_str::<Repository>(r#"{"stargazers_count":"unknown"}"#).is_err()
            );
        }

        #[test]
        fn refresh_retains_known_counts_and_backs_off_after_failure() {
            let now = Instant::now();
            let mut cache = StarCache::default();
            assert!(!cache.fresh(now));
            cache.record(None, now);
            assert_eq!(cache.count, None);
            assert!(cache.fresh(now));
            cache.record(Some(0), now);
            assert_eq!(cache.count, Some(0));
            cache.record(Some(1234), now);
            let expired = now + Duration::from_secs(3600);
            assert!(!cache.fresh(expired));
            cache.record(None, expired);
            assert_eq!(cache.count, Some(1234));
            assert!(cache.fresh(expired));
            assert!(!cache.fresh(expired + Duration::from_secs(3600)));
        }
    }
}

/// Resolve a count cached for one hour, retaining the last value on failure.
#[cfg(feature = "server")]
pub async fn stars() -> Option<u64> {
    cache::stars().await
}

/// Read the last known count without initiating a network request.
#[cfg(feature = "server")]
pub fn known_stars() -> Option<u64> {
    cache::known_stars()
}
