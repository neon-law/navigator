//! Test doubles for the scraper framework. Everything here is
//! `cfg(test)`-only: no orchestration test launches a browser or opens a
//! network connection, and the mock adapter's fetch path goes through the
//! same injected [`HttpFetcher`] seam a real adapter must use.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::scraper::adapter::{AdapterError, NoticeQuery, NoticeRef, SiteAdapter, SiteKey};
use crate::scraper::credentials::SiteLogin;
use crate::scraper::download::{fetch_into, DownloadSink};
use crate::scraper::fetcher::{FetchError, FetchRequest, FetchResponse, HttpFetcher};
use crate::scraper::retry::RetryClock;
use crate::scraper::session::{BrowserSession, SessionError, SessionFactory};

/// One scripted login attempt.
#[derive(Debug)]
pub(crate) enum LoginStep {
    /// The login fails transiently (portal restarted, network blip).
    Transient,
    /// The login fails because the portal's structure drifted.
    Structure,
    /// The login succeeds.
    Succeed,
}

/// One scripted download attempt: a sequence of writes and failures.
#[derive(Debug)]
pub(crate) enum DownloadStep {
    /// Write these bytes to the sink.
    Chunk(Vec<u8>),
    /// Fail the whole attempt transiently (bytes already written stay
    /// staged in the part file).
    Fail(AdapterError),
}

/// A canned listing body the mock adapter's fetch path parses:
/// `entry|title|url|extension` per line, `#` lines are comments.
///
/// # Errors
///
/// Structure drift when the body is not UTF-8 or a row does not have
/// exactly four fields — the same hard-stop shape a real parser returns.
pub(crate) fn parse_listing(body: &[u8]) -> Result<Vec<NoticeRef>, AdapterError> {
    let text = std::str::from_utf8(body).map_err(|_| AdapterError::StructureDrift {
        step: "notice list".to_owned(),
        detail: "the body is not UTF-8".to_owned(),
    })?;
    let mut notices = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.trim().starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('|').collect();
        let [entry, title, url, extension] = fields.as_slice() else {
            return Err(AdapterError::StructureDrift {
                step: "notice list".to_owned(),
                detail: format!("a row has {} fields, expected 4", fields.len()),
            });
        };
        notices.push(NoticeRef {
            entry_number: entry.trim().to_owned(),
            title: title.trim().to_owned(),
            url: url.trim().to_owned(),
            extension: extension.trim().to_owned(),
        });
    }
    Ok(notices)
}

struct MockAdapterCore {
    login_script: VecDeque<LoginStep>,
    notices_script: VecDeque<Result<Vec<NoticeRef>, AdapterError>>,
    download_script: VecDeque<Vec<DownloadStep>>,
    login_calls: u32,
    notices_calls: u32,
    download_calls: u32,
    seen_logins: Vec<(String, String)>,
}

/// A scriptable [`SiteAdapter`]: scripted steps play back per call, and
/// with no script for a step the mock does the real thing through the
/// injected fetcher — list from a canned listing URL, download with
/// [`fetch_into`]. This is the adapter contract test's subject.
pub(crate) struct MockAdapter {
    site: SiteKey,
    core: Arc<Mutex<MockAdapterCore>>,
}

impl MockAdapter {
    /// A mock adapter with empty scripts: login succeeds, discovery and
    /// download go through the fetcher.
    #[must_use]
    pub(crate) fn simple(site: SiteKey) -> Self {
        Self {
            site,
            core: Arc::new(Mutex::new(MockAdapterCore {
                login_script: VecDeque::new(),
                notices_script: VecDeque::new(),
                download_script: VecDeque::new(),
                login_calls: 0,
                notices_calls: 0,
                download_calls: 0,
                seen_logins: Vec::new(),
            })),
        }
    }

    /// Replace the login script; consumed front-to-back, one step per
    /// call, and an exhausted script means success.
    pub(crate) fn login_script(&mut self, steps: Vec<LoginStep>) {
        if let Ok(mut core) = self.core.lock() {
            core.login_script = steps.into();
        }
    }

    /// Replace the notices script; entries are returned verbatim, one per
    /// call, without touching the fetcher.
    pub(crate) fn notices_script(&mut self, results: Vec<Result<Vec<NoticeRef>, AdapterError>>) {
        if let Ok(mut core) = self.core.lock() {
            core.notices_script = results.into();
        }
    }

    /// Replace the download script; each download call consumes one
    /// attempt (a Vec of steps), and an exhausted script falls through to
    /// the fetch path.
    pub(crate) fn download_script(&mut self, attempts: Vec<Vec<DownloadStep>>) {
        if let Ok(mut core) = self.core.lock() {
            core.download_script = attempts.into();
        }
    }

    /// How many times `login` ran.
    #[must_use]
    pub(crate) fn login_calls(&self) -> u32 {
        self.core.lock().map_or(0, |core| core.login_calls)
    }

    /// How many times `download` ran.
    #[must_use]
    pub(crate) fn download_calls(&self) -> u32 {
        self.core.lock().map_or(0, |core| core.download_calls)
    }

    /// The `(username, password)` every login attempt saw, in order —
    /// the proof the framework resolved and handed over the Firm secret.
    #[must_use]
    pub(crate) fn seen_logins(&self) -> Vec<(String, String)> {
        self.core
            .lock()
            .map(|core| core.seen_logins.clone())
            .unwrap_or_default()
    }
}

fn transient(step: &str, detail: String) -> AdapterError {
    AdapterError::Transient {
        step: step.to_owned(),
        detail,
    }
}

#[async_trait::async_trait]
impl SiteAdapter for MockAdapter {
    fn site(&self) -> &SiteKey {
        &self.site
    }

    fn credential_key(&self) -> &str {
        self.site.as_str()
    }

    async fn login(
        &self,
        _session: &BrowserSession,
        login: &SiteLogin,
    ) -> Result<(), AdapterError> {
        let step = {
            let mut core = self.core.lock().expect("mock adapter lock");
            core.login_calls += 1;
            core.seen_logins
                .push((login.username().to_owned(), login.password().to_owned()));
            core.login_script.pop_front()
        };
        match step {
            Some(LoginStep::Transient) | Some(LoginStep::Succeed) | None => Ok(()),
            Some(LoginStep::Structure) => Err(AdapterError::StructureDrift {
                step: "login form".to_owned(),
                detail: "the sign-in button moved".to_owned(),
            }),
        }
    }

    async fn notices(
        &self,
        fetcher: &dyn HttpFetcher,
        query: &NoticeQuery,
    ) -> Result<Vec<NoticeRef>, AdapterError> {
        let scripted = {
            let mut core = self.core.lock().expect("mock adapter lock");
            core.notices_calls += 1;
            core.notices_script.pop_front()
        };
        if let Some(result) = scripted {
            result
        } else {
            let url = format!(
                "https://portal.example.com/{}/notices?docket={}",
                self.site.as_str(),
                query.docket
            );
            let request = FetchRequest {
                url,
                range_start: None,
            };
            let response = fetcher
                .fetch(&request)
                .await
                .map_err(|error| transient("fetch notice list", error.to_string()))?;
            parse_listing(&response.body)
        }
    }

    async fn download(
        &self,
        fetcher: &dyn HttpFetcher,
        notice: &NoticeRef,
        sink: &mut DownloadSink,
    ) -> Result<(), AdapterError> {
        let attempt = {
            let mut core = self.core.lock().expect("mock adapter lock");
            core.download_calls += 1;
            core.download_script.pop_front()
        };
        let scripted = attempt.unwrap_or_default();
        for step in &scripted {
            match step {
                DownloadStep::Chunk(bytes) => sink
                    .extend(bytes)
                    .map_err(|error| transient("sink write", error.to_string()))?,
                DownloadStep::Fail(error) => return Err(error.clone()),
            }
        }
        if !scripted.is_empty() {
            return Ok(());
        }
        // No script: download for real through the injected fetcher, the
        // same way a production adapter must.
        fetch_into(sink, fetcher, &notice.url)
            .await
            .map_err(|error| transient("fetch document", error.to_string()))
    }
}

type MockHandler = Arc<dyn Fn(&FetchRequest) -> Result<FetchResponse, FetchError> + Send + Sync>;

/// A fetcher double: records every request and answers through a canned
/// handler. Tests assert on the request record — the mock never opens a
/// connection.
pub(crate) struct MockFetcher {
    requests: Arc<Mutex<Vec<FetchRequest>>>,
    handler: MockHandler,
}

impl MockFetcher {
    /// A fetcher whose handler panics if used — for tests where no fetch
    /// should happen at all.
    #[must_use]
    pub(crate) fn never() -> Self {
        Self::with_handler(Arc::new(|request: &FetchRequest| {
            panic!("MockFetcher::never was asked to fetch {}", request.url);
        }))
    }

    /// A fetcher backed by an arbitrary handler.
    #[must_use]
    pub(crate) fn with_handler(handler: MockHandler) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            handler,
        }
    }

    /// How many requests named a URL containing `needle`.
    #[must_use]
    pub(crate) fn request_count(&self, needle: &str) -> usize {
        self.requests.lock().map_or(0, |requests| {
            requests
                .iter()
                .filter(|request| request.url.contains(needle))
                .count()
        })
    }
}

#[async_trait::async_trait]
impl HttpFetcher for MockFetcher {
    async fn fetch(&self, request: &FetchRequest) -> Result<FetchResponse, FetchError> {
        if let Ok(mut requests) = self.requests.lock() {
            requests.push(request.clone());
        }
        (self.handler)(request)
    }
}

/// A session factory that never launches a browser: it counts opens and
/// hands out offline sessions.
#[derive(Debug, Default)]
pub(crate) struct StubSessions {
    opens: Arc<AtomicU32>,
}

impl StubSessions {
    /// How many sessions were opened.
    #[must_use]
    pub(crate) fn opens(&self) -> u32 {
        self.opens.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl SessionFactory for StubSessions {
    async fn open(&self, site: &SiteKey) -> Result<BrowserSession, SessionError> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(BrowserSession::offline(
            site,
            std::env::temp_dir().join(format!("navigator-session-{}", uuid::Uuid::now_v7())),
        ))
    }
}

/// A clock that sleeps zero: retry loops in tests run instantly, and the
/// recorded delays (see `retry.rs`) are what tests assert on.
#[derive(Debug, Default)]
pub(crate) struct StubClock;

#[async_trait::async_trait]
impl RetryClock for StubClock {
    async fn sleep(&self, _delay: Duration) {}
}

/// Seed a Firm whose Admin DRI may write Firm secrets, and return the
/// Firm id with the admin person's id. Only an Admin DRI member holds
/// the `ManageIntegrationSecrets` capability (`store::firm_secrets`'s
/// authorize step), so fixtures that provision portal logins need the
/// admin's handle — which `test_support::seed_firm_with_brand` does not
/// return.
pub(crate) async fn seeded_firm_with_admin(
    surreal: &store::surreal::SurrealDb,
) -> (uuid::Uuid, uuid::Uuid) {
    let admin = store::persons::create(
        surreal,
        &store::persons::NewPerson::with_role(
            "Scraper Admin",
            format!("scraper-admin-{}@example.com", uuid::Uuid::now_v7()),
            store::persons::Role::Admin,
        ),
    )
    .await
    .expect("seed the firm admin");
    let firm = store::firms::create(
        surreal,
        &store::firms::NewFirm {
            name: format!("Scraper Firm {}", uuid::Uuid::now_v7()),
            status: "active".to_owned(),
            entity_id: store::test_support::seed_entity(surreal).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .expect("seed the firm");
    (firm.id, admin.id)
}

/// Store the Firm's `CourtPortal`/`PortalLogin` secret — one JSON body
/// keyed by site — through the real `store::firm_secrets` lane with the
/// in-process KMS fake. No new secrets mechanism, in tests or out.
pub(crate) async fn put_portal_secret(
    surreal: &store::surreal::SurrealDb,
    admin_id: uuid::Uuid,
    firm_id: uuid::Uuid,
    value: &serde_json::Value,
) {
    store::firm_secrets::put(
        surreal,
        store::firm_secrets::SecretPutRequest {
            actor_role: store::persons::Role::Admin,
            actor_person_id: Some(admin_id),
            firm_id,
            provider: store::firm_secrets::IntegrationProvider::CourtPortal,
            kind: store::firm_secrets::IntegrationSecretKind::PortalLogin,
            value: &serde_json::to_string(value).expect("the payload serializes"),
        },
        &cloud::FakeKms::new("scraper-test"),
    )
    .await
    .expect("provision the portal login secret");
}
