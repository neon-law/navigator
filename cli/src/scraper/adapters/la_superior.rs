//! The La Superior Court adapter — the first site on the framework, and the
//! proof case for the adapter seam.
//!
//! Scope (this PR): the PUBLIC surface. The court's case-access pages render
//! the Register of Actions server-side after a case-number query; discovery
//! enumerates its entries and retrieval fetches each entry's document
//! through the injected fetcher. A login is a no-op: the run is fully
//! useful against public, unauthenticated data, and the adapter declares
//! [`requires_login`](SiteAdapter::requires_login) `false` so a Firm with
//! no provisioned credential can still scrape. Document images the court
//! gates behind a paid account surface as an
//! [`AdapterError::AccessDenied`] hard stop naming the step — retrying
//! cannot fix a paywall, and nothing partial is filed.
//!
//! Endpoints are the court's own undocumented implementation details (the
//! court publishes no machine-readable API — its public pages document the
//! case-lookup and register-rendering behavior only). The paths here are
//! therefore provisional until a live dogfood, and the recorded fixtures in
//! the tests — not this file's constants — are the contract that catches
//! drift: a portal restructure fails the fixture tests, which is the
//! early-warning the framework prescribes.
//!
//! Parsing is pure: [`parse_register`] reads response bodies handed to it
//! and never opens a connection itself.

use crate::scraper::adapter::{AdapterError, NoticeQuery, NoticeRef, SiteAdapter, SiteKey};
use crate::scraper::credentials::SiteLogin;
use crate::scraper::download::DownloadSink;
use crate::scraper::fetcher::{FetchError, FetchRequest, FetchResponse, HttpFetcher};
use crate::scraper::session::BrowserSession;

/// The site key and credential key the adapter registers under.
pub(crate) const SITE_KEY: &str = "la-superior";

/// The portal base for a run whose `navigator.yaml` carries no override.
pub(crate) const DEFAULT_BASE_URL: &str = "https://www.lacourt.ca.gov";

/// The case-search page that renders the Register of Actions. Provisional
/// until the live dogfood — see the module docs.
const REGISTER_PATH: &str = "/RegisterOfActions";

/// The La Superior adapter: discovery over the public Register of Actions,
/// retrieval over the injected fetcher, no login.
pub(crate) struct LaSuperiorAdapter {
    site: SiteKey,
    base_url: String,
}

impl LaSuperiorAdapter {
    /// Build the adapter against a portal base (the `scrapers:` entry's
    /// `url`, or [`DEFAULT_BASE_URL`]).
    ///
    /// # Errors
    ///
    /// When the checked-in site key literal violates [`SiteKey`]'s
    /// vocabulary — unreachable for this literal, but no panic path exists
    /// by construction.
    pub(crate) fn new(base_url: impl Into<String>) -> Result<Self, crate::scraper::adapter::SiteKeyError> {
        Ok(Self {
            site: SiteKey::new(SITE_KEY)?,
            base_url: base_url.into(),
        })
    }

    /// The portal base this run was bound to.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))] // tests assert the binding
    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The Register of Actions URL for one case number: the configured base,
    /// a constant path, and the docket percent-encoded into the query
    /// string. The host is project config (`scrapers:` in `navigator.yaml`,
    /// `allowed_hosts`-enforced); the docket is encoded so no character can
    /// escape the query value.
    #[must_use]
    pub(crate) fn register_url(&self, docket: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}{REGISTER_PATH}?CaseNumber={}", percent_encode(docket))
    }
}

#[async_trait::async_trait]
impl SiteAdapter for LaSuperiorAdapter {
    fn site(&self) -> &SiteKey {
        &self.site
    }

    fn credential_key(&self) -> &str {
        SITE_KEY
    }

    fn requires_login(&self) -> bool {
        // The public register needs no account. A provisioned credential is
        // still honored if one exists (an authenticated flow is future
        // work); its absence is not an error.
        false
    }

    async fn login(
        &self,
        _session: &BrowserSession,
        _login: Option<&SiteLogin>,
    ) -> Result<(), AdapterError> {
        // Public path: the register is served without a session. Credentials
        // are deferred; when an authenticated flow lands it replaces this.
        Ok(())
    }

    async fn notices(
        &self,
        fetcher: &dyn HttpFetcher,
        query: &NoticeQuery,
    ) -> Result<Vec<NoticeRef>, AdapterError> {
        let url = self.register_url(&query.docket);
        let response = fetch_document("register of actions", fetcher, &url).await?;
        parse_register(&response.body, &self.base_url, query.since)
    }

    async fn download(
        &self,
        fetcher: &dyn HttpFetcher,
        notice: &NoticeRef,
        sink: &mut DownloadSink,
    ) -> Result<(), AdapterError> {
        const STEP: &str = "document download";
        // Status-aware and resumable by hand rather than through
        // `fetch_into`: the adapter must tell a paywall (401/403, a hard
        // stop) apart from a blip (5xx, retried), which the shared helper
        // cannot see. The sink stays the framework's — same part file, same
        // resume offset, same running hash.
        let offset = sink.resume_offset();
        let request = FetchRequest {
            url: notice.url.clone(),
            range_start: (offset > 0).then_some(offset),
        };
        let response = fetcher
            .fetch(&request)
            .await
            .map_err(|error| map_fetch_error(STEP, error))?;
        judge_status(STEP, response.status)?;
        // A non-partial answer replaces the stale part file; a 206 continues it.
        if offset > 0 && response.status != 206 {
            sink.restart()
                .map_err(|error| sink_error(STEP, error))?;
        }
        sink.extend(&response.body)
            .map_err(|error| sink_error(STEP, error))
    }
}

/// One GET of a page-sized resource, with the portal's answer judged before
/// the body is trusted.
async fn fetch_document(
    step: &str,
    fetcher: &dyn HttpFetcher,
    url: &str,
) -> Result<FetchResponse, AdapterError> {
    let request = FetchRequest {
        url: url.to_owned(),
        range_start: None,
    };
    let response = fetcher
        .fetch(&request)
        .await
        .map_err(|error| map_fetch_error(step, error))?;
    judge_status(step, response.status)?;
    Ok(response)
}

/// The portal's status code, as an adapter error. 401/403 hard-stop
/// ([`AdapterError::AccessDenied`]) — the court gates document images behind
/// a paid account, and retrying cannot fix a paywall. 5xx is transient;
/// anything else non-success means the flow moved.
fn judge_status(step: &str, status: u16) -> Result<(), AdapterError> {
    let detail = format!("the portal answered {status}");
    match status {
        401 | 403 => Err(AdapterError::AccessDenied {
            step: step.to_owned(),
            detail,
        }),
        status if status >= 500 => Err(AdapterError::Transient {
            step: step.to_owned(),
            detail,
        }),
        200..=299 => Ok(()),
        status => Err(AdapterError::StructureDrift {
            step: step.to_owned(),
            detail: format!("the portal answered {status}"),
        }),
    }
}

/// Transport failures: a body past the size cap is a wrong link (the flow
/// moved), a request error is a blip.
fn map_fetch_error(step: &str, error: FetchError) -> AdapterError {
    match &error {
        FetchError::TooLarge { .. } => AdapterError::StructureDrift {
            step: step.to_owned(),
            detail: error.to_string(),
        },
        FetchError::Request(_) => AdapterError::Transient {
            step: step.to_owned(),
            detail: error.to_string(),
        },
    }
}

fn sink_error(step: &str, error: crate::scraper::download::DownloadError) -> AdapterError {
    AdapterError::Transient {
        step: step.to_owned(),
        detail: error.to_string(),
    }
}

/// Encode a query value conservatively: the unreserved RFC 3986 set passes,
/// every other byte becomes `%XX`, so no docket string can change the URL's
/// structure.
#[must_use]
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Parse a Register of Actions body into notices. Only entries with a
/// document link become notices — an entry the court serves no document for
/// has nothing to retrieve, and enumerating it would only fail later. The
/// `since` bound drops entries filed strictly before it; an entry whose
/// date cannot be parsed is kept (a date format change must not silently
/// drop documents).
///
/// # Errors
///
/// [`AdapterError::StructureDrift`] when the body carries no parseable
/// register table or a row is not shaped as an entry.
fn parse_register(
    body: &[u8],
    base_url: &str,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Vec<NoticeRef>, AdapterError> {
    use scraper::Selector;

    const STEP: &str = "register of actions";
    let drift = |detail: String| AdapterError::StructureDrift {
        step: STEP.to_owned(),
        detail,
    };
    let text = std::str::from_utf8(body).map_err(|_| {
        drift("the body is not UTF-8".to_owned())
    })?;
    let document = scraper::Html::parse_document(text);
    let rows = Selector::parse("table.roa tr")
        .map_err(|error| drift(format!("the row selector does not compile: {error}")))?;
    // A page with no register table at all — a login redirect, a redesign —
    // is drift, not an empty docket. Only a present-but-empty register is
    // a well-formed empty result.
    let register = Selector::parse("table.roa")
        .map_err(|error| drift(format!("the register selector does not compile: {error}")))?;
    if document.select(&register).next().is_none() {
        return Err(drift(
            "no register of actions table was found in the page".to_owned(),
        ));
    }
    let cells = Selector::parse("td, th").map_err(|error| drift(format!("the cell selector does not compile: {error}")))?;
    let link = Selector::parse("a[href]").map_err(|error| drift(format!("the link selector does not compile: {error}")))?;
    let base = url::Url::parse(base_url)
        .map_err(|error| drift(format!("the configured base url is not a URL: {error}")))?;

    let mut notices = Vec::new();
    for row in document.select(&rows) {
        let values: Vec<String> = row
            .select(&cells)
            .map(|cell| cell.text().collect::<String>().trim().to_owned())
            .collect();
        // Header rows print `th` cells; skip them by shape.
        if row.select(&cells).next().map(|cell| cell.value().name()) != Some("td") {
            continue;
        }
        let [entry_number, filed, title, _rest @ ..] = values.as_slice() else {
            return Err(drift(format!(
                "a row has {} cells, expected at least 4",
                values.len()
            )));
        };
        // No document link, nothing to retrieve.
        let Some(anchor) = row.select(&link).next() else {
            continue;
        };
        let href = anchor.attr("href").unwrap_or_default();
        if entry_number.is_empty() || title.is_empty() {
            return Err(drift("an entry row is missing its number or title".to_owned()));
        }
        let url = base
            .join(href)
            .map_err(|error| drift(format!("a document link does not resolve: {error}")))?;
        if let Some(since) = since {
            if filed_since(filed)
                .is_some_and(|filed| filed < since)
            {
                continue;
            }
        }
        notices.push(NoticeRef {
            entry_number: entry_number.clone(),
            title: title.clone(),
            url: url.to_string(),
            extension: "pdf".to_owned(),
        });
    }
    Ok(notices)
}

/// The court prints filed dates as `MM/DD/YYYY`; anything else does not
/// parse (and the entry is kept — see `parse_register`).
fn filed_since(printed: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone as _;
    let date = chrono::NaiveDate::parse_from_str(printed.trim(), "%m/%d/%Y").ok()?;
    Some(
        chrono::Utc
            .from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight exists")),
    )
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_BASE_URL, LaSuperiorAdapter, SITE_KEY, parse_register, percent_encode};
    use crate::scraper::adapter::{AdapterError, SiteAdapter, SiteKey};
    use crate::scraper::download::DownloadSink;
    use crate::scraper::fetcher::{FetchRequest, FetchResponse};
    use crate::scraper::testing::MockFetcher;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn adapter() -> LaSuperiorAdapter {
        LaSuperiorAdapter::new(DEFAULT_BASE_URL).expect("the checked-in key is valid")
    }

    /// A recorded Register of Actions, shaped the way the fixture tests pin
    /// it: one header row, two document-bearing entries, one linkless entry
    /// the enumeration skips.
    const REGISTER: &str = r#"<html><body>
        <table class="roa">
        <tr><th>No.</th><th>Filed</th><th>Description</th><th>Document</th></tr>
        <tr><td>4</td><td>10/28/2026</td><td>Notice of Hearing</td>
            <td><a href="/paos/ShowDocument.do?docId=88421">Document</a></td></tr>
        <tr><td>5</td><td>10/29/2026</td><td>Proof of Service</td>
            <td><a href="/paos/ShowDocument.do?docId=88422">Document</a></td></tr>
        <tr><td>6</td><td>10/30/2026</td><td>Minute Order</td><td></td></tr>
        </table>
        </body></html>"#;

    fn site() -> SiteKey {
        SiteKey::new(SITE_KEY).expect("valid key")
    }

    fn staging_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("navigator-la-superior-{tag}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("the staging dir is created");
        dir
    }

    #[test]
    fn the_adapter_declares_the_public_path() {
        let adapter = adapter();
        assert_eq!(adapter.site(), &site());
        assert_eq!(adapter.credential_key(), "la-superior");
        assert!(!adapter.requires_login(), "the register is public");
        assert_eq!(adapter.base_url(), DEFAULT_BASE_URL);
    }

    #[tokio::test]
    async fn login_is_a_no_op_on_the_public_path() {
        let session = crate::scraper::session::BrowserSession::offline(
            &site(),
            staging_dir("login"),
        );
        assert!(adapter().login(&session, None).await.is_ok());
    }

    #[test]
    fn discovery_enumerates_document_bearing_entries() {
        let notices = parse_register(REGISTER.as_bytes(), DEFAULT_BASE_URL, None)
            .expect("the register parses");
        assert_eq!(notices.len(), 2, "the linkless entry is skipped");
        assert_eq!(notices[0].entry_number, "4");
        assert_eq!(notices[0].title, "Notice of Hearing");
        assert_eq!(
            notices[0].url,
            "https://www.lacourt.ca.gov/paos/ShowDocument.do?docId=88421"
        );
        assert_eq!(notices[0].extension, "pdf");
        assert_eq!(notices[1].entry_number, "5");
        assert_eq!(notices[1].url, "https://www.lacourt.ca.gov/paos/ShowDocument.do?docId=88422");
    }

    #[test]
    fn discovery_honors_the_since_bound() {
        use chrono::TimeZone as _;
        let since = chrono::Utc.from_utc_datetime(
            &chrono::NaiveDate::from_ymd_opt(2026, 10, 29)
                .expect("valid date")
                .and_hms_opt(0, 0, 0)
                .expect("midnight exists"),
        );
        let notices =
            parse_register(REGISTER.as_bytes(), DEFAULT_BASE_URL, Some(since)).expect("parses");
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].entry_number, "5", "only the later entry survives");
    }

    #[test]
    fn an_entry_with_an_unparseable_date_survives_since() {
        use chrono::TimeZone as _;
        let since = chrono::Utc
            .from_utc_datetime(&chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap().and_hms_opt(0, 0, 0).unwrap());
        let body = r#"<table class="roa">
            <tr><td>7</td><td>29 October 2026</td><td>Order</td>
                <td><a href="/doc.pdf">Document</a></td></tr>
            </table>"#;
        let notices = parse_register(body.as_bytes(), DEFAULT_BASE_URL, Some(since))
            .expect("parses");
        assert_eq!(notices.len(), 1, "a date the court changed format for must not drop the document");
    }

    #[test]
    fn discovery_is_empty_on_a_well_formed_empty_register() {
        let body = r#"<table class="roa"></table>"#;
        let notices = parse_register(body.as_bytes(), DEFAULT_BASE_URL, None).expect("parses");
        assert!(notices.is_empty());
    }

    #[test]
    fn discovery_drifts_on_a_body_without_a_register() {
        let outcome = parse_register(b"<html><body>Sign in</body></html>", DEFAULT_BASE_URL, None);
        assert!(matches!(outcome, Err(AdapterError::StructureDrift { .. })));
    }

    #[test]
    fn a_row_missing_its_number_or_title_is_drift() {
        let body = r#"<table class="roa">
            <tr><td></td><td>10/28/2026</td><td>Notice</td>
                <td><a href="/doc.pdf">Document</a></td></tr>
            </table>"#;
        let outcome = parse_register(body.as_bytes(), DEFAULT_BASE_URL, None);
        assert!(matches!(outcome, Err(AdapterError::StructureDrift { .. })));
    }

    #[test]
    fn the_register_url_encodes_the_case_number() {
        let url = adapter().register_url("24STCV 0123");
        assert!(url.starts_with(DEFAULT_BASE_URL));
        assert!(url.ends_with("?CaseNumber=24STCV%200123"));
    }

    #[tokio::test]
    async fn discovery_fetches_the_register_through_the_fetcher() {
        let handler = |_: &FetchRequest| {
            Ok(FetchResponse {
                status: 200,
                body: REGISTER.as_bytes().to_vec(),
            })
        };
        let fetcher = MockFetcher::with_handler(Arc::new(handler));
        let query = crate::scraper::adapter::NoticeQuery {
            docket: "24STCV01234".to_owned(),
            since: None,
        };
        let notices = adapter()
            .notices(&fetcher, &query)
            .await
            .expect("discovery succeeds");
        assert_eq!(notices.len(), 2);
        assert_eq!(fetcher.request_count("CaseNumber=24STCV01234"), 1);
    }

    #[tokio::test]
    async fn a_paywalled_document_is_access_denied_not_transient() {
        let handler = |_: &FetchRequest| {
            Ok(FetchResponse {
                status: 403,
                body: b"purchase required".to_vec(),
            })
        };
        let fetcher = MockFetcher::with_handler(Arc::new(handler));
        let dir = staging_dir("paywall");
        let mut sink = DownloadSink::open(&dir, dir.join("out.pdf"), crate::scraper::adapter::NoticeKey::new(&site(), "0004", "4"))
            .expect("the sink opens");
        let notice = crate::scraper::adapter::NoticeRef {
            entry_number: "4".to_owned(),
            title: "Notice of Hearing".to_owned(),
            url: "https://www.lacourt.ca.gov/paos/ShowDocument.do?docId=88421".to_owned(),
            extension: "pdf".to_owned(),
        };
        let outcome = adapter().download(&fetcher, &notice, &mut sink).await;
        assert!(matches!(outcome, Err(AdapterError::AccessDenied { .. })));
    }

    #[tokio::test]
    async fn a_server_error_is_transient() {
        let handler = |_: &FetchRequest| {
            Ok(FetchResponse {
                status: 503,
                body: b"busy".to_vec(),
            })
        };
        let fetcher = MockFetcher::with_handler(Arc::new(handler));
        let dir = staging_dir("server-error");
        let mut sink = DownloadSink::open(&dir, dir.join("out.pdf"), crate::scraper::adapter::NoticeKey::new(&site(), "0004", "4"))
            .expect("the sink opens");
        let notice = crate::scraper::adapter::NoticeRef {
            entry_number: "4".to_owned(),
            title: "Notice of Hearing".to_owned(),
            url: "https://www.lacourt.ca.gov/paos/ShowDocument.do?docId=88421".to_owned(),
            extension: "pdf".to_owned(),
        };
        let outcome = adapter().download(&fetcher, &notice, &mut sink).await;
        assert!(matches!(outcome, Err(AdapterError::Transient { .. })));
    }

    #[tokio::test]
    async fn download_streams_the_document_and_resumes_from_the_offset() {
        let document: Vec<u8> = (0..64u8).collect();
        let notice = crate::scraper::adapter::NoticeRef {
            entry_number: "4".to_owned(),
            title: "Notice of Hearing".to_owned(),
            url: "https://www.lacourt.ca.gov/paos/ShowDocument.do?docId=88421".to_owned(),
            extension: "pdf".to_owned(),
        };

        // A fresh download: the first fetch carries no Range header and the
        // whole document lands in the sink.
        let fresh_ranges = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_ranges = Arc::clone(&fresh_ranges);
        let fresh_document = document.clone();
        let fresh_handler = move |request: &FetchRequest| {
            handler_ranges.lock().expect("ranges").push(request.range_start);
            Ok(FetchResponse {
                status: 200,
                body: fresh_document.clone(),
            })
        };
        let fresh_fetcher = MockFetcher::with_handler(Arc::new(fresh_handler));
        let fresh_dir = staging_dir("resume-fresh");
        let key = crate::scraper::adapter::NoticeKey::new(&site(), "0004", "4");
        {
            let mut sink = DownloadSink::open(&fresh_dir, fresh_dir.join("out.pdf"), key.clone())
                .expect("the sink opens");
            adapter()
                .download(&fresh_fetcher, &notice, &mut sink)
                .await
                .expect("the fresh download completes");
            let completed = sink.finish().expect("the sink finishes");
            assert_eq!(completed.bytes, 64);
            assert_eq!(
                std::fs::read(&completed.path).expect("the file reads"),
                document
            );
        }
        assert_eq!(
            *fresh_ranges.lock().expect("ranges"),
            vec![None],
            "the first fetch carried no range"
        );

        // A resumed download: with five bytes already staged by a dead
        // attempt, the reopened sink fetches only the remainder and the part
        // file completes into the same document.
        let requested_ranges = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_ranges = Arc::clone(&requested_ranges);
        let handler_document = document.clone();
        let handler = move |request: &FetchRequest| {
            handler_ranges.lock().expect("ranges").push(request.range_start);
            match request.range_start {
                Some(5) => Ok(FetchResponse {
                    status: 206,
                    body: handler_document[5..].to_vec(),
                }),
                start => Ok(FetchResponse {
                    status: 206,
                    body: handler_document[start.unwrap_or_default() as usize..].to_vec(),
                }),
            }
        };
        let fetcher = MockFetcher::with_handler(Arc::new(handler));
        let dir = staging_dir("resume");
        let staged = dir.join("out.pdf");

        // First attempt: five bytes staged, then the attempt dies (a crash
        // mid-download, say).
        {
            let mut sink = DownloadSink::open(&dir, staged.clone(), key.clone())
                .expect("the sink opens");
            sink.extend(&document[..5]).expect("the partial write lands");
        }

        // The reopened sink continues from the offset; the adapter asks for
        // the remainder and the part file completes.
        let mut sink = DownloadSink::open(&dir, staged.clone(), key.clone())
            .expect("the sink reopens");
        assert_eq!(sink.resume_offset(), 5, "the partial bytes are staged");
        adapter()
            .download(&fetcher, &notice, &mut sink)
            .await
            .expect("the download completes");
        let completed = sink.finish().expect("the sink finishes");
        assert_eq!(completed.bytes, 64);
        assert_eq!(std::fs::read(&completed.path).expect("the file reads"), document);
        assert_eq!(
            *requested_ranges.lock().expect("ranges"),
            vec![Some(5)],
            "the resume asked for the remainder, not the whole document"
        );
    }

    #[test]
    fn percent_encoding_keeps_the_query_shape() {
        assert_eq!(percent_encode("24STCV01234"), "24STCV01234");
        assert_eq!(percent_encode("a b&c=d/e"), "a%20b%26c%3Dd%2Fe");
        assert_eq!(percent_encode("safe-._~"), "safe-._~");
    }
}
