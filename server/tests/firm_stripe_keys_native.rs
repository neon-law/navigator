//! The Firm CLI API writes encrypted credentials through native PostgreSQL authority.
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use store::firm_secrets::{self, IntegrationProvider, IntegrationSecretKind, Mode};
use store::persons::Role;
use tower::ServiceExt;

#[path = "common/bearer.rs"]
mod bearer_common;

async fn directory_json(
    app: &axum::Router,
    path: &str,
    token: &str,
    secret: &str,
) -> serde_json::Value {
    let response = app
        .clone()
        .oneshot(
            Request::get(path)
                .header("authorization", token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(!String::from_utf8_lossy(&body).contains(secret));
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn stripe_command_writes_only_for_its_firm_and_lists_only_metadata() {
    store::test_support::with_postgres(exercise()).await;
}

async fn exercise() {
    let db = store::application_store::ApplicationStore::uninitialized();
    let (firm_id, dri_id) =
        store::test_support::seed_firm_with_new_brand(&db, "synthetic-billing").await;
    let (_, other_id) = store::test_support::seed_firm_with_new_brand(&db, "synthetic-other").await;
    let dri = bearer_common::bearer(dri_id, Role::Admin);
    let other_admin = bearer_common::bearer(other_id, Role::Admin);
    let owner = bearer_common::bearer(other_id, Role::Owner);
    let mut state = portal::test_support::app_state(db.clone()).await;
    state.sessions = portal::SessionStore::new(portal::test_support::TEST_SESSION_KEY);
    state.runtime_kms = std::sync::Arc::new(cloud::FakeKms::new("api-admin-firm-secrets-test-kms"));
    let app = server::neon_router(state, std::path::Path::new(portal::DEFAULT_PUBLIC_DIR));
    let directory = format!("/app/api/firms/{firm_id}/secrets");
    let mode = firm_secrets::stripe_mode().unwrap();
    let (good, wrong, other_mode) = match mode {
        Mode::Sandbox => ("sk_test_example", "sk_live_example", Mode::Live),
        Mode::Live => ("sk_live_example", "sk_test_example", Mode::Sandbox),
    };
    let slot = format!("{directory}/stripe/nevada/secret");
    let request = |method, path: String, token: &str, value: &'static str| {
        app.clone().oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", token)
                .header("content-type", "application/octet-stream")
                .body(Body::from(value))
                .unwrap(),
        )
    };
    for (token, target_mode, value, expected) in [
        (&dri, other_mode, wrong, StatusCode::BAD_REQUEST),
        (&dri, mode, wrong, StatusCode::BAD_REQUEST),
        (&other_admin, mode, good, StatusCode::NOT_FOUND),
        (&owner, mode, good, StatusCode::NOT_FOUND),
        (&dri, mode, good, StatusCode::NO_CONTENT),
    ] {
        let response = request("PUT", format!("{slot}/{target_mode}"), token, value)
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(!String::from_utf8_lossy(&body).contains(value));
    }
    let stored = firm_secrets::metadata_for_firm(
        &db,
        Role::Admin,
        Some(dri_id),
        firm_id,
        IntegrationProvider::Stripe,
        IntegrationSecretKind::StripeNevadaAppKey,
    )
    .await
    .unwrap();
    assert_eq!(stored.actor_id, dri_id);
    let resolved = firm_secrets::resolve_firm_credential(
        &db,
        firm_id,
        IntegrationProvider::Stripe,
        IntegrationSecretKind::StripeNevadaAppKey,
        &cloud::FakeKms::new("api-admin-firm-secrets-test-kms"),
    )
    .await
    .unwrap();
    let matches_submitted_key = resolved.expose_for_provider_call() == good;
    assert!(
        matches_submitted_key,
        "the issuer must receive the submitted key"
    );
    let listing = directory_json(&app, &directory, &dri, good).await;
    assert_eq!(listing["keys"][0]["account"], "nevada");
    assert_eq!(listing["keys"][0]["kind"], "secret");
    assert_eq!(listing["keys"][0]["mode"], mode.to_string());
    assert_eq!(
        request("DELETE", format!("{slot}/{mode}"), &dri, "")
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    let empty = directory_json(&app, &directory, &dri, good).await;
    assert_eq!(empty["keys"], serde_json::json!([]));
}
