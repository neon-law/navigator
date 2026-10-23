//! Forgejo provisioning: each Project's repository is created in its own
//! Firm's namespace with that Firm's own encrypted token, and the repository
//! pointer and its provisioning time land together.
use cloud::FakeKms;
use serde_json::json;
use store::firm_secrets::{put, IntegrationProvider, IntegrationSecretKind, SecretPutRequest};
use store::firms::{self, NewFirm};
use store::persons::{self, NewPerson, Role};
use store::project_surfaces::{forgejo_for_project, reconcile, SurfaceError, SurfaceStatus};
use store::projects::{self, NewProject};
use store::surreal::SurrealDb;
use store::test_support::{mem_surreal, seed_entity};
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KMS_KEY: &str = "projects/fixture/locations/global/keyRings/runtime/cryptoKeys/integration";

struct FirmFixture {
    firm: firms::Firm,
    admin: persons::Person,
    project: projects::Project,
}

async fn firm_with_project(db: &SurrealDb, label: &str) -> FirmFixture {
    let admin = persons::create(
        db,
        &NewPerson::with_role(
            format!("{label} Admin"),
            format!("{label}-{}@example.com", Uuid::now_v7()),
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = firms::create(
        db,
        &NewFirm {
            name: format!("{label} Synthetic Firm"),
            status: "active".into(),
            entity_id: seed_entity(db).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    let project = projects::create(
        db,
        &NewProject {
            code: format!("{label}-matter"),
            name: format!("{label} matter"),
            status: "open".into(),
            entity_id: seed_entity(db).await,
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    FirmFixture {
        firm,
        admin,
        project,
    }
}

async fn store_token(db: &SurrealDb, fixture: &FirmFixture, kms: &FakeKms, token: &str) {
    put(
        db,
        SecretPutRequest {
            actor_role: Role::Admin,
            actor_person_id: Some(fixture.admin.id),
            firm_id: fixture.firm.id,
            provider: IntegrationProvider::Forgejo,
            kind: IntegrationSecretKind::ForgejoAccessToken,
            value: token,
        },
        kms,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn every_missing_piece_is_named_and_nothing_borrows_another_firms_token() {
    let db = mem_surreal().await;
    let kms = FakeKms::new(KMS_KEY);
    let alpha = firm_with_project(&db, "alpha").await;
    let beta = firm_with_project(&db, "beta").await;
    store_token(&db, &alpha, &kms, "alpha-token").await;

    let error = forgejo_for_project(&db, beta.project.id, &kms, |_| None)
        .await
        .unwrap_err();
    let SurfaceError::ForgejoUnavailable(missing) = error else {
        panic!("expected a configuration report, got {error}");
    };
    assert!(missing.contains("NAVIGATOR_FORGEJO_ROOT_URL"), "{missing}");
    assert!(missing.contains("forgejo_access_token"), "{missing}");

    let root = |key: &str| {
        (key == "NAVIGATOR_FORGEJO_ROOT_URL").then(|| "https://forge.example.test".to_string())
    };
    // Beta has no token of its own; Alpha's is never used for it.
    assert!(matches!(
        forgejo_for_project(&db, beta.project.id, &kms, root).await,
        Err(SurfaceError::ForgejoUnavailable(_))
    ));
    let forge = forgejo_for_project(&db, alpha.project.id, &kms, root)
        .await
        .unwrap();
    assert_eq!(forge.owner(), alpha.firm.slug);
}

#[tokio::test]
async fn reconcile_creates_the_firms_private_repository_and_stamps_it_once() {
    let db = mem_surreal().await;
    let kms = FakeKms::new(KMS_KEY);
    let alpha = firm_with_project(&db, "alpha").await;
    store_token(&db, &alpha, &kms, "alpha-token").await;
    let server = MockServer::start().await;
    let repo_path = format!("/api/v1/repos/{}/{}", alpha.firm.slug, alpha.project.code);
    Mock::given(method("GET"))
        .and(path(repo_path.clone()))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("/api/v1/orgs/{}/repos", alpha.firm.slug)))
        .and(header("authorization", "token alpha-token"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "html_url": format!(
                "https://forge.example.test/{}/{}",
                alpha.firm.slug, alpha.project.code
            ),
            "name": alpha.project.code,
            "private": true,
            "owner": { "login": alpha.firm.slug },
        })))
        .expect(1)
        .mount(&server)
        .await;
    let uri = server.uri();
    let forge = forgejo_for_project(&db, alpha.project.id, &kms, |key| {
        (key == "NAVIGATOR_FORGEJO_ROOT_URL").then(|| uri.clone())
    })
    .await
    .unwrap();

    let first = reconcile(
        &db,
        alpha.project.id,
        None::<&cloud::FakeDrive>,
        Some(&forge),
    )
    .await
    .unwrap();
    assert_eq!(first.repository_status, SurfaceStatus::Created);
    let stored = projects::find_by_id(&db, alpha.project.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.repository_url, first.repository_url);
    assert!(stored.forge_provisioned_at.is_some());
    // No source has landed yet, so nothing claims it has.
    assert!(stored.git_initialized_at.is_none());

    // A replay finds the recorded pointer and calls the forge no further.
    let replay = reconcile(
        &db,
        alpha.project.id,
        None::<&cloud::FakeDrive>,
        Some(&forge),
    )
    .await
    .unwrap();
    assert_eq!(replay.repository_status, SurfaceStatus::Present);
    let unchanged = projects::find_by_id(&db, alpha.project.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.forge_provisioned_at, stored.forge_provisioned_at);
}

#[tokio::test]
async fn a_concurrent_pointer_is_never_replaced() {
    let db = mem_surreal().await;
    let alpha = firm_with_project(&db, "alpha").await;
    projects::set_repository_url(
        &db,
        alpha.project.id,
        Some("https://forge.example.test/alpha/first-writer"),
    )
    .await
    .unwrap();
    let row = projects::record_provisioned_repository(
        &db,
        alpha.project.id,
        "https://forge.example.test/alpha/second-writer",
        "2026-10-18T12:00:00Z",
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        row.repository_url.as_deref(),
        Some("https://forge.example.test/alpha/first-writer")
    );
    assert!(row.forge_provisioned_at.is_none());
}
