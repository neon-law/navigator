use axum::{
    body::{to_bytes, Body},
    http::{header, Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use store::{
    firms::{self, FirmMembership, NewFirm, NewPersonFirmRole},
    legal_plans::{self, NewLegalPlan},
    participation::{self, AddParticipantCommand, DriActor, DriRequest},
    persons::{self, NewPerson, Role},
    postgres::PostgresDb,
    projects::{self, NewProject, PlanColumns},
    surreal::SurrealDb,
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

async fn person(db: &SurrealDb, email: &str, role: Role) -> Uuid {
    persons::create(
        db,
        &NewPerson::with_role("Synthetic claims actor", email, role),
    )
    .await
    .unwrap()
    .id
}

async fn create_firm(db: &SurrealDb, entity: Uuid, admin: Uuid) -> Uuid {
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

#[tokio::test]
#[allow(clippy::too_many_lines)] // One authenticated walk proves the life of a claim.
async fn a_closed_project_reconciles_claims_through_the_authenticated_routes() {
    with_postgres(async {
        let db = SurrealDb::uninitialized();
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
            acceptance_sla_hours: 24, turnaround_sla_days: 30, reporting_requirements: None,
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
            minutes: 60,
            description: "Synthetic plan advice".into(),
            billable: true,
            rate_cents_per_hour: 10000,
            source: TimeEntrySource::Manual,
        }).await.unwrap().id;

        let app = crate::router(app_state(db).await);
        let route = format!("/app/api/projects/{}/legal-plan-claims", project.id);
        let collections = format!("/app/api/firms/{firm}/legal-plan-collections");
        let foreign_collections = format!("/app/api/firms/{foreign}/legal-plan-collections");
        let totals_route = format!("{collections}?totals_only=true");
        let read = async |uri: &str, person, role| request(&app, "GET", uri, person, role, None).await;
        assert_eq!(read(&route, admin, Role::Admin).await.0, StatusCode::NOT_FOUND);
        assert_eq!(read(&route, outsider, Role::Admin).await.0, StatusCode::NOT_FOUND);
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
        assert_eq!(state["state"]["work"]["attorney_minutes"], 60);
        let (code, retry) = post(&completed).await;
        assert_eq!(code, StatusCode::OK, "{retry}");
        assert_eq!(retry["evidence"].as_array().unwrap().len(), 1);
        let second_id = Uuid::new_v4();
        assert_eq!(post(&make_event(second_id, work.clone())).await.0, StatusCode::UNPROCESSABLE_ENTITY);
        let mut second_work = work;
        second_work["work"]["time_entry_ids"] = json!([]);
        second_work["work"]["attorney_minutes"] = json!(60);
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
        let submitted = json!({"type":"submission", "provider_reference":"sample-claim", "claimed_cents":9500});
        assert_eq!(post(&make_event(claim_id, submitted.clone())).await.0, StatusCode::OK);
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
        let (status, report) = read(&collections, lawyer, Role::Lawyer).await;
        assert_eq!(status, StatusCode::OK, "{report}");
        let plan = &report["plans"]["ARAG Group Legal"];
        assert_eq!(plan["net_collected_per_attorney_hour_cents"], 2500);
        assert_eq!(plan["approved_outstanding_cents"], 4000);
        assert_eq!(plan["eligible_unsubmitted_count"], 1);
        assert_eq!(plan["items"].as_array().unwrap().len(), 2);
        let (status, report) = read(&totals_route, admin, Role::Admin).await;
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["plans"]["ARAG Group Legal"]["items"], json!([]));
        assert_eq!(report["plans"]["ARAG Group Legal"]["net_collected_cents"], 5000);
        let (status, history) = read(&route, lawyer, Role::Lawyer).await;
        assert_eq!(status, StatusCode::OK, "{history}");
        assert_eq!(history.as_array().unwrap().len(), 2);
    }).await;
}
