//! Plausible page analytics — a per-brand injection into the public page
//! shell, on for the real production deployment only.
//!
//! Each brand face is its own Plausible site with its own script, so the
//! script id is compiled in per [`BrandKey`] by [`script_id`].
//!
//! The ids are the firm's, so the gate is the deployment: [`enabled_from`]
//! admits only an explicit `NAVIGATOR_ENVIRONMENT=production` whose matters
//! are not simulated. That keeps local runs, tests, KIND, and the staging ring
//! (a production profile carrying simulated matters) from counting visits
//! against the live dashboards. An unset `NAVIGATOR_ENVIRONMENT` resolves to
//! the production profile elsewhere, but not here: a bare `cargo run` is not
//! the firm's website.
//!
//! The snippet Plausible publishes is an async vendor script plus an inline
//! stub that queues calls and runs `plausible.init()`. The stub is served
//! instead as the first-party [`PLAUSIBLE_LOADER_HREF`], so it needs no nonce;
//! see [`PlausibleSite::script_tags`] for the ordering that makes that work.
//!
//! [`ConversionReporter`] is the second half: confirmed lead-generating
//! actions (a contact form, a client's "start" click) reported as Plausible
//! custom events over its server-side Events API, gated by the same
//! [`enabled_from`]. This exists because the browser-side snippet above only
//! ever fires page views, and at least one success path (the contact form)
//! redirects success and rejection to the same URL — no page view can stand
//! in for "this submission succeeded." The handler that saw the write commit
//! reports it directly, once, with no personal data in the event payload.

use std::net::IpAddr;
use std::time::Duration;

use views::brand::BrandKey;

/// Plausible Cloud, which serves the script and receives the events.
pub const PLAUSIBLE_ORIGIN: &str = "https://plausible.io";

/// The first-party stub that queues calls until the vendor script arrives.
pub const PLAUSIBLE_LOADER_HREF: &str = "/public/js/plausible.js";
const UNCONFIGURED_DEATH_AND_DIVORCE_ID: &str = "pa-unconfigured-death-and-divorce";
const UNCONFIGURED_AEGIS_ID: &str = "pa-unconfigured-aegis";
const UNCONFIGURED_GROUNDWORK_ID: &str = "pa-unconfigured-groundwork";
const UNCONFIGURED_IEP_DEFENDERS_ID: &str = "pa-unconfigured-iep-defenders";

/// The Plausible script id (the `pa-…` stem of the snippet's script URL) for
/// `key`'s public site.
///
/// Not a secret: it ships in the HTML every visitor receives.
#[must_use]
pub const fn script_id(key: BrandKey) -> Option<&'static str> {
    match key {
        BrandKey::Aegis => Some(UNCONFIGURED_AEGIS_ID),
        BrandKey::Groundwork => Some(UNCONFIGURED_GROUNDWORK_ID),
        BrandKey::GuiebShook => Some("pa-CjqS6UQxwrqmMRdiG44m3"),
        BrandKey::CyberInjuryLaw => Some("pa-Ns-yAKpkSNXzfvnEJYZJX"),
        BrandKey::Neon => Some("pa-dktgfAn-5R5ufpXARu6zb"),
        BrandKey::Vesta => Some("pa-0NXcwgtQALjMsqFs8YbfN"),
        BrandKey::DeleteYourData => Some("pa-h5nRTkQB8L0g9LJDlCctG"),
        BrandKey::LawyerShook => Some("pa-tN2nM3ILjPwZ3kHAqRT2j"),
        BrandKey::Abhaya => Some("pa-VFh7XqWwYjyX-p1k26flH"),
        BrandKey::Misericordia => Some("pa-FRRt7d62fXf8Ixsc6ZmSh"),
        BrandKey::DeleteYourDebt => Some("pa-S3SbSActjysFwN64GUe6D"),
        BrandKey::Daybridge => Some("pa-j2JWmqoBTNJPRb64xIZ2o"),
        BrandKey::Summons => Some("pa-WUB2cKHxULoouY7ALHjwH"),
        BrandKey::DeathAndDivorce => Some(UNCONFIGURED_DEATH_AND_DIVORCE_ID),
        // No analytics site is configured for the practice yet; the
        // unconfigured stub id keeps the public brand excluded until it is.
        BrandKey::IepDefenders => Some(UNCONFIGURED_IEP_DEFENDERS_ID),
    }
}

/// Whether this deployment counts visits: an explicit production profile with
/// real (not simulated) matters. `get` reads the environment, so every shape
/// is unit-testable without mutating process state.
pub fn enabled_from<F: Fn(&str) -> Option<String>>(get: F) -> bool {
    let explicit_production =
        get(store::config::NAVIGATOR_ENVIRONMENT).as_deref() == Some("production");
    explicit_production
        && store::sample_matters_from(store::DeploymentEnvironment::Production, get) == Ok(false)
}

/// One brand's Plausible site, named by its script id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlausibleSite {
    script_id: Option<&'static str>,
}

impl PlausibleSite {
    /// `key`'s compiled site.
    #[must_use]
    pub const fn for_brand(key: BrandKey) -> Self {
        Self {
            script_id: script_id(key),
        }
    }

    /// A site with `script_id`, for tests that inject one on the request.
    #[must_use]
    pub const fn new(script_id: &'static str) -> Self {
        Self {
            script_id: Some(script_id),
        }
    }

    /// The two `<script>` elements that start analytics, for injection at the
    /// end of a public page's `<body>`: the first-party stub, then the vendor
    /// script.
    ///
    /// Both carry `defer`, and the order is load-bearing. Deferred classic
    /// scripts execute in document order, so the stub has defined
    /// `window.plausible` and queued `init()` before the vendor script runs
    /// and drains that queue — the guarantee Plausible's own snippet gets from
    /// an inline block after an `async` tag.
    #[must_use]
    pub fn script_tags(&self) -> String {
        let Some(script_id) = self.script_id else {
            return String::new();
        };
        if [
            UNCONFIGURED_DEATH_AND_DIVORCE_ID,
            UNCONFIGURED_AEGIS_ID,
            UNCONFIGURED_GROUNDWORK_ID,
            UNCONFIGURED_IEP_DEFENDERS_ID,
        ]
        .contains(&script_id)
        {
            return String::new();
        }
        format!(
            "<script src=\"{PLAUSIBLE_LOADER_HREF}\" defer></script>\
             <script src=\"{PLAUSIBLE_ORIGIN}/js/{id}.js\" defer></script>",
            id = webapp::html_escape::escape_attr(script_id),
        )
    }
}

/// A confirmed lead-generating action worth a Plausible goal — named for
/// the string that appears in the dashboard's Goals panel.
///
/// Fired once, from the server, at the moment success is confirmed —
/// never from a page view. The contact flow redirects success and
/// rejection to the same `/contact/sent` URL, so a page view there cannot
/// stand in for "this submission succeeded"; only the handler that saw the
/// write commit knows that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionGoal {
    ContactSubmitted,
    IntakeStarted,
}

impl ConversionGoal {
    /// The event name Plausible's Goals panel matches on.
    #[must_use]
    pub const fn event_name(self) -> &'static str {
        match self {
            Self::ContactSubmitted => webapp::lead_sources::CONTACT_GOAL,
            Self::IntakeStarted => webapp::lead_sources::INTAKE_GOAL,
        }
    }
}

/// Whether `key` has a real Plausible site behind it — the same
/// "unconfigured campaign" exclusion [`PlausibleSite::script_tags`] applies
/// to page views. A conversion for a brand with no page-view tracking would
/// be a goal with no dashboard to appear on.
fn has_configured_site(key: BrandKey) -> bool {
    !PlausibleSite::for_brand(key).script_tags().is_empty()
}

/// `key`'s fixed Plausible-registered domain: the property tied to its
/// [`script_id`], independent of whatever branding this request happens to
/// render under. A white-labeled deployment still reports Neon's own
/// script to Neon's own `neonlaw.com` dashboard, exactly as the compiled
/// script id table already does.
fn conversion_domain(key: BrandKey) -> &'static str {
    key.resolve_branding(&views::brand::DEFAULT_BRANDING)
        .primary_domain
}

/// Plausible's Events API rejects a request with no `User-Agent`. A
/// server-side beacon has no real browser behind it when the inbound
/// request somehow omitted one; this keeps the goal counting rather than
/// silently dropping.
const FALLBACK_USER_AGENT: &str = "Navigator/conversion-beacon";

/// A thin client over Plausible's server-side Events API
/// (`POST {origin}/api/event`) — the only way to record a conversion whose
/// success a page view cannot distinguish from failure. Cloned cheaply
/// (an `Arc`-backed [`reqwest::Client`] plus two small fields), so it lives
/// on [`crate::AppState`] and is shared across requests.
#[derive(Debug, Clone)]
pub struct ConversionReporter {
    origin: String,
    http: reqwest::Client,
    enabled: bool,
}

impl ConversionReporter {
    /// The reporter this deployment actually runs: pointed at Plausible
    /// Cloud, gated the same way page views are ([`enabled_from`]) so
    /// local runs, tests, KIND, and staging never post to the live
    /// dashboards.
    #[must_use]
    pub fn live() -> Self {
        Self::new(
            PLAUSIBLE_ORIGIN,
            enabled_from(|key| std::env::var(key).ok()),
        )
    }

    /// Build a reporter pointed at `origin` — a `wiremock` server in
    /// tests — firing whenever `enabled`, independent of the deployment
    /// environment.
    #[must_use]
    pub fn new(origin: impl Into<String>, enabled: bool) -> Self {
        Self {
            origin: origin.into(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .build()
                .expect("reqwest client with a timeout builds"),
            enabled,
        }
    }

    /// Report `goal` for `key` at `path`, attributed to `ip`/`user_agent` so
    /// Plausible counts it against the same visit its page views did.
    ///
    /// A no-op when disabled or when `key` has no configured site — the
    /// brand-attribution and "unconfigured campaign" gates a caller would
    /// otherwise have to duplicate. Errors are logged, never surfaced: a
    /// conversion beacon must not fail the request that earned it.
    pub async fn record(
        &self,
        key: BrandKey,
        goal: ConversionGoal,
        path: &str,
        ip: IpAddr,
        user_agent: Option<&str>,
    ) {
        if !self.enabled || !has_configured_site(key) {
            return;
        }
        let domain = conversion_domain(key);
        let url = format!("https://{domain}{path}");
        let body = serde_json::json!({
            "name": goal.event_name(),
            "url": url,
            "domain": domain,
        });
        let sent = self
            .http
            .post(format!("{}/api/event", self.origin))
            .header("User-Agent", user_agent.unwrap_or(FALLBACK_USER_AGENT))
            .header("X-Forwarded-For", ip.to_string())
            .json(&body)
            .send()
            .await;
        match sent {
            Ok(response) if !response.status().is_success() => {
                tracing::warn!(
                    status = %response.status(),
                    domain,
                    event = goal.event_name(),
                    "plausible conversion beacon rejected"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    domain,
                    event = goal.event_name(),
                    "plausible conversion beacon failed"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        conversion_domain, enabled_from, script_id, ConversionGoal, ConversionReporter,
        PlausibleSite, PLAUSIBLE_LOADER_HREF,
    };
    use views::brand::BrandKey;

    /// `enabled_from` end to end, over every shape the deployment environment
    /// can take. One injected lookup per row, rather than one test per case.
    #[test]
    fn only_explicit_production_without_simulated_matters_is_enabled() {
        let env = "NAVIGATOR_ENVIRONMENT";
        let sim = "NAVIGATOR_SIMULATED_MATTERS";
        let cases: &[(&[(&str, &str)], bool)] = &[
            (&[(env, "production"), (sim, "false")], true),
            (&[(env, "production")], true),
            // Staging: the production profile over simulated matters.
            (&[(env, "production"), (sim, "true")], false),
            (&[(env, "dev")], false),
            (&[(env, "dev"), (sim, "false")], false),
            // Unset is the local default and the test process, not the website.
            (&[], false),
            (&[(env, "")], false),
            (&[(env, "production"), (sim, "yes")], false),
        ];
        for (pairs, want) in cases {
            let got = enabled_from(|key| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| (*v).to_string())
            });
            assert_eq!(got, *want, "{pairs:?}");
        }
    }

    /// Every brand resolves a distinct, well-shaped script id — the one thing
    /// worth asserting about a table of compiled constants.
    #[test]
    fn every_brand_resolves_a_distinct_plausible_script_id() {
        let ids: Vec<_> = BrandKey::ALL
            .iter()
            .filter_map(|key| script_id(*key))
            .collect();
        for id in &ids {
            assert!(id.starts_with("pa-"), "{id}");
            assert!(
                id.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "{id}"
            );
        }
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(ids.len(), unique.len(), "{ids:?}");
    }

    #[test]
    fn an_unconfigured_campaign_has_no_analytics() {
        assert!(PlausibleSite::for_brand(BrandKey::DeathAndDivorce)
            .script_tags()
            .is_empty());
        assert!(BrandKey::LIVE.iter().all(|key| script_id(*key).is_some()));
    }

    /// Stub first, vendor second, both deferred, neither inline.
    #[test]
    fn the_script_tags_are_the_stub_then_the_vendor_script() {
        let tags = PlausibleSite::new("pa-dktgfAn-5R5ufpXARu6zb").script_tags();
        assert_eq!(
            tags,
            "<script src=\"/public/js/plausible.js\" defer></script>\
             <script src=\"https://plausible.io/js/pa-dktgfAn-5R5ufpXARu6zb.js\" defer></script>"
        );
        assert!(tags.find(PLAUSIBLE_LOADER_HREF).unwrap() < tags.find("plausible.io/js").unwrap());
    }

    #[test]
    fn goal_names_match_the_dashboard_strings() {
        assert_eq!(
            ConversionGoal::ContactSubmitted.event_name(),
            "Contact: Submitted"
        );
        assert_eq!(
            ConversionGoal::IntakeStarted.event_name(),
            "Intake: Started"
        );
    }

    /// Every live brand reports to its own fixed, Plausible-registered
    /// domain — the property tied to its compiled script id — regardless
    /// of whatever branding a request happens to render under.
    #[test]
    fn each_brand_reports_its_own_fixed_registered_domain() {
        for (key, domain) in [
            (BrandKey::Neon, "neonlaw.com"),
            (BrandKey::DeleteYourData, "deleteyourdata.com"),
            (BrandKey::Vesta, "vestaestateplanning.com"),
            (BrandKey::Misericordia, "misericordialaw.com"),
            (BrandKey::Abhaya, "abhayaimmigration.com"),
            (BrandKey::LawyerShook, "lawyershook.com"),
            (BrandKey::GuiebShook, "guiebandshook.com"),
            (BrandKey::DeleteYourDebt, "deleteyourdebt.com"),
            (BrandKey::Summons, "summonsdefense.nyc"),
            (BrandKey::Daybridge, "daybridgedivorce.com"),
        ] {
            assert_eq!(conversion_domain(key), domain, "{key:?}");
        }
    }

    #[tokio::test]
    async fn a_confirmed_conversion_posts_the_domain_event_and_no_personal_data() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/event"))
            .respond_with(ResponseTemplate::new(202))
            .expect(1)
            .mount(&server)
            .await;

        let reporter = ConversionReporter::new(server.uri(), true);
        reporter
            .record(
                BrandKey::Neon,
                ConversionGoal::ContactSubmitted,
                "/contact",
                "203.0.113.9".parse().unwrap(),
                Some("Mozilla/5.0 test-agent"),
            )
            .await;

        let requests = server
            .received_requests()
            .await
            .expect("request recording is on by default");
        let request = &requests[0];
        let body: serde_json::Value = serde_json::from_slice(&request.body).expect("json body");
        assert_eq!(body["name"], "Contact: Submitted");
        assert_eq!(body["domain"], "neonlaw.com");
        assert_eq!(body["url"], "https://neonlaw.com/contact");
        assert_eq!(
            body.as_object().unwrap().len(),
            3,
            "no field beyond name/url/domain: {body}"
        );
        assert_eq!(
            request.headers.get("x-forwarded-for").unwrap(),
            "203.0.113.9"
        );
        assert_eq!(
            request.headers.get("user-agent").unwrap(),
            "Mozilla/5.0 test-agent"
        );
    }

    #[tokio::test]
    async fn a_disabled_reporter_sends_nothing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/event"))
            .respond_with(ResponseTemplate::new(202))
            .expect(0)
            .mount(&server)
            .await;

        let reporter = ConversionReporter::new(server.uri(), false);
        reporter
            .record(
                BrandKey::Neon,
                ConversionGoal::ContactSubmitted,
                "/contact",
                "203.0.113.9".parse().unwrap(),
                None,
            )
            .await;
    }

    #[tokio::test]
    async fn an_unconfigured_brand_sends_nothing() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/event"))
            .respond_with(ResponseTemplate::new(202))
            .expect(0)
            .mount(&server)
            .await;

        let reporter = ConversionReporter::new(server.uri(), true);
        reporter
            .record(
                BrandKey::DeathAndDivorce,
                ConversionGoal::ContactSubmitted,
                "/contact",
                "203.0.113.9".parse().unwrap(),
                None,
            )
            .await;
    }
}
