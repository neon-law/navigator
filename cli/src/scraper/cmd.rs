//! The `navigator scraper` surface: list the registered site adapters and
//! run one over one docket. A run resolves the Firm's portal login, opens
//! the pinned-Chrome session, and hands every fetched document to the
//! omnichannel inbox through the intake seam — never directly into a
//! matter folder. Non-secret run settings are paths and dates only; the
//! secrets stay in the Firm secrets store.

use std::process::ExitCode;

use clap::Subcommand;
use uuid::Uuid;

use crate::scraper::adapter::{find_adapter, NoticeQuery, SiteKey};
use crate::scraper::fetcher::HttpFetcherOverReqwest;
use crate::scraper::intake::UnwiredIntake;
use crate::scraper::retry::{RetryPolicy, TokioClock};
use crate::scraper::session::{ChromiumSessions, SessionConfig};
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
        } => run_site(&site, &docket, since, firm, headed).await,
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
    let state_root = match std::env::current_dir() {
        Ok(dir) => dir.join(".navigator").join("scraper"),
        Err(error) => return exit_failure("locating the checkout", error),
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
        state_dir: state_root,
    });
    let deps = ScraperDeps {
        sessions: &sessions,
        surreal: &surreal,
        firm_id,
        kms: kms.as_ref(),
        fetcher: &fetcher,
        intake: &UnwiredIntake,
        clock: &TokioClock,
        login_policy: RetryPolicy::login_default(),
        fetch_policy: RetryPolicy::fetch_default(),
        staging_dir: staging_dir.clone(),
        ledger_path: staging_dir.join("ledger.json"),
        downloads_dir: site_dir.join("documents"),
        run_state_path: site_dir.join("run-state.json"),
    };

    let query = NoticeQuery {
        docket: docket.to_owned(),
        since: since.map(|date| {
            use chrono::TimeZone as _;
            chrono::Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight exists"))
        }),
    };
    match run_scraper(adapter, &query, &deps).await {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => exit_failure("run failed", error),
    }
}

#[cfg(test)]
mod tests {
    use super::unregistered_site_message;

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
}
