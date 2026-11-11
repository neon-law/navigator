//! The site adapter seam: the contract every portal adapter implements and
//! the identities every layer shares. The framework owns sessions, retries,
//! downloads, and intake; an adapter owns only its site's login flow,
//! navigation, and notice parsing.

use crate::scraper::credentials::SiteLogin;
use crate::scraper::download::DownloadSink;
use crate::scraper::fetcher::HttpFetcher;
use crate::scraper::session::BrowserSession;
pub(crate) use cli::scraper_config::{SiteKey, SiteKeyError};

/// The stable, run-spanning identity of one notice: site, docket, and the
/// court's own entry number. URLs change between sessions (PACER links are
/// minted per login); the key does not, so it is what the download ledger
/// and the run state are keyed by. Serialized transparently as its string
/// form — the ledger and run-state files are keyed by it.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub(crate) struct NoticeKey(String);

impl NoticeKey {
    #[must_use]
    pub(crate) fn new(site: &SiteKey, docket: &str, entry_number: &str) -> Self {
        Self(format!("{site}/{docket}/{entry_number}"))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NoticeKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One notice an adapter discovered, ready to download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoticeRef {
    /// The court's own entry number, e.g. `4` or `29-1`. Normalized to the
    /// `DKT NNNN[-att]` form only at naming time.
    pub entry_number: String,
    /// The notice's title as the court prints it; feeds the DKT filing name.
    pub title: String,
    /// Where the document lives this session. Not stable across runs.
    pub url: String,
    /// The file extension the document should carry (adapters know their
    /// own document types — usually `pdf`).
    pub extension: String,
}

impl NoticeRef {
    /// The notice's stable identity within one site and docket.
    #[must_use]
    pub(crate) fn key(&self, site: &SiteKey, docket: &str) -> NoticeKey {
        NoticeKey::new(site, docket, &self.entry_number)
    }
}

/// What a run is looking for: one docket, optionally since a timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NoticeQuery {
    pub docket: String,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
}

/// Why one adapter step failed. `Transient` is retried with backoff;
/// `StructureDrift` hard-stops the run — nothing partial is ever filed off
/// a portal whose selectors no longer match — and `AccessDenied` hard-stops
/// too: a portal refusing a fetch may need a credential the Firm has not
/// provisioned, and retrying cannot fix that.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum AdapterError {
    #[error("site structure changed: {step} no longer matches ({detail})")]
    StructureDrift { step: String, detail: String },
    #[error("the portal refused access at {step} ({detail})")]
    AccessDenied { step: String, detail: String },
    #[error("transient failure at {step}: {detail}")]
    Transient { step: String, detail: String },
}

/// The contract every portal adapter implements. Login drives the opened
/// browser session; discovery and download parse through the injected
/// fetcher and never open a connection themselves.
#[async_trait::async_trait]
pub(crate) trait SiteAdapter: Send + Sync {
    /// This adapter's stable site key.
    fn site(&self) -> &SiteKey;

    /// The adapter's credential key inside the Firm's `portal_login` secret,
    /// e.g. `la-superior`.
    fn credential_key(&self) -> &str;

    /// Whether the site's flow needs a provisioned login. Public portals
    /// scrape without one: when this is `false`, a missing credential row is
    /// not an error — the run proceeds unauthenticated. A mis-shapen or
    /// incomplete provisioned row still hard-stops, so a provisioning
    /// mistake is never silently ignored.
    fn requires_login(&self) -> bool {
        true
    }

    /// Log the session into the portal. Failures are retried a bounded
    /// number of times before the run hard-stops, so an implementation must
    /// not hammer a portal that is clearly refusing (bad password or a
    /// rewritten login form should surface promptly). `login` is `None` on
    /// the public path of a site whose [`requires_login`](Self::requires_login)
    /// is `false` and has no provisioned credential.
    async fn login(
        &self,
        session: &BrowserSession,
        login: Option<&SiteLogin>,
    ) -> Result<(), AdapterError>;

    /// Discover the notices for one docket.
    async fn notices(
        &self,
        fetcher: &dyn HttpFetcher,
        query: &NoticeQuery,
    ) -> Result<Vec<NoticeRef>, AdapterError>;

    /// Stream one notice's document into the resumable sink. The sink
    /// carries the resume offset and the running hash; `fetch_into` is the
    /// standard way to fill it.
    async fn download(
        &self,
        fetcher: &dyn HttpFetcher,
        notice: &NoticeRef,
        sink: &mut DownloadSink,
    ) -> Result<(), AdapterError>;
}

/// Look an adapter up by site key in a registry slice. The CLI's
/// `scraper run` resolves its `--site` value through this.
#[must_use]
pub(crate) fn find_adapter<'a>(
    adapters: &'a [Box<dyn SiteAdapter>],
    site: &SiteKey,
) -> Option<&'a dyn SiteAdapter> {
    adapters
        .iter()
        .find(|adapter| adapter.site() == site)
        .map(|adapter| &**adapter)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_key_accepts_the_documented_vocabulary() {
        for value in ["la-superior", "pacer", "nysaf", "23-cv"] {
            let key = SiteKey::new(value).expect("a documented key must parse");
            assert_eq!(key.as_str(), value);
            assert_eq!(key.to_string(), value, "Display is the key's string form");
        }
    }

    #[test]
    fn site_key_rejects_vocabulary_outside_letters_digits_and_inner_hyphens() {
        for value in [
            "",            // empty: names no registry entry
            "-lead",       // leading hyphen
            "trail-",      // trailing hyphen
            "LA-Superior", // uppercase is outside the vocabulary
            "la superior", // spaces never appear in keys
            "café",        // non-ASCII bytes are refused
            "la/superior", // path separators would forge state paths
        ] {
            let error =
                SiteKey::new(value).expect_err("a key outside the vocabulary must be refused");
            assert!(
                error.to_string().contains(value),
                "the error must name the offending key, got: {error}"
            );
        }
    }

    #[test]
    fn notice_key_is_site_docket_and_entry_number() {
        let site = SiteKey::new("la-superior").expect("documented key");
        assert_eq!(
            NoticeKey::new(&site, "23-cv-08134", "29-1").as_str(),
            "la-superior/23-cv-08134/29-1"
        );
    }

    #[test]
    fn notice_key_serializes_transparently_as_its_string_form() {
        let site = SiteKey::new("pacer").expect("documented key");
        let key = NoticeKey::new(&site, "4:21-cv-00123", "4");
        let json = serde_json::to_string(&key).expect("notice keys serialize as strings");
        assert_eq!(json, "\"pacer/4:21-cv-00123/4\"");
        let round: NoticeKey = serde_json::from_str(&json).expect("the string form parses back");
        assert_eq!(round, key);
    }

    #[test]
    fn a_notice_ref_keys_itself_by_its_entry_number() {
        let site = SiteKey::new("nysaf").expect("documented key");
        let notice = NoticeRef {
            entry_number: "29-1".to_owned(),
            title: "Notice of Motion".to_owned(),
            url: "https://portal.example/29-1.pdf".to_owned(),
            extension: "pdf".to_owned(),
        };
        assert_eq!(
            notice.key(&site, "23-cv-08134"),
            NoticeKey::new(&site, "23-cv-08134", "29-1")
        );
    }

    /// The registry lookup's contract exercised through the real trait: the
    /// named adapter is returned, a miss is `None`, and identity is by site
    /// key, not by credential key.
    struct StubAdapter {
        key: SiteKey,
        credential: &'static str,
    }

    #[async_trait::async_trait]
    impl SiteAdapter for StubAdapter {
        fn site(&self) -> &SiteKey {
            &self.key
        }

        fn credential_key(&self) -> &str {
            self.credential
        }

        async fn login(
            &self,
            _session: &BrowserSession,
            _login: Option<&SiteLogin>,
        ) -> Result<(), AdapterError> {
            unreachable!("the registry lookup never drives an adapter")
        }

        async fn notices(
            &self,
            _fetcher: &dyn HttpFetcher,
            _query: &NoticeQuery,
        ) -> Result<Vec<NoticeRef>, AdapterError> {
            unreachable!("the registry lookup never drives an adapter")
        }

        async fn download(
            &self,
            _fetcher: &dyn HttpFetcher,
            _notice: &NoticeRef,
            _sink: &mut DownloadSink,
        ) -> Result<(), AdapterError> {
            unreachable!("the registry lookup never drives an adapter")
        }
    }

    #[test]
    fn find_adapter_resolves_by_site_key_and_misses_cleanly() {
        let registry: Vec<Box<dyn SiteAdapter>> = vec![Box::new(StubAdapter {
            key: SiteKey::new("la-superior").expect("documented key"),
            credential: "la-superior-credential",
        })];
        let found = find_adapter(
            &registry,
            &SiteKey::new("la-superior").expect("documented key"),
        )
        .expect("a registered site resolves");
        assert_eq!(found.site().as_str(), "la-superior");
        assert_eq!(found.credential_key(), "la-superior-credential");
        assert!(find_adapter(&registry, &SiteKey::new("pacer").expect("documented key")).is_none());
    }
}
