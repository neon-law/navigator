//! The La Superior Court adapter — the first site on the framework, and the
//! proof case for the adapter seam.
//!
//! Scope (this PR): the PUBLIC surface of the court's Case Access portal
//! (`cap.lacourt.ca.gov`). The portal is an Angular single-page application:
//! its HTML routes serve an application shell that renders client-side, so
//! discovery reads the JSON endpoints the application itself calls
//! (observed live against `cap.lacourt.ca.gov` on 2026-11-01):
//!
//! 1. **Case resolution** — `GET /bff/case-access/case-number?caseNumber=…`
//!    answers `{results: [{caseId, caseNumber, …, provider}], pagination}`.
//!    A docket number resolves to the court's internal case id and the
//!    provider code (e.g. `ECourt`) the detail endpoints key off.
//! 2. **The register's documents** — `GET
//!    /bff/case-details/{caseId}?includeProperties=documents`, with the
//!    search's provider code in a `Provider` request header, answers a
//!    `[{provider, result: [{…, documents: […]}]}]` envelope. Each document
//!    carries the court's own stable `documentId`, name, filed date, and
//!    type. These are the register's document entries and they are the
//!    notices: an entry the court serves no record of has nothing to
//!    retrieve.
//! 3. **Retrieval** — `GET /bff/documents/{documentId}/preview?provider=…`
//!    answers the court's own PDF for the document. On the public surface
//!    this is the court's preview image (the portal's PDF metadata titles it
//!    `Document Preview`), so the notice's title carries a `(preview)`
//!    marker: what files into the matter folder is honestly named as the
//!    court's preview of the document, not a certified full image. The full
//!    image needs the court's paid access — an authenticated lane is future
//!    work, and a `401`/`403` from this endpoint is still an
//!    [`AdapterError::AccessDenied`] hard stop naming the step.
//!
//! No login: the flow above is fully useful against public data, and the
//! adapter declares [`SiteAdapter::requires_login`] `false` so a Firm with
//! no provisioned credential can still scrape. A provisioned credential is
//! honored if one exists; its absence is not an error.
//!
//! Endpoints remain the court's own undocumented implementation details (the
//! court publishes no machine-readable API). The recorded fixtures in the
//! tests — not this file's constants — are the contract that catches drift:
//! a portal restructure, or a route that starts answering the SPA shell
//! again, fails the fixture tests, which is the early warning the framework
//! prescribes. Parsing is pure: [`parse_case_search`] and
//! [`parse_case_documents`] read response bodies handed to them and never
//! open a connection themselves.

use std::collections::{BTreeMap, BTreeSet};

use crate::scraper::adapter::{AdapterError, NoticeQuery, NoticeRef, SiteAdapter, SiteKey};
use crate::scraper::credentials::SiteLogin;
use crate::scraper::download::DownloadSink;
use crate::scraper::fetcher::{FetchError, FetchRequest, FetchResponse, HttpFetcher};
use crate::scraper::session::BrowserSession;

/// The site key and credential key the adapter registers under.
pub(crate) const SITE_KEY: &str = "la-superior";

/// The Case Access portal base for a run whose `navigator.yaml` carries no
/// override. The SPA and its `/bff/` endpoints live on this host.
pub(crate) const DEFAULT_BASE_URL: &str = "https://cap.lacourt.ca.gov";

/// The case-number search: resolves a docket to the court's case id and
/// provider code. Verified live 2026-11-01.
const CASE_SEARCH_PATH: &str = "/bff/case-access/case-number";

/// The case-details document register. The `{caseId}` path segment and the
/// `Provider` request header come from the search response.
const CASE_DETAILS_PATH: &str = "/bff/case-details";

/// The document-image endpoint. `{documentId}` and `{provider}` come from
/// the register and the search response.
const DOCUMENT_PREVIEW_PATH: &str = "/bff/documents";

/// The search page size: one docket number matches (at most) a handful of
/// court cases, and the adapter filters to the exact number anyway.
const SEARCH_PAGE_SIZE: u32 = 10;

/// The La Superior adapter: discovery over the portal's JSON endpoints,
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

    /// The case-number search URL for one docket: the configured base, a
    /// constant path, and the docket percent-encoded into the query string.
    /// The host is project config (`scrapers:` in `navigator.yaml`,
    /// `allowed_hosts`-enforced); the docket is encoded so no character can
    /// escape the query value.
    #[must_use]
    pub(crate) fn case_search_url(&self, docket: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!(
            "{base}{CASE_SEARCH_PATH}?caseNumber={}&pageIndex=0&pageSize={SEARCH_PAGE_SIZE}",
            percent_encode(docket)
        )
    }

    /// The case-details URL for the court's case id — the documents register
    /// — with the path segment encoded the same way.
    #[must_use]
    pub(crate) fn case_details_url(&self, case_id: &str) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!(
            "{base}{CASE_DETAILS_PATH}/{}?includeProperties=documents",
            percent_encode(case_id)
        )
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
        // The public flow needs no account. A provisioned credential is
        // still honored if one exists (an authenticated flow is future
        // work); its absence is not an error.
        false
    }

    async fn login(
        &self,
        _session: &BrowserSession,
        _login: Option<&SiteLogin>,
    ) -> Result<(), AdapterError> {
        // Public path: the JSON endpoints are served without a session.
        // Credentials are deferred; when an authenticated flow lands it
        // replaces this.
        Ok(())
    }

    async fn notices(
        &self,
        fetcher: &dyn HttpFetcher,
        query: &NoticeQuery,
    ) -> Result<Vec<NoticeRef>, AdapterError> {
        // Step 1: resolve the docket number to the court's case id and
        // provider code.
        let search = fetch_document(
            "case search",
            fetcher,
            &self.case_search_url(&query.docket),
            BTreeMap::new(),
        )
        .await?;
        let matches = parse_case_search(&search.body)?;
        let case = resolve_case(&matches, &query.docket)?;
        let Some(case) = case else {
            return Ok(Vec::new());
        };

        // Step 2: the documents register, keyed by the provider header the
        // search taught us.
        let mut headers = BTreeMap::new();
        headers.insert("Provider".to_owned(), case.provider.clone());
        let details = fetch_document(
            "case documents",
            fetcher,
            &self.case_details_url(&case.case_id),
            headers,
        )
        .await?;
        parse_case_documents(&details.body, self.base_url(), &case.provider, query.since)
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
            headers: BTreeMap::new(),
        };
        let response = fetcher
            .fetch(&request)
            .await
            .map_err(|error| map_fetch_error(STEP, &error))?;
        judge_status(STEP, response.status)?;
        // A non-partial answer replaces the stale part file; a 206 continues it.
        if offset > 0 && response.status != 206 {
            sink.restart().map_err(|error| sink_error(STEP, &error))?;
        }
        sink.extend(&response.body)
            .map_err(|error| sink_error(STEP, &error))
    }
}

/// One case the search resolved, as far as discovery needs it: the court's
/// internal case id and the provider code its detail endpoints key off.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedCase {
    case_id: String,
    provider: String,
}

/// One GET of a JSON endpoint, with request headers and the portal's answer
/// judged before the body is trusted.
async fn fetch_document(
    step: &str,
    fetcher: &dyn HttpFetcher,
    url: &str,
    headers: BTreeMap<String, String>,
) -> Result<FetchResponse, AdapterError> {
    let request = FetchRequest {
        url: url.to_owned(),
        range_start: None,
        headers,
    };
    let response = fetcher
        .fetch(&request)
        .await
        .map_err(|error| map_fetch_error(step, &error))?;
    judge_status(step, response.status)?;
    Ok(response)
}

/// The portal's status code, as an adapter error. 401/403 hard-stop
/// ([`AdapterError::AccessDenied`]) — the court gates full document images
/// behind a paid account, and retrying cannot fix a paywall. 5xx is
/// transient; anything else non-success means the flow moved.
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

/// Encode a query or path value conservatively: the unreserved RFC 3986 set
/// passes, every other byte becomes `%XX`, so no docket, case id, or
/// provider code can change the URL's structure.
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

/// The case search response, as the portal shapes it: a `results` array of
/// case summaries. Only the fields discovery reads are declared; the portal
/// carries more (pagination, categories), and their addition is not drift.
#[derive(Debug, serde::Deserialize)]
struct CaseSearchResponse {
    results: Vec<CaseSummary>,
}

#[derive(Debug, serde::Deserialize)]
struct CaseSummary {
    #[serde(rename = "caseId")]
    case_id: String,
    #[serde(rename = "caseNumber")]
    case_number: String,
    provider: String,
}

/// Parse a case-search body into case summaries. A `200` that carries
/// anything else — the SPA shell, a `{"error": …}` body, a reshaped
/// envelope — is drift, never a silently empty docket; only a well-formed
/// response with zero results is empty.
///
/// # Errors
///
/// [`AdapterError::StructureDrift`] when the body is not the search JSON.
fn parse_case_search(body: &[u8]) -> Result<Vec<CaseSummary>, AdapterError> {
    const STEP: &str = "case search";
    let text =
        std::str::from_utf8(body).map_err(|_| drift(STEP, "the body is not UTF-8".to_owned()))?;
    let parsed: CaseSearchResponse = serde_json::from_str(text).map_err(|error| {
        drift(
            STEP,
            format!("the body is not the case-search JSON: {error}"),
        )
    })?;
    Ok(parsed.results)
}

/// Filter the search results to the exact docket number (case-insensitive:
/// the portal echoes the number it matched) and resolve uniqueness. An empty
/// result set is a well-formed empty docket; several distinct case ids for
/// one docket number is ambiguity the adapter must not guess through.
///
/// # Errors
///
/// [`AdapterError::StructureDrift`] when distinct case ids disagree.
fn resolve_case(
    matches: &[CaseSummary],
    docket: &str,
) -> Result<Option<ResolvedCase>, AdapterError> {
    const STEP: &str = "case search";
    let exact: Vec<&CaseSummary> = matches
        .iter()
        .filter(|summary| {
            summary
                .case_number
                .trim()
                .eq_ignore_ascii_case(docket.trim())
        })
        .collect();
    let case_ids: BTreeSet<&str> = exact.iter().map(|summary| summary.case_id.trim()).collect();
    match case_ids.len() {
        0 => Ok(None),
        1 => {
            let summary = exact[0];
            Ok(Some(ResolvedCase {
                case_id: summary.case_id.trim().to_owned(),
                provider: summary.provider.trim().to_owned(),
            }))
        }
        count => Err(drift(
            STEP,
            format!("{count} distinct cases matched docket {docket}"),
        )),
    }
}

/// The documents-register envelope: `[{provider, result: [case]}]`. Only the
/// register's own fields are declared.
#[derive(Debug, serde::Deserialize)]
struct CaseDetailsEnvelope {
    #[serde(rename = "result")]
    cases: Vec<CaseDetails>,
}

#[derive(Debug, serde::Deserialize)]
struct CaseDetails {
    #[serde(rename = "caseId")]
    #[allow(dead_code)] // the envelope's identity; parsed for shape, not used
    case_id: String,
    documents: Option<Vec<CourtDocument>>,
}

#[derive(Debug, serde::Deserialize)]
struct CourtDocument {
    #[serde(rename = "documentId")]
    document_id: String,
    name: String,
    #[serde(rename = "fileDate")]
    file_date: Option<String>,
}

/// Parse the documents register into notices: one per document the court
/// lists. The public image endpoint serves the court's preview of the
/// document, so the title carries the court's own designation — see the
/// module docs. The `since` bound drops documents filed strictly before it;
/// a document whose date cannot be parsed is kept (a date format change
/// must not silently drop documents).
///
/// # Errors
///
/// [`AdapterError::StructureDrift`] when the body is not the register
/// envelope or a resolved case carries no documents property.
fn parse_case_documents(
    body: &[u8],
    base_url: &str,
    provider: &str,
    since: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<Vec<NoticeRef>, AdapterError> {
    const STEP: &str = "case documents";
    let text =
        std::str::from_utf8(body).map_err(|_| drift(STEP, "the body is not UTF-8".to_owned()))?;
    let envelopes: Vec<CaseDetailsEnvelope> = serde_json::from_str(text).map_err(|error| {
        drift(
            STEP,
            format!("the body is not the case-documents JSON: {error}"),
        )
    })?;
    let case = envelopes
        .into_iter()
        .next()
        .and_then(|envelope| envelope.cases.into_iter().next())
        .ok_or_else(|| drift(STEP, "the register envelope carries no case".to_owned()))?;
    let documents = case.documents.ok_or_else(|| {
        drift(
            STEP,
            "the case carries no documents property (the include was ignored)".to_owned(),
        )
    })?;

    let mut notices = Vec::new();
    for document in documents {
        if let Some(since) = since {
            if filed_at(document.file_date.as_deref()).is_some_and(|filed| filed < since) {
                continue;
            }
        }
        // A missing filed date cannot be bounded, so the document survives
        // `since` — the same never-silently-drop rule the date parser has.
        let url = build_preview_url(base_url, provider, &document.document_id);
        notices.push(NoticeRef {
            entry_number: document.document_id,
            title: format!("{} (preview)", document.name),
            url,
            extension: "pdf".to_owned(),
        });
    }
    Ok(notices)
}

/// The document-image URL for one register entry: the court's preview
/// endpoint on the configured base, both variable segments encoded.
#[must_use]
fn build_preview_url(base_url: &str, provider: &str, document_id: &str) -> String {
    let base = base_url.trim_end_matches('/');
    format!(
        "{base}{DOCUMENT_PREVIEW_PATH}/{}/preview?provider={}",
        percent_encode(document_id),
        percent_encode(provider)
    )
}

/// The register prints filed dates as ISO `2024-01-17T00:00:00`; the search
/// prints bare dates `2024-01-17`. Anything else does not parse (and the
/// document is kept — see `parse_case_documents`).
fn filed_at(printed: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::TimeZone as _;
    let printed = printed?.trim();
    if let Ok(datetime) = chrono::NaiveDateTime::parse_from_str(printed, "%Y-%m-%dT%H:%M:%S") {
        return Some(chrono::Utc.from_utc_datetime(&datetime));
    }
    let date = chrono::NaiveDate::parse_from_str(printed, "%Y-%m-%d").ok()?;
    Some(chrono::Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight exists")))
}

/// The drift error for one discovery step.
fn drift(step: &str, detail: String) -> AdapterError {
    AdapterError::StructureDrift {
        step: step.to_owned(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_preview_url, parse_case_documents, parse_case_search, percent_encode, resolve_case,
        LaSuperiorAdapter, CASE_DETAILS_PATH, CASE_SEARCH_PATH, DEFAULT_BASE_URL,
        DOCUMENT_PREVIEW_PATH, SITE_KEY,
    };
    use crate::scraper::adapter::{AdapterError, NoticeKey, SiteAdapter, SiteKey};
    use crate::scraper::download::DownloadSink;
    use crate::scraper::fetcher::{FetchError, FetchRequest, FetchResponse};
    use crate::scraper::testing::{MockFetcher, MockHandler};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn adapter() -> LaSuperiorAdapter {
        LaSuperiorAdapter::new(DEFAULT_BASE_URL).expect("the checked-in key is valid")
    }

    fn site() -> SiteKey {
        SiteKey::new(SITE_KEY).expect("valid key")
    }

    fn staging_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "navigator-la-superior-{tag}-{}",
            uuid::Uuid::now_v7()
        ));
        std::fs::create_dir_all(&dir).expect("the staging dir is created");
        dir
    }

    /// The case-search response, recorded from the live portal's
    /// `/bff/case-access/case-number` endpoint (2026-11-01), trimmed to the
    /// fields the parser reads. Field names are the contract.
    const CASE_SEARCH: &str = r#"{
        "results": [{
            "caseId": "5436252",
            "caseNumber": "24STCV01234",
            "caseTitle": "EDWARD SKOWRONSKI vs AUTO AUCTION GROUP, INC., A CALIFORNIA CORPORATION, et al.",
            "caseType": "Contract/Warranty Breach - Seller Plaintiff (no fraud/negligence)",
            "fileDate": "2024-01-17",
            "status": "Disposed",
            "category": "Civil Unlimited",
            "provider": "ECourt",
            "location": null
        }],
        "pagination": {"pageIndex": 0, "pageSize": 10, "totalPages": 1, "totalCount": 1}
    }"#;

    /// A well-formed search with zero results: an empty docket, not drift.
    const CASE_SEARCH_EMPTY: &str = r#"{
        "results": [],
        "pagination": {"pageIndex": 0, "pageSize": 10, "totalPages": 0, "totalCount": 0}
    }"#;

    /// The documents register, recorded from the live portal's
    /// `/bff/case-details/{caseId}?includeProperties=documents` endpoint
    /// (2026-11-01), trimmed to two documents. Field names are the contract.
    const CASE_DOCUMENTS: &str = r#"{
        "provider": "ECourt",
        "result": [{
            "caseId": "5436252",
            "number": "24STCV01234",
            "title": "EDWARD SKOWRONSKI vs AUTO AUCTION GROUP, INC., A CALIFORNIA CORPORATION, et al.",
            "fileDate": "2024-01-17T00:00:00",
            "status": "Disposed",
            "documentsDownloadable": false,
            "documents": [
                {
                    "documentId": "98046165",
                    "name": "Notice of Case Assignment - Unlimited Civil Case",
                    "fileDate": "2024-01-17T00:00:00",
                    "extension": null,
                    "security": {"description": "FileSecurityLevel", "value": "0"},
                    "type": {"description": "Notice", "value": "NTC"},
                    "pageCount": 2,
                    "status": "Filed",
                    "documentLinks": []
                },
                {
                    "documentId": "100470102",
                    "name": "Minute Order (Case Management Conference)",
                    "fileDate": "2024-05-13T00:00:00",
                    "extension": null,
                    "security": {"description": "FileSecurityLevel", "value": "0"},
                    "type": {"description": "Rulings", "value": "RUL"},
                    "pageCount": 2,
                    "status": "Filed",
                    "documentLinks": []
                }
            ]
        }]
    }"#;

    /// The live portal's SPA shell: every HTML route answers this
    /// client-side-rendered page (recorded 2026-11-01, trimmed). It parses
    /// as neither the search JSON nor the register envelope — the drift
    /// tests pin that a `200` carrying it is a clean stop.
    const SPA_SHELL: &str = r#"<!DOCTYPE html><html lang="en"><head>
        <title>Superior Court of Los Angeles County</title>
        <base href="/">
        <script src="/runtime.4a6f2b8c1d9e0f77.js" defer></script>
        <script src="/polyfills.24a7185106a4ab15.js" defer></script>
        <script src="/main.f30cbfc7de95ba56.js" defer></script>
    </head><body><app-root></app-root></body></html>"#;

    /// A handler that answers the two discovery calls the way the live
    /// portal does, recording every request for assertions.
    fn portal_handler(search_body: &'static str, details_body: &'static str) -> MockHandler {
        Arc::new(move |request: &FetchRequest| {
            let body = if request.url.contains(CASE_SEARCH_PATH) {
                search_body
            } else if request.url.contains(CASE_DETAILS_PATH) {
                details_body
            } else {
                panic!("unexpected fetch of {}", request.url);
            };
            Ok(FetchResponse {
                status: 200,
                body: body.as_bytes().to_vec(),
            })
        })
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
        let session =
            crate::scraper::session::BrowserSession::offline(&site(), staging_dir("login"));
        assert!(adapter().login(&session, None).await.is_ok());
    }

    #[tokio::test]
    async fn discovery_runs_the_working_flow_end_to_end() {
        let fetcher = MockFetcher::with_handler(portal_handler(CASE_SEARCH, CASE_DOCUMENTS));
        let query = crate::scraper::adapter::NoticeQuery {
            docket: "24STCV01234".to_owned(),
            since: None,
        };
        let notices = adapter()
            .notices(&fetcher, &query)
            .await
            .expect("discovery succeeds through the live flow");
        assert_eq!(notices.len(), 2);
        // The court's own document id is the entry number; the DKT naming
        // zero-pads it without truncating.
        assert_eq!(notices[0].entry_number, "98046165");
        assert_eq!(
            notices[0].title,
            "Notice of Case Assignment - Unlimited Civil Case (preview)"
        );
        assert_eq!(
            notices[0].url,
            "https://cap.lacourt.ca.gov/bff/documents/98046165/preview?provider=ECourt"
        );
        assert_eq!(notices[0].extension, "pdf");
        assert_eq!(notices[1].entry_number, "100470102");
        // The register call keyed off the provider the search taught us.
        assert_eq!(
            fetcher.request_count("includeProperties=documents"),
            1,
            "the documents register is fetched once, after the search"
        );
        let recorded = fetcher.requests();
        let details = recorded
            .iter()
            .find(|request| request.url.contains(CASE_DETAILS_PATH))
            .expect("the register call was recorded");
        assert_eq!(
            details.headers.get("Provider").map(String::as_str),
            Some("ECourt"),
            "the register call carries the search's provider"
        );
    }

    #[tokio::test]
    async fn a_200_spa_shell_is_a_clean_stop_on_case_search() {
        let fetcher = MockFetcher::with_handler(portal_handler(SPA_SHELL, CASE_DOCUMENTS));
        let query = crate::scraper::adapter::NoticeQuery {
            docket: "24STCV01234".to_owned(),
            since: None,
        };
        let outcome = adapter().notices(&fetcher, &query).await;
        let error = outcome.expect_err("the SPA shell is not the search JSON");
        assert!(
            matches!(error, AdapterError::StructureDrift { ref step, .. } if step == "case search"),
            "the drift names the step: {error}"
        );
        assert!(
            fetcher.request_count(CASE_DETAILS_PATH) == 0,
            "a drifted search never reaches the register"
        );
    }

    #[tokio::test]
    async fn a_200_spa_shell_is_a_clean_stop_on_the_case_details() {
        let fetcher = MockFetcher::with_handler(portal_handler(CASE_SEARCH, SPA_SHELL));
        let query = crate::scraper::adapter::NoticeQuery {
            docket: "24STCV01234".to_owned(),
            since: None,
        };
        let outcome = adapter().notices(&fetcher, &query).await;
        let error = outcome.expect_err("the SPA shell is not the register envelope");
        assert!(
            matches!(
                error,
                AdapterError::StructureDrift { ref step, .. } if step == "case documents"
            ),
            "the drift names the step: {error}"
        );
    }

    #[tokio::test]
    async fn an_empty_search_is_a_well_formed_empty_docket() {
        let fetcher = MockFetcher::with_handler(portal_handler(CASE_SEARCH_EMPTY, CASE_DOCUMENTS));
        let query = crate::scraper::adapter::NoticeQuery {
            docket: "99XX99999".to_owned(),
            since: None,
        };
        let notices = adapter()
            .notices(&fetcher, &query)
            .await
            .expect("an empty docket is not an error");
        assert!(notices.is_empty());
        assert_eq!(
            fetcher.request_count(CASE_DETAILS_PATH),
            0,
            "an unresolved docket never fetches a register"
        );
    }

    #[test]
    fn an_ambiguous_search_is_drift() {
        let matches = parse_case_search(
            br#"{"results": [
                {"caseId": "1", "caseNumber": "24STCV01234", "provider": "ECourt"},
                {"caseId": "2", "caseNumber": "24STCV01234", "provider": "ECourt"}
            ]}"#,
        )
        .expect("the search parses");
        let outcome = resolve_case(&matches, "24STCV01234");
        assert!(
            matches!(outcome, Err(AdapterError::StructureDrift { .. })),
            "two distinct case ids must not be guessed through"
        );
    }

    #[test]
    fn discovery_honors_the_since_bound() {
        use chrono::TimeZone as _;
        let since = chrono::Utc.from_utc_datetime(
            &chrono::NaiveDate::from_ymd_opt(2024, 3, 1)
                .expect("valid date")
                .and_hms_opt(0, 0, 0)
                .expect("midnight exists"),
        );
        let notices = parse_case_documents(
            CASE_DOCUMENTS.as_bytes(),
            DEFAULT_BASE_URL,
            "ECourt",
            Some(since),
        )
        .expect("parses");
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices[0].entry_number, "100470102",
            "only the later document survives"
        );
    }

    #[test]
    fn a_document_with_an_unparseable_date_survives_since() {
        use chrono::TimeZone as _;
        let since = chrono::Utc.from_utc_datetime(
            &chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        );
        let body = r#"{"provider": "ECourt", "result": [{
            "caseId": "5436252",
            "documents": [{"documentId": "9", "name": "Order", "fileDate": "not a date"}]
        }]}"#;
        let notices =
            parse_case_documents(body.as_bytes(), DEFAULT_BASE_URL, "ECourt", Some(since))
                .expect("parses");
        assert_eq!(
            notices.len(),
            1,
            "a date the court changed format for must not drop the document"
        );
    }

    #[test]
    fn a_register_without_a_documents_property_is_drift() {
        let body = r#"{"provider": "ECourt", "result": [{"caseId": "5436252"}]}"#;
        let outcome = parse_case_documents(body.as_bytes(), DEFAULT_BASE_URL, "ECourt", None);
        assert!(matches!(outcome, Err(AdapterError::StructureDrift { .. })));
    }

    #[test]
    fn an_empty_documents_array_is_a_well_formed_empty_register() {
        let body = r#"{"provider": "ECourt", "result": [{"caseId": "5436252", "documents": []}]}"#;
        let notices = parse_case_documents(body.as_bytes(), DEFAULT_BASE_URL, "ECourt", None)
            .expect("parses");
        assert!(notices.is_empty());
    }

    #[test]
    fn the_url_builders_encode_their_values() {
        assert_eq!(
            adapter().case_search_url("24STCV 0123"),
            format!("{DEFAULT_BASE_URL}{CASE_SEARCH_PATH}?caseNumber=24STCV%200123&pageIndex=0&pageSize=10")
        );
        assert_eq!(
            adapter().case_details_url("5436252"),
            format!("{DEFAULT_BASE_URL}{CASE_DETAILS_PATH}/5436252?includeProperties=documents")
        );
        assert_eq!(
            build_preview_url(DEFAULT_BASE_URL, "ECourt", "98046165"),
            format!("{DEFAULT_BASE_URL}{DOCUMENT_PREVIEW_PATH}/98046165/preview?provider=ECourt")
        );
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
        let mut sink = DownloadSink::open(
            &dir,
            dir.join("out.pdf"),
            NoticeKey::new(&site(), "0004", "4"),
        )
        .expect("the sink opens");
        let notice = crate::scraper::adapter::NoticeRef {
            entry_number: "4".to_owned(),
            title: "Notice of Hearing".to_owned(),
            url: build_preview_url(DEFAULT_BASE_URL, "ECourt", "98046165"),
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
        let mut sink = DownloadSink::open(
            &dir,
            dir.join("out.pdf"),
            NoticeKey::new(&site(), "0004", "4"),
        )
        .expect("the sink opens");
        let notice = crate::scraper::adapter::NoticeRef {
            entry_number: "4".to_owned(),
            title: "Notice of Hearing".to_owned(),
            url: build_preview_url(DEFAULT_BASE_URL, "ECourt", "98046165"),
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
            url: build_preview_url(DEFAULT_BASE_URL, "ECourt", "98046165"),
            extension: "pdf".to_owned(),
        };

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

    #[test]
    fn percent_encoding_keeps_the_query_shape() {
        assert_eq!(percent_encode("24STCV01234"), "24STCV01234");
        assert_eq!(percent_encode("a b&c=d/e"), "a%20b%26c%3Dd%2Fe");
        assert_eq!(percent_encode("safe-._~"), "safe-._~");
    }
}
