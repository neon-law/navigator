#![allow(clippy::doc_markdown)]
//! ENG-862: soft deletion, restoration, Legal Hold, and the retention purge,
//! through the REST doors a Project's `project sync` and an operator reach.
//!
//! A lawyer-tier delete stamps the whole slug chain and hides it from clients
//! and the default lists while a lawyer still reads it; restore returns it with
//! its revision history; the purge expunges only chains past their window that
//! no Legal Hold or unarchived closed matter protects, and writes the same
//! audit record a governed expunge does.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt;
use portal::session::SessionData;
use portal::{AppState, SessionStore};
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const KEY: &str = "api-document-retention-test-key";
const COMMIT: &str = "3f87afb5c0ffee1234567890abcdef1234567890";

struct Fixture {
    app: axum::Router,
    surreal: store::surreal::SurrealDb,
    storage: Arc<dyn cloud::StorageService>,
    project_id: Uuid,
    project_code: String,
    lawyer_id: Uuid,
    admin_id: Uuid,
    lawyer: String,
    outsider: String,
    client: String,
    admin: String,
}

fn bearer(person_id: Uuid, role: Role) -> String {
    let mut s = SessionData::fresh("api-retention-sub", role);
    s.person_id = Some(person_id);
    format!("Bearer {}", SessionStore::new(KEY).encode(&s))
}

async fn person(
    surreal: &store::surreal::SurrealDb,
    name: &str,
    role: Role,
) -> store::persons::Person {
    store::persons::create(
        surreal,
        &store::persons::NewPerson::with_role(name, format!("{name}@example.com"), role),
    )
    .await
    .unwrap()
}

async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let storage: Arc<dyn cloud::StorageService> = Arc::new(
        cloud::FsStorage::new(
            std::env::temp_dir().join(format!("nav-api-retention-{}", Uuid::now_v7())),
        )
        .await
        .unwrap(),
    );
    let project = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: format!("matter-{}", Uuid::now_v7()),
            name: "Matter".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let lawyer = person(&surreal, "lawyer", Role::Lawyer).await;
    store::projects::add_participation(&surreal, project.id, lawyer.id, "lawyer")
        .await
        .unwrap();
    let outsider = person(&surreal, "outsider", Role::Lawyer).await;
    let client = person(&surreal, "client", Role::Client).await;
    store::projects::add_participation(&surreal, project.id, client.id, "client")
        .await
        .unwrap();
    let admin = person(&surreal, "admin", Role::Admin).await;
    store::projects::add_participation(&surreal, project.id, admin.id, "admin")
        .await
        .unwrap();
    let state = AppState {
        sessions: SessionStore::new(KEY),
        storage: storage.clone(),
        ..portal::test_support::app_state(surreal.clone()).await
    };
    Fixture {
        app: server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR)),
        surreal,
        storage,
        project_id: project.id,
        project_code: project.code,
        lawyer_id: lawyer.id,
        admin_id: admin.id,
        lawyer: bearer(lawyer.id, Role::Lawyer),
        outsider: bearer(outsider.id, Role::Lawyer),
        client: bearer(client.id, Role::Client),
        admin: bearer(admin.id, Role::Admin),
    }
}

async fn call(
    fx: &Fixture,
    method: &str,
    uri: &str,
    auth: Option<&str>,
    body: Option<serde_json::Value>,
) -> axum::http::Response<Body> {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    let body = match body {
        Some(body) => {
            req = req.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    fx.app
        .clone()
        .oneshot(req.body(body).unwrap())
        .await
        .unwrap()
}

async fn json(response: axum::http::Response<Body>) -> serde_json::Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

/// File one revision of `slug` and return its asset id.
async fn file(fx: &Fixture, slug: &str, bytes: &[u8], visibility: &str) -> Uuid {
    let response = call(
        fx,
        "POST",
        &format!("/app/api/projects/{}/documents", fx.project_id),
        Some(&fx.lawyer),
        Some(serde_json::json!({
            "filename": slug.rsplit('/').next().unwrap(),
            "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
            "content_type": "text/plain",
            "kind": "memo",
            "visibility": visibility,
            "slug": slug,
        })),
    )
    .await;
    assert!(
        response.status().is_success(),
        "upload of {slug}: {}",
        response.status()
    );
    json(response).await["current_version"]["asset_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

fn door(fx: &Fixture, asset_id: Uuid, verb: &str) -> String {
    format!(
        "/app/api/projects/{}/documents/{asset_id}/{verb}",
        fx.project_id
    )
}

async fn delete(fx: &Fixture, auth: Option<&str>, asset_id: Uuid) -> axum::http::Response<Body> {
    call(
        fx,
        "POST",
        &door(fx, asset_id, "delete"),
        auth,
        Some(serde_json::json!({ "commit_sha": COMMIT })),
    )
    .await
}

async fn retire(fx: &Fixture, auth: Option<&str>, asset_id: Uuid) -> axum::http::Response<Body> {
    call(
        fx,
        "POST",
        &door(fx, asset_id, "delete"),
        auth,
        Some(serde_json::json!({ "manual": true })),
    )
    .await
}

async fn listed(fx: &Fixture, auth: &str, query: &str) -> Vec<String> {
    let response = call(
        fx,
        "GET",
        &format!("/app/api/projects/{}/documents{query}", fx.project_id),
        Some(auth),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    json(response)
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["slug"].as_str().unwrap_or_default().to_string())
        .collect()
}

async fn make_due(fx: &Fixture, slug: &str) {
    fx.surreal
        .query(
            "UPDATE asset SET purge_after = time::now() - 1d \
             WHERE project_id = $project AND slug = $slug",
        )
        .bind((
            "project",
            store::surreal::record_id("project", fx.project_id),
        ))
        .bind(("slug", slug.to_string()))
        .await
        .unwrap()
        .check()
        .unwrap();
}

/// A lawyer-tier delete stamps the whole chain with who and which commit,
/// hides it from clients and the default lists, and leaves a lawyer able to
/// read it. Anonymous is 401, a client 403, a lawyer outside the matter 404.
#[tokio::test]
async fn a_soft_delete_stamps_the_chain_and_hides_it_from_clients() {
    let fx = fixture().await;
    let first = file(&fx, "memos/note.txt", b"first", "client").await;
    let second = file(&fx, "memos/note.txt", b"second", "client").await;
    file(&fx, "memos/other.txt", b"other", "client").await;
    // Filing `second` made `first` internal, so the client lists two.
    assert_eq!(listed(&fx, &fx.client, "").await.len(), 2);

    assert_eq!(
        delete(&fx, None, first).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        delete(&fx, Some(&fx.client), first).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        delete(&fx, Some(&fx.outsider), first).await.status(),
        StatusCode::NOT_FOUND
    );
    let bad = call(
        &fx,
        "POST",
        &door(&fx, first, "delete"),
        Some(&fx.lawyer),
        Some(serde_json::json!({ "commit_sha": "not a sha" })),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    let deleted = delete(&fx, Some(&fx.lawyer), first).await;
    assert_eq!(deleted.status(), StatusCode::OK);
    let body = json(deleted).await;
    assert_eq!(body["revisions"], 2);
    assert_eq!(body["changed"], true);
    assert_eq!(body["slug"], "memos/note.txt");
    for id in [first, second] {
        let row = store::assets::find_by_id(&fx.surreal, id)
            .await
            .unwrap()
            .unwrap();
        assert!(row.deleted_at.is_some());
        assert_eq!(
            row.purge_after.unwrap() - row.deleted_at.unwrap(),
            chrono::Duration::days(180)
        );
        assert_eq!(row.deleted_by_person_id, Some(fx.lawyer_id));
        assert_eq!(row.deleted_commit_sha.as_deref(), Some(COMMIT));
    }

    assert_eq!(listed(&fx, &fx.client, "").await, ["memos/other.txt"]);
    assert_eq!(listed(&fx, &fx.lawyer, "").await, ["memos/other.txt"]);
    assert_eq!(
        listed(&fx, &fx.client, "?include_deleted=true").await,
        ["memos/other.txt"]
    );
    assert_eq!(
        listed(&fx, &fx.lawyer, "?include_deleted=true").await.len(),
        3
    );

    let download = |auth: String| {
        let uri = format!(
            "/app/projects/{}/documents/{second}/download",
            fx.project_code
        );
        let fx = &fx;
        async move { call(fx, "GET", &uri, Some(&auth), None).await.status() }
    };
    assert_eq!(download(fx.client.clone()).await, StatusCode::NOT_FOUND);
    assert_eq!(download(fx.lawyer.clone()).await, StatusCode::OK);

    let revisions = format!(
        "/app/api/projects/{}/documents/revisions?slug=memos/note.txt",
        fx.project_id
    );
    assert_eq!(
        call(&fx, "GET", &revisions, Some(&fx.client), None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let lawyer_view = json(call(&fx, "GET", &revisions, Some(&fx.lawyer), None).await).await;
    assert_eq!(lawyer_view["revisions"].as_array().unwrap().len(), 2);
    assert!(lawyer_view["revisions"][0]["deleted_at"].is_string());
    assert!(lawyer_view["revisions"][0]["purge_after"].is_string());

    let refiled = call(
        &fx,
        "POST",
        &format!("/app/api/projects/{}/documents", fx.project_id),
        Some(&fx.lawyer),
        Some(serde_json::json!({
            "filename": "note.txt",
            "content_base64": base64::engine::general_purpose::STANDARD.encode(b"third"),
            "content_type": "text/plain", "kind": "memo", "slug": "memos/note.txt",
        })),
    )
    .await;
    assert_eq!(refiled.status(), StatusCode::CONFLICT);
    assert_eq!(json(refiled).await["error"], "document_deleted");

    let again = delete(&fx, Some(&fx.lawyer), second).await;
    assert_eq!(again.status(), StatusCode::OK);
    assert_eq!(json(again).await["changed"], false);
}

#[tokio::test]
async fn a_manual_retirement_hides_the_chain_without_a_pointer_removal_commit() {
    let fx = fixture().await;
    let first = file(&fx, "stray.txt", b"first", "internal").await;
    let second = file(&fx, "stray.txt", b"second", "internal").await;

    assert_eq!(
        retire(&fx, Some(&fx.client), second).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        retire(&fx, Some(&fx.outsider), second).await.status(),
        StatusCode::NOT_FOUND
    );

    let bad = call(
        &fx,
        "POST",
        &door(&fx, second, "delete"),
        Some(&fx.lawyer),
        Some(serde_json::json!({ "manual": true, "commit_sha": COMMIT })),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json(bad).await["error"], "invalid_delete_source");

    let response = retire(&fx, Some(&fx.lawyer), second).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = json(response).await;
    assert_eq!(body["slug"], "stray.txt");
    assert_eq!(body["revisions"], 2);
    assert_eq!(listed(&fx, &fx.lawyer, "").await, Vec::<String>::new());
    for id in [first, second] {
        let row = store::assets::find_by_id(&fx.surreal, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.deleted_by_person_id, Some(fx.lawyer_id));
        assert!(row.deleted_at.is_some());
        assert!(row.deleted_commit_sha.is_none());
    }
}

#[tokio::test]
async fn a_closed_repository_archive_is_never_soft_deleted() {
    let fx = fixture().await;
    let archive = {
        let response = call(
            &fx,
            "POST",
            &format!("/app/api/projects/{}/documents", fx.project_id),
            Some(&fx.lawyer),
            Some(serde_json::json!({
                "filename": "closed.zip",
                "content_base64": base64::engine::general_purpose::STANDARD.encode(b"zip"),
                "content_type": "application/zip", "kind": "closed_repository",
                "slug": "closed_repository.zip",
            })),
        )
        .await;
        json(response).await["current_version"]["asset_id"]
            .as_str()
            .unwrap()
            .parse::<Uuid>()
            .unwrap()
    };

    let refused = delete(&fx, Some(&fx.lawyer), archive).await;

    assert_eq!(refused.status(), StatusCode::CONFLICT);
    assert_eq!(json(refused).await["error"], "closed_repository_protected");
    assert!(store::assets::find_by_id(&fx.surreal, archive)
        .await
        .unwrap()
        .unwrap()
        .deleted_at
        .is_none());
    assert_eq!(
        retire(&fx, Some(&fx.lawyer), archive).await.status(),
        StatusCode::CONFLICT
    );
    assert!(store::assets::find_by_id(&fx.surreal, archive)
        .await
        .unwrap()
        .unwrap()
        .deleted_at
        .is_none());
}

/// Restoring within the window returns the document and its full revision
/// history; a client cannot restore; a purged chain cannot be restored.
#[tokio::test]
async fn restoring_returns_the_document_with_its_history() {
    let fx = fixture().await;
    let first = file(&fx, "memos/note.txt", b"first", "client").await;
    let second = file(&fx, "memos/note.txt", b"second", "client").await;
    delete(&fx, Some(&fx.lawyer), second).await;
    assert!(listed(&fx, &fx.lawyer, "").await.is_empty());

    assert_eq!(
        call(
            &fx,
            "POST",
            &door(&fx, first, "restore"),
            Some(&fx.client),
            None
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    let restored = call(
        &fx,
        "POST",
        &door(&fx, first, "restore"),
        Some(&fx.lawyer),
        None,
    )
    .await;
    assert_eq!(restored.status(), StatusCode::OK);
    let body = json(restored).await;
    assert_eq!(body["revisions"], 2);
    assert_eq!(body["changed"], true);

    assert_eq!(
        listed(&fx, &fx.lawyer, "").await.len(),
        2,
        "both revisions return"
    );
    assert_eq!(
        listed(&fx, &fx.client, "").await.len(),
        1,
        "the client reads the shared revision; the superseded one is internal"
    );
    let row = store::assets::find_by_id(&fx.surreal, second)
        .await
        .unwrap()
        .unwrap();
    assert!(row.deleted_at.is_none() && row.purge_after.is_none());
    assert!(row.deleted_by_person_id.is_none() && row.deleted_commit_sha.is_none());

    delete(&fx, Some(&fx.lawyer), second).await;
    make_due(&fx, "memos/note.txt").await;
    let report = portal::expunge::purge::purge_expired_documents(
        &fx.surreal,
        &fx.storage,
        fx.admin_id,
        chrono::Utc::now() + chrono::Duration::days(1),
    )
    .await
    .unwrap();
    assert_eq!(report.purged.len(), 1);
    let gone = call(
        &fx,
        "POST",
        &door(&fx, first, "restore"),
        Some(&fx.lawyer),
        None,
    )
    .await;
    assert_eq!(gone.status(), StatusCode::CONFLICT);
    assert_eq!(json(gone).await["error"], "purged");
}

#[tokio::test]
async fn a_legal_hold_is_lawyer_scoped_and_covers_the_chain() {
    let fx = fixture().await;
    let first = file(&fx, "memos/note.txt", b"first", "internal").await;
    let second = file(&fx, "memos/note.txt", b"second", "internal").await;
    let hold = |held: bool| serde_json::json!({ "held": held });

    for (auth, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(&fx.client), StatusCode::FORBIDDEN),
        (Some(&fx.outsider), StatusCode::NOT_FOUND),
    ] {
        assert_eq!(
            call(
                &fx,
                "POST",
                &door(&fx, first, "hold"),
                auth.map(String::as_str),
                Some(hold(true))
            )
            .await
            .status(),
            expected
        );
    }
    let placed = call(
        &fx,
        "POST",
        &door(&fx, first, "hold"),
        Some(&fx.lawyer),
        Some(hold(true)),
    )
    .await;
    assert_eq!(placed.status(), StatusCode::OK);
    assert_eq!(json(placed).await["revisions"], 2);
    for id in [first, second] {
        assert!(store::assets::find_by_id(&fx.surreal, id)
            .await
            .unwrap()
            .unwrap()
            .legal_hold_at
            .is_some());
    }
    call(
        &fx,
        "POST",
        &door(&fx, second, "hold"),
        Some(&fx.lawyer),
        Some(hold(false)),
    )
    .await;
    assert!(store::assets::find_by_id(&fx.surreal, first)
        .await
        .unwrap()
        .unwrap()
        .legal_hold_at
        .is_none());
}

async fn purge_door(fx: &Fixture, auth: Option<&str>) -> axum::http::Response<Body> {
    call(fx, "POST", "/app/api/documents/purge", auth, None).await
}

/// The purge expunges only chains past their window with no Legal Hold,
/// through the governed-expunge primitive, and never touches a chain twice.
#[tokio::test]
async fn the_purge_expunges_only_expired_unheld_chains_and_audits_each() {
    let fx = fixture().await;
    let expired = file(&fx, "memos/expired.txt", b"expired bytes", "internal").await;
    let held = file(&fx, "memos/held.txt", b"held bytes", "internal").await;
    let fresh = file(&fx, "memos/fresh.txt", b"fresh bytes", "internal").await;
    let live = file(&fx, "memos/live.txt", b"live bytes", "internal").await;
    for id in [expired, held, fresh] {
        delete(&fx, Some(&fx.lawyer), id).await;
    }
    call(
        &fx,
        "POST",
        &door(&fx, held, "hold"),
        Some(&fx.lawyer),
        Some(serde_json::json!({ "held": true })),
    )
    .await;
    make_due(&fx, "memos/expired.txt").await;
    make_due(&fx, "memos/held.txt").await;

    assert_eq!(
        purge_door(&fx, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        purge_door(&fx, Some(&fx.lawyer)).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        purge_door(&fx, Some(&fx.client)).await.status(),
        StatusCode::FORBIDDEN
    );
    let response = purge_door(&fx, Some(&fx.admin)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let report = json(response).await;
    assert_eq!(report["purged"].as_array().unwrap().len(), 1);
    assert_eq!(report["held"], 1);
    assert_eq!(report["awaiting_archive"], 0);

    let key = |id: Uuid| {
        let surreal = fx.surreal.clone();
        async move {
            store::assets::find_by_id(&surreal, id)
                .await
                .unwrap()
                .unwrap()
        }
    };
    let expired_row = key(expired).await;
    assert!(expired_row.purged_at.is_some());
    assert!(
        fx.storage.get(&expired_row.storage_key).await.is_err(),
        "the expunge deleted the bytes"
    );
    for id in [held, fresh, live] {
        let row = key(id).await;
        assert!(row.purged_at.is_none());
        assert!(
            fx.storage.get(&row.storage_key).await.is_ok(),
            "a held, unexpired, or live document keeps its bytes"
        );
    }
    let records = store::expunge_records::for_project(&fx.surreal, fx.project_id)
        .await
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].category,
        store::expunge_records::CATEGORY_RETENTION
    );
    assert_eq!(records[0].authorized_by_person_id, fx.admin_id);
    assert_eq!(records[0].path, "expired.txt");

    let again = json(purge_door(&fx, Some(&fx.admin)).await).await;
    assert_eq!(
        again["purged"].as_array().unwrap().len(),
        0,
        "a purged chain is never touched again"
    );
    assert_eq!(
        store::expunge_records::for_project(&fx.surreal, fx.project_id)
            .await
            .unwrap()
            .len(),
        1
    );

    call(
        &fx,
        "POST",
        &door(&fx, held, "hold"),
        Some(&fx.lawyer),
        Some(serde_json::json!({ "held": false })),
    )
    .await;
    let released = json(purge_door(&fx, Some(&fx.admin)).await).await;
    assert_eq!(
        released["purged"].as_array().unwrap().len(),
        1,
        "releasing the hold lets the purge run"
    );
}

/// A Project that is closed but not yet archived keeps its soft-deleted
/// documents; once the `closed_repository` archive is filed, the purge runs.
#[tokio::test]
async fn the_purge_waits_for_a_closed_project_to_be_archived() {
    let fx = fixture().await;
    let doc = file(&fx, "memos/expired.txt", b"expired bytes", "internal").await;
    delete(&fx, Some(&fx.lawyer), doc).await;
    make_due(&fx, "memos/expired.txt").await;
    fx.surreal
        .query("UPDATE $project SET closed_at = '2026-01-01T00:00:00Z'")
        .bind((
            "project",
            store::surreal::record_id("project", fx.project_id),
        ))
        .await
        .unwrap()
        .check()
        .unwrap();

    let waiting = json(purge_door(&fx, Some(&fx.admin)).await).await;
    assert_eq!(waiting["awaiting_archive"], 1);
    assert_eq!(waiting["purged"].as_array().unwrap().len(), 0);
    assert!(store::assets::find_by_id(&fx.surreal, doc)
        .await
        .unwrap()
        .unwrap()
        .purged_at
        .is_none());

    call(
        &fx,
        "POST",
        &format!("/app/api/projects/{}/documents", fx.project_id),
        Some(&fx.lawyer),
        Some(serde_json::json!({
            "filename": "closed.zip",
            "content_base64": base64::engine::general_purpose::STANDARD.encode(b"zip"),
            "content_type": "application/zip", "kind": "closed_repository",
            "slug": "closed_repository.zip",
        })),
    )
    .await;
    let archived = json(purge_door(&fx, Some(&fx.admin)).await).await;
    assert_eq!(archived["awaiting_archive"], 0);
    assert_eq!(archived["purged"].as_array().unwrap().len(), 1);
}
