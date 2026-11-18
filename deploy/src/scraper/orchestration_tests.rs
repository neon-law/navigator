use super::{
    registered_adapters, run_scraper, EnqueueRecord, NoticeQuery, RunReport, ScraperDeps,
    ScraperError,
};
use crate::scraper::adapter::{find_adapter, AdapterError, SiteKey};
use crate::scraper::fetcher::{FetchRequest, FetchResponse};
use crate::scraper::retry::RetryPolicy;
use crate::scraper::testing::RecordingIntake;
use crate::scraper::testing::{
    DownloadStep, LoginStep, MockAdapter, MockFetcher, StubClock, StubSessions,
};
use std::path::PathBuf;
use std::sync::Arc;
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
    let binding = registered_adapters();
    let real = find_adapter(&binding, &site()).expect("the la-superior adapter is registered");
    assert_eq!(real.site(), &site());
    let pacer = SiteKey::new(crate::scraper::adapters::pacer::SITE_KEY).expect("valid key");
    let pacer_real = find_adapter(&binding, &pacer).expect("the pacer adapter is registered");
    assert_eq!(pacer_real.site(), &pacer);
    assert_eq!(pacer_real.credential_key(), "pacer");
    assert!(pacer_real.requires_login(), "PACER runs behind the account");
    let adapters: Vec<Box<dyn crate::scraper::adapter::SiteAdapter>> =
        vec![Box::new(MockAdapter::simple(site()))];
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
    let failing = crate::scraper::testing::FailingIntake;
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
    assert!(matches!(&error, ScraperError::SiteStructure { step, .. } if step == "notice list"));
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
async fn a_pacer_run_without_a_provisioned_credential_refuses_before_the_browser_opens() {
    let fixture = fixture().await;
    let adapter = crate::scraper::adapters::PacerAdapter::new(
        crate::scraper::adapters::pacer::DEFAULT_BASE_URL,
    )
    .expect("the checked-in key is valid");
    let error = run_scraper(&adapter, &query(), &deps(&fixture))
        .await
        .expect_err("no pacer credential is provisioned");
    assert!(
        matches!(error, ScraperError::Credentials(_)),
        "the run stops at the credential seam: {error}"
    );
    assert_eq!(fixture.factory.opens(), 0, "no browser ever opens");
    assert_eq!(fixture.intake.received().len(), 0);
}

#[tokio::test]
async fn a_pacer_run_with_a_provisioned_credential_logs_in_and_enqueues_notices() {
    let fixture = fixture().await;
    fixture
        .put_portal_secret(&serde_json::json!({
            "pacer": {"username": "firm@example.com", "password": "staple"}
        }))
        .await;
    let handler = |request: &FetchRequest| {
        if request.url.contains("CaseNumber=") {
            Ok(FetchResponse {
                status: 200,
                body: crate::scraper::adapters::pacer::fixtures::DOCKET
                    .as_bytes()
                    .to_vec(),
            })
        } else {
            Ok(FetchResponse {
                status: 200,
                body: b"%PDF-fixture".to_vec(),
            })
        }
    };
    let fetcher = MockFetcher::with_handler(Arc::new(handler));
    let adapter = crate::scraper::adapters::PacerAdapter::new(
        crate::scraper::adapters::pacer::DEFAULT_BASE_URL,
    )
    .expect("the checked-in key is valid");
    let mut deps = deps(&fixture);
    deps.fetcher = &fetcher;
    let report = run_scraper(&adapter, &query(), &deps)
        .await
        .expect("the run completes");
    assert_eq!(report.discovered, 2, "the linkless entry is skipped");
    assert_eq!(report.downloaded, 2);
    assert_eq!(report.enqueued.len(), 2);
    assert!(
        fixture.factory.opens() >= 1,
        "the login hook ran in a session"
    );
    assert_eq!(fixture.intake.received().len(), 2);
}

#[tokio::test]
async fn an_intake_rejection_hard_stops_with_the_step_named() {
    let fixture = provisioned().await;
    let mut adapter = MockAdapter::simple(site());
    adapter.notices_script(vec![Ok(vec![notice("4", "Notice of Hearing")])]);
    adapter.download_script(vec![vec![DownloadStep::Chunk(b"PDF-A".to_vec())]]);
    let rejecting = crate::scraper::testing::FailingIntake;
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
    let path = std::env::temp_dir().join(format!("navigator-run-state-{}", uuid::Uuid::now_v7()));
    state.save(&path).expect("save run state");
    let reloaded = super::RunState::load(&path).expect("load run state");
    assert_eq!(reloaded, state);
    std::fs::remove_file(&path).expect("clean up the temp file");
}

#[test]
fn a_missing_run_state_file_is_a_fresh_run() {
    let path = std::env::temp_dir().join(format!("navigator-run-state-{}", uuid::Uuid::now_v7()));
    let state = super::RunState::load(&path).expect("a missing file is empty");
    assert_eq!(state, super::RunState::default());
}
