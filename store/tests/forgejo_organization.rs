//! Provisioning an existing Firm's Forgejo organization: the provisioning
//! identity creates `<firm-slug>` once, the Firm's own account joins its
//! owners team once, and nothing borrows another Firm's token.
use cloud::FakeKms;
use serde_json::json;
use store::firm_secrets::{put, IntegrationProvider, IntegrationSecretKind, SecretPutRequest};
use store::firms::forgejo_organization::{reconcile, ProvisionError};
use store::firms::{self, NewFirm};
use store::persons::{self, NewPerson, Role};
use store::surreal::SurrealDb;
use store::test_support::{mem_surreal, seed_entity};
use uuid::Uuid;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KMS_KEY: &str = "projects/fixture/locations/global/keyRings/runtime/cryptoKeys/integration";

async fn firm(db: &SurrealDb, label: &str) -> (firms::Firm, persons::Person) {
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
    (firm, admin)
}

async fn store_token(
    db: &SurrealDb,
    firm: &firms::Firm,
    admin: &persons::Person,
    kms: &FakeKms,
    token: &str,
) {
    put(
        db,
        SecretPutRequest {
            actor_role: Role::Admin,
            actor_person_id: Some(admin.id),
            firm_id: firm.id,
            provider: IntegrationProvider::Forgejo,
            kind: IntegrationSecretKind::ForgejoAccessToken,
            value: token,
        },
        kms,
    )
    .await
    .unwrap();
}

fn settings(root: String) -> impl Fn(&str) -> Option<String> {
    move |key: &str| match key {
        "NAVIGATOR_FORGEJO_ROOT_URL" => Some(root.clone()),
        "NAVIGATOR_FORGEJO_PROVISIONER_TOKEN" => Some("provisioner-token".into()),
        _ => None,
    }
}

#[tokio::test]
async fn every_missing_setting_is_named_before_any_request() {
    let db = mem_surreal().await;
    let kms = FakeKms::new(KMS_KEY);
    let (alpha, admin) = firm(&db, "alpha").await;
    let (beta, _) = firm(&db, "beta").await;
    store_token(&db, &alpha, &admin, &kms, "alpha-token").await;

    let error = reconcile(&db, beta.id, &kms, |_| None).await.unwrap_err();
    let ProvisionError::Unavailable(missing) = error else {
        panic!("expected a configuration report, got {error}");
    };
    for named in [
        "NAVIGATOR_FORGEJO_ROOT_URL",
        "NAVIGATOR_FORGEJO_PROVISIONER_TOKEN",
        "forgejo_access_token",
    ] {
        assert!(missing.contains(named), "{missing}");
    }

    // Beta has no token of its own; Alpha's is never used for it.
    let error = reconcile(
        &db,
        beta.id,
        &kms,
        settings("https://forge.example.test".into()),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&error, ProvisionError::Unavailable(missing) if missing.contains("forgejo_access_token")),
        "{error}"
    );

    assert!(matches!(
        reconcile(&db, Uuid::now_v7(), &kms, |_| None).await,
        Err(ProvisionError::NotFound)
    ));
}

#[tokio::test]
async fn the_firms_organization_is_created_once_and_a_rerun_converges() {
    let db = mem_surreal().await;
    let kms = FakeKms::new(KMS_KEY);
    let (alpha, admin) = firm(&db, "alpha").await;
    store_token(&db, &alpha, &admin, &kms, "alpha-token").await;
    let org = alpha.slug.clone();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "token alpha-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "login": "alpha-account" })))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/orgs/{org}")))
        .and(header("authorization", "token provisioner-token"))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/orgs/{org}")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "id": 41, "username": org, "visibility": "private" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orgs"))
        .and(header("authorization", "token provisioner-token"))
        .and(body_json(
            json!({ "username": org, "visibility": "private" }),
        ))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({ "id": 41, "username": org, "visibility": "private" })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/v1/orgs/{org}/teams")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 5, "name": "Owners", "permission": "owner" }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/teams/5/members/alpha-account"))
        .respond_with(ResponseTemplate::new(404))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/teams/5/members/alpha-account"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "login": "alpha-account" })))
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/teams/5/members/alpha-account"))
        .and(header("authorization", "token provisioner-token"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let first = reconcile(&db, alpha.id, &kms, settings(server.uri()))
        .await
        .unwrap();
    assert_eq!(first.organization, alpha.slug);
    assert_eq!(first.forgejo_id, 41);
    assert_eq!(first.owner_login, "alpha-account");
    assert!(first.created && first.owner_added);

    let second = reconcile(&db, alpha.id, &kms, settings(server.uri()))
        .await
        .unwrap();
    assert_eq!(second.organization, alpha.slug);
    assert_eq!(second.forgejo_id, 41);
    assert!(!second.created && !second.owner_added && !second.made_private);
}
