//! Signed SMS redelivery joins one archived receipt and one completed DBOS workflow.
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::routing::post;
use http_body_util::BodyExt as _;
use sqlx::ConnectOptions as _;
use std::{sync::Arc, time::Duration};
use store::{postgres::PostgresDb, surreal::SurrealDb};
use tower::ServiceExt as _;
use uuid::Uuid;
use workflows_service::email_summary::EmailSummaryReport;
use workflows_service::{
    dbos_email_summary as summary,
    dbos_ingress::{EmailSummaryIngress, WorkerIngress},
};

const CASE: &str = "twilio_sms_joins_the_summary_lane_and_dedupes_redelivery";
const CHILD: &str = "NAVIGATOR_SMS_SUMMARY_FIXTURE";
const NUMBER: &str = "+99900001";
const SENDER: &str = "+99900002";
const TOKEN: &str = "synthetic-summary-token";

#[tokio::test]
async fn twilio_sms_joins_the_summary_lane_and_dedupes_redelivery() {
    if std::env::var_os(CHILD).is_some() {
        exercise().await;
        return;
    }
    // The production worker opens its application pool inside detached DBOS steps.
    // Only the child inherits the owned database and workflow selection.
    store::test_support::with_required_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let options = pg.pool().connect_options();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", CASE, "--nocapture"])
            .env(CHILD, "1")
            .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
            .env("NAVIGATOR_POSTGRES_PORT", options.get_port().to_string())
            .env("NAVIGATOR_POSTGRES_USER", "postgres")
            .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
            .env(
                "NAVIGATOR_POSTGRES_DATABASE",
                options.get_database().unwrap(),
            )
            .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
            .env(workflows::trigger::EMAIL_SUMMARY_ENABLED_ENV, "true")
            .env(workflows::trigger::DBOS_INGRESS_TOKEN_ENV, TOKEN)
            .env_remove("RESTATE_BROKER_URL")
            .kill_on_drop(true);
        let mut child = child.spawn().unwrap();
        if let Ok(status) = tokio::time::timeout(Duration::from_secs(90), child.wait()).await {
            assert!(status.unwrap().success());
        } else {
            child.kill().await.unwrap();
            panic!("SMS summary worker exceeded its deadline");
        }
    })
    .await;
}

async fn exercise() {
    let pg = PostgresDb::application().await.unwrap();
    let db = SurrealDb::uninitialized();
    let mut state = portal::test_support::app_state(db.clone()).await;
    let directory = tempfile::tempdir().unwrap();
    state.storage = Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
    let url = pg.pool().connect_options().to_url_lossy().to_string();
    let worker = dbos::DBOS::new(workflows::dbos_engine::config(&url, "sms-test"));
    let slack = Arc::new(workflows::CapturingSlackBot::new());
    let workflow = summary::register(
        &worker,
        summary::Context::new(db.clone(), state.storage.clone(), slack.clone(), None),
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    std::env::set_var(workflows::trigger::DBOS_INGRESS_URL_ENV, &origin);
    let router = workflows_service::health::router_with_dbos_ingress(
        WorkerIngress {
            email_summary: Some(EmailSummaryIngress::new(TOKEN.into(), workflow)),
            ..Default::default()
        },
        None,
    )
    // This case runs the real summary workflow. Its sibling Inbox workflows
    // acknowledge submission here and have their own execution tests.
    .route(
        "/internal/dbos/inbound-message-alert",
        post(accept_inbox_submission),
    )
    .route(
        "/internal/dbos/inbound-triage",
        post(accept_inbox_submission),
    );
    worker.launch().await.unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (firm, _) = store::test_support::seed_firm_with_new_brand(&db, "neon").await;
    seed_sms_summary_mailroom(&db).await;
    let token = store::test_support::twilio::auth_token();
    configure(&mut state, &pg, firm, origin, &token).await;
    let app = server::neon_router(
        state.clone(),
        std::path::Path::new(portal::DEFAULT_PUBLIC_DIR),
    );
    post_sms(&app, &token).await;
    let receipt_id: Uuid = sqlx::query_scalar("SELECT id FROM email_receipt")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let workflow_id = summary::workflow_id(receipt_id);
    let report = worker
        .retrieve_workflow::<EmailSummaryReport, summary::EmailSummaryError>(&workflow_id)
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(report.receipt_id, receipt_id);
    assert_eq!(report.delivery_state, "confirmed");
    let posted = slack.posted_messages();
    assert!(!posted.is_empty());
    post_sms(&app, &token).await;
    assert_eq!(slack.posted_messages(), posted);
    let runs: Vec<String> = sqlx::query_scalar(
        "SELECT workflow_uuid FROM dbos.workflow_status WHERE name = 'EmailSummary'",
    )
    .fetch_all(pg.pool())
    .await
    .unwrap();
    assert_eq!(runs, [workflow_id]);
    assert_eq!(store::email_receipts::count(&db).await.unwrap(), 1);
    let letters = store::letters::list_all(&db).await.unwrap();
    assert_eq!(letters.len(), 1);
    assert_eq!(letters[0].recipient, format!("sms:{NUMBER}"));
    let objects = state.storage.list("inbound/").await.unwrap();
    assert_eq!(objects.len(), 1);
    let raw = state.storage.get(&objects[0].key).await.unwrap().bytes;
    let normalized = workflows::normalize_email(&raw, 32_000).unwrap();
    assert!(normalized.body.contains("Can we move Thursday's call?"));
    assert!(normalized.body.contains(&format!("from {SENDER}")));
    worker.shutdown().await;
    server.abort();
    let _ = server.await;
}

async fn accept_inbox_submission(headers: axum::http::HeaderMap) -> StatusCode {
    assert_eq!(
        headers.get(header::AUTHORIZATION).unwrap(),
        format!("Bearer {TOKEN}").as_str()
    );
    StatusCode::ACCEPTED
}

async fn configure(
    state: &mut portal::AppState,
    pg: &PostgresDb,
    firm: Uuid,
    origin: String,
    token: &str,
) {
    let project =
        store::test_support::seed_project_on_firm(&state.surreal, Some(firm), "Synthetic SMS")
            .await;
    state.summary_intake = Some(portal::inbound_email::SummaryIntakeConfig {
        envelope_recipients: vec![],
        inbound_public_key: String::new(),
        deployment: "synthetic".into(),
        workflow_ingress: origin,
        project_id: "synthetic-project".into(),
        matter_project_id: Some(project.id),
        firm_id: firm,
        provider_account: "synthetic-sms".into(),
        channel_id: "C-SYNTHETIC".into(),
        gemini_model: "synthetic-model".into(),
        gemini_location: "global".into(),
        max_input_chars: workflows::DEFAULT_MAX_INPUT_CHARS,
        max_output_tokens: workflows::DEFAULT_MAX_OUTPUT_TOKENS,
    });
    store::test_support::twilio::register_number(&state.surreal, pg, firm, "neon", NUMBER, token)
        .await;
    state.runtime_kms = Arc::new(cloud::FakeKms::new("spine-test"));
    state.canonical_host = portal::CanonicalHost::new(Some("sms.example.test".into()));
    state.twilio_registry = Some(Arc::new(pg.clone()));
}
async fn post_sms(app: &axum::Router, token: &str) {
    let fields = [
        ("MessageSid".to_owned(), "synthetic-message".to_owned()),
        ("From".to_owned(), SENDER.to_owned()),
        ("To".to_owned(), NUMBER.to_owned()),
        ("Body".to_owned(), "Can we move Thursday's call?".to_owned()),
        ("NumMedia".to_owned(), "0".to_owned()),
    ];
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&fields)
        .finish();
    let path = portal::twilio_sms::SMS_WEBHOOK_PATH;
    let signature = portal::twilio_sms::twilio_signature(
        token,
        &format!("https://sms.example.test{path}"),
        &fields,
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header(header::HOST, "sms.example.test")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header("x-twilio-signature", signature)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(std::str::from_utf8(&body)
        .unwrap()
        .contains("<Response></Response>"));
}

async fn seed_sms_summary_mailroom(db: &SurrealDb) {
    let addr = store::addresses::create(
        db,
        &store::addresses::NewAddress {
            line1: "1 Test".into(),
            city: "Reno".into(),
            region: "NV".into(),
            postal_code: "89501".into(),
            country: "US".into(),
            ..store::addresses::NewAddress::default()
        },
    )
    .await
    .unwrap();
    store::mailrooms::create(db, "HQ", addr.id).await.unwrap();
}
