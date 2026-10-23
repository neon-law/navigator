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
        refreshing: bool,
    }

    impl StarCache {
        fn fresh(&self, now: Instant) -> bool {
            self.checked_at
                .is_some_and(|checked| now.duration_since(checked) < Duration::from_secs(3600))
        }

        fn begin_refresh(&mut self, now: Instant) -> bool {
            if self.refreshing || self.fresh(now) {
                return false;
            }
            self.refreshing = true;
            true
        }

        fn record(&mut self, count: Option<u64>, now: Instant) {
            if count.is_some() {
                self.count = count;
            }
            self.checked_at = Some(now);
            self.refreshing = false;
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
        let cache = CACHE.get_or_init(|| tokio::sync::Mutex::new(StarCache::default()));
        let (count, refresh) = {
            let mut cache = cache.lock().await;
            let refresh = cache.begin_refresh(Instant::now());
            (cache.count, refresh)
        };
        if refresh {
            // The process-wide cache owns this bounded refresh. Runtime shutdown
            // cancels it; page cancellation leaves it available to later requests.
            tokio::spawn(async move {
                // Anonymous access uses no deployment credential. Bound the complete
                // request and body read so a stalled refresh releases its in-flight slot.
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
                cache.lock().await.record(count, Instant::now());
            });
        }
        count
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
        fn cold_and_stale_reads_keep_the_snapshot_while_one_refresh_runs() {
            let now = Instant::now();
            let mut cache = StarCache::default();
            assert!(cache.begin_refresh(now));
            assert_eq!(cache.count, None);
            assert!(!cache.begin_refresh(now));
            cache.record(Some(1234), now);
            assert!(!cache.begin_refresh(now));
            let expired = now + Duration::from_secs(3600);
            assert!(cache.begin_refresh(expired));
            assert_eq!(cache.count, Some(1234));
            assert!(!cache.begin_refresh(expired));
            cache.record(None, expired);
            assert_eq!(cache.count, Some(1234));
            assert!(!cache.begin_refresh(expired));
            assert!(cache.begin_refresh(expired + Duration::from_secs(3600)));
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

/// Return the last count immediately, starting at most one background refresh per hour.
#[cfg(feature = "server")]
pub async fn stars() -> Option<u64> {
    cache::stars().await
}

/// Read the last known count without initiating a network request.
#[cfg(feature = "server")]
pub fn known_stars() -> Option<u64> {
    cache::known_stars()
}
