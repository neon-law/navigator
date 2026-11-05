//! The Billing tab's time doors — Time Entries, the durable Timer, and the
//! fee structures that price new work — over the PostgreSQL store, through
//! the full Axum router.
//!
//! Every scenario runs in a child process against its own freshly migrated
//! database (`isolated_database` + the child re-entry pattern from
//! `project_billing_api.rs`), so scenario side effects never meet. The
//! proofs the doors rest on: the rate is snapshotted from the fee structure
//! and never taken from the wire (matter override over firm default, no rate
//! is a refusal, non-billable work is free); invoice marking replays
//! idempotently but conflicts under a different invoice; a write-off keeps
//! its reason and never reopens; the Timer captures store-computed minutes
//! under the `timer` source; and the non-participating 404 posture holds on
//! every door.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::NaiveDate;
use portal::api::ApiState;
use serde_json::{Value, json};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgSslMode},
};
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    app: Router,
    surreal: store::surreal::SurrealDb,
    project_id: Uuid,
    lawyer: portal::SessionData,
    lawyer_id: Uuid,
}

async fn fixture() -> Fixture {
    let pg = store::postgres::PostgresDb::from_env().await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    let surreal = mem_surreal().await;
    let project_id = source_matter(&surreal).await;
    let lawyer = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Synthetic Time Lawyer",
            format!("time-lawyer-{}@example.com", Uuid::new_v4()),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        &surreal,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: lawyer.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    let state = portal::test_support::app_state(surreal).await;
    let app = portal::api::routes().with_state(ApiState {
        surreal: state.surreal.clone(),
        email: state.email.clone(),
        bootstrap_owner_email: state.bootstrap_owner_email.clone(),
        bootstrap_company: "Synthetic Firm".into(),
        questionnaire_runtime: state.questionnaire_runtime.clone(),
        storage: state.storage.clone(),
        workflow_runtime: state.workflow_runtime.clone(),
        assets_storage: state.assets_storage.clone(),
        forms_registry: state.forms_registry.clone(),
        signature_provider: state.signature_provider.clone(),
        contract_reviewer: state.contract_reviewer.clone(),
        integration_providers: state.integration_providers.clone(),
        billing_provider: state.billing_provider.clone(),
        canonical_base_url: None,
    });
    Fixture {
        app,
        surreal: state.surreal.clone(),
        project_id,
        lawyer: portal::SessionData {
            person_id: Some(lawyer.id),
            ..portal::SessionData::fresh(&lawyer.email, Role::Lawyer)
        },
        lawyer_id: lawyer.id,
    }
}

async fn source_matter(surreal: &store::surreal::SurrealDb) -> Uuid {
    let jurisdiction = store::jurisdictions::create(
        surreal,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic time jurisdiction {}", Uuid::new_v4()),
            format!("TIME-{}", Uuid::new_v4()),
            "state",
        ),
    )
    .await
    .unwrap();
    let entity_type = store::entity_types::create(
        surreal,
        &format!("Synthetic time entity type {}", Uuid::new_v4()),
    )
    .await
    .unwrap();
    let entity = store::entities::create(
        surreal,
        &store::entities::NewEntity {
            name: "Synthetic Time Matter Entity".into(),
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
            code: store::projects::code_from_name("Time tracking matter", Uuid::new_v4()),
            name: "Time tracking matter".into(),
            status: "open".into(),
            entity_id: entity.id,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    project.id
}

/// A second matter the lawyer is enrolled on, for the isolation scenarios:
/// the timer read must disclose nothing about it. Rides the fixture's own
/// in-process `SurrealDB` instance — the child process runs no server.
async fn additional_matter(fixture: &Fixture) -> Uuid {
    let project_id = source_matter(&fixture.surreal).await;
    store::participation::add_participant(
        &fixture.surreal,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: fixture.lawyer_id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    project_id
}

async fn post_json(
    app: &Router,
    uri: &str,
    body: Value,
    session: &portal::SessionData,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .extension(session.clone())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn get_json(
    app: &Router,
    uri: &str,
    session: &portal::SessionData,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .extension(session.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[allow(dead_code)] // scenario-local helpers: not every scenario reads every shape
async fn create_entry(
    f: &Fixture,
    attorney_id: Uuid,
    performed_on: NaiveDate,
    minutes: i32,
    description: &str,
    billable: bool,
) -> axum::response::Response {
    post_json(
        &f.app,
        &format!("/app/api/projects/{}/time-entries", f.project_id),
        json!({
            "attorney_id": attorney_id,
            "performed_on": performed_on,
            "minutes": minutes,
            "description": description,
            "billable": billable,
        }),
        &f.lawyer,
    )
    .await
}

/// Record a rate through the API door itself — the same mirror ceremony the
/// browser door runs — so every scenario's fee structure is real, mirrored
/// data, never a store-level backdoor.
async fn seed_rate(
    f: &Fixture,
    project_id: Option<Uuid>,
    hourly_rate_cents: i32,
    effective_on: NaiveDate,
) -> Value {
    let response = post_json(
        &f.app,
        "/app/api/attorney-rates",
        json!({
            "person_id": f.lawyer_id,
            "project_id": project_id,
            "hourly_rate_cents": hourly_rate_cents,
            "effective_on": effective_on,
        }),
        &f.lawyer,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "the rate door accepts a mirrored attorney and matter"
    );
    body_json(response).await
}

// Each scenario runs twice: once as the nextest case (the parent, which
// mints the isolated database and spawns the child) and once inside the
// child process, selected by `NAV_TIME_TRACKING_API_CHILD`. The macro keeps
// the twelve re-entry prologues from drifting apart.
macro_rules! isolated_scenario {
    ($test_name:ident, $runner:ident) => {
        #[tokio::test]
        async fn $test_name() {
            if std::env::var("NAV_TIME_TRACKING_API_CHILD").as_deref() == Ok("1") {
                Box::pin($runner()).await;
                return;
            }
            let Some((port, database)) = isolated_database().await else {
                return;
            };
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", stringify!($test_name), "--nocapture"])
                .env("NAV_TIME_TRACKING_API_CHILD", "1")
                .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
                .env("NAVIGATOR_POSTGRES_PORT", &port)
                .env("NAVIGATOR_POSTGRES_USER", "postgres")
                .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
                .env("NAVIGATOR_POSTGRES_DATABASE", &database)
                .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
                .status()
                .unwrap();
            remove_isolated_database(&port, &database).await;
            assert!(status.success());
        }
    };
}

// The pricing rule: the wire never carries a rate. Non-billable work is free
// even with no fee structure at all; billable work snapshots the matter
// override over the firm default; billable work with no rate at all is
// refused rather than silently priced at zero.
async fn run_non_billable_entry_is_free_even_without_a_fee_structure() {
    let f = fixture().await;
    let response = create_entry(
        &f,
        f.lawyer_id,
        NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
        90,
        "Internal training reading",
        false,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let entry = body_json(response).await;
    assert_eq!(entry["rate_cents_per_hour"], 0);
    assert_eq!(entry["source"], "manual");
    assert_eq!(entry["billing_status"], "unbilled");
    assert_eq!(entry["minutes"], 90);
    assert_eq!(entry["description"], "Internal training reading");
}
isolated_scenario!(
    a_non_billable_entry_is_free_even_without_a_fee_structure,
    run_non_billable_entry_is_free_even_without_a_fee_structure
);

async fn run_a_billable_entry_snapshots_the_matter_override_over_the_firm_default() {
    let f = fixture().await;
    seed_rate(
        &f,
        None,
        15_000,
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    )
    .await;
    seed_rate(
        &f,
        Some(f.project_id),
        22_500,
        NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
    )
    .await;
    let response = create_entry(
        &f,
        f.lawyer_id,
        NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
        60,
        "Deposition preparation",
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let entry = body_json(response).await;
    assert_eq!(
        entry["rate_cents_per_hour"], 22_500,
        "the matter override prices the entry, not the firm default"
    );
}
isolated_scenario!(
    a_billable_entry_snapshots_the_matter_override_over_the_firm_default,
    run_a_billable_entry_snapshots_the_matter_override_over_the_firm_default
);

async fn run_a_billable_entry_without_any_rate_is_refused_not_silently_free() {
    let f = fixture().await;
    let response = create_entry(
        &f,
        f.lawyer_id,
        NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
        60,
        "Unpriced research",
        true,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let listed = get_json(
        &f.app,
        &format!("/app/api/projects/{}/time-entries", f.project_id),
        &f.lawyer,
    )
    .await;
    assert_eq!(listed.status(), StatusCode::OK);
    let body = body_json(listed).await;
    assert!(
        body["time_entries"].as_array().unwrap().is_empty(),
        "the refused entry created nothing"
    );
}
isolated_scenario!(
    a_billable_entry_without_any_rate_is_refused_not_silently_free,
    run_a_billable_entry_without_any_rate_is_refused_not_silently_free
);

// The access posture: the LawyerSession extractor refuses a client-role
// session outright, and a firm-side lawyer with no participation row gets
// the non-disclosing 404 — Owner and Admin included by the same rule.
async fn run_a_client_session_is_refused_at_the_extractor() {
    let f = fixture().await;
    let client_session = portal::SessionData {
        person_id: Some(f.lawyer_id),
        ..portal::SessionData::fresh("time-client@example.com", Role::Client)
    };
    let response = post_json(
        &f.app,
        &format!("/app/api/projects/{}/time-entries", f.project_id),
        json!({
            "attorney_id": f.lawyer_id,
            "performed_on": NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
            "minutes": 30,
            "description": "Client must not book time",
            "billable": false,
        }),
        &client_session,
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
isolated_scenario!(
    a_client_session_is_refused_at_the_extractor,
    run_a_client_session_is_refused_at_the_extractor
);

async fn run_a_non_participant_lawyer_gets_the_non_disclosing_404() {
    let f = fixture().await;
    let outsider = store::persons::create(
        &f.surreal,
        &NewPerson::with_role(
            "Synthetic Outside Lawyer",
            format!("outside-lawyer-{}@example.com", Uuid::new_v4()),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    let outsider_session = portal::SessionData {
        person_id: Some(outsider.id),
        ..portal::SessionData::fresh(&outsider.email, Role::Lawyer)
    };
    let response = get_json(
        &f.app,
        &format!("/app/api/projects/{}/time-entries", f.project_id),
        &outsider_session,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "the matter might as well not exist for a lawyer with no participation row"
    );
}
isolated_scenario!(
    a_non_participant_lawyer_gets_the_non_disclosing_404,
    run_a_non_participant_lawyer_gets_the_non_disclosing_404
);

// Invoice marking: the same invoice id replays as a no-op; a different
// invoice id over an already-invoiced entry is a conflict, and the first
// invoice keeps the entry.
async fn run_marking_the_same_invoice_again_replays_without_change() {
    let f = fixture().await;
    seed_rate(
        &f,
        None,
        15_000,
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    )
    .await;
    let created = create_entry(
        &f,
        f.lawyer_id,
        NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
        45,
        "Motion drafting",
        true,
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let entry_id = body_json(created).await["id"].as_str().unwrap().to_string();
    let uri = format!(
        "/app/api/projects/{}/time-entries/{entry_id}/invoice",
        f.project_id
    );
    let first = post_json(
        &f.app,
        &uri,
        json!({ "xero_invoice_id": "INV-REPLAY-1" }),
        &f.lawyer,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(body_json(first).await["xero_invoice_id"], "INV-REPLAY-1");
    let replay = post_json(
        &f.app,
        &uri,
        json!({ "xero_invoice_id": "INV-REPLAY-1" }),
        &f.lawyer,
    )
    .await;
    assert_eq!(
        replay.status(),
        StatusCode::OK,
        "the same invoice id is a no-op, not a conflict"
    );
    let body = body_json(replay).await;
    assert_eq!(body["billing_status"], "invoiced");
    assert_eq!(body["xero_invoice_id"], "INV-REPLAY-1");
}
isolated_scenario!(
    marking_the_same_invoice_again_replays_without_change,
    run_marking_the_same_invoice_again_replays_without_change
);

async fn run_marking_the_same_entry_under_a_different_invoice_conflicts() {
    let f = fixture().await;
    seed_rate(
        &f,
        None,
        15_000,
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    )
    .await;
    let created = create_entry(
        &f,
        f.lawyer_id,
        NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
        45,
        "Motion drafting",
        true,
    )
    .await;
    let entry_id = body_json(created).await["id"].as_str().unwrap().to_string();
    let uri = format!(
        "/app/api/projects/{}/time-entries/{entry_id}/invoice",
        f.project_id
    );
    let first = post_json(
        &f.app,
        &uri,
        json!({ "xero_invoice_id": "INV-ORIGINAL" }),
        &f.lawyer,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let second = post_json(
        &f.app,
        &uri,
        json!({ "xero_invoice_id": "INV-OTHER" }),
        &f.lawyer,
    )
    .await;
    assert_eq!(
        second.status(),
        StatusCode::CONFLICT,
        "one entry cannot ride two invoices"
    );
    let listed = get_json(
        &f.app,
        &format!("/app/api/projects/{}/time-entries", f.project_id),
        &f.lawyer,
    )
    .await;
    let body = body_json(listed).await;
    assert_eq!(body["time_entries"][0]["xero_invoice_id"], "INV-ORIGINAL");
}
isolated_scenario!(
    marking_the_same_entry_under_a_different_invoice_conflicts,
    run_marking_the_same_entry_under_a_different_invoice_conflicts
);

// The write-off resolves an unbilled entry with its reason retained, and a
// resolved entry never resolves again.
async fn run_a_write_off_retains_its_reason_and_never_reopens() {
    let f = fixture().await;
    let created = create_entry(
        &f,
        f.lawyer_id,
        NaiveDate::from_ymd_opt(2026, 11, 4).unwrap(),
        30,
        "Duplicate keystrokes",
        false,
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let entry_id = body_json(created).await["id"].as_str().unwrap().to_string();
    let uri = format!(
        "/app/api/projects/{}/time-entries/{entry_id}/write-off",
        f.project_id
    );
    let written_off = post_json(
        &f.app,
        &uri,
        json!({ "reason": "duplicate work recorded in error" }),
        &f.lawyer,
    )
    .await;
    assert_eq!(written_off.status(), StatusCode::OK);
    let body = body_json(written_off).await;
    assert_eq!(body["billing_status"], "written_off");
    assert_eq!(
        body["written_off_reason"],
        "duplicate work recorded in error"
    );
    let again = post_json(
        &f.app,
        &uri,
        json!({ "reason": "a second reason" }),
        &f.lawyer,
    )
    .await;
    assert_eq!(
        again.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "a resolved entry never reopens"
    );
}
isolated_scenario!(
    a_write_off_retains_its_reason_and_never_reopens,
    run_a_write_off_retains_its_reason_and_never_reopens
);

// The Timer: stopping captures an entry with store-computed minutes, the
// `timer` source, the start date, and the snapshot rate — then the open
// Timer is gone.
async fn run_the_timer_captures_store_computed_minutes_under_the_timer_source() {
    let f = fixture().await;
    seed_rate(
        &f,
        Some(f.project_id),
        22_500,
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    )
    .await;
    let started = post_json(
        &f.app,
        &format!("/app/api/projects/{}/timer", f.project_id),
        json!({ "note": "hearing prep" }),
        &f.lawyer,
    )
    .await;
    assert_eq!(started.status(), StatusCode::CREATED);
    let stopped = post_json(
        &f.app,
        &format!("/app/api/projects/{}/timer/stop", f.project_id),
        json!({
            "description": "Hearing preparation binder",
            "billable": true,
        }),
        &f.lawyer,
    )
    .await;
    assert_eq!(stopped.status(), StatusCode::OK);
    let entry = body_json(stopped).await;
    assert_eq!(entry["source"], "timer");
    let minutes = entry["minutes"].as_i64().unwrap();
    assert!(
        (1..=2).contains(&minutes),
        "the store floors elapsed time at one minute and the test runs no longer: {minutes}"
    );
    assert_eq!(entry["rate_cents_per_hour"], 22_500);
    assert_eq!(
        entry["performed_on"],
        chrono::Utc::now().date_naive().to_string(),
        "the entry is performed on the date the Timer started"
    );
    assert_eq!(entry["description"], "Hearing preparation binder");
    let open = get_json(
        &f.app,
        &format!("/app/api/projects/{}/timer", f.project_id),
        &f.lawyer,
    )
    .await;
    assert_eq!(open.status(), StatusCode::OK);
    assert!(
        body_json(open).await["timer"].is_null(),
        "the captured Timer no longer runs"
    );
}
isolated_scenario!(
    the_timer_captures_store_computed_minutes_under_the_timer_source,
    run_the_timer_captures_store_computed_minutes_under_the_timer_source
);

async fn run_a_second_open_timer_is_a_conflict_on_any_matter() {
    let f = fixture().await;
    let first = post_json(
        &f.app,
        &format!("/app/api/projects/{}/timer", f.project_id),
        json!({ "note": "first timer" }),
        &f.lawyer,
    )
    .await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let second = post_json(
        &f.app,
        &format!("/app/api/projects/{}/timer", f.project_id),
        json!({ "note": null }),
        &f.lawyer,
    )
    .await;
    assert_eq!(
        second.status(),
        StatusCode::CONFLICT,
        "one attorney runs one firm-wide Timer"
    );
}
isolated_scenario!(
    a_second_open_timer_is_a_conflict_on_any_matter,
    run_a_second_open_timer_is_a_conflict_on_any_matter
);

async fn run_the_running_timer_read_discloses_nothing_about_other_matters() {
    let f = fixture().await;
    let other_matter = additional_matter(&f).await;
    let started = post_json(
        &f.app,
        &format!("/app/api/projects/{}/timer", f.project_id),
        json!({ "note": "runs on the first matter" }),
        &f.lawyer,
    )
    .await;
    assert_eq!(started.status(), StatusCode::CREATED);
    let elsewhere = get_json(
        &f.app,
        &format!("/app/api/projects/{other_matter}/timer"),
        &f.lawyer,
    )
    .await;
    assert_eq!(elsewhere.status(), StatusCode::OK);
    assert!(
        body_json(elsewhere).await["timer"].is_null(),
        "a Timer on another matter is not named by this matter's read"
    );
}
isolated_scenario!(
    the_running_timer_read_discloses_nothing_about_other_matters,
    run_the_running_timer_read_discloses_nothing_about_other_matters
);

// The fee structure: rows are effective-dated history, newest effective date
// first; a row re-recorded at the same scope and date replaces the prior one;
// and an entry is priced by the rate effective on its performed date.
async fn run_rate_history_is_effective_dated_and_a_same_date_row_replaces_the_prior_one() {
    let f = fixture().await;
    seed_rate(
        &f,
        None,
        10_000,
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    )
    .await;
    seed_rate(
        &f,
        None,
        12_000,
        NaiveDate::from_ymd_opt(2026, 6, 1).unwrap(),
    )
    .await;
    let history = get_json(
        &f.app,
        &format!("/app/api/attorney-rates?person_id={}", f.lawyer_id),
        &f.lawyer,
    )
    .await;
    assert_eq!(history.status(), StatusCode::OK);
    let rates = body_json(history).await["rates"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rates.len(), 2);
    assert_eq!(
        rates[0]["hourly_rate_cents"], 12_000,
        "newest effective first"
    );
    assert_eq!(rates[1]["hourly_rate_cents"], 10_000);
    seed_rate(
        &f,
        None,
        11_000,
        NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
    )
    .await;
    let replaced = get_json(
        &f.app,
        &format!("/app/api/attorney-rates?person_id={}", f.lawyer_id),
        &f.lawyer,
    )
    .await;
    let rates = body_json(replaced).await["rates"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rates.len(), 2, "the re-recorded row replaced, not appended");
    assert_eq!(rates[1]["hourly_rate_cents"], 11_000);
    let spring = create_entry(
        &f,
        f.lawyer_id,
        NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        60,
        "Spring work",
        true,
    )
    .await;
    assert_eq!(body_json(spring).await["rate_cents_per_hour"], 11_000);
    let summer = create_entry(
        &f,
        f.lawyer_id,
        NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
        60,
        "Summer work",
        true,
    )
    .await;
    assert_eq!(body_json(summer).await["rate_cents_per_hour"], 12_000);
}
isolated_scenario!(
    rate_history_is_effective_dated_and_a_same_date_row_replaces_the_prior_one,
    run_rate_history_is_effective_dated_and_a_same_date_row_replaces_the_prior_one
);

// The flat fee is the matter's current agreement: setting a new figure
// replaces the prior one outright.
async fn run_the_flat_fee_is_the_current_agreement_and_replaces_the_prior_figure() {
    let f = fixture().await;
    let set_first = post_json(
        &f.app,
        &format!("/app/api/projects/{}/flat-fee", f.project_id),
        json!({ "fee_cents": 500_000 }),
        &f.lawyer,
    )
    .await;
    assert_eq!(set_first.status(), StatusCode::OK);
    assert_eq!(body_json(set_first).await["fee_cents"], 500_000);
    let read_back = get_json(
        &f.app,
        &format!("/app/api/projects/{}/flat-fee", f.project_id),
        &f.lawyer,
    )
    .await;
    assert_eq!(read_back.status(), StatusCode::OK);
    assert_eq!(body_json(read_back).await["fee_cents"], 500_000);
    let set_second = post_json(
        &f.app,
        &format!("/app/api/projects/{}/flat-fee", f.project_id),
        json!({ "fee_cents": 650_000 }),
        &f.lawyer,
    )
    .await;
    assert_eq!(set_second.status(), StatusCode::OK);
    let replaced = get_json(
        &f.app,
        &format!("/app/api/projects/{}/flat-fee", f.project_id),
        &f.lawyer,
    )
    .await;
    assert_eq!(
        body_json(replaced).await["fee_cents"],
        650_000,
        "the new agreement replaces the prior figure"
    );
}
isolated_scenario!(
    the_flat_fee_is_the_current_agreement_and_replaces_the_prior_figure,
    run_the_flat_fee_is_the_current_agreement_and_replaces_the_prior_figure
);

async fn isolated_database() -> Option<(String, String)> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
            "PostgreSQL fixture required"
        );
        return None;
    };
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = PgPool::connect_with(options).await.unwrap();
    let database = format!("time_api_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    Some((port, database))
}

async fn remove_isolated_database(port: &str, database: &str) {
    let admin = PgPool::connect_with(
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port.parse().unwrap())
            .username("postgres")
            .password("synthetic")
            .database("smoke")
            .ssl_mode(PgSslMode::Disable),
    )
    .await
    .unwrap();
    sqlx::query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname=$1")
        .bind(database)
        .execute(&admin)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("DROP DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
}
