//! The normal plan intake queue and its lawyer decisions against PostgreSQL.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
};
use http_body_util::BodyExt;
use portal::{email::CapturingEmail, session::SESSION_COOKIE_NAME, AppState};
use store::{persons, plan_assignment, plan_intake, postgres::PostgresDb, projects};
use tower::ServiceExt as _;
use uuid::Uuid;

async fn review(
    app: &axum::Router,
    actor: &persons::Person,
    path: &str,
    form: Option<&str>,
) -> Response {
    let mut session = portal::SessionData::fresh("synthetic-plan-intake", actor.role);
    session.person_id = Some(actor.id);
    let token = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY).encode(&session);
    let request = Request::builder()
        .method(if form.is_some() { "POST" } else { "GET" })
        .uri(path)
        .header("Cookie", format!("{SESSION_COOKIE_NAME}={token}"))
        .header("X-CSRF-Token", &session.csrf_token)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form.map_or_else(Body::empty, |form| Body::from(form.to_owned())))
        .unwrap();
    app.clone().oneshot(request).await.unwrap()
}

async fn html(response: Response) -> String {
    assert_eq!(response.status(), StatusCode::OK);
    String::from_utf8(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap()
}

#[tokio::test]
async fn a_lawyer_takes_a_referral_through_the_review_queue_without_sending_mail() {
    const CHILD: &str = "NAVIGATOR_PLAN_INTAKE_REVIEW_CHILD";
    if std::env::var_os(CHILD).is_some() {
        check_review().await;
        return;
    }
    // Dioxus renders on its own executor, outside the fixture's task-local
    // scope. Give the child the ordinary application connection settings.
    store::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let status = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "a_lawyer_takes_a_referral_through_the_review_queue_without_sending_mail",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
            .env(
                "NAVIGATOR_POSTGRES_PORT",
                std::env::var("NAVIGATOR_TEST_POSTGRES_PORT").unwrap(),
            )
            .env("NAVIGATOR_POSTGRES_DATABASE", database)
            .env("NAVIGATOR_POSTGRES_USER", "postgres")
            .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
            .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
            .status()
            .await
            .unwrap();
        assert!(
            status.success(),
            "review the staged assignment and take it through the portal"
        );
    })
    .await;
}

async fn seed_review(
    db: &store::surreal::SurrealDb,
    pg: &PostgresDb,
) -> (persons::Person, persons::Person, Uuid) {
    use store::firms::{self, FirmMembership, NewPersonFirmRole};
    sqlx::query("INSERT INTO brand(id,name,brand_key) VALUES ($1,'Synthetic Brand','neon') ON CONFLICT (brand_key) DO NOTHING")
        .bind(Uuid::now_v7()).execute(pg.pool()).await.unwrap();
    let firm = Uuid::now_v7();
    sqlx::query("INSERT INTO firm(id,name,slug,status) VALUES ($1,'Synthetic Firm','synthetic-intake','active')")
        .bind(firm).execute(pg.pool()).await.unwrap();
    firms::ensure_brand(db, firm, "neon").await.unwrap();
    let lawyer = persons::create(
        db,
        &persons::NewPerson::with_role(
            "Synthetic Review Lawyer",
            "review-lawyer@example.invalid",
            persons::Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    firms::ensure_membership(
        db,
        &NewPersonFirmRole {
            person_id: lawyer.id,
            firm_id: firm,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    let outsider = persons::create(
        db,
        &persons::NewPerson::with_role(
            "Synthetic Outside Lawyer",
            "outside-review@example.invalid",
            persons::Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::legal_plans::create(
        pg,
        firm,
        &store::legal_plans::NewLegalPlan {
            name: "LegalEASE".into(),
            contact: None,
            portal: None,
            acceptance_sla_hours: Some(24),
            turnaround_sla_days: Some(14),
            reporting_requirements: None,
        },
    )
    .await
    .unwrap();
    let provider = plan_assignment::PlanProvider::LegalEase;
    let assignment = plan_assignment::parse(
        provider,
        plan_assignment::fixtures::LEGAL_EASE_SUBJECT,
        &plan_assignment::fixtures::legal_ease_case_review_text(),
    )
    .unwrap();
    let staged = plan_intake::stage(
        db,
        Some(pg),
        &plan_intake::StageInput {
            firm_id: firm,
            conversation_id: Uuid::now_v7(),
            source_message_id: Uuid::now_v7(),
            landing_at: chrono::Utc::now(),
            provider: provider.as_str().into(),
            assignment,
        },
    )
    .await
    .unwrap();
    (lawyer, outsider, staged.id)
}

async fn check_review() {
    let db = store::surreal::SurrealDb::uninitialized();
    let pg = PostgresDb::from_env().await.unwrap();
    let (lawyer, outsider, staged_id) = seed_review(&db, &pg).await;
    let email = Arc::new(CapturingEmail::new());
    let state = AppState {
        email: email.clone(),
        policy: portal::policy::PolicyClient::embedded().unwrap(),
        ..portal::test_support::app_state(db.clone()).await
    };
    store::test_support::seed_notation_catalog(&db, &state.storage)
        .await
        .unwrap();
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let queue = "/app/plan-intake";
    let accept = format!("{queue}/{staged_id}/accept");
    let resolve = format!("{queue}/{staged_id}/resolve-member");
    let take = "attestation=yes&plan_matter_type=&code=sample-lease-nv&plan_state=nv";
    let member = "member_email=synthetic-member%40example.invalid";

    let before = html(review(&app, &lawyer, queue, None).await).await;
    assert!(
        before.contains("Confirm member and check conflicts"),
        "{before}"
    );
    assert!(
        !before.contains("Take assignment and open Pitch"),
        "{before}"
    );
    assert_eq!(
        review(&app, &outsider, &resolve, Some(member))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        review(&app, &outsider, &accept, Some(take)).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        review(&app, &lawyer, &resolve, Some(member)).await.status(),
        StatusCode::SEE_OTHER
    );
    let after = html(review(&app, &lawyer, queue, None).await).await;
    assert!(after.contains("Conflict check clear."), "{after}");
    assert!(after.contains("Take assignment and open Pitch"), "{after}");
    for _ in 0..2 {
        let decision = review(&app, &lawyer, &accept, Some(take)).await;
        assert_eq!(decision.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            decision.headers().get("location").unwrap(),
            "/app/projects/sample-lease-nv"
        );
    }
    let project = projects::find_by_code(&db, "sample-lease-nv")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(project.plan_case_number.as_deref(), Some("400001"));
    assert_eq!(project.plan_member_id.as_deref(), Some("9900001"));
    let notations = store::notations::list_by_project(&db, project.id)
        .await
        .unwrap();
    let [notation] = notations.as_slice() else {
        panic!("one onboarding Notation per accepted referral");
    };
    let template = store::templates::find_by_id(&db, notation.template_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(template.code, "onboarding__legalease");
    assert!(email.captured().is_empty());
}
