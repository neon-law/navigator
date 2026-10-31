//! The site adapter seam: the contract every portal adapter implements and
//! the identities every layer shares. The framework owns sessions, retries,
//! downloads, and intake; an adapter owns only its site's login flow,
//! navigation, and notice parsing.

use crate::scraper::credentials::SiteLogin;
use crate::scraper::download::DownloadSink;
use crate::scraper::fetcher::HttpFetcher;
use crate::scraper::session::BrowserSession;

/// Errors that can come from building a [`SiteKey`].
#[derive(Debug, thiserror::Error)]
#[error("site key `{0}` is invalid: use lowercase letters, digits, and hyphens (for example `la-superior`)")]
pub(crate) struct SiteKeyError(String);

/// Stable identifier for one site the framework scrapes: `la-superior`,
/// `pacer`, `nysaf`. The key names the adapter's registry entry, its
/// on-disk state directory, and its credential entry in the Firm's
/// `portal_login` secret.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct SiteKey(String);

impl SiteKey {
    /// # Errors
    ///
    /// When the key is empty or carries characters outside the vocabulary
    /// (lowercase ASCII letters, digits, hyphens; no leading or trailing
    /// hyphen).
    pub(crate) fn new(value: &str) -> Result<Self, SiteKeyError> {
        let valid = !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !value.starts_with('-')
            && !value.ends_with('-');
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(SiteKeyError(value.to_owned()))
        }
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SiteKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

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
/// a portal whose selectors no longer match.
#[derive(Debug, Clone, thiserror::Error)]
pub(crate) enum AdapterError {
    #[error("site structure changed: {step} no longer matches ({detail})")]
    #[cfg_attr(not(test), allow(dead_code))] // site adapters raise this; the base registers none yet
    StructureDrift { step: String, detail: String },
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

    /// Log the session into the portal. Failures are retried a bounded
    /// number of times before the run hard-stops, so an implementation must
    /// not hammer a portal that is clearly refusing (bad password or a
    /// rewritten login form should surface promptly).
    async fn login(
        &self,
        session: &BrowserSession,
        login: &SiteLogin,
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
