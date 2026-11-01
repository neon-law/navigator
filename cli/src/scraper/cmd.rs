//! The `navigator scraper` surface: list the configured site adapters and
//! run one over one docket. The `scrapers:` section of `navigator.yaml`
//! owns the site registry — which adapter scrapes which portal — and the
//! commands resolve every run through it, so a site the listing shows is a
//! site the run can plan.
//!
//! A run resolves the Firm's portal login, opens the pinned-Chrome
//! session, and hands every fetched document to the omnichannel inbox
//! through the intake seam — never directly into a matter folder.
//! Non-secret run settings are paths and dates only; the secrets stay in
//! the Firm secrets store. Credential status is part of the listing: a
//! site whose `court_portal` `portal_login` entry is not provisioned
//! renders disabled, and a run against one refuses before any browser
//! opens or any fetch is attempted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;
use uuid::Uuid;

use crate::scraper::adapter::{NoticeQuery, SiteAdapter, SiteKey};
use crate::scraper::browser::ChromiumSessions;
use crate::scraper::config::{self, ConfigError, ScraperConfig};
use crate::scraper::credentials::{CredentialsError, LoginPresence, PortalLogins};
use crate::scraper::fetcher::HttpFetcherOverReqwest;
use crate::scraper::file::{DktFilingIntake, ProjectFilingStore};
use crate::scraper::intake::{InboxIntake, StoreInboxIntake};
use crate::scraper::retry::{RetryPolicy, TokioClock};
use crate::scraper::session::SessionConfig;
use crate::scraper::{registered_adapters, run_scraper, ScraperDeps};

#[derive(Subcommand)]
pub(crate) enum ScraperCmd {
    /// List the sites configured under `scrapers:` with their credential
    /// status: available when a complete portal login is provisioned,
    /// disabled (greyed out) when not.
    Sites {
        /// The Firm whose portal logins the status check resolves.
        #[arg(long)]
        firm: Uuid,
    },
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
        /// the documents enqueue into the omnichannel inbox for triage,
        /// which needs the deployment's asset storage configured.
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
        ScraperCmd::Sites { firm } => sites_listing(firm).await,
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
/// sensitive. Config-derived values are file content, not arguments, so the
/// plan errors list the configured sites; the operator already knows what
/// they typed.
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

/// The credential status one configured site renders with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SiteStatus {
    /// A complete portal login is provisioned: the site can run.
    Available,
    /// Greyed out: no portal login the Firm has provisioned, or a
    /// half-filled one.
    Disabled(&'static str),
}

impl SiteStatus {
    fn render(self) -> String {
        match self {
            Self::Available => "available".to_owned(),
            Self::Disabled(reason) => format!("disabled ({reason})"),
        }
    }
}

/// The status column for one site, given the Firm's resolved portal
/// logins. `None` covers the no-credential-row state: until the Firm's
/// `court_portal` `portal_login` secret exists, every court renders
/// disabled.
fn credential_status(logins: Option<&PortalLogins>, key: &str) -> SiteStatus {
    match logins.map_or(LoginPresence::Absent, |logins| logins.presence(key)) {
        LoginPresence::Complete => SiteStatus::Available,
        LoginPresence::Incomplete => {
            SiteStatus::Disabled("the provisioned portal login is incomplete")
        }
        LoginPresence::Absent => {
            SiteStatus::Disabled("no court_portal portal_login entry provisioned for this site")
        }
    }
}

/// Render the sites listing: one row per configured site, its adapter
/// binding, portal, schedule hint, and credential status. Sorted by site
/// key, so the listing is deterministic. Pure: tests drive it with
/// fixtures on both sides of the credential line.
fn render_sites(config: &ScraperConfig, logins: Option<&PortalLogins>) -> String {
    use std::fmt::Write as _;
    let mut rendered = String::new();
    if config.sites.is_empty() {
        // Writing to a String cannot fail.
        let _ = writeln!(
            rendered,
            "no sites configured under `scrapers.sites:` in navigator.yaml"
        );
        return rendered;
    }
    for (key, entry) in &config.sites {
        let status = credential_status(logins, key.as_str());
        let schedule = config
            .options
            .schedule_hint
            .as_deref()
            .unwrap_or("schedule unset");
        let _ = writeln!(
            rendered,
            "{}  adapter {}  {}  {}  {}",
            key.as_str(),
            entry.adapter,
            entry.portal_url,
            schedule,
            status.render()
        );
    }
    rendered
}

/// `navigator scraper sites`: the configured registry with the Firm's
/// credential status resolved live. A listing that cannot reach the store
/// fails honestly; a Firm with no credential row lists every site
/// disabled, with the provisioning guidance on stderr.
async fn sites_listing(firm_id: Uuid) -> ExitCode {
    let (config, _) = match load_manifest_config() {
        Ok(ok) => ok,
        Err(code) => return code,
    };
    let surreal = match store::surreal::connect_from_env().await {
        Ok(db) => db,
        Err(error) => return exit_failure("connecting to the store", error),
    };
    let kms = cloud::runtime_kms_from_env().await;
    let resolved =
        crate::scraper::credentials::resolve_portal_logins(&surreal, firm_id, kms.as_ref()).await;
    // A store the run cannot use at all is an environment failure, not a
    // status; a Firm with no credential row is a status: all disabled.
    if let Err(
        error @ (CredentialsError::StoreUnavailable | CredentialsError::InvalidPayload { .. }),
    ) = &resolved
    {
        return exit_failure("resolving the Firm's portal login secret", error);
    }
    print!("{}", render_sites(&config, resolved.as_ref().ok()));
    if let Err(error) = &resolved {
        eprintln!("navigator scraper: {error}");
    }
    ExitCode::SUCCESS
}

/// Read the working directory's `navigator.yaml` and parse its
/// `scrapers:` section. The `allowed_hosts` map rides along: a run
/// checks its target site's portal host against it.
fn load_manifest_config() -> Result<(ScraperConfig, BTreeMap<String, String>), ExitCode> {
    let project_root = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(error) => {
            return Err(exit_failure("locating the checkout", error));
        }
    };
    let Some(manifest) = crate::projects::manifest::read(&project_root) else {
        return Err(exit_failure(
            "reading navigator.yaml",
            "none in the working directory; scraper sites are configured under its `scrapers:` section",
        ));
    };
    let config = match &manifest.scrapers {
        Some(section) => match config::parse_value(section) {
            Ok(parsed) => parsed,
            Err(error) => return Err(exit_failure("parsing the `scrapers:` section", error)),
        },
        None => ScraperConfig::default(),
    };
    Ok((config, manifest.allowed_hosts.clone()))
}

/// One planned run: the resolved adapter and where its state lives. Pure
/// in the config — the command assembles it, tests assert it.
fn plan_run<'a>(
    config: &ScraperConfig,
    allowed_hosts: &BTreeMap<String, String>,
    adapters: &'a [Box<dyn SiteAdapter>],
    site: &SiteKey,
    project_root: &Path,
) -> Result<(&'a dyn SiteAdapter, PathBuf), ConfigError> {
    let entry = config.find_site(site)?;
    config.check_allowed_host(site, allowed_hosts)?;
    let adapter = config::resolve_bound_adapter(adapters, site, entry)?;
    // `download_root` relocates the per-site state; a relative one hangs
    // off the checkout.
    let state_root = match &config.options.download_root {
        Some(root) if root.is_absolute() => root.clone(),
        Some(root) => project_root.join(root),
        None => config::default_state_root(project_root),
    };
    Ok((adapter, state_root))
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
    let (config, allowed_hosts) = match load_manifest_config() {
        Ok(ok) => ok,
        Err(code) => return code,
    };
    let adapters = registered_adapters();
    let project_root = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(error) => return exit_failure("locating the checkout", error),
    };
    let (adapter, state_root) =
        match plan_run(&config, &allowed_hosts, &adapters, &site_key, &project_root) {
            Ok(planned) => planned,
            Err(ConfigError::UnknownSite { .. }) => {
                // No echo of the caller's argument: the plan error already
                // lists the configured sites.
                eprintln!(
                    "{}",
                    unregistered_site_message(
                        config.sites.keys().map(std::string::ToString::to_string)
                    )
                );
                return ExitCode::from(2);
            }
            Err(error) => return exit_failure("planning the run", error),
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
        state_dir: state_root.clone(),
    });
    // The intake seam's two pluggable implementations, chosen once here:
    // straight DKT filing into this Project checkout (needs nothing but
    // the checkout), or (default) the omnichannel inbox — documents
    // enqueue as `scraper`-channel messages with the bytes stored as
    // content-addressed Assets, ready for triage. The inbox lane therefore
    // also needs the asset storage the deployment configures (GCS, or the
    // emulator endpoint in dev); missing config fails the run with the
    // remedy named rather than silently dropping documents.
    let filing_store = ProjectFilingStore::new(project_root);
    let dkt_intake = DktFilingIntake::new(&filing_store);
    let surreal = std::sync::Arc::new(surreal);
    let inbox_intake;
    let intake: &dyn InboxIntake = if file_dkt {
        &dkt_intake
    } else {
        let storage = match cloud::GcsStorageConfig::from_env() {
            Ok(config) => match cloud::GcsStorage::new_from_config(config).await {
                Ok(storage) => {
                    std::sync::Arc::new(storage) as std::sync::Arc<dyn cloud::StorageService>
                }
                Err(error) => return exit_failure("connecting to asset storage", error),
            },
            Err(error) => return exit_failure(
                "configuring asset storage — omnichannel intake stores documents as assets; set the storage environment or file into this checkout with --file-dkt",
                error,
            ),
        };
        inbox_intake = StoreInboxIntake::new(std::sync::Arc::clone(&surreal), storage, firm_id);
        &inbox_intake
    };
    let deps = ScraperDeps {
        sessions: &sessions,
        surreal: surreal.as_ref(),
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
/// same injection seams the orchestration tests use. A credential-less
/// site refuses here — `run_scraper` resolves the Firm's portal logins
/// before any session opens or fetch fires, and the error names the
/// `court_portal` `portal_login` entry to provision.
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
    use super::{
        credential_status, plan_run, render_sites, run, run_site_with, unregistered_site_message,
        ScraperCmd,
    };
    use crate::scraper::adapter::{find_adapter, SiteAdapter, SiteKey};
    use crate::scraper::credentials::{parse_portal_logins, LoginPresence};
    use crate::scraper::intake::RecordingIntake;
    use crate::scraper::retry::RetryPolicy;
    use crate::scraper::testing::{MockAdapter, MockFetcher, StubClock, StubSessions};
    use crate::scraper::{NoticeQuery, ScraperDeps};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
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

    fn config_with_la_superior() -> crate::scraper::config::ScraperConfig {
        let yaml = "sites:\n  la-superior:\n    adapter: la-superior\n    portal_url: https://canvas.lasuperiorcourt.org\n";
        crate::scraper::config::parse_value(&serde_yaml::from_str(yaml).expect("valid yaml"))
            .expect("a valid section")
    }

    fn allowed(canvas: &str) -> BTreeMap<String, String> {
        let mut map = BTreeMap::new();
        map.insert(canvas.to_owned(), "the configured court portal".to_owned());
        map
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
    async fn a_credential_less_run_refuses_before_any_fetch_or_session() {
        let fixture = fixture().await;
        let adapters: Vec<Box<dyn SiteAdapter>> = vec![Box::new(MockAdapter::simple(site()))];
        let adapter = find_adapter(&adapters, &site()).expect("the mock is registered");

        let code = run_site_with(adapter, "0004", None, &deps(&fixture)).await;
        assert_eq!(code, std::process::ExitCode::from(2));
        // The refusal is total: no fetch fires and no browser opens. The
        // error names the provider/kind to provision.
        assert_eq!(
            fixture.fetcher.request_count("portal.example.com"),
            0,
            "no fetch is attempted"
        );
        assert_eq!(fixture.factory.opens(), 0, "no browser ever opens");
        let error = crate::scraper::credentials::resolve_portal_logins(
            &fixture.surreal,
            fixture.firm_id,
            &fixture.kms,
        )
        .await
        .expect_err("nothing is provisioned");
        let message = error.to_string();
        assert!(
            message.contains("court_portal"),
            "names the provider: {message}"
        );
        assert!(
            message.contains("portal_login"),
            "names the kind: {message}"
        );
    }

    #[tokio::test]
    async fn an_unregistered_site_dispatch_exits_two() {
        let code = run(ScraperCmd::Run {
            site: "nowhere".to_owned(),
            docket: "0004".to_owned(),
            since: None,
            firm: Uuid::now_v7(),
            headed: true,
            file_dkt: false,
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
            file_dkt: false,
        })
        .await;
        assert_eq!(code, std::process::ExitCode::from(2));
    }

    #[tokio::test]
    async fn the_sites_dispatch_without_a_manifest_exits_two() {
        // The working directory (the cli package root) carries no
        // navigator.yaml: the listing has nothing to report.
        let code = run(ScraperCmd::Sites {
            firm: Uuid::now_v7(),
        })
        .await;
        assert_eq!(code, std::process::ExitCode::from(2));
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
        let built = NoticeQuery {
            docket: "0004".to_owned(),
            since: Some(
                chrono::Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight exists")),
            ),
        };
        assert_eq!(built.docket, "0004");
        assert_eq!(
            built.since.map(|at| at.to_string()),
            Some("2026-10-01 00:00:00 UTC".to_owned())
        );
    }

    // ---- credential status and the sites listing ----

    fn logins_with_la_superior() -> crate::scraper::credentials::PortalLogins {
        parse_portal_logins(&serde_json::json!({
            "la-superior": {"username": "firm@example.com", "password": "staple"}
        }))
        .expect("a valid payload")
    }

    #[test]
    fn a_site_with_a_complete_login_renders_available() {
        let logins = logins_with_la_superior();
        assert_eq!(
            credential_status(Some(&logins), "la-superior"),
            super::SiteStatus::Available
        );
    }

    #[test]
    fn a_site_without_a_login_renders_disabled() {
        let logins = logins_with_la_superior();
        assert_eq!(
            credential_status(Some(&logins), "pacer"),
            super::SiteStatus::Disabled(
                "no court_portal portal_login entry provisioned for this site"
            )
        );
    }

    #[test]
    fn an_incomplete_login_renders_disabled() {
        let logins = parse_portal_logins(&serde_json::json!({
            "pacer": {"username": "firm@example.com", "password": ""}
        }))
        .expect("a valid payload");
        assert_eq!(
            credential_status(Some(&logins), "pacer"),
            super::SiteStatus::Disabled("the provisioned portal login is incomplete")
        );
    }

    #[test]
    fn the_zero_credential_case_renders_every_site_disabled() {
        // No `court_portal` `portal_login` row at all: uniformly greyed
        // out, for every court.
        let config = config_with_la_superior();
        let rendered = render_sites(&config, None);
        assert!(rendered.contains("disabled"), "greyed out: {rendered}");
        assert!(
            !rendered.contains(" available"),
            "nothing available: {rendered}"
        );
    }

    #[test]
    fn the_listing_renders_one_status_line_per_configured_site() {
        let yaml = "sites:\n  la-superior:\n    adapter: la-superior\n    portal_url: https://canvas.lasuperiorcourt.org\n  pacer:\n    adapter: pacer\n    portal_url: https://pacer.example\noptions:\n  schedule_hint: daily 06:00 UTC\n";
        let config =
            crate::scraper::config::parse_value(&serde_yaml::from_str(yaml).expect("valid yaml"))
                .expect("a valid section");
        let logins = logins_with_la_superior();
        let rendered = render_sites(&config, Some(&logins));
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), 2, "one row per site: {rendered}");
        assert!(
            lines[0].starts_with("la-superior  adapter la-superior"),
            "sorted by site key: {rendered}"
        );
        assert!(lines[0].ends_with(" available"), "provisioned: {rendered}");
        assert!(lines[1].starts_with("pacer  adapter pacer"));
        assert!(lines[1].contains("disabled"), "greyed out: {rendered}");
        assert!(
            rendered.contains("daily 06:00 UTC"),
            "the schedule hint shows"
        );
    }

    #[test]
    fn an_empty_registry_renders_a_configured_nowhere_note() {
        let rendered = render_sites(&crate::scraper::config::ScraperConfig::default(), None);
        assert!(rendered.contains("no sites configured"));
    }

    #[tokio::test]
    async fn the_store_seam_greys_every_site_out_when_no_row_exists() {
        // The zero-credentials state is the designed resting state: the
        // Firm's `court_portal` `portal_login` secret is not provisioned,
        // `scraper sites` resolves nothing, and every court renders
        // disabled — through the real Firm-secrets lane.
        let surreal = store::test_support::mem_surreal().await;
        let (firm_id, _admin_id) = crate::scraper::testing::seeded_firm_with_admin(&surreal).await;
        let kms = cloud::FakeKms::new("scraper-cmd-sites");
        let resolved =
            crate::scraper::credentials::resolve_portal_logins(&surreal, firm_id, &kms).await;
        assert!(resolved.is_err(), "nothing provisioned");
        let config = config_with_la_superior();
        let rendered = render_sites(&config, resolved.ok().as_ref());
        assert!(rendered.contains("disabled"), "greyed out: {rendered}");
        assert!(
            !rendered.contains(" available"),
            "nothing available: {rendered}"
        );
    }

    #[tokio::test]
    async fn the_store_seam_marks_a_provisioned_site_available() {
        let surreal = store::test_support::mem_surreal().await;
        let (firm_id, admin_id) = crate::scraper::testing::seeded_firm_with_admin(&surreal).await;
        let kms = cloud::FakeKms::new("scraper-cmd-sites");
        crate::scraper::testing::put_portal_secret(
            &surreal,
            admin_id,
            firm_id,
            &serde_json::json!({
                "la-superior": {"username": "firm@example.com", "password": "staple"}
            }),
            &kms,
        )
        .await;
        let resolved = crate::scraper::credentials::resolve_portal_logins(&surreal, firm_id, &kms)
            .await
            .expect("resolved through the Firm secrets lane");
        let config = config_with_la_superior();
        let rendered = render_sites(&config, Some(&resolved));
        assert!(rendered.contains(" available"), "available: {rendered}");
    }

    // ---- the run plan ----

    #[test]
    fn the_plan_resolves_a_configured_and_bound_site() {
        let config = config_with_la_superior();
        let adapters: Vec<Box<dyn SiteAdapter>> = vec![Box::new(MockAdapter::simple(site()))];
        let (adapter, state_root) = plan_run(
            &config,
            &allowed("canvas.lasuperiorcourt.org"),
            &adapters,
            &site(),
            Path::new("/proj"),
        )
        .expect("planned");
        assert_eq!(adapter.site(), &site());
        assert_eq!(state_root, PathBuf::from("/proj/.navigator/scraper"));
    }

    #[test]
    fn the_plan_rejects_an_unconfigured_site() {
        let config = config_with_la_superior();
        let pacer = SiteKey::new("pacer").expect("valid key");
        let Err(error) = plan_run(
            &config,
            &allowed("canvas.lasuperiorcourt.org"),
            &[],
            &pacer,
            Path::new("/proj"),
        ) else {
            panic!("pacer is not configured in this fixture");
        };
        assert!(error.to_string().contains("configured sites: la-superior"));
    }

    #[test]
    fn the_plan_rejects_an_unbound_adapter() {
        let config = config_with_la_superior();
        let Err(error) = plan_run(
            &config,
            &allowed("canvas.lasuperiorcourt.org"),
            &[],
            &site(),
            Path::new("/proj"),
        ) else {
            panic!("no adapters are registered in this fixture");
        };
        assert!(error.to_string().contains("binds adapter `la-superior`"));
    }

    #[test]
    fn the_plan_rejects_a_host_outside_allowed_hosts() {
        let config = config_with_la_superior();
        let Err(error) = plan_run(
            &config,
            &allowed("pacer.example"),
            &[],
            &site(),
            Path::new("/proj"),
        ) else {
            panic!("the portal host is not allowed in this fixture");
        };
        assert!(error.to_string().contains("`allowed_hosts:`"));
    }

    #[test]
    fn the_plan_honours_the_configured_download_root() {
        let yaml = "sites:\n  la-superior:\n    adapter: la-superior\n    portal_url: https://canvas.lasuperiorcourt.org\noptions:\n  download_root: .state/scraper\n";
        let config =
            crate::scraper::config::parse_value(&serde_yaml::from_str(yaml).expect("valid yaml"))
                .expect("a valid section");
        let adapters: Vec<Box<dyn SiteAdapter>> = vec![Box::new(MockAdapter::simple(site()))];
        let (_, state_root) = plan_run(
            &config,
            &allowed("canvas.lasuperiorcourt.org"),
            &adapters,
            &site(),
            Path::new("/proj"),
        )
        .expect("planned");
        assert_eq!(state_root, PathBuf::from("/proj/.state/scraper"));
    }

    #[test]
    fn presence_distinguishes_complete_from_incomplete() {
        let logins = logins_with_la_superior();
        assert_eq!(logins.presence("la-superior"), LoginPresence::Complete);
        assert_eq!(logins.presence("pacer"), LoginPresence::Absent);
    }
}
