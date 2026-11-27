//! Native PostgreSQL journal HTTP authorization, idempotence, and balance boundaries.
use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use portal::session::{now_unix_secs, SessionSource, SessionStore};
use store::persons::{NewPerson, Role};
use tower::ServiceExt;
use uuid::Uuid;

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "synthetic tenancy fixture keeps the complete authorization scenario visible"
)]
async fn journal_routes_refuse_an_unrelated_administrator_and_forged_actor() {
    store::test_support::with_postgres(async {
    let db = store::application_store::ApplicationStore::uninitialized();
    let person = store::persons::create(
        &db,
        &NewPerson::with_role(
            "Synthetic administrator",
            "journal-admin@example.com",
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = store::firms::create(
        &db,
        &store::firms::NewFirm {
            name: format!("Synthetic Firm {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id: entity(&db).await,
            admin_dri_person_id: person.id,
        },
    )
    .await
    .unwrap();
    let outsider = store::persons::create(
        &db,
        &NewPerson::with_role(
            "Synthetic unrelated administrator",
            "journal-outsider@example.com",
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let app = native_router(db).await;
    let token = |role| {
        SessionStore::new(portal::test_support::TEST_SESSION_KEY).encode(&portal::SessionData {
            sub: "synthetic-journal-actor".into(),
            email: None,
            person_id: Some(outsider.id),
            exp: now_unix_secs() + 60,
            role,
            csrf_token: "synthetic-csrf".into(),
            source: SessionSource::Cli,
            provider: None,
            viewing_as_dri: None,
            email_verified: false,
            scope: None,
        })
    };
    for suffix in ["", "/statement", "/migration-triage", "/audits"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/app/api/firms/{}/journal{suffix}", firm.id))
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", token(Role::Admin)),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{suffix}");
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/app/api/firms/{}/journal/migration-triage",
                    firm.id
                ))
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token(Role::Owner)),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(), serde_json::json!([]));
    let entry = serde_json::json!({
        "id":Uuid::now_v7(),"firm_id":firm.id,"key":"synthetic-command","actor_id":Uuid::now_v7(),
        "occurred_at":"2026-10-11T00:00:00Z","memo":"Synthetic receipt",
        "postings":[],"sources":[],"receipts":[],"links":[],"reversal_of":null
    });
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/app/api/firms/{}/journal", firm.id))
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", token(Role::Owner)),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(entry.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let pg = store::postgres::PostgresDb::application().await.unwrap();
    assert!(store::firm_journal::entries(&pg, firm.id).await.unwrap().is_empty());
    assert_eq!(sqlx::query("UPDATE public._sqlx_migrations SET success=false WHERE version=(SELECT max(version) FROM public._sqlx_migrations)")
        .execute(pg.pool()).await.unwrap().rows_affected(), 1);
    let app = portal::router(portal::test_support::app_state(store::application_store::ApplicationStore::uninitialized()).await);
    let response = app.oneshot(Request::builder()
        .uri(format!("/app/api/firms/{}/journal/migration-triage", firm.id))
        .header(header::AUTHORIZATION, format!("Bearer {}", token(Role::Owner)))
        .body(Body::empty()).unwrap()).await.unwrap();
    // Authorization itself must refuse damaged storage before reaching the report.
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }).await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one synthetic end-to-end journal lifecycle verifies HTTP and PostgreSQL together"
)]
async fn journal_routes_preview_post_deduplicate_and_report_balances() {
    store::test_support::with_postgres(async {
    let db = store::application_store::ApplicationStore::uninitialized();
    let person = store::persons::create(
        &db,
        &NewPerson::with_role(
            "Synthetic journal administrator",
            format!("journal-{}@example.com", Uuid::now_v7()),
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = store::firms::create(
        &db,
        &store::firms::NewFirm {
            name: format!("Synthetic journal Firm {}", Uuid::now_v7()),
            status: "active".into(),
            entity_id: entity(&db).await,
            admin_dri_person_id: person.id,
        },
    )
    .await
    .unwrap();
    let app = native_router(db).await;
    let token =
        SessionStore::new(portal::test_support::TEST_SESSION_KEY).encode(&portal::SessionData {
            sub: "synthetic-journal-actor".into(),
            email: None,
            person_id: Some(person.id),
            exp: now_unix_secs() + 120,
            role: Role::Admin,
            csrf_token: "synthetic-csrf".into(),
            source: SessionSource::Cli,
            provider: None,
            viewing_as_dri: None,
            email_verified: false,
            scope: None,
        });
    let route = format!("/app/api/firms/{}/journal", firm.id);
    let entry = serde_json::json!({
        "id":Uuid::now_v7(),"firm_id":firm.id,"key":"synthetic-route-post","actor_id":person.id,
        "occurred_at":"2026-10-11T00:00:00Z","memo":"Synthetic journal receipt",
        "postings":[
            {"account":"operating","ownership":"firm","restrictions":[],
                "unit":{"kind":"fiat","currency":"USD","decimals":2},"quantity":"10000","project_id":null,"category":"cash"},
            {"account":"revenue","ownership":"firm","restrictions":[],
                "unit":{"kind":"fiat","currency":"USD","decimals":2},"quantity":"-10000","project_id":null,"category":"earned_fee"}
        ],"sources":[],"receipts":[],"links":[],"reversal_of":null
    });
    let request = |method: &str, suffix: &str, body: Option<&serde_json::Value>| {
        Request::builder()
            .method(method)
            .uri(format!("{route}{suffix}"))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
            .unwrap()
    };
    let response = app
        .clone()
        .oneshot(request("POST", "/preview", Some(&entry)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = app.clone().oneshot(request("GET", "", None)).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
        serde_json::json!([])
    );
    for _ in 0..2 {
        let response = app
            .clone()
            .oneshot(request("POST", "", Some(&entry)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }
    let response = app.clone().oneshot(request("GET", "", None)).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let rows: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(rows[0]["key"], entry["key"]);
    let response = app
        .clone()
        .oneshot(request("GET", "/statement", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let statement: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(statement["complete_account_reconciliation"], false);
    assert!(!statement["lines"].as_array().unwrap().is_empty());
    let mut conflict = entry.clone();
    conflict["memo"] = "Different synthetic command".into();
    let response = app
        .clone()
        .oneshot(request("POST", "", Some(&conflict)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let mut invalid = entry.clone();
    invalid["id"] = Uuid::now_v7().to_string().into();
    invalid["key"] = "synthetic-unbalanced".into();
    invalid["postings"][0]["quantity"] = "1".into();
    let response = app
        .clone()
        .oneshot(request("POST", "/preview", Some(&invalid)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let missing_project = Uuid::now_v7();
    for (suffix, body) in [
        (
            "/mercury",
            serde_json::json!({"project_id":missing_project,"account_id":Uuid::now_v7(),"transaction_id":Uuid::now_v7()}),
        ),
        (
            "/xero",
            serde_json::json!({"project_id":missing_project,"account_id":null,"object_id":Uuid::now_v7(),"object_type":"invoice"}),
        ),
        (
            "/solana",
            serde_json::json!({"project_id":missing_project,"network":"devnet","wallet":"synthetic-wallet","signature":"synthetic-signature","locator":null}),
        ),
    ] {
        let response = app
            .clone()
            .oneshot(request("POST", suffix, Some(&body)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    let job = Uuid::now_v7();
    let reconciliation = serde_json::json!({
        "id":job,"signal_cursor":0,"mode":"preview",
        "scope":{"firm_id":firm.id,"period_start":"2026-10-01T00:00:00Z",
            "period_end":"2026-10-31T00:00:00Z","complete_statement_document_id":null,
            "sources":[{"provider":"manual","scope":"synthetic","account":"operating",
                "object_type":"transaction","external_id":"synthetic-missing","locator":null}]},
        "snapshots":[],"proposed_entries":[],"approvals":[]
    });
    let response = app
        .clone()
        .oneshot(request("POST", "/reconcile", Some(&reconciliation)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let audit: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(audit["job_id"], job.to_string());
    assert!(!audit["exceptions"].as_array().unwrap().is_empty());
    assert_eq!(audit["complete_account_reconciliation"], false);
    for suffix in ["/audits", "/signals?after=0", "/migration-triage"] {
        let response = app
            .clone()
            .oneshot(request("GET", suffix, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    }).await;
}

async fn entity(db: &store::application_store::ApplicationStore) -> Uuid {
    let suffix = Uuid::now_v7();
    let jurisdiction = store::jurisdictions::create(
        db,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic jurisdiction {suffix}"),
            suffix.to_string(),
            "state",
        ),
    )
    .await
    .unwrap();
    let kind = store::entity_types::create(db, &format!("Synthetic kind {suffix}"))
        .await
        .unwrap();
    store::entities::create(
        db,
        &store::entities::NewEntity {
            name: format!("Synthetic entity {suffix}"),
            entity_type_id: kind.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap()
    .id
}

async fn native_router(db: store::application_store::ApplicationStore) -> axum::Router {
    let pg = store::postgres::PostgresDb::application().await.unwrap();
    portal::router(portal::test_support::app_state(db).await).layer(axum::middleware::from_fn(
        move |request: Request<Body>, next: axum::middleware::Next| {
            let pg = pg.clone();
            async move {
                store::postgres::PostgresDb::with_application_db(pg, next.run(request))
                    .await
                    .expect("each request requires the fixture pool admission")
            }
        },
    ))
}
