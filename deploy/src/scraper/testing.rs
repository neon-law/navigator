//! Test doubles for the scraper framework. Everything here is
//! `cfg(test)`-only: no orchestration test launches a browser or opens a
//! network connection, and the mock adapter's fetch path goes through the
//! same injected [`HttpFetcher`] seam a real adapter must use.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::scraper::adapter::{AdapterError, NoticeQuery, NoticeRef, SiteAdapter, SiteKey};
use crate::scraper::credentials::SiteLogin;
use crate::scraper::download::{DownloadError, DownloadSink};
use crate::scraper::fetcher::{FetchError, FetchRequest, FetchResponse, HttpFetcher};
use crate::scraper::intake::{InboxIntake, IntakeError, IntakeReceipt, ScrapedDocument};
use crate::scraper::session::{BrowserSession, SessionError, SessionFactory};

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
    notices_script: VecDeque<Result<Vec<NoticeRef>, AdapterError>>,
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
                notices_script: VecDeque::new(),
                login_calls: 0,
                notices_calls: 0,
                download_calls: 0,
                seen_logins: Vec::new(),
            })),
        }
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
        login: Option<&SiteLogin>,
    ) -> Result<(), AdapterError> {
        let mut core = self.core.lock().expect("mock adapter lock");
        core.login_calls += 1;
        if let Some(login) = login {
            core.seen_logins
                .push((login.username().to_owned(), login.password().to_owned()));
        }
        Ok(())
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
                headers: std::collections::BTreeMap::new(),
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
        self.core.lock().expect("mock adapter lock").download_calls += 1;
        // No script: download for real through the injected fetcher, the
        // same way a production adapter must.
        fetch_into(sink, fetcher, &notice.url)
            .await
            .map_err(|error| transient("fetch document", error.to_string()))
    }
}

pub(crate) type MockHandler =
    Arc<dyn Fn(&FetchRequest) -> Result<FetchResponse, FetchError> + Send + Sync>;

/// A fetcher double: records every request and answers through a canned
/// handler. Tests assert on the request record — the mock never opens a
/// connection.
pub(crate) struct MockFetcher {
    requests: Arc<Mutex<Vec<FetchRequest>>>,
    handler: MockHandler,
}

impl MockFetcher {
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

    /// Snapshots of every recorded request, in order — for assertions on
    /// headers and shape that a URL needle cannot see.
    #[must_use]
    pub(crate) fn requests(&self) -> Vec<FetchRequest> {
        self.requests
            .lock()
            .map_or_else(|_| Vec::new(), |requests| requests.clone())
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

impl StubSessions {}

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

/// Fetch `url` into `sink`, resuming: the first request carries no `Range`
/// header, a resumed request carries `bytes=<offset>-`, and a server that
/// answers a range request with a full `200` body discards the stale part
/// and restarts. The scripted adapters' no-script download path fills its
/// sink through this helper, so orchestration tests exercise the same
/// resume semantics a production adapter gets from the sink.
///
/// # Errors
///
/// Fetch failures and staging I/O; see [`DownloadError`].
pub(crate) async fn fetch_into(
    sink: &mut DownloadSink,
    fetcher: &dyn HttpFetcher,
    url: &str,
) -> Result<(), DownloadError> {
    let resume_offset = sink.resume_offset();
    let request = FetchRequest {
        url: url.to_owned(),
        range_start: (resume_offset > 0).then_some(resume_offset),
        headers: std::collections::BTreeMap::new(),
    };
    let FetchResponse { status, body } = fetcher.fetch(&request).await?;
    // 206 Partial Content continues the staged bytes; anything else answers
    // the whole document, so the stale part must go.
    if resume_offset > 0 && status != 206 {
        sink.restart()?;
    }
    sink.extend(&body)
}

/// The intake double: records every document handed across the seam and can
/// be scripted to reject transiently, so orchestration tests exercise the
/// retry path without an inbox.
#[derive(Debug, Default)]
pub(crate) struct RecordingIntake {
    received: Mutex<Vec<ScrapedDocument>>,
    /// The first N `enqueue` calls fail with [`IntakeError::Transient`].
    pub(crate) transient_failures_first: AtomicU32,
    calls: AtomicU32,
}

impl RecordingIntake {
    /// Snapshots, in enqueue order, everything the intake accepted.
    #[must_use]
    pub(crate) fn received(&self) -> Vec<ScrapedDocument> {
        self.received.lock().expect("uncontended").clone()
    }

    #[must_use]
    pub(crate) fn calls(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }

    /// A double whose first `n` `enqueue` calls fail transiently, then
    /// recover — for retry-path tests.
    pub(crate) fn with_transient_failures_first(n: u32) -> Self {
        Self {
            received: Mutex::new(Vec::new()),
            transient_failures_first: AtomicU32::new(n),
            calls: AtomicU32::new(0),
        }
    }
}

#[async_trait::async_trait]
impl InboxIntake for RecordingIntake {
    async fn enqueue(&self, document: &ScrapedDocument) -> Result<IntakeReceipt, IntakeError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let failures = self.transient_failures_first.load(Ordering::SeqCst);
        if call < failures {
            return Err(IntakeError::Transient {
                detail: "the inbox is not answering (scripted)".to_owned(),
            });
        }
        self.received
            .lock()
            .expect("uncontended")
            .push(document.clone());
        Ok(IntakeReceipt {
            message_id: uuid::Uuid::now_v7(),
            deduped: false,
        })
    }
}
