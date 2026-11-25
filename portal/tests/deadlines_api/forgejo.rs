use super::*;
use base64::Engine as _;
use sha1::{Digest as _, Sha1};
use wiremock::{
    matchers::{header as with_header, method, path},
    Mock, MockServer, ResponseTemplate,
};

async fn provider(forge: &MockServer, route: &str, body: Value) {
    Mock::given(method("GET"))
        .and(path(route))
        .and(with_header("authorization", "token fixture-secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(forge)
        .await;
}

async fn mount_pointer(forge: &MockServer, owner: &str, evidence: &Value, size: i64) -> String {
    let document = json!({
        "kind":"order", "visibility":"internal",
        "current_version":{
            "version":1, "created_at":"2026-01-01T00:00:00Z",
            "asset_id":evidence["id"], "size_bytes":size, "sha256":evidence["sha256_hex"]
        }
    })
    .to_string();
    let commit = "a".repeat(40);
    let tree = "b".repeat(40);
    let mut digest = Sha1::new();
    digest.update(format!("blob {}\0", document.len()));
    digest.update(document.as_bytes());
    let blob = hex::encode(digest.finalize());
    let repo = format!("/api/v1/repos/{owner}/library");
    let info = json!({"id":17, "name":"library", "owner":{"login":owner}, "private":true});
    provider(forge, &repo, info.clone()).await;
    provider(forge, "/api/v1/repositories/17", info).await;
    provider(
        forge,
        &format!("{repo}/git/commits/{commit}"),
        json!({"sha":commit, "commit":{"tree":{"sha":tree}}}),
    )
    .await;
    provider(
        forge,
        &format!("{repo}/git/trees/{tree}"),
        json!({
            "sha":tree, "truncated":false,
            "tree":[{"sha":blob, "path":"entered.yaml", "mode":"100644"}]
        }),
    )
    .await;
    provider(
        forge,
        &format!("{repo}/git/blobs/{blob}"),
        json!({
            "sha":blob, "size":document.len(), "encoding":"base64",
            "content":base64::engine::general_purpose::STANDARD.encode(&document)
        }),
    )
    .await;
    commit
}

#[tokio::test]
async fn a_verified_library_pointer_opens_the_filed_record_and_archived_page_survives_an_outage() {
    with_postgres(check_pinned_library()).await;
}

async fn check_pinned_library() {
    let forge = MockServer::start().await;
    let fixture = fixture_with_integrations(Some(&forge)).await;
    let pg = store::postgres::PostgresDb::application().await.unwrap();
    let (owner,): (String,) =
        sqlx::query_as("SELECT f.slug FROM firm f JOIN project p ON p.firm_id=f.id WHERE p.id=$1")
            .bind(fixture.project_id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    let project_url = format!("/app/api/projects/{}", fixture.project_id);
    let queue = format!("{project_url}/court-date-proposals");
    let mut evidence = file_synthetic_order(&fixture, &project_url).await;
    let size: i64 = sqlx::query_scalar("SELECT byte_size FROM asset WHERE id=$1")
        .bind(Uuid::parse_str(evidence["id"].as_str().unwrap()).unwrap())
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let commit = mount_pointer(&forge, &owner, &evidence, size).await;
    evidence["forgejo"] =
        json!({"repository":"firm_library","commit":commit,"path":"entered.yaml"});
    let first = queue_order(&fixture, &queue, &evidence, "2030-07-16").await;
    let id = first["id"].as_str().unwrap();
    let (_, list) = send(
        &fixture.app,
        "GET",
        &queue,
        &fixture.participant,
        Vec::new(),
    )
    .await;
    let source = &list["proposals"][0]["evidence"][0];
    let opener = source["forgejo_url"].as_str().unwrap();
    let archive = source["archive_url"].as_str().unwrap();
    let open = || {
        Request::builder()
            .uri(opener)
            .extension(fixture.participant.clone())
            .body(Body::empty())
            .unwrap()
    };
    let page = fixture.app.clone().oneshot(open()).await.unwrap();
    assert_eq!(page.status(), 303);
    assert_eq!(
        page.headers()[header::LOCATION],
        format!(
            "{}/{owner}/library/src/commit/{commit}/entered.yaml",
            forge.uri()
        )
    );
    assert_eq!(page.headers()[header::CACHE_CONTROL], "private, no-store");
    assert_eq!(page.headers()[header::REFERRER_POLICY], "no-referrer");
    forge.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&forge)
        .await;
    assert_eq!(
        fixture.app.clone().oneshot(open()).await.unwrap().status(),
        503
    );
    let (status, _) = send(
        &fixture.app,
        "POST",
        &format!("{queue}/{id}/confirm"),
        &fixture.participant,
        serde_json::to_vec(&json!({"revision":1})).unwrap(),
    )
    .await;
    assert_eq!(status, 503);
    let archived = fixture
        .app
        .oneshot(
            Request::builder()
                .uri(archive)
                .extension(fixture.participant)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(archived.status(), 200);
    assert_eq!(archived.headers()[header::CONTENT_TYPE], "image/png");
    let png = axum::body::to_bytes(archived.into_body(), 1_000_000)
        .await
        .unwrap();
    assert!(png.starts_with(b"\x89PNG"));
}
