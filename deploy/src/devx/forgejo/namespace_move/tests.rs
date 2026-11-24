use super::*;
use axum::{
    extract::{Path, State},
    http::Method,
    routing::any,
    Json, Router,
};
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

async fn provider(
    State(moved): State<Arc<AtomicBool>>,
    Path(path): Path<String>,
    method: Method,
) -> (StatusCode, Json<Value>) {
    let org = if moved.load(Ordering::SeqCst) {
        "new-firm"
    } else {
        "old-firm"
    };
    if path == "orgs/old-firm/rename" && method == Method::POST {
        // The external side effect succeeds but its response is lost.
        moved.store(true, Ordering::SeqCst);
        return (StatusCode::SERVICE_UNAVAILABLE, Json(Value::Null));
    }
    let body = if path == format!("orgs/{org}") {
        json!({"username":org,"id":21})
    } else if path == format!("repos/{org}/sample") {
        json!({"owner":{"id":21},"id":812,"full_name":format!("{org}/sample")})
    } else if path == "user" {
        json!({"login":"test-owner"})
    } else if path == format!("users/test-owner/orgs/{org}/permissions") {
        json!({"is_owner":true})
    } else {
        return (StatusCode::NOT_FOUND, Json(Value::Null));
    };
    (StatusCode::OK, Json(body))
}

#[tokio::test]
async fn interrupted_rename_resumes_with_original_project_and_repository_ids() {
    store::test_support::with_postgres(rehearse("old-firm")).await;
}

#[tokio::test]
async fn different_firm_and_provider_names_resume_to_one_verified_namespace() {
    store::test_support::with_postgres(rehearse("original-practice")).await;
}

async fn rehearse(firm_slug: &str) {
    let pg = PostgresDb::application().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let moved = Arc::new(AtomicBool::new(false));
    let app = Router::new()
        .route("/api/v1/{*path}", any(provider))
        .with_state(moved.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let forge = Forge {
        origin: origin.clone(),
        client: Client::new(),
        token: "synthetic".into(),
    };
    let firm: Uuid = sqlx::query_scalar(
        "INSERT INTO firm (name,slug,status) VALUES ('Synthetic',$1,'active') RETURNING id",
    )
    .bind(firm_slug)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    let entity:Uuid=sqlx::query_scalar("WITH j AS (INSERT INTO jurisdiction (name,code) VALUES ('Synthetic','ST') RETURNING id), t AS (INSERT INTO entity_type (name) VALUES ('Synthetic') RETURNING id) INSERT INTO entity (name,entity_type_id,jurisdiction_id) SELECT 'Synthetic',t.id,j.id FROM t,j RETURNING id").fetch_one(pg.pool()).await.unwrap();
    let old_url = format!("{origin}/old-firm/sample.git");
    let project:Uuid=sqlx::query_scalar("INSERT INTO project (code,name,status,firm_id,entity_id,repository_url) VALUES ('sample','Synthetic','open',$1,$2,$3) RETURNING id").bind(firm).bind(entity).bind(&old_url).fetch_one(pg.pool()).await.unwrap();
    let external = "https://example.test/old-firm/other";
    sqlx::query("INSERT INTO project (code,name,status,firm_id,entity_id,repository_url) VALUES ('other','Synthetic','open',$1,$2,$3)").bind(firm).bind(entity).bind(external).execute(pg.pool()).await.unwrap();
    let mut args = FirmNamespaceMove {
        firm,
        from: firm_slug.into(),
        forgejo_from: (firm_slug != "old-firm").then(|| "old-firm".into()),
        to: "new-firm".into(),
        organization_id: 20,
        apply: true,
        token_stdin: false,
    };
    assert!(coordinate(&pg, &forge, &args).await.is_err());
    args.organization_id = 21;
    args.apply = false;
    coordinate(&pg, &forge, &args).await.unwrap();
    assert!(!moved.load(Ordering::SeqCst));
    args.apply = true;
    assert!(coordinate(&pg, &forge, &args).await.is_err());
    assert!(moved.load(Ordering::SeqCst));
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT slug FROM firm WHERE id=$1")
            .bind(firm)
            .fetch_one(pg.pool())
            .await
            .unwrap(),
        firm_slug
    );
    assert!(sqlx::query("UPDATE firm SET slug='new-firm' WHERE id=$1")
        .bind(firm)
        .execute(pg.pool())
        .await
        .is_err());

    assert!(
        sqlx::query("DELETE FROM firm_namespace_move WHERE firm_id=$1")
            .bind(firm)
            .execute(pg.pool())
            .await
            .is_err()
    );
    assert_runtime_cannot_complete(&pg).await;
    if args.forgejo_from.is_some() {
        let previous = args.forgejo_from.replace("unrelated".into());
        assert!(coordinate(&pg, &forge, &args).await.is_err());
        args.forgejo_from = previous;
    }
    coordinate(&pg, &forge, &args).await.unwrap();
    coordinate(&pg, &forge, &args).await.unwrap();
    let (url,other,count):(String,String,i64)=sqlx::query_as("SELECT repository_url, (SELECT repository_url FROM project WHERE code='other'), (SELECT count(*) FROM firm_namespace_move WHERE completed_at IS NOT NULL) FROM project WHERE id=$1").bind(project).fetch_one(pg.pool()).await.unwrap();
    assert_eq!(
        (url, other, count),
        (
            format!("{origin}/new-firm/sample.git"),
            external.to_owned(),
            1
        )
    );
    let retired_names = std::collections::BTreeSet::from([firm_slug, args.source_organization()]);
    for name in retired_names {
        let retired_url = format!("{origin}/{name}/sample.git");
        let error = sqlx::query("UPDATE project SET repository_url=$1 WHERE id=$2")
            .bind(retired_url)
            .bind(project)
            .execute(pg.pool())
            .await
            .unwrap_err();
        assert_eq!(
            error
                .as_database_error()
                .and_then(sqlx::error::DatabaseError::code)
                .as_deref(),
            Some("23514")
        );
    }
    assert_removable_after_completion(&pg, firm).await;
    server.abort();
}

async fn assert_runtime_cannot_complete(pg: &PostgresDb) {
    let mut unprivileged = pg.pool().begin().await.unwrap();
    sqlx::query(
        "SELECT set_config('navigator.firm_namespace_move',id::text,true) FROM firm_namespace_move",
    )
    .execute(&mut *unprivileged)
    .await
    .unwrap();
    sqlx::query("SET LOCAL ROLE pg_write_all_data")
        .execute(&mut *unprivileged)
        .await
        .unwrap();
    let refused = sqlx::query("UPDATE firm SET slug='new-firm'")
        .execute(&mut *unprivileged)
        .await
        .unwrap_err();
    assert_eq!(
        refused
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23514")
    );
    unprivileged.rollback().await.unwrap();
}

async fn assert_removable_after_completion(pg: &PostgresDb, firm: Uuid) {
    sqlx::query("DELETE FROM project WHERE firm_id=$1")
        .bind(firm)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("DELETE FROM firm WHERE id=$1")
        .bind(firm)
        .execute(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM firm_namespace_move WHERE firm_id=$1")
            .bind(firm)
            .fetch_one(pg.pool())
            .await
            .unwrap(),
        0
    );
}
