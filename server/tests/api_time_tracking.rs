#![allow(clippy::doc_markdown)]
//! Integration tests for the time-tracking doors: the Billing tab's time
//! surface beside the project-tasks API — manual entries, invoice marking and
//! write-offs, the durable Timer (start, stop, discard), and the fee
//! structures the entries are priced against.
//!
//! Like the task doors, these run on the PostgreSQL lane
//! (`NAVIGATOR_TEST_POSTGRES_PORT`): each test process builds one
//! canonical-schema database, points the app's shared `PostgresDb::from_env()`
//! pool at it, and drives the real handlers through the full router — embedded
//! Rego policy included. Without the lane they skip, unless
//! `NAV_REQUIRE_POSTGRES=1` turns the skip into a failure.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use portal::session::SessionData;
use portal::{AppState, SessionStore};
use serde_json::json;
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const KEY: &str = "api-time-tracking-test-key";

static DATABASE: tokio::sync::OnceCell<Option<String>> = tokio::sync::OnceCell::const_new();

/// Create one canonical-schema database on the lane and point the app's
/// `PostgresDb::from_env()` pool at it. `None` when the lane is absent —
/// the test then skips.
async fn time_store() -> Option<&'static str> {
    DATABASE
        .get_or_init(|| async {
            let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
                assert_ne!(
                    std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
                    Ok("1"),
                    "the PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
                );
                return None;
            };
            let admin_database = std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE")
                .unwrap_or_else(|_| "smoke".into());
            let options = sqlx::postgres::PgConnectOptions::new()
                .host("127.0.0.1")
                .port(port.parse().expect("the lane port is a number"))
                .username("postgres")
                .password("synthetic")
                .ssl_mode(sqlx::postgres::PgSslMode::Disable);
            let admin = sqlx::PgPool::connect_with(options.clone().database(&admin_database))
                .await
                .unwrap();
            let database = format!("time_api_test_{}", Uuid::now_v7().simple());
            sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
                .execute(&admin)
                .await
                .unwrap();
            admin.close().await;
            let setup = sqlx::PgPool::connect_with(options.database(&database))
                .await
                .unwrap();
            // The graph migration needs the AGE extension, which only a
            // superuser creates: the lane's superuser init path stands in
            // for the application's `roles.sql`.
            sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS age; LOAD 'age';")
                .execute(&setup)
                .await
                .unwrap();
            sqlx::raw_sql(store::postgres_schema::canonical_schema())
                .execute(&setup)
                .await
                .unwrap();
            setup.close().await;
            std::env::set_var("NAVIGATOR_POSTGRES_HOST", "127.0.0.1");
            std::env::set_var("NAVIGATOR_POSTGRES_PORT", &port);
            std::env::set_var("NAVIGATOR_POSTGRES_USER", "postgres");
            std::env::set_var("NAVIGATOR_POSTGRES_PASSWORD", "synthetic");
            std::env::set_var("NAVIGATOR_POSTGRES_DATABASE", &database);
            std::env::set_var("NAVIGATOR_POSTGRES_SSLMODE", "disable");
            Some(database)
        })
        .await
        .as_deref()
}

fn bearer(person_id: Uuid, role: Role) -> String {
    let mut s = SessionData::fresh("api-time-tracking-sub", role);
    s.person_id = Some(person_id);
    format!("Bearer {}", SessionStore::new(KEY).encode(&s))
}

struct Fixture {
    app: axum::Router,
    project_id: Uuid,
    other_project_id: Uuid,
    lawyer: String,
    lawyer_id: Uuid,
    attorney: String,
    attorney_id: Uuid,
    client: String,
    outsider: String,
    clerk: String,
}

async fn seed_matter(surreal: &store::surreal::SurrealDb, code: &str) -> Uuid {
    // Identity rows are resolved for real (see api_project_tasks.rs): the
    // pre-command mirror refuses a matter whose entity references rows that
    // exist nowhere, and every door would answer 503.
    let jurisdiction = store::jurisdictions::create(
        surreal,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Test Jurisdiction {code}"),
            format!("tj-{}", Uuid::now_v7().simple()),
            "state",
        ),
    )
    .await
    .unwrap();
    let entity_type = store::entity_types::create(surreal, &format!("Test Entity Type {code}"))
        .await
        .unwrap();
    let entity = store::entities::create(
        surreal,
        &store::entities::NewEntity {
            name: format!("Matter Entity {code}"),
            entity_type_id: entity_type.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap();
    let project = store::projects::create(
        surreal,
        &store::projects::NewProject {
            code: code.to_string(),
            name: "Matter".into(),
            status: "open".into(),
            entity_id: entity.id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    project.id
}

async fn build_fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let storage: Arc<dyn cloud::StorageService> = Arc::new(
        cloud::FsStorage::new(
            std::env::temp_dir().join(format!("nav-api-time-{}", Uuid::now_v7())),
        )
        .await
        .unwrap(),
    );
    let project_id = seed_matter(&surreal, &format!("matter-{}", Uuid::now_v7())).await;
    let other_project_id = seed_matter(&surreal, &format!("other-{}", Uuid::now_v7())).await;
    let run = Uuid::now_v7().simple();
    let lawyer = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Lawyer",
            format!("lawyer-{run}@example.com"),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project_id, lawyer.id, "lawyer")
        .await
        .unwrap();
    store::projects::add_participation(&surreal, other_project_id, lawyer.id, "lawyer")
        .await
        .unwrap();
    // A second attorney on the matter: the attribution target for manual
    // entries and the subject of the rate-precedent fixture.
    let attorney = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Attorney",
            format!("attorney-{run}@example.com"),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project_id, attorney.id, "lawyer")
        .await
        .unwrap();
    store::projects::add_participation(&surreal, other_project_id, attorney.id, "lawyer")
        .await
        .unwrap();
    let client = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Client",
            format!("client-{run}@example.com"),
            Role::Client,
        ),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project_id, client.id, "client")
        .await
        .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Outsider",
            format!("outsider-{run}@example.com"),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    let clerk = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role(
            "Clerk",
            format!("clerk-{run}@example.com"),
            Role::Clerk,
        ),
    )
    .await
    .unwrap();

    let state = AppState {
        sessions: SessionStore::new(KEY),
        storage,
        ..portal::test_support::app_state(surreal.clone()).await
    };
    Fixture {
        app: server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
        project_id,
        other_project_id,
        lawyer: bearer(lawyer.id, Role::Lawyer),
        lawyer_id: lawyer.id,
        attorney: bearer(attorney.id, Role::Lawyer),
        attorney_id: attorney.id,
        client: bearer(client.id, Role::Client),
        outsider: bearer(outsider.id, Role::Lawyer),
        clerk: bearer(clerk.id, Role::Clerk),
    }
}

async fn get(fx: &Fixture, path: &str, auth: Option<&str>) -> axum::http::Response<Body> {
    let mut req = Request::builder().method("GET").uri(path);
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    fx.app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn post(
    fx: &Fixture,
    path: &str,
    auth: Option<&str>,
    body: serde_json::Value,
) -> axum::http::Response<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    fx.app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn body_json(resp: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn set_rate(fx: &Fixture, person_id: Uuid, project_id: Option<Uuid>, cents: i32) {
    let resp = post(
        fx,
        "/app/api/attorney-rates",
        Some(&fx.lawyer),
        json!({
            "person_id": person_id,
            "project_id": project_id,
            "hourly_rate_cents": cents,
            "effective_on": "2026-01-01"
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED, "the rate lands");
}

/// Log one manual entry attributed to the fixture's second attorney.
async fn log_entry(
    fx: &Fixture,
    project_id: Uuid,
    minutes: i32,
    billable: bool,
) -> axum::http::Response<Body> {
    post(
        fx,
        &format!("/app/api/projects/{project_id}/time-entries"),
        Some(&fx.attorney),
        json!({
            "attorney_id": fx.attorney_id,
            "performed_on": "2026-11-01",
            "minutes": minutes,
            "description": "Reviewed the production dossier",
            "billable": billable
        }),
    )
    .await
}

#[tokio::test]
async fn a_timer_runs_stops_and_captures_its_entry() {
    let Some(_) = time_store().await else {
        return;
    };
    let fx = build_fixture().await;
    set_rate(&fx, fx.lawyer_id, None, 25_000).await;
    let timer_path = format!("/app/api/projects/{}/timer", fx.project_id);

    // Start: the durable row exists immediately.
    let resp = post(
        &fx,
        &timer_path,
        Some(&fx.lawyer),
        json!({"note": "discovery"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let timer = body_json(resp).await;
    assert_eq!(
        timer["project_id"].as_str(),
        Some(fx.project_id.to_string().as_str()),
        "the timer runs on the matter that started it"
    );

    // The recovery read: the open timer is the table's truth.
    let read = body_json(get(&fx, &timer_path, Some(&fx.lawyer)).await).await;
    assert_eq!(
        read["timer"]["id"].as_str(),
        timer["id"].as_str(),
        "the open timer is recovered from started_at"
    );

    // Stop: the captured entry is one row, timer-sourced, rate-snapshotted,
    // and never zero minutes.
    let resp = post(
        &fx,
        &format!("{timer_path}/stop"),
        Some(&fx.lawyer),
        json!({"description": "Drafted the discovery responses", "billable": true}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let captured = body_json(resp).await;
    assert_eq!(captured["source"], "timer");
    assert_eq!(
        captured["rate_cents_per_hour"], 25_000,
        "the rate is the firm default"
    );
    assert!(
        captured["minutes"].as_i64().unwrap() >= 1,
        "a stop records at least one minute"
    );
    assert_eq!(captured["billing_status"], "unbilled");

    // The timer is gone, and the entry is on the matter's list.
    let read = body_json(get(&fx, &timer_path, Some(&fx.lawyer)).await).await;
    assert_eq!(read["timer"], serde_json::Value::Null);
    let entries = body_json(
        get(
            &fx,
            &format!("/app/api/projects/{}/time-entries", fx.project_id),
            Some(&fx.lawyer),
        )
        .await,
    )
    .await["time_entries"]
        .as_array()
        .expect("the list wraps its rows")
        .clone();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["source"], "timer");
}

#[tokio::test]
async fn a_second_open_timer_is_a_conflict() {
    let Some(_) = time_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let first = format!("/app/api/projects/{}/timer", fx.project_id);
    let second = format!("/app/api/projects/{}/timer", fx.other_project_id);

    let resp = post(&fx, &first, Some(&fx.lawyer), json!({})).await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // One open timer per attorney, firm-wide: starting a second, even on
    // another matter the attorney works, is a conflict — and the first
    // timer keeps running.
    let resp = post(&fx, &second, Some(&fx.lawyer), json!({})).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let read = body_json(get(&fx, &first, Some(&fx.lawyer)).await).await;
    assert_ne!(
        read["timer"],
        serde_json::Value::Null,
        "the first timer stands"
    );
}

#[tokio::test]
async fn discard_records_nothing() {
    let Some(_) = time_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let timer_path = format!("/app/api/projects/{}/timer", fx.project_id);

    let resp = post(&fx, &timer_path, Some(&fx.lawyer), json!({})).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let timer = body_json(resp).await;

    let resp = post(
        &fx,
        &format!("{timer_path}/discard"),
        Some(&fx.lawyer),
        json!({}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let read = body_json(get(&fx, &timer_path, Some(&fx.lawyer)).await).await;
    assert_eq!(read["timer"], serde_json::Value::Null);
    let entries = body_json(
        get(
            &fx,
            &format!("/app/api/projects/{}/time-entries", fx.project_id),
            Some(&fx.lawyer),
        )
        .await,
    )
    .await["time_entries"]
        .as_array()
        .unwrap()
        .clone();
    assert!(entries.is_empty(), "a discarded timer never bills");
    assert_ne!(
        timer["id"],
        serde_json::Value::Null,
        "the timer had existed"
    );
}

#[tokio::test]
async fn rate_precedence_resolves_matter_over_firm() {
    let Some(_) = time_store().await else {
        return;
    };
    let fx = build_fixture().await;
    set_rate(&fx, fx.attorney_id, None, 25_000).await;
    set_rate(&fx, fx.attorney_id, Some(fx.project_id), 30_000).await;

    // On the overridden matter: the override.
    let resp = log_entry(&fx, fx.project_id, 90, true).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let entry = body_json(resp).await;
    assert_eq!(
        entry["rate_cents_per_hour"], 30_000,
        "the matter override prices the matter's work"
    );

    // On the other matter, same attorney: the firm default.
    let resp = log_entry(&fx, fx.other_project_id, 60, true).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let entry = body_json(resp).await;
    assert_eq!(entry["rate_cents_per_hour"], 25_000);

    // Non-billable work never carries a price.
    let resp = log_entry(&fx, fx.project_id, 30, false).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let entry = body_json(resp).await;
    assert_eq!(entry["rate_cents_per_hour"], 0);

    // And the rate history reads back as it was written.
    let rates = body_json(
        get(
            &fx,
            &format!("/app/api/attorney-rates?person_id={}", fx.attorney_id),
            Some(&fx.lawyer),
        )
        .await,
    )
    .await["rates"]
        .as_array()
        .expect("the history wraps its rows")
        .clone();
    assert_eq!(rates.len(), 2, "firm default and matter override");
}

#[tokio::test]
async fn billable_work_without_a_rate_is_refused() {
    let Some(_) = time_store().await else {
        return;
    };
    let fx = build_fixture().await;

    // No rate has been recorded for the attorney: a billable entry would
    // silently price at zero, so it is refused instead.
    let resp = log_entry(&fx, fx.project_id, 45, true).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Non-billable work needs no rate.
    let resp = log_entry(&fx, fx.project_id, 45, false).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn billing_status_transitions_are_audited_and_terminal() {
    let Some(_) = time_store().await else {
        return;
    };
    let fx = build_fixture().await;
    set_rate(&fx, fx.attorney_id, None, 25_000).await;

    let invoiced = log_entry(&fx, fx.project_id, 60, true).await;
    assert_eq!(invoiced.status(), StatusCode::CREATED);
    let entry_id: Uuid = body_json(invoiced).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let invoice_path = format!(
        "/app/api/projects/{}/time-entries/{entry_id}/invoice",
        fx.project_id
    );

    // The entry flips to invoiced under the Xero invoice that carries it.
    let resp = post(
        &fx,
        &invoice_path,
        Some(&fx.lawyer),
        json!({"xero_invoice_id": "INV-17"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["billing_status"], "invoiced");

    // The replay of the same invoice is a no-op; a different invoice on the
    // same entry is a conflict — it would misstate which invoice billed it.
    let resp = post(
        &fx,
        &invoice_path,
        Some(&fx.lawyer),
        json!({"xero_invoice_id": "INV-17"}),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the same invoice replays as a no-op"
    );
    let resp = post(
        &fx,
        &invoice_path,
        Some(&fx.lawyer),
        json!({"xero_invoice_id": "INV-18"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // A resolved entry never reopens: the write-off is refused.
    let resp = post(
        &fx,
        &format!(
            "/app/api/projects/{}/time-entries/{entry_id}/write-off",
            fx.project_id
        ),
        Some(&fx.lawyer),
        json!({"reason": "Clerical error"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // A fresh entry writes off cleanly, keeping its reason.
    let written = log_entry(&fx, fx.project_id, 15, true).await;
    let written_id: Uuid = body_json(written).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let resp = post(
        &fx,
        &format!(
            "/app/api/projects/{}/time-entries/{written_id}/write-off",
            fx.project_id
        ),
        Some(&fx.lawyer),
        json!({"reason": "Absorbed into the flat fee"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_json(resp).await["billing_status"], "written_off");
}

#[tokio::test]
async fn another_matters_time_is_hidden() {
    let Some(_) = time_store().await else {
        return;
    };
    let fx = build_fixture().await;
    set_rate(&fx, fx.attorney_id, None, 25_000).await;

    let resp = log_entry(&fx, fx.project_id, 60, true).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let entry_id: Uuid = body_json(resp).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // The lawyer is on both matters, but the entry belongs to the first:
    // naming it under the second matter's URL is the non-disclosing 404.
    let foreign_invoice = format!(
        "/app/api/projects/{}/time-entries/{entry_id}/invoice",
        fx.other_project_id
    );
    let resp = post(
        &fx,
        &foreign_invoice,
        Some(&fx.lawyer),
        json!({"xero_invoice_id": "INV-17"}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // A Timer started on the first matter answers neither the second
    // matter's read nor its stop.
    let timer_path = format!("/app/api/projects/{}/timer", fx.project_id);
    let resp = post(&fx, &timer_path, Some(&fx.lawyer), json!({})).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let foreign_timer = format!("/app/api/projects/{}/timer", fx.other_project_id);
    let read = body_json(get(&fx, &foreign_timer, Some(&fx.lawyer)).await).await;
    assert_eq!(
        read["timer"],
        serde_json::Value::Null,
        "a timer on another matter is not disclosed here"
    );
    let resp = post(
        &fx,
        &format!("{foreign_timer}/stop"),
        Some(&fx.lawyer),
        json!({"description": "Misattributed", "billable": true}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // And the timer still runs where it was started.
    let read = body_json(get(&fx, &timer_path, Some(&fx.lawyer)).await).await;
    assert_ne!(read["timer"], serde_json::Value::Null);
}

#[tokio::test]
async fn non_participants_are_hidden() {
    let Some(_) = time_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let entries_path = format!("/app/api/projects/{}/time-entries", fx.project_id);
    let timer_path = format!("/app/api/projects/{}/timer", fx.project_id);

    // A lawyer without a participation row: 404, the matter does not exist
    // for them — never a 403 naming it.
    assert_eq!(
        post(
            &fx,
            &entries_path,
            Some(&fx.outsider),
            json!({
                "attorney_id": fx.attorney_id,
                "performed_on": "2026-11-01",
                "minutes": 30,
                "description": "Sneak",
                "billable": true
            })
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        get(&fx, &entries_path, Some(&fx.outsider)).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        post(&fx, &timer_path, Some(&fx.outsider), json!({}))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    // A client-side participant is not lawyer tier: route admission refuses
    // before the matter question is ever asked.
    assert_eq!(
        post(
            &fx,
            &entries_path,
            Some(&fx.client),
            json!({
                "attorney_id": fx.attorney_id,
                "performed_on": "2026-11-01",
                "minutes": 30,
                "description": "Sneak",
                "billable": true
            })
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(&fx, &entries_path, Some(&fx.client)).await.status(),
        StatusCode::FORBIDDEN
    );

    // A clerk is the same refusal.
    assert_eq!(
        get(&fx, &timer_path, Some(&fx.clerk)).await.status(),
        StatusCode::FORBIDDEN
    );

    // No session at all: unauthorized.
    assert_eq!(
        get(&fx, &entries_path, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_flat_fee_door_round_trips() {
    let Some(_) = time_store().await else {
        return;
    };
    let fx = build_fixture().await;
    let fee_path = format!("/app/api/projects/{}/flat-fee", fx.project_id);

    let read = body_json(get(&fx, &fee_path, Some(&fx.lawyer)).await).await;
    assert_eq!(
        read["fee_cents"],
        serde_json::Value::Null,
        "no fee is set yet"
    );

    let resp = post(
        &fx,
        &fee_path,
        Some(&fx.lawyer),
        json!({"fee_cents": 150_000}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let read = body_json(get(&fx, &fee_path, Some(&fx.lawyer)).await).await;
    assert_eq!(read["fee_cents"], 150_000);

    // Replacing the figure is the whole edit surface; captured invoices are
    // unaffected.
    let resp = post(
        &fx,
        &fee_path,
        Some(&fx.lawyer),
        json!({"fee_cents": 175_000}),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let read = body_json(get(&fx, &fee_path, Some(&fx.lawyer)).await).await;
    assert_eq!(read["fee_cents"], 175_000);
}

#[tokio::test]
async fn the_entry_list_is_newest_work_first() {
    let Some(_) = time_store().await else {
        return;
    };
    let fx = build_fixture().await;
    set_rate(&fx, fx.attorney_id, None, 25_000).await;

    let entries_path = format!("/app/api/projects/{}/time-entries", fx.project_id);
    for (date, minutes) in [("2026-10-30", 30), ("2026-11-01", 45), ("2026-10-31", 60)] {
        let resp = post(
            &fx,
            &entries_path,
            Some(&fx.lawyer),
            json!({
                "attorney_id": fx.attorney_id,
                "performed_on": date,
                "minutes": minutes,
                "description": format!("Work on {date}"),
                "billable": true
            }),
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::CREATED,
            "the entry for {date} lands"
        );
    }

    // Newest work first, whatever order the rows were written in.
    let entries = body_json(get(&fx, &entries_path, Some(&fx.lawyer)).await).await["time_entries"]
        .as_array()
        .expect("the list wraps its rows")
        .clone();
    let dates: Vec<&str> = entries
        .iter()
        .map(|entry| {
            entry["performed_on"]
                .as_str()
                .expect("each row dates its work")
        })
        .collect();
    assert_eq!(dates, vec!["2026-11-01", "2026-10-31", "2026-10-30"]);
}
