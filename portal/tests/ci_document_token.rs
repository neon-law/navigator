//! Router-level least-privilege tests for the minted document-verification CI credential.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, Response, StatusCode};
use axum::Router;
use portal::github_oidc::{GitHubActionsClaims, GitHubOidc};
use portal::session::{now_unix_secs, SessionSource, SessionStore};
use portal::test_support::TEST_SESSION_KEY;
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;

struct Fixture {
    app: Router,
    project_id: Uuid,
    other_project_id: Uuid,
    lawyer_id: Uuid,
}

async fn record_surface_coordinates(surreal: &store::surreal::SurrealDb, project_id: Uuid) {
    store::projects::set_repository_url(
        surreal,
        project_id,
        Some("https://github.com/neon-law-staging/acme"),
    )
    .await
    .unwrap();
    store::projects::set_internal_slack_channel_url(
        surreal,
        project_id,
        "https://slack.example/archives/C0SYNTHETIC",
    )
    .await
    .unwrap();
    store::projects::set_private_notion_page_url(
        surreal,
        project_id,
        "https://notion.example/sample-page",
    )
    .await
    .unwrap();
}

#[expect(
    clippy::too_many_lines,
    reason = "synthetic tenancy fixture keeps the complete authorization scenario visible"
)]
async fn fixture() -> Fixture {
    let surreal = mem_surreal().await;
    let project_id = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "acme".into(),
            name: "Acme".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id;
    let other_project_id = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "widgets".into(),
            name: "Widgets".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id;
    record_surface_coordinates(&surreal, project_id).await;
    let lawyer = store::persons::create(
        &surreal,
        &NewPerson::with_role("Synthetic Lawyer", "ci-lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    for project in [project_id, other_project_id] {
        store::projects::designate_dri_in_surreal(
            &surreal,
            project,
            lawyer.id,
            store::projects::DriSide::Lawyer,
        )
        .await
        .unwrap();
    }

    let mut state = portal::test_support::app_state(surreal.clone()).await;
    let args = store::documents::IngestArgs {
        project_id,
        source: store::documents::source::UPLOAD,
        filename: "agreement.pdf",
        kind: "unclassified",
        content_type: "application/pdf",
        description: None,
        visibility: store::documents::visibility::INTERNAL,
        secondary_storage_key: None,
    };
    let identity = store::documents::DocumentIdentity {
        slug: Some("agreement"),
        ..Default::default()
    };
    store::assets::file_revision(
        &surreal,
        &state.storage,
        &args,
        &identity,
        b"synthetic bytes",
    )
    .await
    .unwrap();
    for project in [project_id, other_project_id] {
        let case = store::cases::open_case(
            &surreal,
            &store::cases::NewCase {
                project_id: project,
                caption: "Synthetic private caption",
                forum: None,
                jurisdiction: None,
                docket_number: None,
                judge: None,
                posture: "plaintiff",
            },
        )
        .await
        .unwrap();
        store::cases::record_entry(
            &surreal,
            &store::cases::NewDocketEntry {
                case_id: case.id,
                entry_number: "1",
                kind: store::cases::EntryKind::Pleading,
                title: "Synthetic private docket title",
                party: None,
                filed_or_served_on: None,
                scheduled_on: None,
                supersedes: None,
                document_asset_id: None,
                notation_id: None,
            },
        )
        .await
        .unwrap();
        for complete in [false, true] {
            let deadline = store::project_deadlines::create(
                &surreal,
                &store::project_deadlines::NewDeadline {
                    title: None,
                    project_id: project,
                    due_at: "2026-11-02T17:00:00-08:00".into(),
                    authority_kind: "internal".into(),
                    authority: "Synthetic authority".into(),
                    source: "Synthetic private deadline".into(),
                    source_document_id: None,
                    owner_person_id: None,
                    calendar_role: "firm".into(),
                    internal_lead_days: None,
                    client_lead_days: None,
                    replay_key: None,
                },
            )
            .await
            .unwrap();
            if complete {
                store::project_deadlines::complete(&surreal, project, deadline.id)
                    .await
                    .unwrap();
            }
        }
    }
    state.canonical_host = portal::CanonicalHost::new(Some("staging.neonlaw.com".into()));
    state.github_oidc = GitHubOidc::fixed(GitHubActionsClaims {
        sub: "repo:neon-law-staging/acme:ref:refs/heads/main".into(),
        repository: "neon-law-staging/acme".into(),
        repository_owner: "neon-law-staging".into(),
        git_ref: "refs/heads/main".into(),
        event_name: "push".into(),
        jti: "jti-document-acme".into(),
        exp: 4_000_000_000,
        ..Default::default()
    });

    Fixture {
        app: portal::router(state),
        project_id,
        other_project_id,
        lawyer_id: lawyer.id,
    }
}

async fn mint(app: &Router) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/ci/document-token")
                .header(header::HOST, "staging.neonlaw.com")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"token":"github-jwt"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    json_response(response).await
}

async fn json_response(response: Response<Body>) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&body).unwrap_or_else(
        |_| serde_json::json!({"body": String::from_utf8_lossy(&body).to_string()}),
    );
    (status, json)
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

async fn request(
    app: &Router,
    method: &str,
    uri: &str,
    authorization: Option<&str>,
    cookie: Option<&str>,
    body: Body,
) -> (StatusCode, serde_json::Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "staging.neonlaw.com");
    if let Some(authorization) = authorization {
        builder = builder.header(header::AUTHORIZATION, authorization);
    }
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, format!("navigator_session={cookie}"));
    }
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    json_response(response).await
}

#[tokio::test]
async fn a_minted_document_token_allows_only_its_project_metadata_for_bearer_and_cookie() {
    let fixture = fixture().await;
    let (status, minted) = mint(&fixture.app).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(minted["project_code"], "acme");
    let token = minted["token"].as_str().unwrap();

    for credential in [Some(bearer(token)), None] {
        let cookie = credential.is_none().then_some(token);
        let authorization = credential.as_deref();
        let (status, projects) = request(
            &fixture.app,
            "GET",
            "/app/api/projects",
            authorization,
            cookie,
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(projects.as_array().unwrap().len(), 1);
        assert_eq!(projects[0]["code"], "acme");
        assert_eq!(projects[0]["status"], "open");
        assert_eq!(
            projects[0]["repository_url"],
            "https://github.com/neon-law-staging/acme"
        );
        // `id`, `code`, `status`, `repository_url` — the four fields the live
        // gate (ENG-842) and the document verifier need, and nothing more:
        // `name`, `brand`, `entity_id`, `description`, and the Slack/Notion
        // URLs stay off this response.
        assert_eq!(projects[0].as_object().unwrap().len(), 4);

        let (status, revisions) = request(
            &fixture.app,
            "GET",
            &format!(
                "/app/api/projects/{}/documents/revisions?slug=agreement",
                fixture.project_id
            ),
            authorization,
            cookie,
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(revisions["revisions"].as_array().unwrap().len(), 1);
        let (status, counts) = request(
            &fixture.app,
            "GET",
            &format!("/app/api/projects/{}/section-counts", fixture.project_id),
            authorization,
            cookie,
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            counts,
            serde_json::json!({"docket":1,"deadlines":1,"documents":1})
        );
    }
}

#[tokio::test]
async fn a_minted_document_token_refuses_cross_project_downloads_writes_and_routes() {
    let fixture = fixture().await;
    let (_, minted) = mint(&fixture.app).await;
    let token = minted["token"].as_str().unwrap();
    let auth = bearer(token);

    for (authorization, cookie) in [(Some(auth.as_str()), None), (None, Some(token))] {
        for (method, uri, body) in [
            (
                "GET",
                format!(
                    "/app/api/projects/{}/section-counts",
                    fixture.other_project_id
                ),
                Body::empty(),
            ),
            (
                "POST",
                format!("/app/api/projects/{}/section-counts", fixture.project_id),
                Body::from("{}"),
            ),
            (
                "GET",
                format!("/app/api/projects/{}/documents", fixture.project_id),
                Body::empty(),
            ),
            (
                "GET",
                format!("/app/api/projects/{}/deadlines", fixture.project_id),
                Body::empty(),
            ),
            (
                "GET",
                format!("/app/api/projects/{}/docket", fixture.project_id),
                Body::empty(),
            ),
            (
                "GET",
                format!(
                    "/app/api/projects/{}/documents/revisions?slug=agreement",
                    fixture.other_project_id
                ),
                Body::empty(),
            ),
            (
                "GET",
                format!("/app/projects/acme/documents/{}/download", Uuid::new_v4()),
                Body::empty(),
            ),
            (
                "GET",
                format!(
                    "/app/lawyer/notations/{}/documents/document",
                    Uuid::new_v4()
                ),
                Body::empty(),
            ),
            (
                "POST",
                format!("/app/api/projects/{}/documents", fixture.project_id),
                Body::from("{}"),
            ),
            (
                "POST",
                "/app/projects/acme/documents/upload".to_string(),
                Body::empty(),
            ),
            ("GET", "/app/api/people".to_string(), Body::empty()),
        ] {
            let (status, body) =
                request(&fixture.app, method, &uri, authorization, cookie, body).await;
            let body_text = body.to_string();
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body_text}");
            assert_eq!(body["error"], "scope_violation");
            assert!(
                !body_text.contains("synthetic bytes"),
                "{method} {uri}: {body_text}"
            );
            assert!(
                !body_text.contains("https://"),
                "{method} {uri}: {body_text}"
            );
        }
    }
}

#[tokio::test]
async fn a_document_token_reads_only_its_project_surface_coordinates() {
    let fixture = fixture().await;
    let (_, minted) = mint(&fixture.app).await;
    let token = minted["token"].as_str().unwrap();
    let auth = bearer(token);
    let own = format!("/app/api/project-surfaces/{}", fixture.project_id);
    let other = format!("/app/api/project-surfaces/{}", fixture.other_project_id);

    for (authorization, cookie) in [(Some(auth.as_str()), None), (None, Some(token))] {
        let (status, body) = request(
            &fixture.app,
            "GET",
            &own,
            authorization,
            cookie,
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["code"], "acme");
        assert_eq!(
            body["repository_url"],
            "https://github.com/neon-law-staging/acme"
        );
        assert_eq!(
            body["lawyer_dri"],
            serde_json::json!(["ci-lawyer@example.com"])
        );
        assert_eq!(body["client_dri"], serde_json::json!([]));
        assert_eq!(
            body["private_slack_channel"],
            "https://slack.example/archives/C0SYNTHETIC"
        );
        assert_eq!(
            body["private_notion_page"],
            "https://notion.example/sample-page"
        );
        assert_eq!(body["dangling_dri_person_ids"], serde_json::json!([]));
        let mut keys: Vec<_> = body.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "client_dri".to_string(),
                "code".to_string(),
                "dangling_dri_person_ids".to_string(),
                "lawyer_dri".to_string(),
                "private_notion_page".to_string(),
                "private_slack_channel".to_string(),
                "repository_url".to_string(),
            ]
        );
        let rendered = body.to_string();
        for forbidden in ["token", "secret", "credential", "password", "ciphertext"] {
            assert!(
                !rendered.to_ascii_lowercase().contains(forbidden),
                "{forbidden} leaked in {rendered}"
            );
        }

        for (method, uri) in [("GET", other.as_str()), ("POST", own.as_str())] {
            let (status, body) = request(
                &fixture.app,
                method,
                uri,
                authorization,
                cookie,
                Body::empty(),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {body}");
            assert_eq!(body["error"], "scope_violation");
        }
    }
}

#[tokio::test]
async fn old_unscoped_ci_credentials_fail_closed_through_bearer_and_cookie_admission() {
    let fixture = fixture().await;
    let sessions = SessionStore::new(TEST_SESSION_KEY);
    let legacy = sessions.encode(&portal::SessionData {
        sub: "legacy-ci".into(),
        email: Some("ci-lawyer@example.com".into()),
        person_id: Some(fixture.lawyer_id),
        exp: now_unix_secs() + 60,
        role: Role::Lawyer,
        csrf_token: "legacy-csrf".into(),
        source: SessionSource::Ci,
        provider: None,
        viewing_as_dri: None,
        scope: None,
    });

    let (bearer_status, _) = request(
        &fixture.app,
        "GET",
        "/app/api/projects",
        Some(&bearer(&legacy)),
        None,
        Body::empty(),
    )
    .await;
    let (cookie_status, _) = request(
        &fixture.app,
        "GET",
        "/app/api/projects",
        None,
        Some(&legacy),
        Body::empty(),
    )
    .await;
    assert_eq!(bearer_status, StatusCode::UNAUTHORIZED);
    assert_eq!(cookie_status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_document_token_expiry_is_enforced_for_bearer_and_cookie() {
    let fixture = fixture().await;
    let (_, minted) = mint(&fixture.app).await;
    let sessions = SessionStore::new(TEST_SESSION_KEY);
    let mut expired = sessions.decode(minted["token"].as_str().unwrap()).unwrap();
    expired.exp = now_unix_secs() - 1;
    let expired = sessions.encode(&expired);

    for (authorization, cookie) in [(Some(bearer(&expired)), None), (None, Some(expired))] {
        let (status, _) = request(
            &fixture.app,
            "GET",
            "/app/api/projects",
            authorization.as_deref(),
            cookie.as_deref(),
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn a_github_oidc_document_mint_is_single_use_and_pr_merge_refs_are_read_only() {
    let fixture = fixture().await;
    let (first, _) = mint(&fixture.app).await;
    assert_eq!(first, StatusCode::OK);
    let (replay, body) = mint(&fixture.app).await;
    assert_eq!(replay, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "unauthorized");

    let surreal = mem_surreal().await;
    let project_id = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "branch".into(),
            name: "Branch".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id;
    store::projects::set_repository_url(
        &surreal,
        project_id,
        Some("https://github.com/neon-law-staging/branch"),
    )
    .await
    .unwrap();
    let lawyer = store::persons::create(
        &surreal,
        &NewPerson::with_role("Branch Lawyer", "branch-lawyer@example.com", Role::Lawyer),
    )
    .await
    .unwrap();
    store::projects::designate_dri_in_surreal(
        &surreal,
        project_id,
        lawyer.id,
        store::projects::DriSide::Lawyer,
    )
    .await
    .unwrap();
    let mut state = portal::test_support::app_state(surreal).await;
    state.canonical_host = portal::CanonicalHost::new(Some("staging.neonlaw.com".into()));
    state.github_oidc = GitHubOidc::fixed(GitHubActionsClaims {
        sub: "repo:neon-law-staging/branch:pull_request".into(),
        repository: "neon-law-staging/branch".into(),
        repository_owner: "neon-law-staging".into(),
        git_ref: "refs/pull/17/merge".into(),
        event_name: "pull_request".into(),
        jti: "jti-document-pr".into(),
        exp: 4_000_000_000,
        ..Default::default()
    });
    let app = portal::router(state);
    let (status, body) = mint(&app).await;
    assert_eq!(status, StatusCode::OK);
    let token = body["token"].as_str().unwrap();
    let (status, body) = request(
        &app,
        "POST",
        "/app/api/projects/whatever/documents",
        Some(&bearer(token)),
        None,
        Body::from("{}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "scope_violation");
}

/// Filesystem storage whose `head` is slow, like a distant bucket.
struct SlowHead {
    inner: cloud::FsStorage,
    delay: std::time::Duration,
}

#[async_trait::async_trait]
impl cloud::StorageService for SlowHead {
    async fn put(
        &self,
        key: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<(), cloud::StorageError> {
        self.inner.put(key, bytes, content_type).await
    }
    async fn get(&self, key: &str) -> Result<cloud::StoredObject, cloud::StorageError> {
        self.inner.get(key).await
    }
    async fn delete(&self, key: &str) -> Result<(), cloud::StorageError> {
        self.inner.delete(key).await
    }
    async fn head(&self, key: &str) -> Result<Option<cloud::ObjectHead>, cloud::StorageError> {
        tokio::time::sleep(self.delay).await;
        self.inner.head(key).await
    }
    async fn signed_url(
        &self,
        key: &str,
        expires_in: std::time::Duration,
    ) -> Result<String, cloud::StorageError> {
        self.inner.signed_url(key, expires_in).await
    }
}

async fn integrity_app() -> (Router, Arc<dyn cloud::StorageService>, Uuid) {
    integrity_app_with(0, None).await
}

async fn seed_extra_documents(
    surreal: &store::surreal::SurrealDb,
    storage: &Arc<dyn cloud::StorageService>,
    project_id: Uuid,
    extra: usize,
) {
    for index in 0..extra {
        let slug = format!("synthetic/{index:04}.pdf");
        store::assets::file_revision(
            surreal,
            storage,
            &store::documents::IngestArgs {
                project_id,
                source: store::documents::source::UPLOAD,
                filename: &format!("{index:04}.pdf"),
                kind: "unclassified",
                content_type: "application/pdf",
                description: None,
                visibility: store::documents::visibility::INTERNAL,
                secondary_storage_key: None,
            },
            &store::documents::DocumentIdentity {
                slug: Some(&slug),
                ..Default::default()
            },
            format!("synthetic bytes {index}").as_bytes(),
        )
        .await
        .unwrap();
    }
    store::assets::file_revision(
        surreal,
        storage,
        &store::documents::IngestArgs {
            project_id,
            source: store::documents::source::UPLOAD,
            filename: "agreement.pdf",
            kind: "unclassified",
            content_type: "application/pdf",
            description: None,
            visibility: store::documents::visibility::INTERNAL,
            secondary_storage_key: None,
        },
        &store::documents::DocumentIdentity {
            slug: Some("agreement"),
            ..Default::default()
        },
        b"synthetic integrity bytes",
    )
    .await
    .unwrap();
}

/// A matter with `1 + extra` documents. `head_delay` slows every storage
/// `head`.
async fn integrity_app_with(
    extra: usize,
    head_delay: Option<std::time::Duration>,
) -> (Router, Arc<dyn cloud::StorageService>, Uuid) {
    let surreal = mem_surreal().await;
    let project_id = store::projects::create(
        &surreal,
        &store::projects::NewProject {
            code: "acme".into(),
            name: "Acme".into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(&surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id;
    store::projects::set_repository_url(
        &surreal,
        project_id,
        Some("https://github.com/neon-law-staging/acme"),
    )
    .await
    .unwrap();
    let lawyer = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Synthetic Lawyer",
            "integrity-lawyer@example.com",
            Role::Lawyer,
        ),
    )
    .await
    .unwrap();
    store::projects::designate_dri_in_surreal(
        &surreal,
        project_id,
        lawyer.id,
        store::projects::DriSide::Lawyer,
    )
    .await
    .unwrap();
    let root = std::env::temp_dir().join(format!("navigator-integrity-{}", Uuid::now_v7()));
    let fs = cloud::FsStorage::new(&root).await.unwrap();
    let storage: Arc<dyn cloud::StorageService> = match head_delay {
        Some(delay) => Arc::new(SlowHead { inner: fs, delay }),
        None => Arc::new(fs),
    };
    let mut state = portal::test_support::app_state(surreal.clone()).await;
    state.storage = storage.clone();
    seed_extra_documents(&surreal, &state.storage, project_id, extra).await;
    state.canonical_host = portal::CanonicalHost::new(Some("staging.neonlaw.com".into()));
    state.github_oidc = GitHubOidc::fixed(GitHubActionsClaims {
        sub: "repo:neon-law-staging/acme:ref:refs/heads/main".into(),
        repository: "neon-law-staging/acme".into(),
        repository_owner: "neon-law-staging".into(),
        git_ref: "refs/heads/main".into(),
        event_name: "push".into(),
        jti: format!("jti-integrity-{}", Uuid::now_v7()),
        exp: 4_000_000_000,
        ..Default::default()
    });
    (portal::router(state), storage, project_id)
}

#[tokio::test]
async fn a_document_token_reports_a_missing_object_without_dropping_the_row() {
    let (app, storage, project_id) = integrity_app().await;
    let (status, minted) = mint(&app).await;
    assert_eq!(status, StatusCode::OK);
    let token = minted["token"].as_str().unwrap();
    let uri = format!("/app/api/projects/{project_id}/documents/integrity");

    let (status, body) =
        request(&app, "GET", &uri, Some(&bearer(token)), None, Body::empty()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["assets"].as_array().unwrap().len(), 1);
    assert_eq!(body["assets"][0]["exists"], true);
    assert_eq!(body["assets"][0]["slug"], "agreement");
    assert!(body["integrations"].as_array().unwrap().is_empty());

    let key = format!(
        "projects/acme/documents/{}",
        store::assets::sha256_hex(b"synthetic integrity bytes")
    );
    storage.delete(&key).await.unwrap();

    let (status, body) =
        request(&app, "GET", &uri, Some(&bearer(token)), None, Body::empty()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["assets"][0]["exists"], false);
    assert!(body["assets"][0].get("size_bytes").is_none());
    assert!(body["assets"][0]["asset_id"].is_string());

    let (status, body) = request(
        &app,
        "GET",
        &format!("{uri}?deep=true"),
        Some(&bearer(token)),
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["assets"][0]["exists"], false);
    assert_eq!(body["assets"][0]["sha256_matches"], false);
}

/// LAW-146: a 380-pointer matter on slow storage is checked in bounded
/// pages. Each page answers well inside a gateway timeout, the pages together
/// carry every row exactly once, and a page past the cap is clamped.
#[tokio::test]
async fn a_large_matter_on_slow_storage_is_checked_in_bounded_pages() {
    let (app, _storage, project_id) =
        integrity_app_with(379, Some(std::time::Duration::from_millis(40))).await;
    let (_, minted) = mint(&app).await;
    let token = minted["token"].as_str().unwrap();
    let uri = format!("/app/api/projects/{project_id}/documents/integrity");

    let mut seen = Vec::new();
    let mut offset = 0;
    let mut pages = 0;
    loop {
        let started = std::time::Instant::now();
        let (status, body) = request(
            &app,
            "GET",
            &format!("{uri}?offset={offset}&limit=50"),
            Some(&bearer(token)),
            None,
            Body::empty(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        // Sequential heads would take 50 * 40ms = 2s; eight at a time is
        // a quarter of that.
        assert!(started.elapsed() < std::time::Duration::from_millis(1800));
        assert_eq!(body["total"], 380);
        let rows = body["assets"].as_array().unwrap();
        assert!(rows.len() <= 50);
        for row in rows {
            assert_eq!(row["exists"], true);
            assert_eq!(row["operative"], true);
            assert_eq!(row["version"], 1);
            seen.push(row["asset_id"].as_str().unwrap().to_string());
        }
        pages += 1;
        match body["next_offset"].as_u64() {
            Some(next) => offset = usize::try_from(next).unwrap(),
            None => break,
        }
    }
    assert_eq!(pages, 8);
    assert_eq!(seen.len(), 380);
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 380);

    let (_, body) = request(
        &app,
        "GET",
        &format!("{uri}?limit=100000"),
        Some(&bearer(token)),
        None,
        Body::empty(),
    )
    .await;
    assert_eq!(body["assets"].as_array().unwrap().len(), 100);
    assert_eq!(body["next_offset"], 100);

    let (_, body) = request(
        &app,
        "GET",
        &format!("{uri}?offset=9999&limit=10"),
        Some(&bearer(token)),
        None,
        Body::empty(),
    )
    .await;
    assert!(body["assets"].as_array().unwrap().is_empty());
    assert!(body.get("next_offset").is_none());
}
