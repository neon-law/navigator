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
pub(crate) mod adapters;
pub(crate) mod browser;
pub(crate) mod cmd;
pub(crate) mod config;
pub(crate) mod credentials;
pub(crate) mod download;
pub(crate) mod fetcher;
pub(crate) mod file;
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
    #[error(
        "the portal refused access: {step} ({detail}); this surface may need a credential the \
         Firm has not provisioned"
    )]
    AccessDenied { step: String, detail: String },
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
    pub application_store: &'a store::application_store::ApplicationStore,
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
    // launches a session. The exception is a public site (`requires_login`
    // is `false`): a credential that simply is not provisioned is fine
    // there — the run proceeds unauthenticated — while a store failure or
    // a malformed provisioned row still hard-stops, never silently ignored.
    let resolved =
        credentials::resolve_portal_logins(deps.application_store, deps.firm_id, deps.kms).await;
    let resolved = match resolved {
        Ok(resolved) => resolved,
        Err(CredentialsError::NotConfigured { .. }) if !adapter.requires_login() => {
            // A public site needs no provisioned credential: the run
            // proceeds unauthenticated with an empty set of logins.
            credentials::PortalLogins::default()
        }
        Err(error) => return Err(error.into()),
    };
    let login = match resolved.login_for(adapter.credential_key()) {
        Ok(login) => {
            login.ensure_complete(adapter.credential_key())?;
            Some(login)
        }
        Err(CredentialsError::NotProvisioned { .. }) if !adapter.requires_login() => None,
        Err(error) => return Err(error.into()),
    };

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
    login: Option<&credentials::SiteLogin>,
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
            AdapterError::AccessDenied { step, detail } => {
                ScraperError::AccessDenied { step, detail }
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
        AdapterError::AccessDenied { step, detail } => ScraperError::AccessDenied { step, detail },
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
/// adapters register here as they land — La Superior first, PACER as the
/// second.
#[must_use]
pub(crate) fn registered_adapters() -> Vec<Box<dyn SiteAdapter>> {
    let mut adapters: Vec<Box<dyn SiteAdapter>> = Vec::new();
    // The checked-in key literals are valid, so a failure here is a bug,
    // not a runtime state; surface it loudly rather than panic. `scraper
    // run` reports an unregistered site and `sites` lists the rest.
    match crate::scraper::adapters::LaSuperiorAdapter::new(
        crate::scraper::adapters::la_superior::DEFAULT_BASE_URL,
    ) {
        Ok(adapter) => adapters.push(Box::new(adapter)),
        Err(error) => {
            eprintln!("scraper: the built-in la-superior site key is invalid: {error}");
        }
    }
    match crate::scraper::adapters::PacerAdapter::new(
        crate::scraper::adapters::pacer::DEFAULT_BASE_URL,
    ) {
        Ok(adapter) => adapters.push(Box::new(adapter)),
        Err(error) => {
            eprintln!("scraper: the built-in pacer site key is invalid: {error}");
        }
    }
    adapters
}

#[cfg(test)]
#[path = "orchestration_tests.rs"]
mod tests;
