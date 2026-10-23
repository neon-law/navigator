#![allow(clippy::doc_markdown)]
//! Integration tests for `POST /app/api/projects/{id}/documents` — the REST door
//! that files a document into a matter.
//!
//! The write engine (`matter_documents::record_document`) is shared with the
//! lawyer upload control, so this focuses on what the REST adapter adds: it takes
//! the bytes base64-encoded (not multipart), lawyer-tier only (client 403, anon
//! 401), the matter-scope gate (a non-participant lawyer is 404), a missing `kind`
//! is 400, undecodable base64 is 400, a `kind` outside the asset lane is 400 (not
//! a 500), and a filed document actually lands.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use portal::session::SessionData;
use portal::{AppState, SessionStore};
use store::persons::Role;
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

const KEY: &str = "api-project-documents-test-key";

struct Fixture {
    app: axum::Router,
    surreal: store::surreal::SurrealDb,
    storage: Arc<dyn cloud::StorageService>,
    project_id: Uuid,
    lawyer: String,
    outsider: String,
    client: String,
    admin: String,
}

fn bearer(person_id: Uuid, role: Role) -> String {
    let mut s = SessionData::fresh("api-doc-sub", role);
    s.person_id = Some(person_id);
    format!("Bearer {}", SessionStore::new(KEY).encode(&s))
}

async fn build_fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let storage: Arc<dyn cloud::StorageService> = Arc::new(
        cloud::FsStorage::new(
            std::env::temp_dir().join(format!("nav-api-docs-{}", Uuid::now_v7())),
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
    let lawyer = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Lawyer", "lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    store::projects::add_participation(&surreal, project.id, lawyer.id, "lawyer")
        .await
        .unwrap();
    let outsider = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Outsider", "outsider@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    let client = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Client", "client@example.com", Role::Client),
    )
    .await
    .unwrap();
    let admin = store::persons::create(
        &surreal,
        &store::persons::NewPerson::with_role("Admin", "admin@example.com", Role::Admin),
    )
    .await
    .unwrap();
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
        lawyer: bearer(lawyer.id, Role::Lawyer),
        outsider: bearer(outsider.id, Role::Lawyer),
        client: bearer(client.id, Role::Client),
        admin: bearer(admin.id, Role::Admin),
    }
}

async fn upload(
    fx: &Fixture,
    auth: Option<&str>,
    body: serde_json::Value,
) -> axum::http::Response<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/app/api/projects/{}/documents", fx.project_id))
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

async fn patch_document(
    fx: &Fixture,
    auth: Option<&str>,
    asset_id: Uuid,
    body: serde_json::Value,
) -> axum::http::Response<Body> {
    let mut req = Request::builder()
        .method("PATCH")
        .uri(format!(
            "/app/api/projects/{}/documents/{asset_id}",
            fx.project_id
        ))
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

async fn repair_storage(
    fx: &Fixture,
    auth: Option<&str>,
    asset_id: Uuid,
    dry_run: bool,
) -> axum::http::Response<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!(
            "/app/api/projects/{}/documents/{asset_id}/storage",
            fx.project_id
        ))
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    fx.app
        .clone()
        .oneshot(
            req.body(Body::from(
                serde_json::json!({ "dry_run": dry_run }).to_string(),
            ))
            .unwrap(),
        )
        .await
        .unwrap()
}

async fn set_visibility(
    fx: &Fixture,
    auth: Option<&str>,
    asset_id: Uuid,
    visibility: &str,
) -> axum::http::Response<Body> {
    let mut req = Request::builder()
        .method("PATCH")
        .uri(format!(
            "/app/api/projects/{}/documents/{asset_id}",
            fx.project_id
        ))
        .header("content-type", "application/json");
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    fx.app
        .clone()
        .oneshot(
            req.body(Body::from(
                serde_json::json!({ "visibility": visibility }).to_string(),
            ))
            .unwrap(),
        )
        .await
        .unwrap()
}

fn doc_body() -> serde_json::Value {
    // "test document" base64-encoded.
    serde_json::json!({
        "filename": "note.txt",
        "content_base64": "dGVzdCBkb2N1bWVudA==",
        "content_type": "text/plain",
        "kind": "unclassified",
        "slug": "notes/note.txt"
    })
}

#[tokio::test]
async fn a_participant_lawyer_files_a_document() {
    let fx = build_fixture().await;
    let resp = upload(&fx, Some(&fx.lawyer), doc_body()).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["kind"], "unclassified");
    assert_eq!(json["visibility"], "internal");
    assert_eq!(json["current_version"]["version"], 1);
    let document_id: Uuid = json["current_version"]["asset_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        store::assets::find_by_id(&fx.surreal, document_id)
            .await
            .unwrap()
            .is_some(),
        "the filed document is a real asset"
    );
}

/// ENG-481: the archive command records a repository's commit SHA in
/// `metadata`, and this is the wire path that carries it from the HTTP body
/// to the asset row without dropping it.
#[tokio::test]
async fn metadata_passes_through_to_the_stored_asset() {
    let fx = build_fixture().await;
    let mut body = doc_body();
    body["metadata"] = serde_json::json!({ "commit_sha": "deadbeefcafe" });
    let resp = upload(&fx, Some(&fx.lawyer), body).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let document_id: Uuid = json["current_version"]["asset_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let asset = store::assets::find_by_id(&fx.surreal, document_id)
        .await
        .unwrap()
        .expect("the filed document is a real asset");
    assert_eq!(
        asset.metadata.as_ref().and_then(|m| m.get("commit_sha")),
        Some(&serde_json::Value::String("deadbeefcafe".to_string()))
    );
}

/// A body naming no `metadata` at all still succeeds — the field is
/// optional, matching every other document-upload field this door accepts.
#[tokio::test]
async fn metadata_is_optional() {
    let fx = build_fixture().await;
    let resp = upload(&fx, Some(&fx.lawyer), doc_body()).await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let document_id: Uuid = json["current_version"]["asset_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let asset = store::assets::find_by_id(&fx.surreal, document_id)
        .await
        .unwrap()
        .expect("the filed document is a real asset");
    assert_eq!(asset.metadata, None);
}

#[tokio::test]
async fn a_repeated_slug_is_one_idempotent_revision_chain() {
    let fx = build_fixture().await;
    let first = upload(&fx, Some(&fx.lawyer), doc_body()).await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first_json: serde_json::Value =
        serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();

    let unchanged = upload(&fx, Some(&fx.lawyer), doc_body()).await;
    assert_eq!(unchanged.status(), StatusCode::OK);
    let unchanged_json: serde_json::Value =
        serde_json::from_slice(&unchanged.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(unchanged_json, first_json);

    let mut revised = doc_body();
    revised["content_base64"] = serde_json::json!(base64_of("revised document"));
    let second = upload(&fx, Some(&fx.lawyer), revised).await;
    assert_eq!(second.status(), StatusCode::CREATED);
    let second_json: serde_json::Value =
        serde_json::from_slice(&second.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(second_json["current_version"]["version"], 2);
    assert_eq!(
        second_json["previous_version"],
        first_json["current_version"]["asset_id"]
    );
    assert_eq!(
        store::assets::revisions(&fx.surreal, fx.project_id, "notes/note.txt")
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn a_pointer_visibility_edit_is_scoped_and_idempotent() {
    let fx = build_fixture().await;
    let filed = upload(&fx, Some(&fx.lawyer), doc_body()).await;
    let json: serde_json::Value =
        serde_json::from_slice(&filed.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let asset_id = json["current_version"]["asset_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let changed = set_visibility(&fx, Some(&fx.lawyer), asset_id, "client").await;
    assert_eq!(changed.status(), StatusCode::OK);
    let asset = store::assets::find_by_id(&fx.surreal, asset_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(asset.visibility, "client");

    let unchanged = set_visibility(&fx, Some(&fx.lawyer), asset_id, "client").await;
    let body: serde_json::Value =
        serde_json::from_slice(&unchanged.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["changed"], false);

    assert_eq!(
        set_visibility(&fx, Some(&fx.outsider), asset_id, "internal")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        set_visibility(&fx, Some(&fx.client), asset_id, "internal")
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn a_non_participant_lawyer_is_404() {
    let fx = build_fixture().await;
    let resp = upload(&fx, Some(&fx.outsider), doc_body()).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_client_is_403_and_anonymous_is_401() {
    let fx = build_fixture().await;
    assert_eq!(
        upload(&fx, Some(&fx.client), doc_body()).await.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        upload(&fx, None, doc_body()).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn undecodable_base64_is_400() {
    let fx = build_fixture().await;
    let resp = upload(
        &fx,
        Some(&fx.lawyer),
        serde_json::json!({
            "filename": "note.txt",
            "content_base64": "!!! not base64 !!!",
            "kind": "unclassified"
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// A `kind` the asset lane does not accept is the **caller's** error, so it is a
/// 400 carrying `invalid_kind` — not the 500 that `ApiError::Db` used to produce
/// by collapsing every ingest failure into "the database failed".
///
/// `review_queue_workbench` is the sharp case rather than gibberish: it is a real
/// `rules::kind::Kind`, valid in the Template lane, and refused only here. A door
/// that parsed the value without checking the lane would let it through.
#[tokio::test]
async fn a_kind_outside_the_asset_lane_is_400_naming_the_accepted_values() {
    let fx = build_fixture().await;
    let mut body = doc_body();
    body["kind"] = serde_json::json!("review_queue_workbench");
    let resp = upload(&fx, Some(&fx.lawyer), body).await;

    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "an unaccepted kind is the caller's mistake, not a server fault"
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        json["error"].as_str(),
        Some("invalid_kind"),
        "the body carries the machine-readable error identifier"
    );

    // The message has to let an integrator fix the request without reading our
    // source: it names what they sent and what would have been accepted.
    let message = json["message"].as_str().expect("a message is present");
    assert!(
        message.contains("review_queue_workbench"),
        "the message names the rejected value, got: {message}"
    );
    for accepted in [
        "onboarding",
        "unclassified",
        "certificate_of_naturalization",
        "exhibit",
    ] {
        assert!(
            message.contains(accepted),
            "the message lists the accepted kind `{accepted}`, got: {message}"
        );
    }

    // The refusal is real: the store wrote nothing.
    assert!(
        store::assets::for_project(&fx.surreal, fx.project_id)
            .await
            .unwrap()
            .is_empty(),
        "a refused kind files no document"
    );
}

/// Omitting `kind` is the caller's error, not a silent `unclassified` default.
/// The message names the accepted asset-lane values so an integrator can fix
/// the request without reading source.
#[tokio::test]
async fn a_missing_kind_is_400_naming_the_accepted_values() {
    let fx = build_fixture().await;
    let mut body = doc_body();
    body.as_object_mut().unwrap().remove("kind");
    let resp = upload(&fx, Some(&fx.lawyer), body).await;

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"].as_str(), Some("kind_required"));
    let message = json["message"].as_str().expect("a message is present");
    for accepted in [
        "onboarding",
        "unclassified",
        "certificate_of_naturalization",
    ] {
        assert!(
            message.contains(accepted),
            "the message lists the accepted kind `{accepted}`, got: {message}"
        );
    }
    assert!(
        store::assets::for_project(&fx.surreal, fx.project_id)
            .await
            .unwrap()
            .is_empty(),
        "a missing kind files no document"
    );
}

#[tokio::test]
async fn a_blank_kind_is_400() {
    let fx = build_fixture().await;
    let mut body = doc_body();
    body["kind"] = serde_json::json!("  ");
    let resp = upload(&fx, Some(&fx.lawyer), body).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"].as_str(), Some("kind_required"));
}

/// A value that is not a `Kind` at all is refused the same way — the door does
/// not silently coerce it to `unclassified`.
#[tokio::test]
async fn an_unknown_kind_is_400() {
    let fx = build_fixture().await;
    let mut body = doc_body();
    body["kind"] = serde_json::json!("not-a-kind");
    let resp = upload(&fx, Some(&fx.lawyer), body).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"].as_str(), Some("invalid_kind"));
}

/// The other half of the classification, and the one that would catch an
/// over-eager 400: every kind the asset lane accepts still files a document.
/// Driven from `rules::kind` itself, so widening the lane cannot leave this
/// test asserting a stale vocabulary.
#[tokio::test]
async fn every_accepted_kind_still_files_a_document() {
    let fx = build_fixture().await;
    for kind in rules::kind::Kind::ALL
        .iter()
        .filter(|k| k.valid_for(rules::kind::Lane::Asset))
    {
        let mut body = doc_body();
        body["kind"] = serde_json::json!(kind.as_str());
        body["slug"] = serde_json::json!(format!("examples/{}.txt", kind.as_str()));
        // Distinct bytes per kind so each is a fresh ingest rather than a dedup.
        body["content_base64"] = serde_json::json!(base64_of(kind.as_str()));
        let resp = upload(&fx, Some(&fx.lawyer), body).await;
        assert_eq!(
            resp.status(),
            StatusCode::CREATED,
            "`{}` is an asset-lane kind and must be accepted",
            kind.as_str()
        );
    }
}

async fn slugless_asset(fx: &Fixture, bytes: &[u8]) -> store::assets::Asset {
    let ingested = store::documents::ingest_bytes(
        &fx.surreal,
        &fx.storage,
        &store::documents::IngestArgs {
            project_id: fx.project_id,
            source: store::documents::source::UPLOAD,
            filename: "note.txt",
            kind: "unclassified",
            content_type: "text/plain",
            description: None,
            visibility: store::documents::visibility::INTERNAL,
            secondary_storage_key: None,
        },
        bytes,
    )
    .await
    .unwrap();
    store::assets::find_by_id(&fx.surreal, ingested.asset_id)
        .await
        .unwrap()
        .unwrap()
}

/// A lawyer sets a slug on a slugless row. A client is forbidden, a lawyer
/// outside the matter is not found, and a second slug is a conflict.
#[tokio::test]
async fn assigning_a_slug_is_lawyer_scoped_and_refuses_a_second_slug() {
    let fx = build_fixture().await;
    let asset = slugless_asset(&fx, b"slugless document").await;
    assert!(asset.slug.is_none());

    assert_eq!(
        patch_document(
            &fx,
            Some(&fx.client),
            asset.id,
            serde_json::json!({ "slug": "notes/note.txt" }),
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        patch_document(
            &fx,
            Some(&fx.outsider),
            asset.id,
            serde_json::json!({ "slug": "notes/note.txt" }),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );

    let set = patch_document(
        &fx,
        Some(&fx.lawyer),
        asset.id,
        serde_json::json!({ "slug": "notes/note.txt", "kind": "exhibit" }),
    )
    .await;
    assert_eq!(set.status(), StatusCode::OK);
    let stored = store::assets::find_by_id(&fx.surreal, asset.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.slug.as_deref(), Some("notes/note.txt"));
    assert_eq!(stored.kind.as_deref(), Some("exhibit"));
    assert_eq!(stored.sha256_hex, asset.sha256_hex);
    assert_eq!(stored.storage_key, asset.storage_key);

    let again = patch_document(
        &fx,
        Some(&fx.lawyer),
        asset.id,
        serde_json::json!({ "slug": "other/note.txt" }),
    )
    .await;
    assert_eq!(again.status(), StatusCode::CONFLICT);
    let body: serde_json::Value =
        serde_json::from_slice(&again.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["error"], "slug_present");
}

/// A lawyer replaces a legacy kind the asset lane rejects on a slugged row.
/// A client is forbidden, and an accepted kind is a conflict.
#[tokio::test]
async fn replacing_a_legacy_kind_is_lawyer_scoped_and_refuses_an_accepted_kind() {
    let fx = build_fixture().await;
    let asset = slugless_asset(&fx, b"legacy classification").await;
    fx.surreal
        .query("UPDATE $id SET slug = 'notes/note.txt', kind = 'Formation Filing'")
        .bind(("id", store::surreal::record_id("asset", asset.id)))
        .await
        .unwrap()
        .check()
        .unwrap();

    assert_eq!(
        patch_document(
            &fx,
            Some(&fx.client),
            asset.id,
            serde_json::json!({ "kind": "filing" }),
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );

    let set = patch_document(
        &fx,
        Some(&fx.lawyer),
        asset.id,
        serde_json::json!({ "kind": "filing" }),
    )
    .await;
    assert_eq!(set.status(), StatusCode::OK);
    let stored = store::assets::find_by_id(&fx.surreal, asset.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.kind.as_deref(), Some("filing"));
    assert_eq!(stored.slug.as_deref(), Some("notes/note.txt"));
    assert_eq!(stored.sha256_hex, asset.sha256_hex);

    let again = patch_document(
        &fx,
        Some(&fx.lawyer),
        asset.id,
        serde_json::json!({ "kind": "letter" }),
    )
    .await;
    assert_eq!(again.status(), StatusCode::CONFLICT);
    let body: serde_json::Value =
        serde_json::from_slice(&again.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["error"], "kind_accepted");
}

/// An upload records how an executed document was signed: the asset row
/// stores it, the returned pointer carries it, and the revisions door
/// reports it. A method outside the closed list files nothing.
#[tokio::test]
async fn an_upload_records_the_signing_method() {
    let fx = build_fixture().await;
    let mut body = doc_body();
    body["slug"] = serde_json::json!("onboarding/letter_signed.txt");
    body["filename"] = serde_json::json!("letter_signed.txt");
    body["signing"] = serde_json::json!({ "method": "wet_ink", "signed_on": "2026-09-04" });

    let filed = upload(&fx, Some(&fx.lawyer), body.clone()).await;
    assert_eq!(filed.status(), StatusCode::CREATED);
    let pointer: serde_json::Value =
        serde_json::from_slice(&filed.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(
        pointer["signing"],
        serde_json::json!({ "method": "wet_ink", "signed_on": "2026-09-04" })
    );
    let asset_id: Uuid = pointer["current_version"]["asset_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let stored = store::assets::find_by_id(&fx.surreal, asset_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.signing.map(|signing| signing.method),
        Some(store::documents::SigningMethod::WetInk)
    );

    let revisions = fx
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/app/api/projects/{}/documents/revisions?slug=onboarding/letter_signed.txt",
                    fx.project_id
                ))
                .header("authorization", &fx.lawyer)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revisions.status(), StatusCode::OK);
    let revisions: serde_json::Value =
        serde_json::from_slice(&revisions.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(
        revisions["revisions"][0]["signing"]["method"],
        serde_json::json!("wet_ink")
    );

    let mut bad = doc_body();
    bad["slug"] = serde_json::json!("onboarding/other.txt");
    bad["filename"] = serde_json::json!("other.txt");
    bad["content_base64"] = serde_json::json!(base64_of("another signed scan"));
    bad["signing"] = serde_json::json!({ "method": "notarized", "signed_on": "2026-09-04" });
    let refused = upload(&fx, Some(&fx.lawyer), bad).await;
    assert!(refused.status().is_client_error(), "{}", refused.status());
    assert!(
        store::assets::revisions(&fx.surreal, fx.project_id, "onboarding/other.txt")
            .await
            .unwrap()
            .is_empty()
    );
}

/// An upload records a packet's notarial acknowledgments on the asset and
/// its pointer. An entry with a bad state code is refused before any byte
/// is stored, and a notary's name is not a field at all.
#[tokio::test]
async fn an_upload_records_its_notarizations() {
    let fx = build_fixture().await;
    let entry = serde_json::json!({
        "method": "in_person",
        "acknowledged_on": "2026-09-04",
        "state": "NV",
        "county": "Clark",
        "commission_expires": "2029-01-31",
    });
    let mut body = doc_body();
    body["slug"] = serde_json::json!("estate/will_signed.txt");
    body["filename"] = serde_json::json!("will_signed.txt");
    body["notarization"] = serde_json::json!([entry]);

    let filed = upload(&fx, Some(&fx.lawyer), body).await;
    assert_eq!(filed.status(), StatusCode::CREATED);
    let pointer: serde_json::Value =
        serde_json::from_slice(&filed.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(pointer["notarization"], serde_json::json!([entry]));
    let asset_id: Uuid = pointer["current_version"]["asset_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let stored = store::assets::find_by_id(&fx.surreal, asset_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.notarization.len(), 1);
    assert_eq!(stored.notarization[0].county, "Clark");

    let mut bad_state = entry.clone();
    bad_state["state"] = serde_json::json!("Nevada");
    let mut named_notary = entry.clone();
    named_notary["notary_name"] = serde_json::json!("Someone");
    for (index, bad) in [bad_state, named_notary].into_iter().enumerate() {
        let slug = format!("estate/other-{index}.txt");
        let mut body = doc_body();
        body["slug"] = serde_json::json!(slug);
        body["filename"] = serde_json::json!(format!("other-{index}.txt"));
        body["content_base64"] = serde_json::json!(base64_of(&format!("another scan {index}")));
        body["notarization"] = serde_json::json!([bad]);
        let refused = upload(&fx, Some(&fx.lawyer), body).await;
        assert!(refused.status().is_client_error(), "{}", refused.status());
        assert!(store::assets::revisions(&fx.surreal, fx.project_id, &slug)
            .await
            .unwrap()
            .is_empty());
    }
}

async fn reclassify(
    fx: &Fixture,
    auth: Option<&str>,
    asset_id: Uuid,
    body: serde_json::Value,
) -> axum::http::Response<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!(
            "/app/api/projects/{}/documents/{asset_id}/reclassify",
            fx.project_id
        ))
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

async fn error_code(response: axum::http::Response<Body>) -> String {
    let body: serde_json::Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    body["error"].as_str().unwrap_or_default().to_string()
}

/// A lawyer corrects a chain filed under the wrong accepted kind. A client
/// is forbidden, anonymous is unauthorized, a lawyer outside the matter is
/// not found, `dry_run` changes nothing, the bytes and slug stay put, and a
/// same-kind or legacy-kind row is a conflict.
#[tokio::test]
async fn reclassifying_an_accepted_kind_is_lawyer_scoped_and_keeps_the_bytes() {
    let fx = build_fixture().await;
    let asset = slugless_asset(&fx, b"draft filed as a memo").await;
    fx.surreal
        .query("UPDATE $id SET slug = 'drafts/note.txt', kind = 'memo'")
        .bind(("id", store::surreal::record_id("asset", asset.id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let body = serde_json::json!({ "kind": "unclassified" });

    assert_eq!(
        reclassify(&fx, None, asset.id, body.clone()).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        reclassify(&fx, Some(&fx.client), asset.id, body.clone())
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        reclassify(&fx, Some(&fx.outsider), asset.id, body.clone())
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    let preview = reclassify(
        &fx,
        Some(&fx.lawyer),
        asset.id,
        serde_json::json!({ "kind": "unclassified", "dry_run": true }),
    )
    .await;
    assert_eq!(preview.status(), StatusCode::OK);
    let unchanged = store::assets::find_by_id(&fx.surreal, asset.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.kind.as_deref(), Some("memo"));

    let set = reclassify(&fx, Some(&fx.lawyer), asset.id, body.clone()).await;
    assert_eq!(set.status(), StatusCode::OK);
    let stored = store::assets::find_by_id(&fx.surreal, asset.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.kind.as_deref(), Some("unclassified"));
    assert_eq!(stored.slug.as_deref(), Some("drafts/note.txt"));
    assert_eq!(stored.sha256_hex, asset.sha256_hex);
    assert_eq!(stored.storage_key, asset.storage_key);
    assert_eq!(stored.byte_size, asset.byte_size);

    let again = reclassify(&fx, Some(&fx.admin), asset.id, body).await;
    assert_eq!(again.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(again).await, "kind_unchanged");

    let bad = reclassify(
        &fx,
        Some(&fx.lawyer),
        asset.id,
        serde_json::json!({ "kind": "review_queue_workbench" }),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(bad).await, "invalid_kind");

    fx.surreal
        .query("UPDATE $id SET kind = 'Formation Filing'")
        .bind(("id", store::surreal::record_id("asset", asset.id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let legacy = reclassify(
        &fx,
        Some(&fx.lawyer),
        asset.id,
        serde_json::json!({ "kind": "filing" }),
    )
    .await;
    assert_eq!(legacy.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(legacy).await, "kind_not_accepted");
}

async fn bare_slugged_asset(fx: &Fixture, bytes: &[u8], slug: &str) -> store::assets::Asset {
    let asset = slugless_asset(fx, bytes).await;
    fx.surreal
        .query("UPDATE $id SET slug = $slug")
        .bind(("id", store::surreal::record_id("asset", asset.id)))
        .bind(("slug", slug.to_string()))
        .await
        .unwrap()
        .check()
        .unwrap();
    asset
}

/// LAW-81: a lawyer moves a bare legacy slug (no extension of its own) to
/// one that carries the filename's extension, across the whole revision
/// chain. A client is forbidden, a lawyer outside the matter is not found,
/// and `dry_run` changes nothing.
#[tokio::test]
async fn reslugging_is_lawyer_scoped_and_moves_a_bare_slug_without_touching_bytes() {
    let fx = build_fixture().await;
    let asset = bare_slugged_asset(&fx, b"legacy bare slug", "summons-wendell-prine").await;

    assert_eq!(
        patch_document(
            &fx,
            Some(&fx.client),
            asset.id,
            serde_json::json!({ "slug": "summons-wendell-prine.txt", "reslug": true }),
        )
        .await
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        patch_document(
            &fx,
            Some(&fx.outsider),
            asset.id,
            serde_json::json!({ "slug": "summons-wendell-prine.txt", "reslug": true }),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );

    let preview = patch_document(
        &fx,
        Some(&fx.lawyer),
        asset.id,
        serde_json::json!({ "slug": "summons-wendell-prine.txt", "reslug": true, "dry_run": true }),
    )
    .await;
    assert_eq!(preview.status(), StatusCode::OK);
    let unchanged = store::assets::find_by_id(&fx.surreal, asset.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        unchanged.slug.as_deref(),
        Some("summons-wendell-prine"),
        "dry_run must not write"
    );

    let moved = patch_document(
        &fx,
        Some(&fx.lawyer),
        asset.id,
        serde_json::json!({ "slug": "summons-wendell-prine.txt", "reslug": true }),
    )
    .await;
    assert_eq!(moved.status(), StatusCode::OK);
    let stored = store::assets::find_by_id(&fx.surreal, asset.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.slug.as_deref(), Some("summons-wendell-prine.txt"));
    assert_eq!(stored.sha256_hex, asset.sha256_hex);
    assert_eq!(stored.storage_key, asset.storage_key);
    assert_eq!(stored.kind, asset.kind);
}

async fn json_body(response: axum::http::Response<Body>) -> serde_json::Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

/// A complete, valid identity moves to another valid identity — the filing
/// status folders — across its whole revision chain. Hashes, storage keys,
/// kinds, and the revision order survive; `dry_run` writes nothing.
#[tokio::test]
async fn reslug_moves_a_valid_identity_and_keeps_its_revision_chain() {
    let fx = build_fixture().await;
    upload(&fx, Some(&fx.lawyer), doc_body()).await;
    let mut revised = doc_body();
    revised["content_base64"] = serde_json::json!(base64_of("revised document"));
    let second = json_body(upload(&fx, Some(&fx.lawyer), revised).await).await;
    let latest_id: Uuid = second["current_version"]["asset_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let before = store::assets::revisions(&fx.surreal, fx.project_id, "notes/note.txt")
        .await
        .unwrap();
    assert_eq!(before.len(), 2);
    let target = "filed/forms/note.txt";

    let preview = patch_document(
        &fx,
        Some(&fx.lawyer),
        latest_id,
        serde_json::json!({ "slug": target, "reslug": true, "dry_run": true }),
    )
    .await;
    assert_eq!(preview.status(), StatusCode::OK);
    let preview = json_body(preview).await;
    assert_eq!(preview["slug"], target);
    assert_eq!(preview["previous_slug"], "notes/note.txt");
    assert_eq!(preview["dry_run"], true);
    let untouched = store::assets::revisions(&fx.surreal, fx.project_id, "notes/note.txt")
        .await
        .unwrap();
    assert_eq!(untouched.len(), 2, "dry_run must not write");
    assert!(store::assets::revisions(&fx.surreal, fx.project_id, target)
        .await
        .unwrap()
        .is_empty());

    let moved = patch_document(
        &fx,
        Some(&fx.lawyer),
        latest_id,
        serde_json::json!({ "slug": target, "reslug": true }),
    )
    .await;
    assert_eq!(moved.status(), StatusCode::OK);
    assert!(
        store::assets::revisions(&fx.surreal, fx.project_id, "notes/note.txt")
            .await
            .unwrap()
            .is_empty()
    );
    let after = store::assets::revisions(&fx.surreal, fx.project_id, target)
        .await
        .unwrap();
    assert_eq!(after.len(), before.len());
    for old in &before {
        let new = after.iter().find(|row| row.id == old.id).unwrap();
        assert_eq!(new.sha256_hex, old.sha256_hex);
        assert_eq!(new.storage_key, old.storage_key);
        assert_eq!(new.byte_size, old.byte_size);
        assert_eq!(new.kind, old.kind);
    }
    assert_eq!(
        after.iter().map(|row| row.id).collect::<Vec<_>>(),
        before.iter().map(|row| row.id).collect::<Vec<_>>(),
        "the revision order is the same chain"
    );
}

/// A target that already names a chain, and a target equal to the current
/// slug, are refused; both leave the rows unchanged.
#[tokio::test]
async fn reslug_refuses_an_occupied_target_and_a_no_op_move() {
    let fx = build_fixture().await;
    let valid = bare_slugged_asset(&fx, b"already a valid identity", "note.txt").await;
    let occupied = bare_slugged_asset(&fx, b"a different document", "other.txt").await;

    let taken = patch_document(
        &fx,
        Some(&fx.lawyer),
        occupied.id,
        serde_json::json!({ "slug": "note.txt", "reslug": true }),
    )
    .await;
    assert_eq!(taken.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(taken).await["error"], "slug_taken");
    let stored_occupied = store::assets::find_by_id(&fx.surreal, occupied.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored_occupied.slug.as_deref(), Some("other.txt"));

    let same = patch_document(
        &fx,
        Some(&fx.lawyer),
        valid.id,
        serde_json::json!({ "slug": "note.txt", "reslug": true }),
    )
    .await;
    assert_eq!(same.status(), StatusCode::CONFLICT);
    assert_eq!(json_body(same).await["error"], "slug_unchanged");
}

/// A target that is not a valid identity for the row's filename is refused
/// even when the current slug is valid.
#[tokio::test]
async fn reslug_refuses_an_invalid_target_for_a_valid_identity() {
    let fx = build_fixture().await;
    let valid = bare_slugged_asset(&fx, b"valid identity", "note.txt").await;
    let response = patch_document(
        &fx,
        Some(&fx.lawyer),
        valid.id,
        serde_json::json!({ "slug": "filed/forms/note.pdf", "reslug": true }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(json_body(response).await["error"], "invalid_slug");
}

/// A row belonging to another matter is refused even when the caller can see
/// that other matter — the store's own scope check, not only the door's.
#[tokio::test]
async fn reslug_refuses_an_asset_scoped_to_another_project() {
    let fx = build_fixture().await;
    let asset = slugless_asset(&fx, b"scoped to the first matter").await;
    fx.surreal
        .query("UPDATE $id SET slug = 'note'")
        .bind(("id", store::surreal::record_id("asset", asset.id)))
        .await
        .unwrap()
        .check()
        .unwrap();

    let other_project = store::projects::create(
        &fx.surreal,
        &store::projects::NewProject {
            code: format!("matter-{}", Uuid::now_v7()),
            name: "Other Matter".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&fx.surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let other_lawyer = store::persons::create(
        &fx.surreal,
        &store::persons::NewPerson::with_role(
            "Other Lawyer",
            "other-lawyer@example.com",
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::projects::add_participation(&fx.surreal, other_project.id, other_lawyer.id, "lawyer")
        .await
        .unwrap();

    let req = Request::builder()
        .method("PATCH")
        .uri(format!(
            "/app/api/projects/{}/documents/{}",
            other_project.id, asset.id
        ))
        .header("content-type", "application/json")
        .header("authorization", bearer(other_lawyer.id, Role::Lawyer))
        .body(Body::from(
            serde_json::json!({ "slug": "note.txt", "reslug": true }).to_string(),
        ))
        .unwrap();
    let resp = fx.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let stored = store::assets::find_by_id(&fx.surreal, asset.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.slug.as_deref(), Some("note"));
}

/// Storage repair is admin-only. A lawyer is forbidden. With no sibling the
/// row is unchanged.
#[tokio::test]
async fn repairing_storage_is_admin_only_and_refuses_without_a_sibling() {
    let fx = build_fixture().await;
    let asset = slugless_asset(&fx, b"only copy").await;
    fx.storage.delete(&asset.storage_key).await.unwrap();

    assert_eq!(
        repair_storage(&fx, Some(&fx.lawyer), asset.id, false)
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let refused = repair_storage(&fx, Some(&fx.admin), asset.id, false).await;
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let body: serde_json::Value =
        serde_json::from_slice(&refused.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["error"], "no_sibling");
    let stored = store::assets::find_by_id(&fx.surreal, asset.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.storage_key, asset.storage_key);
    assert!(fx.storage.head(&asset.storage_key).await.unwrap().is_none());
}

/// Minimal base64 encoder for the fixture bytes — the test needs distinct
/// content per kind, not a dependency.
fn base64_of(text: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
