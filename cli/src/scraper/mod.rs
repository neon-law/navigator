//! The scraper framework base: one pipeline every site adapter runs
//! through, and the pieces that never change per site.
//!
//! A run resolves the Firm's `CourtPortal`/`PortalLogin` secret, opens a
//! Chromium session through the pinned Chrome for Testing stack, logs in
//! with a bounded retry budget, discovers the notices for one docket, and
//! downloads each notice's document into a resumable, hash-verified
//! staging area. Finished documents cross the [`intake::InboxIntake`] seam
//! into the omnichannel inbox — never directly into a matter folder;
//! triage is the only path in.
//!
//! The failure states are where the design earns its keep: a missing or
//! malformed credential hard-stops before any browser opens, structure
//! drift hard-stops with the failing step named, transient failures ride
//! bounded exponential backoff, and a run that dies mid-download resumes
//! from the last byte offset without re-fetching a single completed
//! document.

pub(crate) mod adapter;
pub(crate) mod browser;
pub(crate) mod cmd;
pub(crate) mod credentials;
pub(crate) mod download;
pub(crate) mod fetcher;
pub(crate) mod fs;
pub(crate) mod intake;
pub(crate) mod retry;
pub(crate) mod session;
#[cfg(test)]
pub(crate) mod testing;

pub(crate) use cmd::{run, ScraperCmd};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::scraper::adapter::{AdapterError, NoticeKey, NoticeQuery, SiteAdapter};
use crate::scraper::credentials::CredentialsError;
use crate::scraper::download::{staging_name, DownloadError, DownloadLedger, DownloadSink};
use crate::scraper::intake::{InboxIntake, IntakeError, NamingError, ScrapedDocument};
use crate::scraper::retry::{with_retries, RetryClock, RetryFailure, RetryPolicy};
use crate::scraper::session::{BrowserSession, SessionError, SessionFactory};

/// Why a run failed. Every variant is named for the operator: which step,
/// what to check, and — for credentials — what to provision.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ScraperError {
    #[error(transparent)]
    Credentials(#[from] CredentialsError),
    #[error("the browser session failed")]
    Session(#[from] SessionError),
    #[error("login to `{site}` failed after {attempts} attempt(s): {last}")]
    LoginFailed {
        site: String,
        attempts: u32,
        last: String,
    },
    #[error("site structure changed: {step} no longer matches ({detail}); nothing was filed")]
    SiteStructure { step: String, detail: String },
    #[error("gave up on {step} after {attempts} attempt(s): {detail}")]
    TransientExhausted {
        step: String,
        attempts: u32,
        detail: String,
    },
    #[error("the download failed")]
    Download(#[from] DownloadError),
    #[error("the omnichannel intake failed")]
    Intake(#[from] IntakeError),
    #[error("documents could not be given their DKT filing names")]
    Naming(#[from] NamingError),
    #[error("the run state file is not valid")]
    State(#[from] serde_json::Error),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Run progress across restarts: the notices whose documents are staged
/// AND enqueued. A resumed run skips these before touching the network.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RunState {
    #[serde(default)]
    completed: BTreeSet<NoticeKey>,
}

impl RunState {
    /// Load the run state, if one exists; a missing file is a fresh run.
    ///
    /// # Errors
    ///
    /// When the file exists but cannot be parsed or read.
    pub(crate) fn load(path: &Path) -> Result<Self, ScraperError> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ScraperError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Whether `key` already finished in a previous (or earlier) attempt.
    #[must_use]
    pub(crate) fn contains(&self, key: &NoticeKey) -> bool {
        self.completed.contains(key)
    }

    /// Record `key` as done.
    pub(crate) fn mark(&mut self, key: NoticeKey) {
        self.completed.insert(key);
    }

    /// Persist atomically, so a crash mid-save cannot lose progress
    /// already enqueued.
    ///
    /// # Errors
    ///
    /// When the write fails.
    pub(crate) fn save(&self, path: &Path) -> Result<(), ScraperError> {
        let bytes = serde_json::to_vec_pretty(self)?;
        crate::scraper::fs::atomic_write(path, &bytes).map_err(|source| ScraperError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

/// One enqueued document, as the run report records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnqueueRecord {
    pub key: String,
    pub message_id: uuid::Uuid,
    pub deduped: bool,
    pub resumed: bool,
}

/// What one run did: the counts the operator skims first, then every
/// enqueued document by key and inbox message. This transcript is the
/// run's evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RunReport {
    pub site: String,
    pub docket: String,
    pub since: Option<String>,
    pub discovered: usize,
    pub downloaded: usize,
    pub resumed: usize,
    /// Notices skipped because the run state already marked them done.
    pub skipped_completed: usize,
    /// Notices enqueued straight from previously staged bytes (ledger hit,
    /// no fetch).
    pub skipped_filed: usize,
    pub enqueued: Vec<EnqueueRecord>,
    pub bytes: u64,
}

impl std::fmt::Display for RunReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.since {
            Some(at) => writeln!(
                formatter,
                "scrape of {} docket {} since {at}",
                self.site, self.docket
            )?,
            None => writeln!(formatter, "scrape of {} docket {}", self.site, self.docket)?,
        }
        writeln!(
            formatter,
            "  discovered {}; downloaded {}; resumed {}; staged already {}; marked done already {}",
            self.discovered,
            self.downloaded,
            self.resumed,
            self.skipped_filed,
            self.skipped_completed
        )?;
        for record in &self.enqueued {
            writeln!(
                formatter,
                "  enqueued {} as inbox message {}{}",
                record.key,
                record.message_id,
                if record.deduped { " (deduped)" } else { "" }
            )?;
        }
        write!(formatter, "  {} byte(s) staged", self.bytes)
    }
}

/// Everything a run needs, injected. Production wires the real store, KMS,
/// Chrome sessions, reqwest fetcher, and intake; orchestration tests
/// inject doubles for every one.
pub(crate) struct ScraperDeps<'a> {
    pub sessions: &'a dyn SessionFactory,
    pub surreal: &'a store::surreal::SurrealDb,
    pub firm_id: uuid::Uuid,
    pub kms: &'a dyn cloud::RuntimeKms,
    pub fetcher: &'a dyn crate::scraper::fetcher::HttpFetcher,
    pub intake: &'a dyn InboxIntake,
    pub clock: &'a dyn RetryClock,
    pub login_policy: RetryPolicy,
    pub fetch_policy: RetryPolicy,
    pub staging_dir: PathBuf,
    pub ledger_path: PathBuf,
    pub downloads_dir: PathBuf,
    pub run_state_path: PathBuf,
}

/// Run one site adapter over one docket: credentials, session, login,
/// discover, download, enqueue — with teardown on every path.
///
/// # Errors
///
/// Every failure state in the module docs; the error names the step and
/// the remedy.
pub(crate) async fn run_scraper(
    adapter: &dyn SiteAdapter,
    query: &NoticeQuery,
    deps: &ScraperDeps<'_>,
) -> Result<RunReport, ScraperError> {
    // The credential resolves before any browser opens: a missing or
    // malformed secret hard-stops with a provisioning error and never
    // launches a session.
    let resolved = credentials::resolve_portal_logins(deps.surreal, deps.firm_id, deps.kms).await?;
    let login = resolved.login_for(adapter.credential_key())?;
    login.ensure_complete(adapter.credential_key())?;

    let session = deps.sessions.open(adapter.site()).await?;
    let outcome = scrape_notices(adapter, query, deps, &session, login).await;
    if let Err(error) = session.shutdown().await {
        // Teardown runs on every path; report it only when it is the
        // primary failure rather than piling onto one.
        if outcome.is_ok() {
            return Err(error.into());
        }
        eprintln!("scraper: session teardown also failed: {error}");
    }
    outcome
}

/// The logged-in middle of a run: discover notices, download each one, and
/// enqueue each document. Split out so teardown can wrap it.
async fn scrape_notices(
    adapter: &dyn SiteAdapter,
    query: &NoticeQuery,
    deps: &ScraperDeps<'_>,
    session: &BrowserSession,
    login: &credentials::SiteLogin,
) -> Result<RunReport, ScraperError> {
    let mut report = RunReport {
        site: adapter.site().to_string(),
        docket: query.docket.clone(),
        since: query.since.map(|since| since.to_rfc3339()),
        ..RunReport::default()
    };

    // Login failure: bounded retries, then a hard stop — no lockout
    // loops. Structure drift during login stops on the first attempt.
    with_retries(&deps.login_policy, deps.clock, is_transient, || {
        adapter.login(session, login)
    })
    .await
    .map_err(|failure| {
        let attempts = failure.attempts;
        match failure.last {
            AdapterError::StructureDrift { step, detail } => {
                ScraperError::SiteStructure { step, detail }
            }
            last @ AdapterError::Transient { .. } => ScraperError::LoginFailed {
                site: adapter.site().to_string(),
                attempts,
                last: last.to_string(),
            },
        }
    })?;
    // Capture the logged-in session immediately: a cookie file from a
    // successful login is worth having even if discovery then fails.
    session.persist_cookies().await?;

    let notices = with_retries(&deps.fetch_policy, deps.clock, is_transient, || {
        adapter.notices(deps.fetcher, query)
    })
    .await
    .map_err(|failure| map_step_failure("notice discovery", failure))?;
    report.discovered = notices.len();

    let mut ledger = DownloadLedger::load(deps.ledger_path.clone())?;
    let mut run_state = RunState::load(&deps.run_state_path)?;
    crate::scraper::fs::ensure_dir(&deps.downloads_dir).map_err(|source| ScraperError::Io {
        path: deps.downloads_dir.clone(),
        source,
    })?;

    for notice in &notices {
        let key = notice.key(adapter.site(), &query.docket);
        if run_state.contains(&key) {
            report.skipped_completed += 1;
            continue;
        }
        let (sha256, bytes, resumed, newly_fetched) =
            fetch_document_bytes(adapter, deps, notice, &key, &mut ledger, &mut report).await?;

        let document = ScrapedDocument::from_download(
            adapter.site().clone(),
            &query.docket,
            notice,
            sha256,
            bytes,
        )?;
        let receipt = with_retries(
            &deps.fetch_policy,
            deps.clock,
            |error: &IntakeError| matches!(error, IntakeError::Transient { .. }),
            || deps.intake.enqueue(&document),
        )
        .await
        .map_err(|failure| ScraperError::TransientExhausted {
            step: format!("intake of {key}"),
            attempts: failure.attempts,
            detail: failure.last.to_string(),
        })?;
        run_state.mark(key.clone());
        run_state.save(&deps.run_state_path)?;
        report.enqueued.push(EnqueueRecord {
            key: key.to_string(),
            message_id: receipt.message_id,
            deduped: receipt.deduped,
            resumed,
        });
        if newly_fetched {
            report.downloaded += 1;
            if resumed {
                report.resumed += 1;
            }
        }
    }
    Ok(report)
}
/// The bytes of one notice document: read back from a download the ledger
/// already recorded, or streamed now through resumable retries. Counts the
/// bytes on whichever path ran.
async fn fetch_document_bytes(
    adapter: &dyn SiteAdapter,
    deps: &ScraperDeps<'_>,
    notice: &crate::scraper::adapter::NoticeRef,
    key: &NoticeKey,
    ledger: &mut DownloadLedger,
    report: &mut RunReport,
) -> Result<(String, Vec<u8>, bool, bool), ScraperError> {
    match ledger.completed(key) {
        Some(entry) if entry.path.is_file() => {
            report.skipped_filed += 1;
            report.bytes += entry.bytes;
            let bytes = std::fs::read(&entry.path).map_err(|source| ScraperError::Io {
                path: entry.path.clone(),
                source,
            })?;
            Ok((entry.sha256.clone(), bytes, false, false))
        }
        entry => {
            if entry.is_some() {
                ledger.forget(key)?;
            }
            let staged_path =
                deps.downloads_dir
                    .join(format!("{}.{}", staging_name(key), notice.extension));
            // Each attempt reopens the sink: a failed attempt's bytes are
            // already in the part file, so the next attempt — this run's
            // or a later run's — continues from that byte offset.
            let had_partial =
                DownloadSink::open(&deps.staging_dir, staged_path.clone(), key.clone())
                    .map(|sink| sink.resume_offset() > 0)?;
            let staging_dir = deps.staging_dir.clone();
            let final_path = staged_path.clone();
            let final_key = key.clone();
            // The closure owns a copy; the original key stays for the
            // intake bookkeeping in the caller.
            let closure_key = key.clone();
            with_retries(&deps.fetch_policy, deps.clock, is_transient, move || {
                // Owned clones move into the future, so nothing borrowed
                // from the closure body escapes across attempts.
                let staged_path = staged_path.clone();
                let staging_dir = staging_dir.clone();
                let key = closure_key.clone();
                async move {
                    let mut sink =
                        DownloadSink::open(&staging_dir, staged_path, key).map_err(|error| {
                            AdapterError::Transient {
                                step: "download staging".to_owned(),
                                detail: error.to_string(),
                            }
                        })?;
                    adapter.download(deps.fetcher, notice, &mut sink).await
                }
            })
            .await
            .map_err(|failure| map_step_failure(&format!("download of {key}"), failure))?;
            let sink = DownloadSink::open(&deps.staging_dir, final_path, final_key)?;
            let completed = sink.finish()?;
            ledger.record(completed.clone())?;
            report.bytes += completed.bytes;
            let bytes = std::fs::read(&completed.path).map_err(|source| ScraperError::Io {
                path: completed.path.clone(),
                source,
            })?;
            Ok((completed.sha256, bytes, had_partial, true))
        }
    }
}

/// Only transient adapter failures retry; structure drift is a hard stop.
fn is_transient(error: &AdapterError) -> bool {
    matches!(error, AdapterError::Transient { .. })
}

/// Map a step's retry failure onto the run error, naming the step.
fn map_step_failure(step: &str, failure: RetryFailure<AdapterError>) -> ScraperError {
    match failure.last {
        AdapterError::StructureDrift { step, detail } => {
            ScraperError::SiteStructure { step, detail }
        }
        AdapterError::Transient {
            step: transient_step,
            detail,
        } => ScraperError::TransientExhausted {
            step: format!("{step}: {transient_step}"),
            attempts: failure.attempts,
            detail,
        },
    }
}

/// The adapter registry: every portal the framework can scrape today. Site
/// adapters register here as they land — the La Superior adapter is the
/// first.
#[must_use]
pub(crate) fn registered_adapters() -> Vec<Box<dyn SiteAdapter>> {
    vec![]
}

#[cfg(test)]
mod tests {
    use super::{
        registered_adapters, run_scraper, EnqueueRecord, NoticeQuery, RunReport, ScraperDeps,
        ScraperError,
    };
    use crate::scraper::adapter::{find_adapter, AdapterError, SiteKey};
    use crate::scraper::intake::RecordingIntake;
    use crate::scraper::retry::RetryPolicy;
    use crate::scraper::testing::{
        DownloadStep, LoginStep, MockAdapter, MockFetcher, StubClock, StubSessions,
    };
    use std::path::PathBuf;
    use std::time::Duration;

    fn site() -> SiteKey {
        SiteKey::new("la-superior").expect("valid key")
    }

    fn notice(entry: &str, title: &str) -> crate::scraper::adapter::NoticeRef {
        crate::scraper::adapter::NoticeRef {
            entry_number: entry.to_owned(),
            title: title.to_owned(),
            url: format!("https://portal.example.com/doc/{entry}"),
            extension: "pdf".to_owned(),
        }
    }

    fn instant_policy(max_attempts: u32) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            base_delay: Duration::from_millis(1),
        }
    }

    struct Fixture {
        surreal: store::surreal::SurrealDb,
        admin_id: uuid::Uuid,
        firm_id: uuid::Uuid,
        kms: cloud::FakeKms,
        factory: StubSessions,
        fetcher: MockFetcher,
        intake: RecordingIntake,
        clock: StubClock,
        root: PathBuf,
    }

    async fn fixture() -> Fixture {
        let surreal = store::test_support::mem_surreal().await;
        let (firm_id, admin_id) = crate::scraper::testing::seeded_firm_with_admin(&surreal).await;
        Fixture {
            surreal,
            admin_id,
            firm_id,
            kms: cloud::FakeKms::new("scraper-test"),
            factory: StubSessions::default(),
            fetcher: MockFetcher::never(),
            intake: RecordingIntake::default(),
            clock: StubClock,
            root: std::env::temp_dir().join(format!("navigator-scraper-{}", uuid::Uuid::now_v7())),
        }
    }

    async fn provisioned() -> Fixture {
        let fixture = fixture().await;
        fixture
            .put_portal_secret(&serde_json::json!({
                "la-superior": {"username": "firm@example.com", "password": "staple"}
            }))
            .await;
        fixture
    }

    impl Fixture {
        fn site_dir(&self) -> PathBuf {
            self.root.join("la-superior")
        }

        fn run_state_path(&self) -> PathBuf {
            self.site_dir().join("run-state.json")
        }

        fn downloads_dir(&self) -> PathBuf {
            self.site_dir().join("documents")
        }

        fn staged_dir(&self) -> PathBuf {
            self.site_dir().join("staging")
        }

        async fn put_portal_secret(&self, value: &serde_json::Value) {
            crate::scraper::testing::put_portal_secret(
                &self.surreal,
                self.admin_id,
                self.firm_id,
                value,
                &self.kms,
            )
            .await;
        }
    }

    fn deps(fixture: &Fixture) -> ScraperDeps<'_> {
        ScraperDeps {
            sessions: &fixture.factory,
            surreal: &fixture.surreal,
            firm_id: fixture.firm_id,
            kms: &fixture.kms,
            fetcher: &fixture.fetcher,
            intake: &fixture.intake,
            clock: &fixture.clock,
            login_policy: instant_policy(3),
            fetch_policy: instant_policy(3),
            staging_dir: fixture.staged_dir(),
            ledger_path: fixture.staged_dir().join("ledger.json"),
            downloads_dir: fixture.downloads_dir(),
            run_state_path: fixture.run_state_path(),
        }
    }

    fn query() -> NoticeQuery {
        NoticeQuery {
            docket: "0004".to_owned(),
            since: None,
        }
    }

    #[test]
    fn the_registry_looks_adapters_up_by_site_key() {
        let adapters: Vec<Box<dyn crate::scraper::adapter::SiteAdapter>> =
            vec![Box::new(MockAdapter::simple(site()))];
        assert!(
            find_adapter(&registered_adapters(), &site()).is_none(),
            "no real adapters yet"
        );
        let found = find_adapter(&adapters, &site()).expect("the mock is found");
        assert_eq!(found.site(), &site());
        assert!(find_adapter(&adapters, &SiteKey::new("pacer").expect("valid key")).is_none());
    }

    #[tokio::test]
    async fn a_complete_run_enqueues_every_notice_under_dkt_names() {
        let fixture = provisioned().await;
        let mut adapter = MockAdapter::simple(site());
        adapter.notices_script(vec![Ok(vec![
            notice("4", "Notice of Hearing"),
            notice("5", "Order Granting Extension"),
        ])]);
        adapter.download_script(vec![
            vec![DownloadStep::Chunk(b"PDF-A".to_vec())],
            vec![DownloadStep::Chunk(b"PDF-B".to_vec())],
        ]);
        let report = run_scraper(&adapter, &query(), &deps(&fixture))
            .await
            .expect("the run completes");
        assert_eq!(report.discovered, 2);
        assert_eq!(report.downloaded, 2);
        assert_eq!(report.enqueued.len(), 2);
        assert_eq!(report.bytes, 10);
        let received = fixture.intake.received();
        assert_eq!(received.len(), 2);
        assert_eq!(received[0].filename, "DKT 0004 - Notice of Hearing.pdf");
        assert_eq!(received[0].bytes, b"PDF-A".to_vec());
        assert_eq!(
            received[1].filename,
            "DKT 0005 - Order Granting Extension.pdf"
        );
        assert!(fixture.run_state_path().exists(), "run state persists");
        assert_eq!(fixture.factory.opens(), 1, "exactly one browser session");
    }

    #[tokio::test]
    async fn a_second_run_refetches_nothing() {
        let fixture = provisioned().await;
        let mut adapter = MockAdapter::simple(site());
        adapter.notices_script(vec![
            Ok(vec![notice("4", "Notice of Hearing")]),
            Ok(vec![notice("4", "Notice of Hearing")]),
        ]);
        adapter.download_script(vec![vec![DownloadStep::Chunk(b"PDF-A".to_vec())]]);
        let first = run_scraper(&adapter, &query(), &deps(&fixture))
            .await
            .expect("the first run completes");
        let second = run_scraper(&adapter, &query(), &deps(&fixture))
            .await
            .expect("the second run completes");
        assert_eq!(first.downloaded, 1);
        assert_eq!(second.downloaded, 0);
        assert_eq!(second.skipped_completed, 1, "the run state marks them done");
        assert_eq!(second.enqueued.len(), 0, "nothing is re-enqueued");
        assert_eq!(
            fixture.fetcher.request_count("portal.example.com"),
            0,
            "not even the listing needs refetching when the run state covers it"
        );
        assert_eq!(fixture.intake.received().len(), 1);
        assert_eq!(adapter.download_calls(), 1);
    }

    #[tokio::test]
    async fn a_second_run_enqueues_a_staged_document_without_refetching() {
        let fixture = provisioned().await;
        let mut adapter = MockAdapter::simple(site());
        adapter.notices_script(vec![
            Ok(vec![notice("7", "Status Report")]),
            Ok(vec![notice("7", "Status Report")]),
        ]);
        // Run one downloads and stages the bytes, but the intake is down
        // and the transient budget is one: the run dies after staging.
        let failing = crate::scraper::intake::FailingIntake;
        let mut failed_deps = deps(&fixture);
        failed_deps.intake = &failing;
        failed_deps.fetch_policy = instant_policy(1);
        adapter.download_script(vec![vec![DownloadStep::Chunk(b"staged-bytes".to_vec())]]);
        let error = run_scraper(&adapter, &query(), &failed_deps)
            .await
            .expect_err("the run dies before intake");
        assert!(
            error.to_string().contains("intake of la-superior/0004/7"),
            "the error names the step: {error}"
        );
        assert!(fixture.intake.received().is_empty());
        // Run two picks the staged bytes up from the ledger — no fetch, no
        // re-download — and enqueues them.
        let report = run_scraper(&adapter, &query(), &deps(&fixture))
            .await
            .expect("the resumed run completes");
        assert_eq!(report.skipped_filed, 1);
        assert_eq!(adapter.download_calls(), 1, "no second download");
        let received = fixture.intake.received();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].bytes, b"staged-bytes".to_vec());
        assert_eq!(received[0].filename, "DKT 0007 - Status Report.pdf");
    }

    #[tokio::test]
    async fn an_interrupted_download_retries_and_completes() {
        use sha2::{Digest, Sha256};

        let fixture = provisioned().await;
        let mut adapter = MockAdapter::simple(site());
        adapter.notices_script(vec![Ok(vec![notice("4", "Notice of Hearing")])]);
        // Attempt one writes half the bytes and dies mid-transfer; attempt
        // two continues the same sink and finishes the document.
        adapter.download_script(vec![
            vec![
                DownloadStep::Chunk(b"first-half-".to_vec()),
                DownloadStep::Fail(AdapterError::Transient {
                    step: "download".to_owned(),
                    detail: "connection reset".to_owned(),
                }),
            ],
            vec![DownloadStep::Chunk(b"second-half".to_vec())],
        ]);
        let report = run_scraper(&adapter, &query(), &deps(&fixture))
            .await
            .expect("the retry completes the download");
        assert_eq!(report.downloaded, 1);
        assert_eq!(report.resumed, 0, "the retry never closed the sink");
        let received = fixture.intake.received();
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].bytes, b"first-half-second-half".to_vec());
        let mut hasher = Sha256::new();
        hasher.update(b"first-half-second-half");
        let expected = crate::scraper::download::hex_digest(hasher.finalize().as_slice());
        assert_eq!(received[0].sha256, expected);
        assert_eq!(
            report.enqueued[0].key, "la-superior/0004/4",
            "the notice key is stable and visible in the report"
        );
    }

    #[tokio::test]
    async fn a_partial_file_never_reaches_the_intake_when_the_run_dies() {
        let fixture = provisioned().await;
        let mut adapter = MockAdapter::simple(site());
        adapter.notices_script(vec![Ok(vec![notice("4", "Notice of Hearing")])]);
        adapter.download_script(vec![vec![
            DownloadStep::Chunk(b"partial".to_vec()),
            DownloadStep::Fail(AdapterError::Transient {
                step: "download".to_owned(),
                detail: "connection reset".to_owned(),
            }),
        ]]);
        // max_attempts = 1: the single failure exhausts the budget.
        let mut hard_deps = deps(&fixture);
        hard_deps.fetch_policy = instant_policy(1);
        let error = run_scraper(&adapter, &query(), &hard_deps)
            .await
            .expect_err("the run hard-stops");
        assert!(matches!(error, ScraperError::TransientExhausted { .. }));
        assert!(
            fixture.intake.received().is_empty(),
            "no partial document enqueued"
        );
        assert_eq!(
            std::fs::read_dir(fixture.downloads_dir()).map_or(0, std::iter::Iterator::count),
            0,
            "no finished document was staged"
        );
        // The next run resumes from the part file and completes.
        adapter.notices_script(vec![Ok(vec![notice("4", "Notice of Hearing")])]);
        adapter.download_script(vec![vec![DownloadStep::Chunk(b"-rest".to_vec())]]);
        let report = run_scraper(&adapter, &query(), &deps(&fixture))
            .await
            .expect("the resumed run completes");
        assert_eq!(report.resumed, 1, "the second run resumed the partial");
        assert_eq!(fixture.intake.received()[0].bytes, b"partial-rest".to_vec());
    }

    #[tokio::test]
    async fn login_retries_are_bounded_then_the_run_stops() {
        let fixture = provisioned().await;
        let mut adapter = MockAdapter::simple(site());
        adapter.login_script(vec![
            LoginStep::Transient,
            LoginStep::Transient,
            LoginStep::Transient,
        ]);
        let error = run_scraper(&adapter, &query(), &deps(&fixture))
            .await
            .expect_err("login exhausts the budget");
        assert!(
            matches!(&error, ScraperError::LoginFailed { attempts, .. } if *attempts == 3),
            "bounded retries, then a hard stop: {error}"
        );
        assert_eq!(fixture.intake.received().len(), 0);
    }

    #[tokio::test]
    async fn login_recovers_after_transient_failures() {
        let fixture = provisioned().await;
        let mut adapter = MockAdapter::simple(site());
        adapter.login_script(vec![LoginStep::Transient, LoginStep::Succeed]);
        adapter.notices_script(vec![Ok(vec![])]);
        run_scraper(&adapter, &query(), &deps(&fixture))
            .await
            .expect("login recovers");
        assert_eq!(adapter.login_calls(), 2);
        let seen = adapter.seen_logins();
        assert_eq!(seen.len(), 2, "every attempt sees the same resolved login");
        assert_eq!(
            seen[0],
            ("firm@example.com".to_owned(), "staple".to_owned())
        );
        assert_eq!(seen[1], seen[0]);
        assert_eq!(fixture.factory.opens(), 1, "one session across the retry");
    }

    #[tokio::test]
    async fn structure_drift_stops_the_run_immediately() {
        let fixture = provisioned().await;
        let mut adapter = MockAdapter::simple(site());
        adapter.login_script(vec![LoginStep::Structure]);
        let error = run_scraper(&adapter, &query(), &deps(&fixture))
            .await
            .expect_err("drift hard-stops");
        assert!(matches!(error, ScraperError::SiteStructure { .. }));
        assert_eq!(adapter.login_calls(), 1, "no retry for drift");
    }

    #[tokio::test]
    async fn drift_in_discovery_files_nothing() {
        let fixture = provisioned().await;
        let mut adapter = MockAdapter::simple(site());
        adapter.notices_script(vec![Err(AdapterError::StructureDrift {
            step: "notice list".to_owned(),
            detail: "selector not found".to_owned(),
        })]);
        let error = run_scraper(&adapter, &query(), &deps(&fixture))
            .await
            .expect_err("drift hard-stops");
        assert!(
            matches!(&error, ScraperError::SiteStructure { step, .. } if step == "notice list")
        );
        assert_eq!(fixture.intake.received().len(), 0);
        assert_eq!(adapter.download_calls(), 0);
    }

    #[tokio::test]
    async fn a_missing_credential_hard_stops_before_any_browser_opens() {
        let fixture = fixture().await;
        let adapter = MockAdapter::simple(site());
        let error = run_scraper(&adapter, &query(), &deps(&fixture))
            .await
            .expect_err("nothing is provisioned");
        assert!(
            matches!(error, ScraperError::Credentials(_)),
            "the run stops at the credential seam: {error}"
        );
        assert_eq!(fixture.factory.opens(), 0, "no browser ever opens");
        assert_eq!(fixture.intake.received().len(), 0);
    }

    #[tokio::test]
    async fn a_malformed_credential_payload_hard_stops_before_any_browser_opens() {
        let fixture = fixture().await;
        fixture.put_portal_secret(&serde_json::json!([])).await;
        let adapter = MockAdapter::simple(site());
        let error = run_scraper(&adapter, &query(), &deps(&fixture))
            .await
            .expect_err("the payload is not an object");
        assert!(
            error
                .to_string()
                .contains("not shaped as a portal login payload"),
            "the error names the remedy: {error}"
        );
        assert_eq!(fixture.factory.opens(), 0);
    }

    #[tokio::test]
    async fn an_intake_rejection_hard_stops_with_the_step_named() {
        let fixture = provisioned().await;
        let mut adapter = MockAdapter::simple(site());
        adapter.notices_script(vec![Ok(vec![notice("4", "Notice of Hearing")])]);
        adapter.download_script(vec![vec![DownloadStep::Chunk(b"PDF-A".to_vec())]]);
        let rejecting = crate::scraper::intake::FailingIntake;
        let mut deps = deps(&fixture);
        deps.intake = &rejecting;
        let error = run_scraper(&adapter, &query(), &deps)
            .await
            .expect_err("intake rejects");
        assert!(
            error.to_string().contains("intake of la-superior/0004/4"),
            "the error names the step: {error}"
        );
    }

    #[tokio::test]
    async fn a_run_report_prints_its_transcript() {
        let report = RunReport {
            site: "la-superior".to_owned(),
            docket: "0004".to_owned(),
            since: None,
            discovered: 1,
            downloaded: 1,
            resumed: 0,
            skipped_completed: 0,
            skipped_filed: 0,
            enqueued: vec![EnqueueRecord {
                key: "la-superior/0004/4".to_owned(),
                message_id: uuid::Uuid::now_v7(),
                deduped: false,
                resumed: false,
            }],
            bytes: 5,
        };
        let rendered = format!("{report}");
        assert!(rendered.contains("scrape of la-superior docket 0004"));
        assert!(rendered.contains("discovered 1; downloaded 1"));
        assert!(rendered.contains("la-superior/0004/4"));
        assert!(rendered.contains("5 byte(s) staged"));
    }

    #[test]
    fn run_state_serializes_across_processes() {
        let key = crate::scraper::adapter::NoticeKey::new(&site(), "0004", "12");
        let mut state = super::RunState::default();
        assert!(!state.contains(&key));
        state.mark(key.clone());
        assert!(state.contains(&key));
        let path =
            std::env::temp_dir().join(format!("navigator-run-state-{}", uuid::Uuid::now_v7()));
        state.save(&path).expect("save run state");
        let reloaded = super::RunState::load(&path).expect("load run state");
        assert_eq!(reloaded, state);
        std::fs::remove_file(&path).expect("clean up the temp file");
    }

    #[test]
    fn a_missing_run_state_file_is_a_fresh_run() {
        let path =
            std::env::temp_dir().join(format!("navigator-run-state-{}", uuid::Uuid::now_v7()));
        let state = super::RunState::load(&path).expect("a missing file is empty");
        assert_eq!(state, super::RunState::default());
    }
}
