//! Signed Forgejo 15.0.9 OIDC tokens reach the real CI mint handler and stay
//! limited to the exact live Project's document read surface.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use portal::forgejo_oidc::{ForgejoActionsClaims, ForgejoOidc};
use portal::session::{now_unix_secs, SessionSource, SessionStore};
use portal::test_support::{
    sign_rs256_claims, test_oidc_jwks_rsa, TEST_OIDC_KID, TEST_SESSION_KEY,
};
use store::persons::{NewPerson, Role};
use store::test_support::mem_surreal;
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Fixture {
    app: Router,
    _server: MockServer,
    issuer: String,
    project_id: Uuid,
    other_project_id: Uuid,
}

async fn fixture() -> Fixture {
    let server = MockServer::start().await;
    let issuer = format!("{}/firm/api/actions", server.uri());
    let discovery = serde_json::json!({
        "issuer": issuer.clone(),
        "jwks_uri": format!("{}/firm/api/actions/.well-known/keys", server.uri()),
    });
    let jwks = serde_json::json!({"keys": [test_oidc_jwks_rsa()]});
    Mock::given(method("GET"))
        .and(path("/firm/api/actions/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(discovery))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/firm/api/actions/.well-known/keys"))
        .respond_with(ResponseTemplate::new(200).set_body_json(jwks))
        .mount(&server)
        .await;

    let surreal = mem_surreal().await;
    let project_id = create_project(&surreal, "acme").await;
    let other_project_id = create_project(&surreal, "widgets").await;
    store::projects::set_repository_url(
        &surreal,
        project_id,
        Some(&format!("https://{}/firm/firm/acme.git", server.address())),
    )
    .await
    .unwrap();
    // The second project is hosted elsewhere. A token from this Forgejo
    // issuer cannot claim or inherit that binding.
    store::projects::set_repository_url(
        &surreal,
        other_project_id,
        Some("https://other.example/firm/firm/widgets.git"),
    )
    .await
    .unwrap();
    let lawyer = store::persons::create(
        &surreal,
        &NewPerson::with_role(
            "Synthetic Forgejo Lawyer",
            "forgejo-ci@example.com",
            Role::Lawyer,
        ),
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
    let document_args = store::documents::IngestArgs {
        project_id,
        source: store::documents::source::UPLOAD,
        filename: "agreement.pdf",
        kind: "unclassified",
        content_type: "application/pdf",
        description: None,
        visibility: store::documents::visibility::INTERNAL,
        secondary_storage_key: None,
    };
    store::assets::file_revision(
        &surreal,
        &state.storage,
        &document_args,
        &store::documents::DocumentIdentity {
            slug: Some("agreement"),
            ..Default::default()
        },
        b"synthetic Forgejo-gated bytes",
    )
    .await
    .unwrap();
    state.canonical_host = portal::CanonicalHost::new(Some("staging.neonlaw.com".into()));
    let verifier = ForgejoOidc::local_test_issuer(
        &issuer,
        "navigator-ci",
        ".forgejo/workflows/project-gate.yml",
    )
    .unwrap();
    Fixture {
        app: portal::router_with_forgejo_oidc(state, verifier),
        _server: server,
        issuer,
        project_id,
        other_project_id,
    }
}

async fn create_project(surreal: &store::surreal::SurrealDb, code: &str) -> Uuid {
    store::projects::create(
        surreal,
        &store::projects::NewProject {
            code: code.into(),
            name: code.into(),
            status: "open".into(),
            entity_id: store::test_support::seed_entity(surreal).await,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id
}

fn claims(issuer: &str, repository: &str, iat: i64) -> ForgejoActionsClaims {
    let now = now_unix_secs();
    ForgejoActionsClaims {
        sub: format!("repo:{repository}:ref:refs/heads/main"),
        iss: issuer.into(),
        aud: "navigator-ci".into(),
        exp: now + 900 + (iat - now),
        iat,
        nbf: iat,
        actor: "workflow-bot".into(),
        base_ref: String::new(),
        event_name: "push".into(),
        head_ref: String::new(),
        git_ref: "refs/heads/main".into(),
        // Forgejo 15.0.9's GenerateGiteaContext sets this claim false for
        // every event. It is not branch-protection evidence.
        ref_protected: "false".into(),
        ref_type: "branch".into(),
        repository: repository.into(),
        repository_owner: repository.split('/').next().unwrap().into(),
        run_attempt: "1".into(),
        run_id: "44".into(),
        run_number: "99".into(),
        sha: "a".repeat(40),
        workflow: "project-gate.yml".into(),
        workflow_ref: format!("{repository}/.forgejo/workflows/project-gate.yml@refs/heads/main"),
    }
}

async fn mint(app: &Router, token: &str) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/ci/forgejo/document-token")
                .header(header::HOST, "staging.neonlaw.com")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"token":token}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn request(app: &Router, method: &str, uri: &str, token: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, "staging.neonlaw.com")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn forgejo_signed_token_mints_one_project_document_read_session_and_rejects_replay() {
    let fixture = fixture().await;
    let issued_at = now_unix_secs();
    let token = sign_rs256_claims(
        TEST_OIDC_KID,
        &claims(&fixture.issuer, "firm/acme", issued_at),
    );

    let (status, minted) = mint(&fixture.app, &token).await;
    assert_eq!(status, StatusCode::OK, "{minted}");
    assert_eq!(minted["project_code"], "acme");
    let session_token = minted["token"].as_str().unwrap();
    let session = SessionStore::new(TEST_SESSION_KEY)
        .decode(session_token)
        .unwrap();
    assert_eq!(session.source, SessionSource::Ci);
    assert!(session.exp <= issued_at + 900);
    assert!(matches!(
        session.scope,
        Some(portal::session::SessionScope::Document(_))
    ));

    assert_eq!(
        request(
            &fixture.app,
            "GET",
            &format!(
                "/app/api/projects/{}/documents/revisions?slug=agreement",
                fixture.project_id
            ),
            session_token,
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(
        request(
            &fixture.app,
            "GET",
            &format!(
                "/app/api/projects/{}/documents/revisions?slug=agreement",
                fixture.other_project_id
            ),
            session_token,
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(
            &fixture.app,
            "POST",
            &format!("/app/api/projects/{}/documents", fixture.project_id),
            session_token,
        )
        .await,
        StatusCode::FORBIDDEN
    );

    let (status, _) = mint(&fixture.app, &token).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn distinct_forgejo_tokens_in_the_same_run_can_renew_but_unbound_repo_cannot_mint() {
    let fixture = fixture().await;
    let now = now_unix_secs();
    let first = sign_rs256_claims(TEST_OIDC_KID, &claims(&fixture.issuer, "firm/acme", now));
    let renewed = sign_rs256_claims(
        TEST_OIDC_KID,
        &claims(&fixture.issuer, "firm/acme", now + 1),
    );
    assert_ne!(first, renewed);
    assert_eq!(mint(&fixture.app, &first).await.0, StatusCode::OK);
    assert_eq!(mint(&fixture.app, &renewed).await.0, StatusCode::OK);

    let foreign = sign_rs256_claims(TEST_OIDC_KID, &claims(&fixture.issuer, "firm/widgets", now));
    assert_eq!(mint(&fixture.app, &foreign).await.0, StatusCode::FORBIDDEN);

    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/ci/seed-token")
                .header(header::HOST, "staging.neonlaw.com")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::json!({"token":first}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn forgejo_signed_claims_outside_the_trusted_contract_fail_closed() {
    let fixture = fixture().await;
    let now = now_unix_secs();
    let mut cases = Vec::new();

    let mut bad = claims(&fixture.issuer, "firm/acme", now);
    bad.iss = "https://foreign.example/api/actions".into();
    cases.push((bad, None, StatusCode::UNAUTHORIZED));
    let mut bad = claims(&fixture.issuer, "firm/acme", now);
    bad.aud = "another-deployment".into();
    cases.push((bad, None, StatusCode::UNAUTHORIZED));
    let mut bad = claims(&fixture.issuer, "firm/acme", now);
    bad.sub = "repo:firm/other:ref:refs/heads/main".into();
    cases.push((bad, None, StatusCode::FORBIDDEN));
    let mut bad = claims(&fixture.issuer, "firm/acme", now);
    bad.workflow = "untrusted.yml".into();
    cases.push((bad, None, StatusCode::FORBIDDEN));
    let mut bad = claims(&fixture.issuer, "firm/acme", now);
    bad.workflow_ref = "firm/acme/.forgejo/workflows/other.yml@refs/heads/main".into();
    cases.push((bad, None, StatusCode::FORBIDDEN));
    let mut bad = claims(&fixture.issuer, "firm/acme", now);
    bad.git_ref = "refs/heads/release".into();
    cases.push((bad, None, StatusCode::FORBIDDEN));
    let mut bad = claims(&fixture.issuer, "firm/acme", now);
    bad.event_name = "pull_request_target".into();
    cases.push((bad, None, StatusCode::FORBIDDEN));
    let mut bad = claims(&fixture.issuer, "firm/acme", now);
    bad.sha = "not-a-commit".into();
    cases.push((bad, None, StatusCode::FORBIDDEN));
    let mut bad = claims(&fixture.issuer, "firm/acme", now);
    bad.sha = "a".repeat(41);
    cases.push((bad, None, StatusCode::FORBIDDEN));
    let mut bad = claims(&fixture.issuer, "firm/acme", now);
    bad.exp = now - 1;
    cases.push((bad, None, StatusCode::UNAUTHORIZED));
    let mut bad = claims(&fixture.issuer, "firm/acme", now + 60);
    bad.exp += 60;
    cases.push((bad, None, StatusCode::UNAUTHORIZED));

    for (claims, kid, expected) in cases {
        let token = sign_rs256_claims(kid.unwrap_or(TEST_OIDC_KID), &claims);
        assert_eq!(mint(&fixture.app, &token).await.0, expected, "{claims:?}");
    }
    let unknown_kid = sign_rs256_claims("unknown-key", &claims(&fixture.issuer, "firm/acme", now));
    assert_eq!(
        mint(&fixture.app, &unknown_kid).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn supported_pull_request_head_claims_can_only_mint_read_scope() {
    let fixture = fixture().await;
    let mut pr = claims(&fixture.issuer, "firm/acme", now_unix_secs());
    pr.event_name = "pull_request".into();
    pr.sub = "repo:firm/acme:pull_request".into();
    pr.git_ref = "refs/pull/17/head".into();
    pr.base_ref = "main".into();
    pr.head_ref = "feature".into();
    pr.workflow_ref = "firm/acme/.forgejo/workflows/project-gate.yml@refs/pull/17/head".into();
    // GitHub's merge-ref spelling is not what Forgejo signs, so it is refused.
    let mut github_shaped = pr.clone();
    github_shaped.git_ref = "refs/pull/17/merge".into();
    github_shaped.workflow_ref =
        "firm/acme/.forgejo/workflows/project-gate.yml@refs/pull/17/merge".into();
    let refused = sign_rs256_claims(TEST_OIDC_KID, &github_shaped);
    assert_eq!(mint(&fixture.app, &refused).await.0, StatusCode::FORBIDDEN);
    let token = sign_rs256_claims(TEST_OIDC_KID, &pr);
    let (status, minted) = mint(&fixture.app, &token).await;
    assert_eq!(status, StatusCode::OK, "{minted}");
    let session_token = minted["token"].as_str().unwrap();
    assert_eq!(
        request(
            &fixture.app,
            "POST",
            &format!("/app/api/projects/{}/documents", fixture.project_id),
            session_token,
        )
        .await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn discovery_metadata_must_keep_the_trusted_issuer_and_same_origin_jwks() {
    for (discovery_issuer, jwks_uri) in [
        ("https://attacker.example/api/actions", None),
        ("{issuer}", Some("https://attacker.example/keys")),
        ("{issuer}", Some("{issuer}/keys")),
    ] {
        let server = MockServer::start().await;
        let issuer = format!("{}/api/actions", server.uri());
        let metadata_issuer = if discovery_issuer == "{issuer}" {
            issuer.clone()
        } else {
            discovery_issuer.to_owned()
        };
        let keys = match jwks_uri {
            None => format!("{}/api/actions/.well-known/keys", server.uri()),
            Some("{issuer}/keys") => format!("{issuer}/keys"),
            Some(uri) => uri.to_owned(),
        };
        Mock::given(method("GET"))
            .and(path("/api/actions/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": metadata_issuer,
                "jwks_uri": keys,
            })))
            .mount(&server)
            .await;
        let verifier = ForgejoOidc::local_test_issuer(
            &issuer,
            "navigator-ci",
            ".forgejo/workflows/project-gate.yml",
        )
        .unwrap();
        let state = portal::ci_auth::CiAuthState {
            sessions: SessionStore::new(TEST_SESSION_KEY),
            surreal: mem_surreal().await,
            github_oidc: portal::github_oidc::GitHubOidc::rejecting(),
            forgejo_oidc: verifier,
            canonical_host: portal::CanonicalHost::new(None),
        };
        let app = portal::ci_auth::routes(state);
        let claims = claims(&issuer, "firm/acme", now_unix_secs());
        let token = sign_rs256_claims(TEST_OIDC_KID, &claims);
        assert_eq!(mint(&app, &token).await.0, StatusCode::UNAUTHORIZED);
    }
}
