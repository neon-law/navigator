//! One versioned scenario table shared by the API and live-browser acceptance
//! runs for the synthetic portfolio.

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{body::Body, http::Request, Router};
use portal::session::{SessionData, SESSION_COOKIE_NAME};
use portal::SessionStore;
use serde_json::{json, Value};
use tower::ServiceExt;

pub const TARGET: &str = store::synthetic_portfolio::STAGING_TARGET;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lens {
    Firm,
    Client,
}

#[derive(Debug, Clone, Copy)]
pub struct Scenario {
    pub id: &'static str,
    pub lens: Lens,
    pub actor_email: &'static str,
    pub route: &'static str,
    pub status: u16,
    pub present: &'static [&'static str],
    pub absent: &'static [&'static str],
    pub screenshot: &'static str,
}

pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        id: "firm.lifecycle-list",
        lens: Lens::Firm,
        actor_email: "lawyer@neonlaw.com",
        route: "/app/projects",
        status: 200,
        present: &[
            "Fixture Halcyon Ventures — Pitch",
            "Fixture Ledgerwood Freight — Invoice Matrix",
            "Fixture Ashgrove Analytics — Reopened Matter",
        ],
        absent: &["Fixture Vantongeren Imports — EUR Invoice Group"],
        screenshot: "firm-lifecycle-list.png",
    },
    Scenario {
        id: "firm.closed-lifecycle-list",
        lens: Lens::Firm,
        actor_email: "lawyer@neonlaw.com",
        route: "/app/projects/closed",
        status: 200,
        present: &[
            "Fixture Ashworth Logistics — Closed Engagement",
            "Fixture Fenwick Robotics — Declined Pitch",
            "Fixture Moorcroft Textiles — Archived Matter",
        ],
        absent: &["Fixture Halcyon Ventures — Pitch"],
        screenshot: "firm-closed-lifecycle-list.png",
    },
    Scenario {
        id: "client.participation-list",
        lens: Lens::Client,
        actor_email: "client@neonlaw.com",
        route: "/app/projects",
        status: 200,
        present: &[
            "Fixture Ridgeline — Synthetic Portfolio Matter",
            "Fixture Ledgerwood Freight — Invoice Matrix",
            "Fixture Kestrel Robotics — Pooled Draw Matter A",
        ],
        absent: &[
            "Fixture Vantongeren Imports — EUR Invoice Group",
            "Fixture Windrow Logistics — Pooled Draw Matter B",
        ],
        screenshot: "client-participation-list.png",
    },
    Scenario {
        id: "client.invoices-currencies",
        lens: Lens::Client,
        actor_email: "client@neonlaw.com",
        route: "/app/projects/synthetic-portfolio-invoice-matrix",
        status: 200,
        present: &[
            "Fixture Ledgerwood Freight — Invoice Matrix",
            "Status: AUTHORISED",
            "Status: PAID",
            "$1,200.00",
            "$800.00",
            "$2,000.00",
            "2026-01-15",
            "2026-04-15",
        ],
        absent: &["EUR", "synthetic-portfolio-iolta-account"],
        screenshot: "client-invoice-currencies.png",
    },
    Scenario {
        id: "client.trust-allocation",
        lens: Lens::Client,
        actor_email: "client@neonlaw.com",
        route: "/app/projects/synthetic-portfolio-pooled-draw-a",
        status: 200,
        present: &[
            "How your funds were applied",
            "Paid in: $1,000.00",
            "$600.00",
            "$400.00",
        ],
        absent: &["$1,400.00", "synthetic-portfolio-iolta-account"],
        screenshot: "client-trust-allocation.png",
    },
    Scenario {
        id: "client.documents-portal",
        lens: Lens::Client,
        actor_email: "client@neonlaw.com",
        route: "/app/projects/synthetic-portfolio-001",
        status: 200,
        present: &[
            "synthetic-portfolio-intake.md",
            "Sample matters",
            "purposefully a demo environment",
        ],
        absent: &[
            "synthetic-portfolio-firm-only.md",
            "synthetic-portfolio-invoice",
            "synthetic-portfolio-iolta-account",
            "Client portal",
        ],
        screenshot: "client-documents-portal.png",
    },
    Scenario {
        id: "client.offboarding-document",
        lens: Lens::Client,
        actor_email: "client@neonlaw.com",
        route: "/app/projects/synthetic-portfolio-closed-pitch",
        status: 200,
        present: &[
            "Fixture Fenwick Robotics — Declined Pitch",
            "synthetic-portfolio-closed-pitch-offboarding__letter.md",
        ],
        absent: &[
            "synthetic-portfolio-firm-only.md",
            "synthetic-portfolio-iolta-account",
        ],
        screenshot: "client-offboarding-document.png",
    },
    Scenario {
        id: "client.cross-project-denial",
        lens: Lens::Client,
        actor_email: "client@neonlaw.com",
        route: "/app/projects/synthetic-portfolio-invoice-eur",
        status: 404,
        present: &[],
        absent: &[
            "Fixture Vantongeren Imports — EUR Invoice Group",
            "synthetic-portfolio-invoice-eur-unpaid",
            "€900.00",
        ],
        screenshot: "client-cross-project-denial.png",
    },
];

#[derive(Debug, Clone)]
pub struct ApiResult {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct ScenarioResult {
    pub scenario_id: &'static str,
    pub actual_status: u16,
    pub passed: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct ApplyCounts {
    pub first_created: usize,
    pub second_created: usize,
    pub second_unchanged: usize,
}

#[must_use]
pub fn scenario(id: &str) -> Option<&'static Scenario> {
    SCENARIOS.iter().find(|scenario| scenario.id == id)
}

pub fn assert_result(id: &str, status: u16, body: &str) -> Result<(), String> {
    check_result(id, status, body).map_err(|error| format!("{error}; body: {}", truncate(body)))
}

pub fn check_result(id: &str, status: u16, body: &str) -> Result<(), String> {
    let scenario =
        scenario(id).ok_or_else(|| format!("unknown portfolio acceptance scenario `{id}`"))?;
    let body = body.to_lowercase();
    if status != scenario.status {
        return Err(format!(
            "{} expected HTTP {}, got {status}",
            scenario.id, scenario.status,
        ));
    }
    for fact in scenario.present {
        if !body.contains(&fact.to_lowercase()) {
            return Err(format!("{} expected visible fact {fact:?}", scenario.id));
        }
    }
    for fact in scenario.absent {
        if body.contains(&fact.to_lowercase()) {
            return Err(format!("{} unexpectedly disclosed {fact:?}", scenario.id));
        }
    }
    Ok(())
}

/// Configure the app process to render the synthetic-matters disclosure used
/// by the staging acceptance target. Call before the first HTML response: the
/// portal resolves this deployment setting once at process startup.
pub fn configure_test_environment() {
    std::env::set_var("NAVIGATOR_ENVIRONMENT", "production");
    std::env::set_var("NAVIGATOR_SIMULATED_MATTERS", "true");
}

pub async fn request_api(app: &Router, sessions: &SessionStore, id: &str) -> Result<ApiResult> {
    let scenario = scenario(id)
        .ok_or_else(|| anyhow::anyhow!("unknown portfolio acceptance scenario `{id}`"))?;
    let surreal = crate::shared_surreal().await;
    let person = store::persons::find_by_email_ci(&surreal, scenario.actor_email)
        .await
        .context("look up acceptance actor")?
        .ok_or_else(|| {
            anyhow::anyhow!("acceptance actor {} is not seeded", scenario.actor_email)
        })?;
    let role = person.role;
    let session = SessionData {
        sub: format!("acceptance-{}", scenario.actor_email),
        email: Some(scenario.actor_email.to_string()),
        person_id: Some(person.id),
        exp: portal::session::now_unix_secs() + 60,
        role,
        csrf_token: "acceptance-csrf".into(),
        source: portal::session::SessionSource::Browser,
        provider: None,
        viewing_as_dri: None,
        scope: None,
    };
    let cookie = format!("{SESSION_COOKIE_NAME}={}", sessions.encode(&session));
    let request = Request::builder()
        .uri(scenario.route)
        .header("cookie", cookie)
        .body(Body::empty())
        .context("build acceptance request")?;
    let response = app
        .clone()
        .oneshot(request)
        .await
        .context("dispatch acceptance request")?;
    let status = response.status().as_u16();
    let body = crate::body_string(response).await;
    Ok(ApiResult { status, body })
}

/// Apply the shared fixture and prove a second apply reports no writes in the
/// same isolated feature-test store.
pub async fn prepare_test_fixture() -> ApplyCounts {
    static APPLIED: tokio::sync::OnceCell<ApplyCounts> = tokio::sync::OnceCell::const_new();
    *APPLIED
        .get_or_init(|| async {
            let surreal = crate::shared_surreal().await;
            let storage = crate::fs_storage("portfolio-acceptance").await;
            store::seed::seed_canonical(&surreal, &storage)
                .await
                .expect("seed canonical fixture dependencies");
            let first = store::synthetic_portfolio::apply(
                &surreal,
                &storage,
                store::DeploymentEnvironment::Dev,
                Some(TARGET),
            )
            .await
            .expect("apply versioned synthetic portfolio");
            let second = store::synthetic_portfolio::apply(
                &surreal,
                &storage,
                store::DeploymentEnvironment::Dev,
                Some(TARGET),
            )
            .await
            .expect("repeat synthetic portfolio apply");
            assert_eq!(first.version, store::synthetic_portfolio::PORTFOLIO_VERSION);
            assert_eq!(first.target, TARGET);
            assert_eq!(second.version, first.version);
            assert_eq!(second.target, first.target);
            assert_eq!(second.created(), 0, "repeat apply must not write fixtures");
            bridge_acceptance_people(&surreal).await;
            add_firm_only_document(&surreal, &storage).await;
            ApplyCounts {
                first_created: first.created(),
                second_created: second.created(),
                second_unchanged: second.unchanged(),
            }
        })
        .await
}

pub fn report_json(results: &[ScenarioResult], counts: ApplyCounts) -> Result<String> {
    serde_json::to_string_pretty(&report_value(results, counts))
        .context("serialize acceptance report")
}

#[must_use]
pub fn report_value(results: &[ScenarioResult], counts: ApplyCounts) -> Value {
    json!({
        "fixture_version": store::synthetic_portfolio::PORTFOLIO_VERSION,
        "target": TARGET,
        "fixture_application": {
            "first_apply_created": counts.first_created,
            "second_apply_created": counts.second_created,
            "second_apply_unchanged": counts.second_unchanged,
        },
        "results": results.iter().map(|result| {
            let scenario = scenario(result.scenario_id)
                .expect("invariant: report results originate from SCENARIOS");
            json!({
                "scenario_id": scenario.id,
                "actor": scenario.actor_email,
                "lens": match scenario.lens { Lens::Firm => "firm", Lens::Client => "client" },
                "route": scenario.route,
                "expected_status": scenario.status,
                "actual_status": result.actual_status,
                "expected_visible_facts": scenario.present,
                "expected_absent_facts": scenario.absent,
                "screenshot_checkpoint": scenario.screenshot,
                "result": if result.passed { "pass" } else { "fail" },
                "error": result.error,
            })
        }).collect::<Vec<_>>(),
    })
}

/// Link the synthetic portfolio to the acceptance actors used by API and
/// browser runners without introducing real client data.
pub async fn bridge_acceptance_people(surreal: &store::surreal::SurrealDb) {
    let client = ensure_actor(surreal, "client@neonlaw.com", store::persons::Role::Client).await;
    for code in [
        "synthetic-portfolio-001",
        "synthetic-portfolio-invoice-matrix",
        "synthetic-portfolio-pooled-draw-a",
        "synthetic-portfolio-closed-pitch",
    ] {
        add_person_to_project(surreal, code, client.id, "client").await;
    }
    let lawyer = ensure_actor(surreal, "lawyer@neonlaw.com", store::persons::Role::Lawyer).await;
    for code in [
        "synthetic-portfolio-pitch",
        "synthetic-portfolio-closed-active",
        "synthetic-portfolio-closed-pitch",
        "synthetic-portfolio-archived",
        "synthetic-portfolio-reopened",
        "synthetic-portfolio-invoice-matrix",
        "synthetic-portfolio-pooled-draw-a",
    ] {
        add_person_to_project(surreal, code, lawyer.id, "lawyer").await;
    }
}

async fn ensure_actor(
    surreal: &store::surreal::SurrealDb,
    email: &str,
    role: store::persons::Role,
) -> store::persons::Person {
    store::test_support::ensure_person(
        surreal,
        &store::persons::NewPerson {
            oidc_subject: Some(format!("rauthy-{email}-subject")),
            ..store::persons::NewPerson::with_role(email.to_string(), email.to_string(), role)
        },
    )
    .await
}

async fn add_person_to_project(
    surreal: &store::surreal::SurrealDb,
    code: &str,
    person_id: uuid::Uuid,
    participation: &str,
) {
    let project = store::projects::find_by_code(surreal, code)
        .await
        .expect("look up portfolio project")
        .unwrap_or_else(|| panic!("portfolio project `{code}` is missing"));
    if store::projects::participation_for_person(surreal, person_id, project.id)
        .await
        .expect("check portfolio participation")
        .is_none()
    {
        store::projects::add_participation(surreal, project.id, person_id, participation)
            .await
            .expect("add local Rauthy actor to portfolio project");
    }
}

/// Add the internal-only document used to prove that client pages do not
/// disclose firm material.
pub async fn add_firm_only_document(
    surreal: &store::surreal::SurrealDb,
    storage: &Arc<dyn cloud::StorageService>,
) {
    let project = store::projects::find_by_code(surreal, "synthetic-portfolio-001")
        .await
        .expect("look up base portfolio project")
        .expect("base portfolio project is applied");
    store::documents::ingest_bytes_exactly_once(
        surreal,
        storage,
        &store::documents::IngestArgs {
            project_id: project.id,
            source: "generated",
            filename: "synthetic-portfolio-firm-only.md",
            kind: "unclassified",
            content_type: "text/markdown",
            description: Some("Invented firm-only acceptance document."),
            secondary_storage_key: None,
            visibility: store::documents::visibility::INTERNAL,
        },
        b"# Synthetic firm-only acceptance document\n",
    )
    .await
    .expect("ingest synthetic firm-only acceptance document");
}

fn truncate(body: &str) -> String {
    const LIMIT: usize = 500;
    if body.len() <= LIMIT {
        body.to_string()
    } else {
        format!("{}…", &body[..LIMIT])
    }
}
