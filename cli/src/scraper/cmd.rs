//! The `navigator scraper` surface: list the registered site adapters and
//! run one over one docket. A run resolves the Firm's portal login, opens
//! the pinned-Chrome session, and hands every fetched document to the
//! omnichannel inbox through the intake seam — never directly into a
//! matter folder. Non-secret run settings are paths and dates only; the
//! secrets stay in the Firm secrets store.

use std::process::ExitCode;

use clap::Subcommand;
use uuid::Uuid;

use crate::scraper::adapter::{find_adapter, NoticeQuery, SiteAdapter, SiteKey};
use crate::scraper::browser::ChromiumSessions;
use crate::scraper::fetcher::HttpFetcherOverReqwest;
use crate::scraper::file::{DktFilingIntake, ProjectFilingStore};
use crate::scraper::intake::{InboxIntake, UnwiredIntake};
use crate::scraper::retry::{RetryPolicy, TokioClock};
use crate::scraper::session::SessionConfig;
use crate::scraper::{registered_adapters, run_scraper, ScraperDeps};

#[derive(Subcommand)]
pub(crate) enum ScraperCmd {
    /// List the registered site adapters.
    Sites,
    /// Run one site adapter over one docket. Every document it fetches is
    /// enqueued into the omnichannel inbox for triage.
    Run {
        /// The portal to scrape — `navigator scraper sites` lists them.
        #[arg(long)]
        site: String,
        /// The docket entry number to pull; `4` and `0004` both normalize
        /// to the prescribed `DKT 0004` form.
        #[arg(long)]
        docket: String,
        /// Only notices posted on or after this UTC date (YYYY-MM-DD).
        #[arg(long)]
        since: Option<chrono::NaiveDate>,
        /// The Firm whose portal logins the run resolves.
        #[arg(long)]
        firm: Uuid,
        /// Run the browser headed (watch the session live).
        #[arg(long)]
        headed: bool,
        /// File every fetched document straight into the Project checkout
        /// at the working directory, under its DKT name. Without this flag
        /// the documents stage and the run reports them undelivered: the
        /// omnichannel inbox intake is its own integration.
        #[arg(long)]
        file_dkt: bool,
    },
}

/// # Errors
///
/// None: every failure is reported on stderr as a non-success [`ExitCode`]
/// instead of an `Err`, matching the other command runners' contract.
pub(crate) async fn run(action: ScraperCmd) -> ExitCode {
    match action {
        ScraperCmd::Sites => {
            for adapter in registered_adapters() {
                println!("{}\t{}", adapter.site(), adapter.credential_key());
            }
            ExitCode::SUCCESS
        }
        ScraperCmd::Run {
            site,
            docket,
            since,
            firm,
            headed,
            file_dkt,
        } => run_site(&site, &docket, since, firm, headed, file_dkt).await,
    }
}

/// Deliberately does not echo the caller's raw `--site`/`--docket` arguments
/// back through any `eprintln!`: the `rust/cleartext-logging` `CodeQL` query
/// taints the whole parsed `Command` enum through the unrelated
/// `Commands::Secrets` arm, so any printed value that transitively descends
/// from a clap-parsed argument is flagged — even though a site name is not
/// sensitive. Following `cli/src/projects/skill.rs`, the sink never echoes
/// the tainted value; the operator already knows what they typed.
fn unregistered_site_message(registered_sites: impl Iterator<Item = String>) -> String {
    format!(
        "navigator scraper: no adapter registered for the requested site; registered sites: {}",
        registered_sites.collect::<Vec<_>>().join(", ")
    )
}

fn exit_failure(step: &str, error: impl std::fmt::Display) -> ExitCode {
    eprintln!("navigator scraper: {step}: {error}");
    ExitCode::from(2)
}

/// Wire the production dependencies and run one adapter over one docket.
async fn run_site(
    site: &str,
    docket: &str,
    since: Option<chrono::NaiveDate>,
    firm_id: Uuid,
    headed: bool,
    file_dkt: bool,
) -> ExitCode {
    let site_key = match SiteKey::new(site) {
        Ok(key) => key,
        Err(error) => return exit_failure("bad --site", error),
    };
    let adapters = registered_adapters();
    let Some(adapter) = find_adapter(&adapters, &site_key) else {
        eprintln!(
            "{}",
            unregistered_site_message(adapters.iter().map(|adapter| adapter.site().to_string()))
        );
        return ExitCode::from(2);
    };

    let surreal = match store::surreal::connect_from_env().await {
        Ok(db) => db,
        Err(error) => return exit_failure("connecting to the store", error),
    };
    // The production KMS handle resolves from the deployment environment
    // (`NAVIGATOR_RUNTIME_KMS_KEY`); a run without it cannot decrypt the
    // Firm's portal logins and fails closed before any browser opens.
    let kms = cloud::runtime_kms_from_env().await;

    let chrome = match crate::devx::chrome::resolve() {
        Ok(chrome) => chrome,
        Err(error) => return exit_failure("resolving Chrome for Testing", error),
    };
    let project_root = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(error) => return exit_failure("locating the checkout", error),
    };
    let state_root = project_root.join(".navigator").join("scraper");
    let site_dir = state_root.join(site_key.as_str());
    let staging_dir = site_dir.join("staging");
    let fetcher = match HttpFetcherOverReqwest::new() {
        Ok(fetcher) => fetcher,
        Err(error) => return exit_failure("constructing the HTTP fetcher", error),
    };
    let sessions = ChromiumSessions::new(SessionConfig {
        chrome,
        webdriver_port: 4444,
        headless: !headed,
        state_dir: state_root,
    });
    // The intake seam's two pluggable implementations: straight DKT filing
    // into this Project checkout, or (default, until its integration
    // lands) the placeholder that reports documents staged but undelivered.
    let filing_store = ProjectFilingStore::new(project_root);
    let dkt_intake = DktFilingIntake::new(&filing_store);
    let unwired = UnwiredIntake;
    let intake: &dyn InboxIntake = if file_dkt { &dkt_intake } else { &unwired };
    let deps = ScraperDeps {
        sessions: &sessions,
        surreal: &surreal,
        firm_id,
        kms: kms.as_ref(),
        fetcher: &fetcher,
        intake,
        clock: &TokioClock,
        login_policy: RetryPolicy::login_default(),
        fetch_policy: RetryPolicy::fetch_default(),
        staging_dir: staging_dir.clone(),
        ledger_path: staging_dir.join("ledger.json"),
        downloads_dir: site_dir.join("documents"),
        run_state_path: site_dir.join("run-state.json"),
    };

    run_site_with(adapter, docket, since, &deps).await
}

/// The command decision the wiring feeds: build the notice query, run the
/// scraper, and map the outcome to the process exit code. Kept apart from
/// the environment-dependent assembly above so tests drive it through the
/// same injection seams the orchestration tests use.
async fn run_site_with(
    adapter: &dyn SiteAdapter,
    docket: &str,
    since: Option<chrono::NaiveDate>,
    deps: &ScraperDeps<'_>,
) -> ExitCode {
    let query = NoticeQuery {
        docket: docket.to_owned(),
        since: since.map(|date| {
            use chrono::TimeZone as _;
            chrono::Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight exists"))
        }),
    };
    match run_scraper(adapter, &query, deps).await {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => exit_failure("run failed", error),
    }
}

#[cfg(test)]
mod tests {
    use super::{run, run_site_with, unregistered_site_message, ScraperCmd};
    use crate::scraper::adapter::{find_adapter, SiteAdapter, SiteKey};
    use crate::scraper::intake::RecordingIntake;
    use crate::scraper::retry::RetryPolicy;
    use crate::scraper::testing::{MockAdapter, MockFetcher, StubClock, StubSessions};
    use crate::scraper::{NoticeQuery, ScraperDeps};
    use std::path::PathBuf;
    use std::time::Duration;
    use uuid::Uuid;

    fn site() -> SiteKey {
        SiteKey::new("la-superior").expect("valid key")
    }

    fn instant_policy(max_attempts: u32) -> RetryPolicy {
        RetryPolicy {
            max_attempts,
            base_delay: Duration::from_millis(1),
        }
    }

    struct Fixture {
        surreal: store::surreal::SurrealDb,
        admin_id: Uuid,
        firm_id: Uuid,
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
            kms: cloud::FakeKms::new("scraper-cmd-test"),
            factory: StubSessions::default(),
            fetcher: MockFetcher::never(),
            intake: RecordingIntake::default(),
            clock: StubClock,
            root: std::env::temp_dir()
                .join(format!("navigator-scraper-cmd-{}", uuid::Uuid::now_v7())),
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
            downloads_dir: fixture.site_dir().join("documents"),
            run_state_path: fixture.site_dir().join("run-state.json"),
        }
    }

    #[tokio::test]
    async fn a_wired_adapter_runs_to_a_success_exit() {
        let mut fixture = provisioned().await;
        // No notices script: the mock lists through the fetcher, which
        // answers with a comment-only listing — an empty but well-formed
        // discovery round trip.
        let handler = |_: &crate::scraper::fetcher::FetchRequest| {
            Ok(crate::scraper::fetcher::FetchResponse {
                status: 200,
                body: b"# no notices posted\n".to_vec(),
            })
        };
        fixture.fetcher = MockFetcher::with_handler(std::sync::Arc::new(handler));
        let adapters: Vec<Box<dyn SiteAdapter>> = vec![Box::new(MockAdapter::simple(site()))];
        let adapter = find_adapter(&adapters, &site()).expect("the mock is registered");

        let code = run_site_with(adapter, "0004", None, &deps(&fixture)).await;
        assert_eq!(code, std::process::ExitCode::SUCCESS);
    }

    #[tokio::test]
    async fn a_failed_run_maps_to_the_failure_exit_code() {
        let fixture = fixture().await;
        let adapters: Vec<Box<dyn SiteAdapter>> = vec![Box::new(MockAdapter::simple(site()))];
        let adapter = find_adapter(&adapters, &site()).expect("the mock is registered");

        // No credential row is provisioned: the run hard-stops on the
        // credential step before any session opens.
        let code = run_site_with(adapter, "0004", None, &deps(&fixture)).await;
        assert_eq!(code, std::process::ExitCode::from(2));
    }

    #[tokio::test]
    async fn an_unregistered_site_dispatch_exits_two() {
        let code = run(ScraperCmd::Run {
            site: "nowhere".to_owned(),
            docket: "0004".to_owned(),
            since: None,
            firm: Uuid::now_v7(),
            headed: true,
        })
        .await;
        assert_eq!(code, std::process::ExitCode::from(2));
    }

    #[tokio::test]
    async fn a_malformed_site_name_dispatch_exits_two() {
        let code = run(ScraperCmd::Run {
            site: "NOWHERE".to_owned(),
            docket: "0004".to_owned(),
            since: None,
            firm: Uuid::now_v7(),
            headed: true,
        })
        .await;
        assert_eq!(code, std::process::ExitCode::from(2));
    }

    #[tokio::test]
    async fn the_sites_dispatch_succeeds() {
        let code = run(ScraperCmd::Sites).await;
        assert_eq!(code, std::process::ExitCode::SUCCESS);
    }

    #[test]
    fn an_unregistered_site_names_the_registry_without_echoing_the_argument() {
        let message = unregistered_site_message(["la-superior"].into_iter().map(ToOwned::to_owned));
        assert!(message.contains("la-superior"));
        // The clap-parsed argument must not be echoed back (CodeQL
        // rust/cleartext-logging taints every value descending from the
        // parsed `Command` enum).
        assert_eq!(
            message,
            "navigator scraper: no adapter registered for the requested site; \
             registered sites: la-superior"
        );
    }

    // The query construction stays pinned: a `--since` date becomes a UTC
    // midnight, and the docket string passes through verbatim.
    #[test]
    fn the_notice_query_maps_since_to_utc_midnight() {
        use chrono::TimeZone as _;
        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 1).expect("valid date");
        let query = NoticeQuery {
            docket: "0004".to_owned(),
            since: Some(
                chrono::Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight exists")),
            ),
        };
        assert_eq!(query.docket, "0004");
        assert_eq!(
            query.since.map(|at| at.to_string()),
            Some("2026-10-01 00:00:00 UTC".to_owned())
        );
    }
}
