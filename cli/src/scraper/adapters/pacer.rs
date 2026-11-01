//! The PACER adapter — Public Access to Court Electronic Records, the
//! federal docket service, as a compatibility target.
//!
//! Scope (this PR): the retrieval structure, built on the proven framework
//! seams. Discovery enumerates a docket report's document-bearing entries;
//! retrieval streams each entry's document through the injected fetcher
//! into the resumable sink; and the billing-confirmation step — the one
//! part of PACER every other adapter lacks — is a named hard stop, never
//! an automated acceptance: PACER meters billable pages, and a run must
//! never spend the Firm's money without a provisioned credential and a
//! billing policy. The login orchestration hook is invoked exactly when a
//! credential resolves; a credential-less run refuses before any browser
//! opens, through the framework's `requires_login` contract.
//!
//! Discrepancy kept visible (the spec's own instruction): the research
//! behind the approved spec recorded **no** live PACER client surface —
//! no endpoints, no page shape ("PACER appears only in prose and error
//! strings"). The URLs and page shapes below are therefore a provisional
//! model, not a recorded integration: the portal base is the public PACER
//! host, and the recorded fixtures in the tests — not these constants —
//! are the contract that catches drift, exactly the la-superior
//! arrangement. The live dogfood, which needs a provisioned
//! `court_portal` `portal_login` entry for the Firm, replaces the model
//! with the recorded page shape and lands the sign-in form mechanics in
//! the login hook, the one place the recorded flow belongs.
//!
//! Parsing is pure: [`parse_docket`] and [`billing_confirmation`] read
//! response bodies handed to them and never open a connection themselves.

use crate::scraper::adapter::{AdapterError, NoticeQuery, NoticeRef, SiteAdapter, SiteKey};
use crate::scraper::credentials::SiteLogin;
use crate::scraper::download::DownloadSink;
use crate::scraper::fetcher::{FetchError, FetchRequest, FetchResponse, HttpFetcher};
use crate::scraper::session::BrowserSession;

/// The site key and credential key the adapter registers under.
pub(crate) const SITE_KEY: &str = "pacer";

/// The portal base for a run whose `navigator.yaml` carries no override.
/// Provisional until the live dogfood — see the module docs.
pub(crate) const DEFAULT_BASE_URL: &str = "https://www.pacer.gov";

/// The docket-report page that enumerates a case's entries. Provisional
/// until the live dogfood — see the module docs.
const DOCKET_REPORT_PATH: &str = "/docket-report";

/// The PACER adapter: discovery over the docket report, retrieval over
/// the injected fetcher, billing-aware at every step, login behind the
/// Firm's provisioned credential.
pub(crate) struct PacerAdapter {
    site: SiteKey,
    base_url: String,
}

impl PacerAdapter {
    /// Build the adapter against a portal base (the `scrapers:` entry's
    /// `url`, or [`DEFAULT_BASE_URL`]).
    ///
    /// # Errors
    ///
    /// When the checked-in site key literal violates [`SiteKey`]'s
    /// vocabulary — unreachable for this literal, but no panic path exists
    /// by construction.
    pub(crate) fn new(
        base_url: impl Into<String>,
    ) -> Result<Self, crate::scraper::adapter::SiteKeyError> {
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

    /// The docket-report URL for one case number: the configured base, a
    /// constant path, and the docket percent-encoded into the query
    /// string. The host is project config (`scrapers:` in `navigator.yaml`,
    /// `allowed_hosts`-enforced); the docket is encoded so no character can
    /// escape the query value.
    #[must_use]
    pub(crate) fn docket_report_url(&self, docket: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!(
            "{base}{DOCKET_REPORT_PATH}?CaseNumber={}",
            percent_encode(docket)
        )
    }
}

#[async_trait::async_trait]
impl SiteAdapter for PacerAdapter {
    fn site(&self) -> &SiteKey {
        &self.site
    }

    fn credential_key(&self) -> &str {
        SITE_KEY
    }

    fn requires_login(&self) -> bool {
        // Every surface this adapter targets sits behind the account, and
        // document retrieval is metered billing: there is no useful public
        // path. A credential-less run refuses before any browser opens
        // (the framework's `requires_login` contract), which is also what
        // keeps a run from ever touching a billable page without the
        // Firm's provisioned credential.
        true
    }

    async fn login(
        &self,
        _session: &BrowserSession,
        login: Option<&SiteLogin>,
    ) -> Result<(), AdapterError> {
        // `requires_login` is `true`, so the framework hands the hook a
        // complete login or never opens a session; `None` here would be a
        // contract break, not a site condition.
        login.ok_or_else(|| AdapterError::StructureDrift {
            step: "login".to_owned(),
            detail: "the orchestration passed no portal login to a site that requires one"
                .to_owned(),
        })?;
        // The sign-in form itself is the live-dogfood step: the spec's
        // research recorded no PACER login flow, so none is invented here.
        // The hook is the one place the recorded flow lands once a
        // credential exists to rehearse it against; until then the
        // framework's persisted-cookie seam and this hook carry the
        // session state.
        Ok(())
    }

    async fn notices(
        &self,
        fetcher: &dyn HttpFetcher,
        query: &NoticeQuery,
    ) -> Result<Vec<NoticeRef>, AdapterError> {
        let url = self.docket_report_url(&query.docket);
        let response = fetch_document("docket report", fetcher, &url).await?;
        parse_docket(&response.body, &self.base_url, query.since)
    }

    async fn download(
        &self,
        fetcher: &dyn HttpFetcher,
        notice: &NoticeRef,
        sink: &mut DownloadSink,
    ) -> Result<(), AdapterError> {
        const STEP: &str = "document download";
        // Status-aware and resumable by hand rather than through
        // `fetch_into`: the adapter must tell a charge gate (a billing
        // confirmation, a paywall 401/403 — hard stops) apart from a blip
        // (5xx, retried), which the shared helper cannot see. The sink
        // stays the framework's — same part file, same resume offset,
        // same running hash.
        let offset = sink.resume_offset();
        let request = FetchRequest {
            url: notice.url.clone(),
            range_start: (offset > 0).then_some(offset),
        };
        let response = fetcher
            .fetch(&request)
            .await
            .map_err(|error| map_fetch_error(STEP, &error))?;
        judge_status(STEP, response.status)?;
        gate_billing(STEP, &response.body)?;
        // A non-partial answer replaces the stale part file; a 206 continues it.
        if offset > 0 && response.status != 206 {
            sink.restart().map_err(|error| sink_error(STEP, &error))?;
        }
        sink.extend(&response.body)
            .map_err(|error| sink_error(STEP, &error))
    }
}

// The step helpers below are adapter-local on purpose: the adapters stay
// independently patchable — one portal's status judging must never change
// because another's did.

/// One GET of a page-sized resource, with the portal's answer judged and
/// the billing gate applied before the body is trusted.
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
        .map_err(|error| map_fetch_error(step, &error))?;
    judge_status(step, response.status)?;
    gate_billing(step, &response.body)?;
    Ok(response)
}

/// The portal's status code, as an adapter error. 401/403 hard-stop
/// ([`AdapterError::AccessDenied`]) — a portal refusing access cannot be
/// retried into cooperation. 5xx is transient; anything else non-success
/// means the flow moved.
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

/// The billing gate: PACER answers a billable request with a
/// charge-confirmation page instead of the content. Navigator never
/// accepts charges automatically — a Firm's money is spent only on
/// purpose — so the gate hard-stops with the confirmation's own words
/// named. This is the same `AccessDenied` treatment the framework
/// prescribes for paywalls: retrying cannot fix a charge gate.
fn gate_billing(step: &str, body: &[u8]) -> Result<(), AdapterError> {
    let Some(charges) = billing_confirmation(body) else {
        return Ok(());
    };
    Err(AdapterError::AccessDenied {
        step: step.to_owned(),
        detail: format!(
            "the portal asked to confirm charges before delivering this ({charges}); \
             Navigator never accepts charges automatically — provision the Firm \
             credential and set a billing policy first"
        ),
    })
}

/// Transport failures: a body past the size cap is a wrong link (the flow
/// moved), a request error is a blip.
fn map_fetch_error(step: &str, error: &FetchError) -> AdapterError {
    match error {
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

fn sink_error(step: &str, error: &crate::scraper::download::DownloadError) -> AdapterError {
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
    use std::fmt::Write as _;

    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Parse a docket-report body into notices. Only entries with a document
/// link become notices — an entry the portal serves no document for has
/// nothing to retrieve, and enumerating it would only fail later. The
/// `since` bound drops entries filed strictly before it; an entry whose
/// date cannot be parsed is kept (a date format change must not silently
/// drop documents).
///
/// # Errors
///
/// [`AdapterError::StructureDrift`] when the body carries no parseable
/// docket table or a row is not shaped as an entry.
fn parse_docket(
    body: &[u8],
    base_url: &str,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Vec<NoticeRef>, AdapterError> {
    use scraper::Selector;

    const STEP: &str = "docket report";
    let drift = |detail: String| AdapterError::StructureDrift {
        step: STEP.to_owned(),
        detail,
    };
    let text = std::str::from_utf8(body).map_err(|_| drift("the body is not UTF-8".to_owned()))?;
    let document = scraper::Html::parse_document(text);
    let rows = Selector::parse("table.docket tr")
        .map_err(|error| drift(format!("the row selector does not compile: {error}")))?;
    // A page with no docket table at all — a login redirect, a redesign —
    // is drift, not an empty docket. Only a present-but-empty docket is a
    // well-formed empty result.
    let docket_table = Selector::parse("table.docket")
        .map_err(|error| drift(format!("the docket selector does not compile: {error}")))?;
    if document.select(&docket_table).next().is_none() {
        return Err(drift(
            "no docket report table was found in the page".to_owned(),
        ));
    }
    let cells = Selector::parse("td, th")
        .map_err(|error| drift(format!("the cell selector does not compile: {error}")))?;
    let link = Selector::parse("a[href]")
        .map_err(|error| drift(format!("the link selector does not compile: {error}")))?;
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
        let [entry_number, filed, description, _rest @ ..] = values.as_slice() else {
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
        if entry_number.is_empty() || description.is_empty() {
            return Err(drift(
                "an entry row is missing its number or description".to_owned(),
            ));
        }
        let url = base
            .join(href)
            .map_err(|error| drift(format!("a document link does not resolve: {error}")))?;
        if let Some(since) = since {
            if filed_since(filed).is_some_and(|filed| filed < since) {
                continue;
            }
        }
        notices.push(NoticeRef {
            entry_number: entry_number.clone(),
            title: description.clone(),
            url: url.to_string(),
            extension: "pdf".to_owned(),
        });
    }
    Ok(notices)
}

/// The portal prints filed dates as `MM/DD/YYYY`; anything else does not
/// parse (and the entry is kept — see `parse_docket`).
fn filed_since(printed: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone as _;
    let date = chrono::NaiveDate::parse_from_str(printed.trim(), "%m/%d/%Y").ok()?;
    Some(chrono::Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight exists")))
}

/// Whether a response body is the portal's charge-confirmation page, and
/// what it asked: a billable request can answer with a confirmation
/// instead of the content. Detected by the page's `#billing-confirmation`
/// marker in the provisional model — the recorded fixtures pin the shape,
/// and the live dogfood revisits it. A document body (a PDF) never parses
/// as the confirmation, so the check is cheap on the happy path: HTML
/// bodies only.
fn billing_confirmation(body: &[u8]) -> Option<String> {
    if !body.starts_with(b"<") {
        return None;
    }
    let text = std::str::from_utf8(body).ok()?;
    let document = scraper::Html::parse_document(text);
    let selector = scraper::Selector::parse("#billing-confirmation").ok()?;
    let element = document.select(&selector).next()?;
    let charges = element.text().collect::<String>().trim().to_owned();
    (!charges.is_empty()).then_some(charges)
}

/// Recorded fixtures, shared with the orchestration tests in
/// `crate::scraper` so the base proves the same HTML the adapter tests
/// pin. Provisional until the live dogfood — see the module docs.
#[cfg(test)]
pub(crate) mod fixtures {
    /// A recorded docket report, shaped the way the fixture tests pin it:
    /// one header row, two document-bearing entries, one linkless entry
    /// the enumeration skips.
    pub(crate) const DOCKET: &str = r#"<html><body>
        <table class="docket">
        <tr><th>No.</th><th>Date Filed</th><th>Description</th><th>Document</th></tr>
        <tr><td>4</td><td>10/28/2026</td><td>Notice of Hearing</td>
            <td><a href="/doc/88421.pdf">Document</a></td></tr>
        <tr><td>5</td><td>10/29/2026</td><td>Order Granting Extension</td>
            <td><a href="/doc/88422.pdf">Document</a></td></tr>
        <tr><td>6</td><td>10/30/2026</td><td>Minute Order</td><td></td></tr>
        </table>
        </body></html>"#;

    /// A recorded charge-confirmation page: the portal's answer to a
    /// billable request. The gate must name these charges and never
    /// accept them.
    pub(crate) const CONFIRMATION: &str = r#"<html><body>
        <div id="billing-confirmation">2 page(s) at $0.10 per page: $0.20 total. Accept charges to retrieve this document.</div>
        </body></html>"#;
}

#[cfg(test)]
mod tests {
    use super::fixtures::{CONFIRMATION, DOCKET};
    use super::{
        billing_confirmation, parse_docket, percent_encode, PacerAdapter, DEFAULT_BASE_URL,
        SITE_KEY,
    };
    use crate::scraper::adapter::{AdapterError, NoticeKey, NoticeQuery, SiteAdapter, SiteKey};
    use crate::scraper::credentials::parse_portal_logins;
    use crate::scraper::download::DownloadSink;
    use crate::scraper::fetcher::{FetchRequest, FetchResponse};
    use crate::scraper::session::SessionFactory;
    use crate::scraper::testing::{MockFetcher, StubSessions};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn adapter() -> PacerAdapter {
        PacerAdapter::new(DEFAULT_BASE_URL).expect("the checked-in key is valid")
    }

    fn site() -> SiteKey {
        SiteKey::new(SITE_KEY).expect("valid key")
    }

    fn staging_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("navigator-pacer-{tag}-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).expect("the staging dir is created");
        dir
    }

    fn notice() -> crate::scraper::adapter::NoticeRef {
        crate::scraper::adapter::NoticeRef {
            entry_number: "4".to_owned(),
            title: "Notice of Hearing".to_owned(),
            url: format!("{DEFAULT_BASE_URL}/doc/88421.pdf"),
            extension: "pdf".to_owned(),
        }
    }

    fn sink(tag: &str) -> (PathBuf, DownloadSink) {
        let dir = staging_dir(tag);
        let key = NoticeKey::new(&site(), "0004", "4");
        let sink = DownloadSink::open(&dir, dir.join("out.pdf"), key).expect("the sink opens");
        (dir, sink)
    }

    #[test]
    fn the_adapter_declares_the_authenticated_path() {
        let adapter = adapter();
        assert_eq!(adapter.site(), &site());
        assert_eq!(adapter.credential_key(), "pacer");
        assert!(adapter.requires_login(), "PACER runs behind the account");
        assert_eq!(adapter.base_url(), DEFAULT_BASE_URL);
    }

    #[tokio::test]
    async fn login_refuses_a_missing_login_as_a_contract_break() {
        let session =
            crate::scraper::session::BrowserSession::offline(&site(), staging_dir("login-none"));
        let outcome = adapter().login(&session, None).await;
        assert!(
            matches!(outcome, Err(AdapterError::StructureDrift { .. })),
            "a requires_login site must never see a None login"
        );
    }

    #[tokio::test]
    async fn the_login_hook_accepts_a_provisioned_credential() {
        let session =
            crate::scraper::session::BrowserSession::offline(&site(), staging_dir("login-some"));
        let logins = parse_portal_logins(&serde_json::json!({
            "pacer": {"username": "firm@example.com", "password": "staple"}
        }))
        .expect("a valid payload");
        let login = logins.login_for("pacer").expect("the pacer entry resolves");
        assert!(
            adapter().login(&session, Some(login)).await.is_ok(),
            "the hook runs with a resolved credential"
        );
    }

    #[test]
    fn discovery_enumerates_document_bearing_entries() {
        let notices =
            parse_docket(DOCKET.as_bytes(), DEFAULT_BASE_URL, None).expect("the docket parses");
        assert_eq!(notices.len(), 2, "the linkless entry is skipped");
        assert_eq!(notices[0].entry_number, "4");
        assert_eq!(notices[0].title, "Notice of Hearing");
        assert_eq!(notices[0].url, format!("{DEFAULT_BASE_URL}/doc/88421.pdf"));
        assert_eq!(notices[0].extension, "pdf");
        assert_eq!(notices[1].entry_number, "5");
        assert_eq!(notices[1].url, format!("{DEFAULT_BASE_URL}/doc/88422.pdf"));
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
            parse_docket(DOCKET.as_bytes(), DEFAULT_BASE_URL, Some(since)).expect("parses");
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].entry_number, "5",
            "only the later entry survives"
        );
    }

    #[test]
    fn an_entry_with_an_unparseable_date_survives_since() {
        use chrono::TimeZone as _;
        let since = chrono::Utc.from_utc_datetime(
            &chrono::NaiveDate::from_ymd_opt(2026, 10, 1)
                .expect("valid date")
                .and_hms_opt(0, 0, 0)
                .expect("midnight exists"),
        );
        let body = r#"<table class="docket">
            <tr><td>7</td><td>29 October 2026</td><td>Order</td>
                <td><a href="/doc.pdf">Document</a></td></tr>
            </table>"#;
        let notices = parse_docket(body.as_bytes(), DEFAULT_BASE_URL, Some(since)).expect("parses");
        assert_eq!(
            notices.len(),
            1,
            "a date the portal changed format for must not drop the document"
        );
    }

    #[test]
    fn discovery_is_empty_on_a_well_formed_empty_docket() {
        let body = r#"<table class="docket"></table>"#;
        let notices = parse_docket(body.as_bytes(), DEFAULT_BASE_URL, None).expect("parses");
        assert!(notices.is_empty());
    }

    #[test]
    fn discovery_drifts_on_a_body_without_a_docket() {
        let outcome = parse_docket(b"<html><body>Sign in</body></html>", DEFAULT_BASE_URL, None);
        assert!(matches!(outcome, Err(AdapterError::StructureDrift { .. })));
    }

    #[test]
    fn a_row_missing_its_number_or_description_is_drift() {
        let body = r#"<table class="docket">
            <tr><td></td><td>10/28/2026</td><td>Notice</td>
                <td><a href="/doc.pdf">Document</a></td></tr>
            </table>"#;
        let outcome = parse_docket(body.as_bytes(), DEFAULT_BASE_URL, None);
        assert!(matches!(outcome, Err(AdapterError::StructureDrift { .. })));
    }

    #[test]
    fn the_docket_report_url_encodes_the_case_number() {
        let url = adapter().docket_report_url("00CV 00123");
        assert!(url.starts_with(DEFAULT_BASE_URL));
        assert!(url.ends_with("?CaseNumber=00CV%2000123"));
    }

    #[test]
    fn percent_encoding_keeps_the_query_shape() {
        assert_eq!(percent_encode("00CV00123"), "00CV00123");
        assert_eq!(percent_encode("a b&c=d/e"), "a%20b%26c%3Dd%2Fe");
        assert_eq!(percent_encode("safe-._~"), "safe-._~");
    }

    #[tokio::test]
    async fn discovery_fetches_the_docket_report_through_the_fetcher() {
        let handler = |_: &FetchRequest| {
            Ok(FetchResponse {
                status: 200,
                body: DOCKET.as_bytes().to_vec(),
            })
        };
        let fetcher = MockFetcher::with_handler(Arc::new(handler));
        let query = NoticeQuery {
            docket: "00CV00123".to_owned(),
            since: None,
        };
        let notices = adapter()
            .notices(&fetcher, &query)
            .await
            .expect("discovery succeeds");
        assert_eq!(notices.len(), 2);
        assert_eq!(fetcher.request_count("CaseNumber=00CV00123"), 1);
    }

    #[test]
    fn a_charge_confirmation_page_is_recognized_by_its_marker() {
        let charges =
            billing_confirmation(CONFIRMATION.as_bytes()).expect("the confirmation is detected");
        assert!(
            charges.contains("$0.20"),
            "the charges are named: {charges}"
        );
        assert!(
            billing_confirmation(b"%PDF-1.7 the document bytes").is_none(),
            "a document body is never a confirmation"
        );
        assert!(
            billing_confirmation(b"<html><body>no marker here</body></html>").is_none(),
            "an ordinary page is not a confirmation"
        );
    }

    #[tokio::test]
    async fn a_billing_confirmation_is_surfaced_never_accepted() {
        let handler = |_: &FetchRequest| {
            Ok(FetchResponse {
                status: 200,
                body: CONFIRMATION.as_bytes().to_vec(),
            })
        };
        let fetcher = MockFetcher::with_handler(Arc::new(handler));
        let (dir, mut sink) = sink("billing");
        let outcome = adapter().download(&fetcher, &notice(), &mut sink).await;
        assert!(
            matches!(outcome, Err(AdapterError::AccessDenied { .. })),
            "the charge gate hard-stops: {outcome:?}"
        );
        assert_eq!(
            fetcher.request_count("/doc/88421.pdf"),
            1,
            "the gate never refetches to auto-accept the charges"
        );
        drop(sink);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[tokio::test]
    async fn a_billing_confirmation_names_the_charges_and_the_remedy() {
        let handler = |_: &FetchRequest| {
            Ok(FetchResponse {
                status: 200,
                body: CONFIRMATION.as_bytes().to_vec(),
            })
        };
        let fetcher = MockFetcher::with_handler(Arc::new(handler));
        let (dir, mut sink) = sink("billing-detail");
        let error = adapter()
            .download(&fetcher, &notice(), &mut sink)
            .await
            .expect_err("the gate stops the download");
        let message = error.to_string();
        assert!(
            message.contains("$0.20"),
            "the charges are named: {message}"
        );
        assert!(
            message.contains("never accepts charges automatically"),
            "the remedy is named: {message}"
        );
        drop(sink);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[tokio::test]
    async fn a_billing_confirmation_on_the_docket_report_is_access_denied() {
        // The gate rides the shared GET helper, so a billable docket
        // report surfaces the same named hard stop — not a misleading
        // structure-drift.
        let handler = |_: &FetchRequest| {
            Ok(FetchResponse {
                status: 200,
                body: CONFIRMATION.as_bytes().to_vec(),
            })
        };
        let fetcher = MockFetcher::with_handler(Arc::new(handler));
        let query = NoticeQuery {
            docket: "00CV00123".to_owned(),
            since: None,
        };
        let outcome = adapter().notices(&fetcher, &query).await;
        assert!(matches!(outcome, Err(AdapterError::AccessDenied { .. })));
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
        let (dir, mut sink) = sink("paywall");
        let outcome = adapter().download(&fetcher, &notice(), &mut sink).await;
        assert!(matches!(outcome, Err(AdapterError::AccessDenied { .. })));
        drop(sink);
        std::fs::remove_dir_all(&dir).expect("clean up");
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
        let (dir, mut sink) = sink("server-error");
        let outcome = adapter().download(&fetcher, &notice(), &mut sink).await;
        assert!(matches!(outcome, Err(AdapterError::Transient { .. })));
        drop(sink);
        std::fs::remove_dir_all(&dir).expect("clean up");
    }

    #[tokio::test]
    async fn download_streams_the_document_and_resumes_from_the_offset() {
        let document: Vec<u8> = (0..64u8).collect();
        let notice = notice();

        // A fresh download: the first fetch carries no Range header and the
        // whole document lands in the sink.
        let fresh_ranges = Arc::new(std::sync::Mutex::new(Vec::new()));
        let handler_ranges = Arc::clone(&fresh_ranges);
        let fresh_document = document.clone();
        let fresh_handler = move |request: &FetchRequest| {
            handler_ranges
                .lock()
                .expect("ranges")
                .push(request.range_start);
            Ok(FetchResponse {
                status: 200,
                body: fresh_document.clone(),
            })
        };
        let fresh_fetcher = MockFetcher::with_handler(Arc::new(fresh_handler));
        let fresh_dir = staging_dir("resume-fresh");
        let key = NoticeKey::new(&site(), "0004", "4");
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
            handler_ranges
                .lock()
                .expect("ranges")
                .push(request.range_start);
            match request.range_start {
                Some(5) => Ok(FetchResponse {
                    status: 206,
                    body: handler_document[5..].to_vec(),
                }),
                start => {
                    let from = usize::try_from(start.unwrap_or_default())
                        .expect("a recorded range start fits usize");
                    Ok(FetchResponse {
                        status: 206,
                        body: handler_document[from..].to_vec(),
                    })
                }
            }
        };
        let fetcher = MockFetcher::with_handler(Arc::new(handler));
        let dir = staging_dir("resume");
        let staged = dir.join("out.pdf");

        // First attempt: five bytes staged, then the attempt dies (a crash
        // mid-download, say).
        {
            let mut sink =
                DownloadSink::open(&dir, staged.clone(), key.clone()).expect("the sink opens");
            sink.extend(&document[..5])
                .expect("the partial write lands");
        }

        // The reopened sink continues from the offset; the adapter asks for
        // the remainder and the part file completes.
        let mut sink =
            DownloadSink::open(&dir, staged.clone(), key.clone()).expect("the sink reopens");
        assert_eq!(sink.resume_offset(), 5, "the partial bytes are staged");
        adapter()
            .download(&fetcher, &notice, &mut sink)
            .await
            .expect("the download completes");
        let completed = sink.finish().expect("the sink finishes");
        assert_eq!(completed.bytes, 64);
        assert_eq!(
            std::fs::read(&completed.path).expect("the file reads"),
            document
        );
        assert_eq!(
            *requested_ranges.lock().expect("ranges"),
            vec![Some(5)],
            "the resume asked for the remainder, not the whole document"
        );
    }

    #[tokio::test]
    async fn the_stub_session_factory_never_launches_a_browser_for_pacer() {
        // The login hook takes the session by reference and the framework
        // owns its lifecycle; the adapter itself never opens one. (The
        // stub factory is what orchestration tests inject — this pins
        // that the hook accepts the resulting offline session.)
        let factory = StubSessions::default();
        let session = factory.open(&site()).await.expect("the stub opens");
        let logins = parse_portal_logins(&serde_json::json!({
            "pacer": {"username": "firm@example.com", "password": "staple"}
        }))
        .expect("a valid payload");
        let login = logins.login_for("pacer").expect("the pacer entry resolves");
        assert!(adapter().login(&session, Some(login)).await.is_ok());
        session.shutdown().await.expect("the stub shuts down");
    }
}
