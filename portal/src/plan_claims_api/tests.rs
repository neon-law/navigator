use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use store::{
    application_store::ApplicationStore,
    firms::{self, FirmMembership, NewFirm, NewPersonFirmRole},
    legal_plans::{self, NewLegalPlan},
    participation::{self, AddParticipantCommand, DriActor, DriRequest},
    persons::{self, NewPerson, Role},
    postgres::PostgresDb,
    projects::{self, NewProject, PlanColumns},
    test_support::with_postgres,
    time_entries::{self, NewTimeEntry, TimeEntrySource},
};
use tower::ServiceExt;
use uuid::Uuid;

use crate::{
    session::{SessionSource, SessionStore},
    test_support::{app_state, TEST_SESSION_KEY},
    SessionData,
};

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    person_id: Uuid,
    role: Role,
    body: Option<&Value>,
) -> (StatusCode, Value) {
    let token = SessionStore::new(TEST_SESSION_KEY).encode(&SessionData {
        person_id: Some(person_id),
        source: SessionSource::Cli,
        ..SessionData::fresh("synthetic-claims", role)
    });
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn person(db: &ApplicationStore, email: &str, role: Role) -> Uuid {
    persons::create(
        db,
        &NewPerson::with_role("Synthetic claims actor", email, role),
    )
    .await
    .unwrap()
    .id
}

async fn create_firm(db: &ApplicationStore, entity: Uuid, admin: Uuid) -> Uuid {
    firms::create(
        db,
        &NewFirm {
            name: format!("Synthetic Claim Firm {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id: entity,
            admin_dri_person_id: admin,
        },
    )
    .await
    .unwrap()
    .id
}

async fn billing_page(app: &Router, code: &str, person: Uuid, role: Role) -> (StatusCode, String) {
    let token = SessionStore::new(TEST_SESSION_KEY).encode(&SessionData {
        person_id: Some(person),
        ..SessionData::fresh("synthetic-billing-reader", role)
    });
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/app/projects/{code}/billing"))
                .header(
                    header::COOKIE,
                    format!("{}={token}", crate::session::SESSION_COOKIE_NAME),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let html = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    (status, html)
}

fn claim_row(html: &str, claim_id: Uuid) -> &str {
    html.split_once(&format!(r#"id="claim-{claim_id}""#))
        .expect("the claim has a row")
        .1
        .split_once("</tr>")
        .expect("the row is complete")
        .0
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One authenticated walk proves the life of a claim.
async fn a_closed_project_reconciles_claims_through_the_authenticated_routes() {
    // The renderer spawns its own tasks. Give the child the owned fixture's
    // connection settings so page reads and API writes use the same database.
    if std::env::var_os("NAV_TEST_CLAIM_PAGE_CHILD").is_none() {
        with_postgres(async {
            let pg = PostgresDb::application().await.unwrap();
            let options = pg.pool().connect_options();
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "a_closed_project_reconciles_claims_through_the_authenticated_routes",
                    "--nocapture",
                ])
                .env("NAV_TEST_CLAIM_PAGE_CHILD", "1")
                .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
                .env("NAVIGATOR_POSTGRES_PORT", options.get_port().to_string())
                .env(
                    "NAVIGATOR_POSTGRES_DATABASE",
                    options.get_database().unwrap(),
                )
                .env("NAVIGATOR_POSTGRES_USER", "postgres")
                .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
                .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
                .status()
                .unwrap();
            assert!(status.success());
        })
        .await;
        return;
    }
    Box::pin(async {
        let db = ApplicationStore::uninitialized();
        let pg = PostgresDb::application().await.unwrap();
        let pool = pg.pool();
        sqlx::query("INSERT INTO brand(name,brand_key) VALUES ('Synthetic Brand','neon')")
            .execute(pool).await.unwrap();
        let entity: Uuid = sqlx::query_scalar(
            "WITH kind AS (INSERT INTO entity_type(name) VALUES ('Synthetic Claim Entity Type') RETURNING id),
              place AS (INSERT INTO jurisdiction(name) VALUES ('Synthetic Claim Jurisdiction') RETURNING id)
              INSERT INTO entity(name,entity_type_id,jurisdiction_id)
              SELECT 'Synthetic Claim Entity',kind.id,place.id FROM kind, place RETURNING id",
        ).fetch_one(pool).await.unwrap();
        let foreign_entity: Uuid = sqlx::query_scalar(
            "INSERT INTO entity(name,entity_type_id,jurisdiction_id)
             SELECT 'Second Synthetic Claim Entity',entity_type_id,jurisdiction_id FROM entity WHERE id=$1 RETURNING id",
        ).bind(entity).fetch_one(pool).await.unwrap();
        let admin = person(&db, "claim-admin@example.com", Role::Admin).await;
        let outsider = person(&db, "claim-outsider@example.com", Role::Admin).await;
        let lawyer = person(&db, "claim-lawyer@example.com", Role::Lawyer).await;
        let firm = create_firm(&db, entity, admin).await;
        let foreign = create_firm(&db, foreign_entity, outsider).await;
        firms::ensure_membership(&db, &NewPersonFirmRole {
            person_id: lawyer, firm_id: firm, membership: FirmMembership::Lawyer, is_dri: false,
        }).await.unwrap();
        let plan = legal_plans::create(&pg, firm, &NewLegalPlan {
            name: "ARAG Group Legal".into(), contact: None, portal: None,
            acceptance_sla_hours: Some(24), turnaround_sla_days: Some(30), reporting_requirements: None,
        }).await.unwrap();
        let project = projects::create(&db, &NewProject {
            code: "synthetic-closed-claim".into(), name: "Synthetic closed claim".into(),
            status: "closed".into(), firm_id: Some(firm), entity_id: entity,
            ..Default::default()
        }).await.unwrap();
        participation::add_participant(&db, &AddParticipantCommand {
            project_id: project.id, person_id: lawyer, dri: DriRequest::Unchanged,
            actor: DriActor::System,
        }).await.unwrap();
        assert_eq!(project.status, "closed");
        projects::set_plan_columns(&db, &pg, project.id, &PlanColumns {
            legal_plan_id: Some(plan.id),
            plan_case_number: Some("sample-case".into()),
            ..PlanColumns::default()
        }).await.unwrap().unwrap();
        let entry_id = time_entries::create(&pg, project.id, lawyer, &NewTimeEntry {
            attorney_id: lawyer,
            performed_on: "2026-11-01".parse().unwrap(),
            minutes: 18,
            description: "Synthetic plan advice".into(),
            billable: true,
            rate_cents_per_hour: 10_000,
            source: TimeEntrySource::Manual,
        }).await.unwrap().id;

        let app = crate::router(app_state(db.clone()).await);
        let route = format!("/app/api/projects/{}/legal-plan-claims", project.id);
        let project_billing = format!("/app/api/projects/{}/billing", project.id);
        let collections = format!("/app/api/firms/{firm}/legal-plan-collections");
        let foreign_collections = format!("/app/api/firms/{foreign}/legal-plan-collections");
        let totals_route = format!("{collections}?totals_only=true");
        let read = async |uri: &str, person, role| request(&app, "GET", uri, person, role, None).await;
        assert_eq!(read(&route, admin, Role::Admin).await.0, StatusCode::NOT_FOUND);
        assert_eq!(read(&route, outsider, Role::Admin).await.0, StatusCode::NOT_FOUND);
        assert_eq!(read(&project_billing, outsider, Role::Admin).await.0, StatusCode::NOT_FOUND);
        let (status, personal) = read("/app/api/billing", lawyer, Role::Lawyer).await;
        assert_eq!(status, StatusCode::OK, "{personal}");
        assert_eq!(personal["my_time"][0]["id"], json!(entry_id));
        assert!(personal.get("project_time").is_none());
        assert_eq!(read(&foreign_collections, lawyer, Role::Lawyer).await.0, StatusCode::NOT_FOUND);
        assert_eq!(read(&totals_route, lawyer, Role::Lawyer).await.0, StatusCode::NOT_FOUND);
        let (code, report) = read(&collections, lawyer, Role::Lawyer).await;
        assert_eq!(code, StatusCode::OK, "{report}");
        assert_eq!(report["plans"]["ARAG Group Legal"]["closed_projects_without_claim_count"], 1);

        let claim_id = Uuid::new_v4();
        let make_event = |id, event: Value| json!({
            "claim_id": id, "event_id": Uuid::new_v4(), "occurred_on": "2026-11-01", "event": event
        });
        let work = json!({
            "type":"work_recorded", "work": {
                "service":"arag_advice_only", "benefit":"SAMPLE", "performed_on":"2026-11-01",
                "time_entry_ids": [entry_id],
                "authorization": {"type":"fixed_fee", "reference":"sample-benefit", "cents":10000}
            }
        });
        let post = async |body: &Value| request(&app, "POST", &route, lawyer, Role::Lawyer, Some(body)).await;
        let completed = make_event(claim_id, work.clone());
        let (code, state) = post(&completed).await;
        assert_eq!(code, StatusCode::OK, "{state}");
        assert_eq!(state["state"]["work"]["attorney_minutes"], 18);
        let (code, retry) = post(&completed).await;
        assert_eq!(code, StatusCode::OK, "{retry}");
        assert_eq!(retry["evidence"].as_array().unwrap().len(), 1);
        let (status, html) = billing_page(&app, &project.code, lawyer, Role::Lawyer).await;
        assert_eq!(status, StatusCode::OK);
        assert!(html.contains("Synthetic plan advice"), "recorded time is visible");
        assert!(html.contains("Legal plan claims"), "the matter shows its carrier claim beside time");
        let claims = claim_row(&html, claim_id);
        for value in ["ARAG", "sample-case", "SAMPLE", "2026-11-01", ">18<", "Completed", "$100.00"] {
            assert!(claims.contains(value), "missing claim value {value}");
        }
        assert_eq!(claims.matches(">18<").count(), 1);
        assert!(!claims.contains(">36<"));
        assert_eq!(html.matches(&format!(r##"href="#claim-{claim_id}""##)).count(), 1);
        assert!(!html.contains("sample-benefit"), "authorization evidence stays off the page");
        assert_eq!(billing_page(&app, &project.code, admin, Role::Admin).await.0, StatusCode::NOT_FOUND);
        participation::add_participant(&db, &AddParticipantCommand {
            project_id: project.id, person_id: outsider, dri: DriRequest::Unchanged,
            actor: DriActor::System,
        }).await.unwrap();
        let (status, outside) = billing_page(&app, &project.code, outsider, Role::Admin).await;
        assert_eq!(status, StatusCode::OK);
        assert!(outside.contains("Synthetic plan advice"));
        assert!(!outside.contains("sample-case"));
        assert!(!outside.contains("Legal plan claims"));
        let second_id = Uuid::new_v4();
        assert_eq!(post(&make_event(second_id, work.clone())).await.0, StatusCode::UNPROCESSABLE_ENTITY);
        let mut second_work = work;
        second_work["work"]["time_entry_ids"] = json!([]);
        second_work["work"]["attorney_minutes"] = json!(18);
        second_work["work"]["authorization"] = json!({
            "type": "capped_hours", "reference": "sample-capped-benefit",
            "rate_cents_per_hour": 10_000, "covered_minutes": 240, "pending_minutes": 0
        });
        assert_eq!(post(&make_event(second_id, second_work)).await.0, StatusCode::OK);

        let mut attempt = make_event(claim_id, json!({"type":"prepared"}));
        attempt["event_id"] = completed["event_id"].clone();
        assert_eq!(post(&attempt).await.0, StatusCode::CONFLICT);
        assert_eq!(post(&make_event(claim_id, json!({"type":"prepared"}))).await.0, StatusCode::OK);
        assert_eq!(post(&make_event(second_id, json!({"type":"prepared"}))).await.0, StatusCode::OK);

        let case_only = make_event(claim_id, json!({
            "type":"submission", "provider_reference":"sample-case"
        }));
        assert_eq!(post(&case_only).await.0, StatusCode::UNPROCESSABLE_ENTITY);
        let mut submitted = json!({"type":"submission", "provider_reference":"sample-claim", "claimed_cents":9500});
        assert_eq!(post(&make_event(claim_id, submitted.clone())).await.0, StatusCode::OK);
        submitted["claimed_cents"] = json!(3000);
        assert_eq!(post(&make_event(second_id, submitted)).await.0, StatusCode::CONFLICT);
        assert_eq!(post(&make_event(claim_id, json!({
            "type":"correction", "provider_reference":"SAMPLE-CLAIM", "claimed_cents":9000
        }))).await.0, StatusCode::OK);
        assert_eq!(post(&make_event(claim_id, json!({
            "type":"approval", "reference":"sample-approval", "cents":9000
        }))).await.0, StatusCode::OK);

        let payment = make_event(claim_id, json!({
            "type":"payment", "reference":"sample-remittance-a", "cents":3000
        }));
        assert_eq!(post(&payment).await.0, StatusCode::OK);
        assert_eq!(post(&payment).await.0, StatusCode::OK);
        let duplicate_payment = make_event(claim_id, payment["event"].clone());
        assert_eq!(post(&duplicate_payment).await.0, StatusCode::CONFLICT);
        let (status, changed) = post(&make_event(claim_id, json!({
            "type":"payment", "reference":"sample-remittance-b", "cents":2000
        }))).await;
        assert_eq!(status, StatusCode::OK, "{changed}");
        assert_eq!(changed["approved_outstanding_cents"], 4000);
        assert_eq!(changed["state"]["net_cents"], 5000);
        let (_, page) = billing_page(&app, &project.code, lawyer, Role::Lawyer).await;
        // Equal dates and durations do not make the separately attested work a linked entry.
        assert!(!page.contains(&format!(r##"href="#claim-{second_id}""##)));
        let capped = claim_row(&page, second_id);
        assert!(capped.contains(">18<"));
        assert!(capped.contains("$30.00"));
        let row = claim_row(&page, claim_id);
        for value in ["Approved", "$100.00", "$90.00", "$40.00", "$50.00"] {
            assert!(row.contains(value), "missing recorded claim amount or status {value}");
        }
        let (status, report) = read(&collections, lawyer, Role::Lawyer).await;
        assert_eq!(status, StatusCode::OK, "{report}");
        let plan = &report["plans"]["ARAG Group Legal"];
        assert_eq!(plan["net_collected_per_attorney_hour_cents"], 8333);
        assert_eq!(plan["approved_outstanding_cents"], 4000);
        assert_eq!(plan["eligible_unsubmitted_count"], 1);
        assert_eq!(plan["eligible_unsubmitted_cents"], 3_000);
        let filing_review = report["filing_review"].clone();
        assert_eq!(filing_review["ARAG"]["eligible_unsubmitted_cents"], 3_000);
        assert_eq!(plan["items"].as_array().unwrap().len(), 2);
        let (status, report) = read(&totals_route, admin, Role::Admin).await;
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["filing_review"], filing_review);
        assert_eq!(report["plans"]["ARAG Group Legal"]["items"], json!([]));
        assert_eq!(report["plans"]["ARAG Group Legal"]["net_collected_cents"], 5000);
        let (status, history) = read(&route, lawyer, Role::Lawyer).await;
        assert_eq!(status, StatusCode::OK, "{history}");
        assert_eq!(history.as_array().unwrap().len(), 2);
        let (status, billing) = read(&project_billing, lawyer, Role::Lawyer).await;
        assert_eq!(status, StatusCode::OK, "{billing}");
        assert_eq!(billing["project_time"][0]["timekeeper"], json!(lawyer));
        let measured = billing["needs_my_action"].as_array().unwrap().iter()
            .chain(billing["other_claims"].as_array().unwrap())
            .find(|item| item["id"] == json!(claim_id)).unwrap();
        assert_eq!(measured["projected_fee_cents"], 10_000);
        assert_eq!(measured["approved_outstanding_cents"], 4_000);
        assert_eq!(measured["net_cash_received_cents"], 5_000);

        let unverified = Uuid::new_v4();
        let work = json!({"type":"work_recorded", "work": {
            "service":"arag_representation", "benefit":"SAMPLE-UNVERIFIED", "performed_on":"2026-11-01"
        }});
        assert_eq!(post(&make_event(unverified, work)).await.0, StatusCode::OK);
        let (_, html) = billing_page(&app, &project.code, lawyer, Role::Lawyer).await;
        let row = claim_row(&html, unverified);
        assert!(row.contains("Unverified"));
        assert!(!row.contains(">0<"), "unknown minutes stay unrecorded");
    }).await;
}
