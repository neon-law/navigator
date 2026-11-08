//! REST document-upload size boundary tests.

use axum::body::Body;
use axum::http::{header, Request};
use axum::Router;
use base64::Engine as _;
use portal::api::ApiState;
use serde_json::{json, Value};
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;

// These tests exercise the route in-process, so no external matter data is used.

struct Fixture {
    app: Router,
    project_id: uuid::Uuid,
    session: portal::SessionData,
    surreal: store::surreal::SurrealDb,
    storage: std::sync::Arc<dyn cloud::StorageService>,
}

async fn fixture(code: &str) -> Fixture {
    let db = mem_surreal().await;
    let project_id = store::test_support::seed_project_surreal(&db, code).await;
    let lawyer = store::test_support::ensure_person(
        &db,
        &NewPerson {
            name: "Synthetic Lawyer".into(),
            email: "synthetic-lawyer@example.com".into(),
            role: Role::Lawyer,
            ..Default::default()
        },
    )
    .await;
    store::projects::designate_dri_in_surreal(
        &db,
        project_id,
        lawyer.id,
        store::projects::DriSide::Lawyer,
    )
    .await
    .expect("seed the synthetic lawyer's participation row");

    let state = portal::test_support::app_state(db).await;
    let api_state = ApiState {
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
    };
    let session = portal::SessionData {
        person_id: Some(lawyer.id),
        ..portal::SessionData::fresh("synthetic-lawyer-sub", Role::Lawyer)
    };

    Fixture {
        app: portal::api::routes().with_state(api_state),
        project_id,
        session,
        surreal: state.surreal.clone(),
        storage: state.storage.clone(),
    }
}

async fn post_transcript(
    fixture: &Fixture,
    source_id: uuid::Uuid,
    source_sha: &str,
    bytes: &[u8],
    quality: Option<&str>,
    link: bool,
) {
    let mut body = json!({
        "filename": "order.transcript.md",
        "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
        "content_type": "text/markdown",
        "kind": "transcript",
        "slug": "transcripts/order.transcript.md",
        "visibility": "internal",
    });
    if let Some(quality) = quality {
        body["transcript_quality"] = json!(quality);
    }
    if link {
        body["derived_from"] = json!({
            "document_id": source_id,
            "version": 1,
            "sha256": source_sha,
        });
    }
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/app/api/projects/{}/documents",
                    fixture.project_id
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .extension(fixture.session.clone())
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success());
}

#[tokio::test]
async fn transcript_upload_records_source_link_and_proofread_revision_inherits_it() {
    let fixture = fixture("transcript-provenance").await;
    let source_slug = "pleadings/order.pdf";
    let source_bytes = b"synthetic source PDF bytes";
    let source_args = store::documents::IngestArgs {
        project_id: fixture.project_id,
        source: store::documents::source::UPLOAD,
        filename: "order.pdf",
        kind: "filing",
        content_type: "application/pdf",
        description: None,
        secondary_storage_key: None,
        visibility: store::documents::visibility::INTERNAL,
    };
    let source = store::assets::file_revision(
        &fixture.surreal,
        &fixture.storage,
        &source_args,
        &store::documents::DocumentIdentity {
            slug: Some(source_slug),
            ..Default::default()
        },
        source_bytes,
    )
    .await
    .expect("file synthetic source revision");
    let source_id = match source {
        store::assets::Filed::Revision(asset) => asset.asset_id,
        store::assets::Filed::Unchanged { asset_id } => asset_id,
    };
    let source_sha = store::assets::sha256_hex(source_bytes);

    post_transcript(
        &fixture,
        source_id,
        &source_sha,
        b"machine text",
        Some("machine"),
        true,
    )
    .await;
    post_transcript(
        &fixture,
        source_id,
        &source_sha,
        b"proofread text",
        Some("proofread"),
        false,
    )
    .await;
    let revisions = store::assets::revisions(
        &fixture.surreal,
        fixture.project_id,
        "transcripts/order.transcript.md",
    )
    .await
    .expect("read transcript revisions");
    assert_eq!(revisions.len(), 2);
    assert_eq!(
        revisions[0].transcript_quality.as_deref(),
        Some("proofread")
    );
    assert_eq!(revisions[1].transcript_quality.as_deref(), Some("machine"));
    assert_eq!(
        revisions[0]
            .derived_from
            .as_ref()
            .and_then(|value| value["document_id"].as_str()),
        Some(source_id.to_string().as_str())
    );
    assert_eq!(
        revisions[0]
            .derived_from
            .as_ref()
            .and_then(|value| value["sha256"].as_str()),
        Some(source_sha.as_str())
    );
}

fn request_body(bytes: &[u8]) -> Vec<u8> {
    request_body_with_slug(bytes, None)
}

fn request_body_with_slug(bytes: &[u8], slug: Option<&str>) -> Vec<u8> {
    request_body_of_kind(bytes, slug, "unclassified")
}

fn request_body_of_kind(bytes: &[u8], slug: Option<&str>, kind: &str) -> Vec<u8> {
    // The door requires the slug to carry the source filename's extension
    // (its Pointer representation), so the synthetic filename follows the
    // slug's basename; a slugless body keeps the original shape.
    let filename = match slug {
        Some(s) => s.rsplit('/').next().unwrap_or(s).to_string(),
        None => "synthetic-document.bin".to_string(),
    };
    request_body_named(bytes, slug, kind, &filename)
}

fn request_body_named(bytes: &[u8], slug: Option<&str>, kind: &str, filename: &str) -> Vec<u8> {
    let content_type = if filename.to_ascii_lowercase().ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    };
    let mut body = serde_json::json!({
        "filename": filename,
        "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
        "content_type": content_type,
        "kind": kind,
        "visibility": "internal"
    });
    if let Some(slug) = slug {
        body["slug"] = json!(slug);
    }
    serde_json::to_vec(&body).expect("serialize synthetic upload")
}

/// The buffered door with the conditional append's `expected_head` field
/// (LAW-284); `None` posts exactly the pre-LAW-284 body.
async fn upload_with_expected(
    fixture: &Fixture,
    bytes: &[u8],
    slug: &str,
    expected_head: Option<Value>,
) -> Value {
    let mut body: Value =
        serde_json::from_slice(&request_body_with_slug(bytes, Some(slug))).unwrap();
    if let Some(expected_head) = expected_head {
        body["expected_head"] = expected_head;
    }
    post_upload(fixture, serde_json::to_vec(&body).unwrap()).await
}

/// One buffered POST of pre-serialized request bytes through the router.
async fn post_upload(fixture: &Fixture, request_bytes: Vec<u8>) -> Value {
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/app/api/projects/{}/documents",
                    fixture.project_id
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .extension(fixture.session.clone())
                .body(Body::from(request_bytes))
                .expect("build upload"),
        )
        .await
        .expect("run upload");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read upload response");
    let json: Value = serde_json::from_slice(&body).expect("upload response is JSON");
    json!({ "status": status.as_u16(), "body": json })
}

/// The live head's `(asset_id, version, sha256)` triple as the wire sees
/// it: newest first, the head's one-based version is the chain's length.
async fn expected_head_of(fixture: &Fixture, slug: &str) -> Value {
    let chain = store::assets::revisions(&fixture.surreal, fixture.project_id, slug)
        .await
        .expect("read the live chain");
    let head = chain.first().cloned().expect("the chain has a head");
    json!({
        "asset_id": head.id,
        "version": chain.len(),
        "sha256": head.sha256_hex,
    })
}

async fn upload(fixture: &Fixture, bytes: &[u8], content_length: Option<&str>) -> Value {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!(
            "/app/api/projects/{}/documents",
            fixture.project_id
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .extension(fixture.session.clone());
    if let Some(content_length) = content_length {
        builder = builder.header(header::CONTENT_LENGTH, content_length);
    }
    let response = fixture
        .app
        .clone()
        .oneshot(
            builder
                .body(Body::from(request_body(bytes)))
                .expect("build upload"),
        )
        .await
        .expect("run upload");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read upload response");
    let json: Value = serde_json::from_slice(&body).expect("upload response is JSON");
    json!({ "status": status.as_u16(), "body": json })
}

#[tokio::test]
async fn accepts_a_document_well_over_the_framework_default() {
    let fixture = fixture("upload-limit-accepted").await;
    let bytes = vec![b'x'; 2 * 1024 * 1024];
    let result = upload(&fixture, &bytes, None).await;

    assert_eq!(result["status"], 201);
    assert_eq!(result["body"]["current_version"]["size_bytes"], bytes.len());
}

#[tokio::test]
async fn accepts_court_hearing_audio_over_the_general_limit_as_a_transcript() {
    let fixture = fixture("upload-limit-transcript").await;
    let bytes = vec![b'x'; store::documents::MAX_DOCUMENT_UPLOAD_BYTES + 1];
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/app/api/projects/{}/documents",
                    fixture.project_id
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .extension(fixture.session.clone())
                .body(Body::from(request_body_of_kind(&bytes, None, "transcript")))
                .expect("build upload"),
        )
        .await
        .expect("run upload");

    assert_eq!(response.status().as_u16(), 201);
}

#[tokio::test]
async fn refuses_a_document_over_the_limit_with_both_sizes_for_lied_and_missing_lengths() {
    let fixture = fixture("upload-limit-refused").await;
    let actual = store::documents::MAX_DOCUMENT_UPLOAD_BYTES + 1;
    let bytes = vec![b'x'; actual];
    for result in [
        upload(&fixture, &bytes, Some("1")).await,
        upload(&fixture, &bytes, None).await,
    ] {
        let message = result["body"]["message"].as_str().expect("size message");

        assert_eq!(result["status"], 400);
        assert_eq!(result["body"]["error"], "document_too_large");
        assert!(message.contains(&store::documents::MAX_DOCUMENT_UPLOAD_BYTES.to_string()));
        assert!(message.contains(&actual.to_string()));
        assert!(!message.contains("compress"));
        assert!(!message.contains("re-render"));
    }
}

#[tokio::test]
async fn refuses_a_slug_that_cannot_be_represented_by_a_pointer_path() {
    let fixture = fixture("slug-pointer-shape").await;
    let request = request_body_named(
        b"synthetic document",
        Some("dkt-001-complaint"),
        "unclassified",
        "synthetic-document.bin",
    );
    let result = post_upload(&fixture, request).await;

    assert_eq!(result["status"], 400);
    assert_eq!(result["body"]["error"], "invalid_slug");
    assert!(result["body"]["message"]
        .as_str()
        .expect("slug message")
        .contains("synthetic-document.bin"));
}

/// One POST of pre-serialized JSON to a Project documents path, through the
/// session extension. The caller owns the status assertions.
async fn post_documents_json(
    fixture: &Fixture,
    path_suffix: &str,
    request_bytes: Vec<u8>,
) -> (axum::http::StatusCode, Value) {
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/app/api/projects/{}/{}",
                    fixture.project_id, path_suffix
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .extension(fixture.session.clone())
                .body(Body::from(request_bytes))
                .expect("build request"),
        )
        .await
        .expect("issue the request");
    let status = response.status();
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response"),
    )
    .expect("response is JSON");
    (status, body)
}

/// One streaming upload end to end through the router: open, one chunk,
/// complete. The complete call carries `expected_head` when given (LAW-284).
async fn stream_bytes(
    fixture: &Fixture,
    slug: &str,
    bytes: &[u8],
    expected_head: Option<Value>,
) -> Value {
    // The door requires the slug to carry the source filename's extension,
    // so the streaming open names the slug's basename.
    let filename = slug.rsplit('/').next().unwrap_or(slug).to_string();
    let content_type = if filename.to_ascii_lowercase().ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    };
    let open_body = json!({
        "filename": filename,
        "content_type": content_type,
        "kind": "unclassified",
        "visibility": "internal",
        "slug": slug,
        "declared_size": bytes.len(),
        "declared_sha256": store::documents::sha256_hex(bytes),
    });
    let (status, session) = post_documents_json(
        fixture,
        "documents/upload-session",
        serde_json::to_vec(&open_body).unwrap(),
    )
    .await;
    assert!(status.is_success(), "the session opened: {status}");
    let session_id = session["id"].as_str().expect("session id").to_string();
    assert!(
        session["chunk_size"].as_i64().unwrap() > i64::try_from(bytes.len()).unwrap(),
        "the fixture fits one server chunk"
    );

    let chunk_body = json!({
        "session_id": session_id,
        "offset": 0,
        "content_base64": base64::engine::general_purpose::STANDARD.encode(bytes),
    });
    let (status, _) = post_documents_json(
        fixture,
        "documents/upload-session/chunk",
        serde_json::to_vec(&chunk_body).unwrap(),
    )
    .await;
    assert!(status.is_success(), "the chunk stuck");

    let mut complete_body = json!({ "session_id": session_id });
    if let Some(expected_head) = expected_head {
        complete_body["expected_head"] = expected_head;
    }
    let (status, body) = post_documents_json(
        fixture,
        "documents/upload-session/complete",
        serde_json::to_vec(&complete_body).unwrap(),
    )
    .await;
    json!({ "status": status.as_u16(), "body": body })
}

/// LAW-284 back-compat: a buffered door call with no `expected_head` field
/// behaves exactly as before — `201` for a new revision, `200` for an
/// identical retry, and no conditional refusal of any kind.
#[tokio::test]
async fn a_buffered_upload_without_an_expected_head_behaves_as_before() {
    let fixture = fixture("cas-backcompat-buffered").await;
    let first = upload_with_expected(&fixture, b"plain bytes", "plain.pdf", None).await;
    assert_eq!(first["status"], 201, "{first}");
    let retry = upload_with_expected(&fixture, b"plain bytes", "plain.pdf", None).await;
    assert_eq!(
        retry["status"], 200,
        "the identical retry is unchanged: {retry}"
    );
    let next = upload_with_expected(&fixture, b"other bytes", "plain.pdf", None).await;
    assert_eq!(next["status"], 201, "different bytes still append: {next}");

    let chain = store::assets::revisions(&fixture.surreal, fixture.project_id, "plain.pdf")
        .await
        .expect("read the chain");
    assert_eq!(chain.len(), 2, "exactly the two appended revisions");
}

/// LAW-284: a buffered conditional append whose expected head the live
/// chain no longer names is refused with `409 head_conflict` naming the
/// live head's triple — and inserts nothing.
#[tokio::test]
async fn a_conditional_buffered_upload_refuses_a_stale_head_and_names_the_live_one() {
    let fixture = fixture("cas-buffered-stale").await;
    let first = upload_with_expected(&fixture, b"v1", "brief.pdf", None).await;
    assert_eq!(first["status"], 201, "{first}");
    let stale = expected_head_of(&fixture, "brief.pdf").await;

    // The concurrent writer this PR races against: a different upload
    // extends the chain between my read and my conditional append.
    let concurrent = upload_with_expected(&fixture, b"v2", "brief.pdf", None).await;
    assert_eq!(concurrent["status"], 201, "{concurrent}");

    let refused = upload_with_expected(&fixture, b"v3", "brief.pdf", Some(stale)).await;
    assert_eq!(refused["status"], 409, "{refused}");
    assert_eq!(refused["body"]["error"], "head_conflict");
    let current = &refused["body"]["current"];
    let chain = store::assets::revisions(&fixture.surreal, fixture.project_id, "brief.pdf")
        .await
        .expect("read the chain");
    assert_eq!(current["asset_id"], chain[0].id.to_string(), "{refused}");
    assert_eq!(current["version"], 2, "{refused}");
    assert_eq!(current["sha256"], chain[0].sha256_hex, "{refused}");
    assert_eq!(chain.len(), 2, "the refusal inserted nothing");
}

/// LAW-284: a buffered conditional append whose expected head still names
/// the live head files normally (`201`), and one whose bytes the live head
/// already holds — the stale-precondition retry of an ambiguous upload —
/// resolves to the unchanged pointer without a second row.
#[tokio::test]
async fn a_conditional_buffered_upload_against_the_live_head_files_and_dedupes() {
    let fixture = fixture("cas-buffered-live").await;
    let first = upload_with_expected(&fixture, b"v1", "brief.pdf", None).await;
    assert_eq!(first["status"], 201, "{first}");
    let expected = expected_head_of(&fixture, "brief.pdf").await;

    let second = upload_with_expected(&fixture, b"v2", "brief.pdf", Some(expected.clone())).await;
    assert_eq!(second["status"], 201, "{second}");

    // The retry that lost the race: v2's bytes, v1's stale precondition.
    let stale = json!({
        "asset_id": first["body"]["current_version"]["asset_id"],
        "version": 1,
        "sha256": store::assets::sha256_hex(b"v1"),
    });
    let retry = upload_with_expected(&fixture, b"v2", "brief.pdf", Some(stale)).await;
    assert_eq!(
        retry["status"], 200,
        "same bytes resolve unchanged: {retry}"
    );

    let chain = store::assets::revisions(&fixture.surreal, fixture.project_id, "brief.pdf")
        .await
        .expect("read the chain");
    assert_eq!(chain.len(), 2);
    assert_eq!(
        chain
            .iter()
            .filter(|asset| asset.sha256_hex == store::assets::sha256_hex(b"v2"))
            .count(),
        1,
        "no duplicate digest row"
    );
}

/// LAW-284: a malformed expected head — a version that names no revision,
/// or a digest that cannot match — is the caller's mistake, refused `400`
/// before any read or insert.
#[tokio::test]
async fn a_malformed_expected_head_is_a_bad_request() {
    let fixture = fixture("cas-buffered-malformed").await;
    for expected in [
        json!({
            "asset_id": uuid::Uuid::now_v7(),
            "version": 0,
            "sha256": store::assets::sha256_hex(b"v1"),
        }),
        json!({
            "asset_id": uuid::Uuid::now_v7(),
            "version": 1,
            "sha256": "not-a-sha",
        }),
    ] {
        let refused =
            upload_with_expected(&fixture, b"v1", "brief.pdf", Some(expected.clone())).await;
        assert_eq!(refused["status"], 400, "{expected}");
        assert_eq!(
            refused["body"]["error"], "invalid_expected_head",
            "{expected}"
        );
    }
    let chain = store::assets::revisions(&fixture.surreal, fixture.project_id, "brief.pdf")
        .await
        .expect("read the chain");
    assert!(chain.is_empty(), "nothing was inserted");
}

/// LAW-284 back-compat: a streaming completion with no `expected_head`
/// field behaves exactly as before — `201`, one filed revision.
#[tokio::test]
async fn a_streaming_completion_without_an_expected_head_behaves_as_before() {
    let fixture = fixture("cas-backcompat-streaming").await;
    let filed = stream_bytes(&fixture, "hearing.mp3", b"streamed bytes", None).await;
    assert_eq!(filed["status"], 201, "{filed}");
    let chain = store::assets::revisions(&fixture.surreal, fixture.project_id, "hearing.mp3")
        .await
        .expect("read the chain");
    assert_eq!(chain.len(), 1);
}

/// LAW-284: the streaming completion is the filing moment. A stale
/// expected head is refused `409 head_conflict` naming the live head with
/// the session kept — so re-completing against the adopted triple, the
/// restart a client performs, files normally.
#[tokio::test]
async fn a_streaming_completion_refuses_a_stale_head_and_the_restart_files() {
    let fixture = fixture("cas-streaming-stale").await;
    // A first revision on the slug, then the head the streamer read.
    let first = upload_with_expected(&fixture, b"v1", "hearing.mp3", None).await;
    assert_eq!(first["status"], 201, "{first}");
    let stale = expected_head_of(&fixture, "hearing.mp3").await;

    // The concurrent writer extends the chain while the stream runs.
    let concurrent = upload_with_expected(&fixture, b"v2", "hearing.mp3", None).await;
    assert_eq!(concurrent["status"], 201, "{concurrent}");

    let refused = stream_bytes(&fixture, "hearing.mp3", b"v3 streamed", Some(stale.clone())).await;
    assert_eq!(refused["status"], 409, "{refused}");
    assert_eq!(refused["body"]["error"], "head_conflict");
    let chain = store::assets::revisions(&fixture.surreal, fixture.project_id, "hearing.mp3")
        .await
        .expect("read the chain");
    assert_eq!(
        refused["body"]["current"]["asset_id"],
        chain[0].id.to_string()
    );
    assert_eq!(refused["body"]["current"]["version"], 2);
    assert_eq!(chain.len(), 2, "the refusal filed nothing");

    // The restart: the same session re-completes against the head the
    // conflict named — the session row and its staged bytes survived.
    let restarted = stream_bytes(
        &fixture,
        "hearing.mp3",
        b"v3 streamed",
        Some(refused["body"]["current"].clone()),
    )
    .await;
    assert_eq!(restarted["status"], 201, "the restart files: {restarted}");
    let chain = store::assets::revisions(&fixture.surreal, fixture.project_id, "hearing.mp3")
        .await
        .expect("read the chain");
    assert_eq!(chain.len(), 3);
    assert_eq!(
        chain[0].sha256_hex,
        store::assets::sha256_hex(b"v3 streamed")
    );
}
