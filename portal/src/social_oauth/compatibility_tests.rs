use super::{
    authorization_url, complete_authorization, connect_start, disconnect, discover_identities,
    exchange_code, exchange_threads_short_token, granted_scopes, oauth_config_from,
    scopes_are_sufficient, CallbackQuery, ConnectForm, DisconnectForm, OAuthConfig, Provider,
    ProviderEndpoints, TokenResponse,
};
use super::{identity_matches, ConnectedIdentity};
use axum::http::{header::LOCATION, StatusCode};
use axum::response::IntoResponse;
use chrono::Utc;
use store::brands::NewBrand;
use store::firms::NewFirm;
use store::persons::{NewPerson, Role};
use store::social_accounts::SocialProvider;
use store::test_support::{self, mem_surreal};
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn config() -> OAuthConfig {
    OAuthConfig {
        client_id: "synthetic-client".into(),
        client_secret: "synthetic-secret".into(),
        redirect_uri: "https://navigator.example/social/oauth/callback".into(),
    }
}

fn endpoints(server: &MockServer) -> ProviderEndpoints {
    ProviderEndpoints {
        meta_graph: server.uri(),
        threads_graph: server.uri(),
        youtube_token: format!("{}/token", server.uri()),
        youtube_channels: format!("{}/channels", server.uri()),
        x_api: server.uri(),
        tiktok: format!("{}/tiktok", server.uri()),
        linkedin_token: format!("{}/token", server.uri()),
        linkedin_introspect: format!("{}/introspect", server.uri()),
        linkedin_rest: server.uri(),
    }
}

async fn practice(db: &store::surreal::SurrealDb) -> (Uuid, String) {
    let admin = store::persons::create(
        db,
        &NewPerson::with_role(
            "Social Admin",
            format!("social-admin-{}@example.com", Uuid::now_v7()),
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = store::firms::create(
        db,
        &NewFirm {
            name: format!("Social Firm {}", admin.id),
            status: "active".into(),
            entity_id: test_support::seed_entity(db).await,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    let brand_key = format!("social-{}", Uuid::now_v7());
    store::brands::create(
        db,
        Role::Admin,
        Some(admin.id),
        &NewBrand {
            name: format!("Social Brand {brand_key}"),
            key: brand_key.clone(),
            firm_id: Some(firm.id),
            primary_color: None,
            accent_color: None,
            typeface: None,
        },
    )
    .await
    .unwrap();
    (admin.id, brand_key)
}

fn location_of(response: &axum::response::Response) -> (StatusCode, String) {
    let status = response.status();
    let location = response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (status, location)
}

async fn finish(
    state: crate::AppState,
    query: CallbackQuery,
    endpoints: &ProviderEndpoints,
) -> (StatusCode, String) {
    location_of(
        &complete_authorization(state, query, Some(config()), endpoints)
            .await
            .into_response(),
    )
}

fn facebook_permissions() -> serde_json::Value {
    serde_json::json!({
        "data": [
            {"permission": "pages_show_list", "status": "granted"},
            {"permission": "pages_read_engagement", "status": "granted"},
            {"permission": "pages_manage_posts", "status": "granted"},
            {"permission": "pages_manage_engagement", "status": "granted"}
        ]
    })
}

async fn mount_facebook_oauth(
    server: &MockServer,
    permissions: serde_json::Value,
    accounts: Option<serde_json::Value>,
) {
    Mock::given(method("POST"))
        .and(path("/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "user-token",
            "expires_in": 3600
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/oauth/access_token"))
        .and(wiremock::matchers::query_param(
            "grant_type",
            "fb_exchange_token",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "long-user-token",
            "expires_in": 5_183_944
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/me/permissions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(permissions))
        .mount(server)
        .await;
    if let Some(accounts) = accounts {
        Mock::given(method("GET"))
            .and(path("/me/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(accounts))
            .mount(server)
            .await;
    }
}

async fn save_facebook_selection(
    db: &store::surreal::SurrealDb,
    actor: Uuid,
    brand: &str,
    identity: &str,
) {
    store::social_account_selections::save(
        db,
        Role::Admin,
        actor,
        brand,
        SocialProvider::Facebook,
        identity,
    )
    .await
    .unwrap();
}

async fn begin_facebook(db: &store::surreal::SurrealDb, admin: Uuid, brand_key: &str) -> String {
    store::social_account_selections::save(
        db,
        Role::Admin,
        admin,
        brand_key,
        SocialProvider::Facebook,
        "page-99",
    )
    .await
    .unwrap();
    let attempt = store::social_oauth_attempts::begin(
        db,
        Role::Admin,
        admin,
        brand_key,
        "facebook",
        Utc::now(),
    )
    .await
    .unwrap();
    store::social_oauth_attempts::bind_account(db, &attempt, "page-99")
        .await
        .unwrap();
    attempt.state
}

#[test]
fn selected_meta_consent_requests_both_publishing_grants_without_threads() {
    let attempt = store::social_oauth_attempts::SocialOAuthAttempt {
        state: "synthetic-state".into(),
        pkce_verifier: "synthetic-verifier".into(),
        firm_id: Uuid::nil(),
        brand_key: "synthetic".into(),
        provider: "facebook".into(),
        provider_account_id: Some("page-99".into()),
        instagram_account_id: Some("ig-99".into()),
        actor_person_id: Uuid::nil(),
        expires_at: Utc::now(),
    };
    let url =
        super::selected_authorization_url(Provider::Facebook, &config(), &attempt, true).unwrap();
    let scopes: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key == "scope")
        .map(|(_, value)| value.into_owned())
        .collect();
    assert_eq!(scopes.len(), 1);
    for required in [
        "pages_show_list",
        "pages_read_engagement",
        "pages_manage_posts",
        "pages_manage_engagement",
        "instagram_basic",
        "instagram_content_publish",
        "instagram_manage_comments",
        "instagram_manage_messages",
    ] {
        assert!(scopes[0].split(',').any(|scope| scope == required));
    }
    assert!(!scopes[0].contains("threads_"));
    assert_eq!(url.host_str(), Some("www.facebook.com"));
}

#[test]
fn authorization_urls_bind_state_redirect_and_supported_pkce() {
    let config = config();
    for provider in [
        Provider::Facebook,
        Provider::Instagram,
        Provider::Threads,
        Provider::Youtube,
    ] {
        let url =
            authorization_url(provider, &config, "synthetic-state", "synthetic-verifier").unwrap();
        let query = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            query.get("client_id").map(std::convert::AsRef::as_ref),
            Some("synthetic-client")
        );
        assert_eq!(
            query.get("state").map(std::convert::AsRef::as_ref),
            Some("synthetic-state")
        );
        assert_eq!(
            query.get("redirect_uri").map(std::convert::AsRef::as_ref),
            Some("https://navigator.example/social/oauth/callback")
        );
        assert_eq!(query.contains_key("code_challenge"), provider.uses_pkce());
        if provider == Provider::Threads {
            assert_eq!(
                query
                    .get("enable_fb_login")
                    .map(std::convert::AsRef::as_ref),
                Some("0")
            );
        }
        if provider == Provider::Youtube {
            assert_eq!(
                query.get("access_type").map(std::convert::AsRef::as_ref),
                Some("offline")
            );
        }
    }
}

#[test]
fn provider_scopes_must_be_granted_before_account_storage() {
    assert_eq!(
        Provider::Instagram.scopes(),
        "pages_show_list,pages_read_engagement,instagram_basic,instagram_content_publish,\
         instagram_manage_comments,instagram_manage_messages"
    );
    assert_eq!(
        Provider::Youtube.scopes(),
        "https://www.googleapis.com/auth/youtube.force-ssl https://www.googleapis.com/auth/youtube.upload"
    );
    assert!(scopes_are_sufficient(
        Provider::Facebook,
        &[
            "pages_show_list".into(),
            "pages_read_engagement".into(),
            "pages_manage_posts".into(),
            "pages_manage_engagement".into(),
        ]
    ));
    assert!(!scopes_are_sufficient(
        Provider::Facebook,
        &["pages_show_list".into()]
    ));
    assert!(scopes_are_sufficient(
        Provider::Instagram,
        &[
            "pages_show_list".into(),
            "pages_read_engagement".into(),
            "instagram_basic".into(),
            "instagram_content_publish".into(),
            "instagram_manage_comments".into(),
            "instagram_manage_messages".into(),
        ]
    ));
    // Instagram Login's scopes do not authorize the Facebook Login host.
    assert!(!scopes_are_sufficient(
        Provider::Instagram,
        &[
            "instagram_business_basic".into(),
            "instagram_business_content_publish".into(),
            "instagram_business_manage_comments".into(),
            "instagram_business_manage_messages".into(),
        ]
    ));
    let scopes = vec![
        "threads_basic".to_string(),
        "threads_content_publish".to_string(),
        "threads_manage_replies".to_string(),
    ];
    assert!(scopes_are_sufficient(Provider::Threads, &scopes));
    assert!(!scopes_are_sufficient(
        Provider::Threads,
        &["threads_basic".to_string()]
    ));
    assert!(scopes_are_sufficient(
        Provider::Youtube,
        &[
            "https://www.googleapis.com/auth/youtube.force-ssl".into(),
            "https://www.googleapis.com/auth/youtube.upload".into(),
        ]
    ));
    assert!(!scopes_are_sufficient(
        Provider::Youtube,
        &["https://www.googleapis.com/auth/youtube.upload".into()]
    ));
}

#[test]
fn oauth_config_rejects_empty_secrets_and_unsafe_redirects() {
    assert!(oauth_config_from(
        Some("id".into()),
        Some("secret".into()),
        Some("https://navigator.example/social/oauth/callback".into()),
    )
    .is_some());
    assert!(oauth_config_from(
        Some("id".into()),
        Some("secret".into()),
        Some("http://localhost/social/oauth/callback".into()),
    )
    .is_some());
    assert!(oauth_config_from(
        Some(String::new()),
        Some("secret".into()),
        Some("https://navigator.example/cb".into())
    )
    .is_none());
    assert!(oauth_config_from(
        Some("id".into()),
        Some("secret".into()),
        Some("https://navigator.example/cb#fragment".into()),
    )
    .is_none());
    assert!(oauth_config_from(
        Some("id".into()),
        Some("secret".into()),
        Some("http://example.com/cb".into()),
    )
    .is_none());
    assert!(oauth_config_from(
        None,
        Some("secret".into()),
        Some("https://navigator.example/cb".into())
    )
    .is_none());
}

#[tokio::test]
async fn grouped_meta_discovery_reports_safe_distinct_failures() {
    let cases = [
        (
            ResponseTemplate::new(403).set_body_string("private-response"),
            "Meta rejected the Page lookup",
        ),
        (
            ResponseTemplate::new(200).set_body_string("private-response"),
            "Meta returned an invalid Page response",
        ),
        (
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": []})),
            "The selected Facebook Page was not returned by Meta",
        ),
        (
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": [{"id": "selected-page", "access_token": "private-page-token",
                          "instagram_business_account": {"id": "other-instagram"}}]
            })),
            "The selected Instagram account is not linked to the selected Facebook Page",
        ),
    ];
    let token = TokenResponse {
        access_token: "synthetic-token".into(),
        refresh_token: None,
        expires_in: None,
        scope: None,
    };
    let http = reqwest::Client::new();
    for (response, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/accounts"))
            .respond_with(response)
            .mount(&server)
            .await;
        let result = super::discover_selected_meta(
            &http,
            &endpoints(&server),
            &token,
            "selected-page",
            "selected-instagram",
        )
        .await;
        assert!(matches!(result, Err(message) if message == expected));
    }
    let server = MockServer::start().await;
    let mut bad_endpoint = endpoints(&server);
    bad_endpoint.meta_graph = "invalid-url".into();
    let result = super::discover_selected_meta(
        &http,
        &bad_endpoint,
        &token,
        "selected-page",
        "selected-instagram",
    )
    .await;
    assert!(matches!(
        result,
        Err("Meta could not be reached for the Page lookup")
    ));
}

#[tokio::test]
async fn grouped_meta_discovery_verifies_the_selected_pair_after_pagination() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/me/accounts"))
        .and(|request: &wiremock::Request| {
            !request.url.query_pairs().any(|(key, _)| key == "after")
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [],
            "paging": {
                "next": "https://other.example/unused",
                "cursors": {"after": "synthetic-cursor"}
            }
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/me/accounts"))
        .and(wiremock::matchers::query_param("after", "synthetic-cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{"id": "selected-page", "access_token": "synthetic-page-token",
                      "instagram_business_account": {"id": "selected-instagram"}}]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let token = TokenResponse {
        access_token: "synthetic-token".into(),
        refresh_token: None,
        expires_in: None,
        scope: None,
    };

    let identities = super::discover_selected_meta(
        &reqwest::Client::new(),
        &endpoints(&server),
        &token,
        "selected-page",
        "selected-instagram",
    )
    .await
    .unwrap();

    assert_eq!(identities.len(), 2);
    assert!(identity_matches(
        Provider::Facebook,
        &identities[0],
        "selected-page"
    ));
    assert!(identity_matches(
        Provider::Instagram,
        &identities[1],
        "selected-instagram"
    ));
}

#[tokio::test]
async fn facebook_token_permissions_and_pages_are_discovered() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "user-token",
            "expires_in": 3600
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/oauth/access_token"))
        .and(wiremock::matchers::query_param(
            "grant_type",
            "fb_exchange_token",
        ))
        .and(wiremock::matchers::query_param(
            "fb_exchange_token",
            "user-token",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "long-user-token",
            "token_type": "bearer",
            "expires_in": 5_183_944
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/me/permissions"))
        .and(wiremock::matchers::query_param(
            "access_token",
            "long-user-token",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                {"permission": "pages_show_list", "status": "granted"},
                {"permission": "pages_read_engagement", "status": "granted"},
                {"permission": "pages_manage_posts", "status": "granted"},
                {"permission": "pages_manage_engagement", "status": "granted"},
                {"permission": "email", "status": "declined"}
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/me/accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{
                "id": "page-1",
                "name": "Synthetic Page",
                "access_token": "page-token",
                "instagram_business_account": {"id": "ig-1", "username": "synthetic"}
            }]
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let endpoints = endpoints(&server);
    let token = exchange_code(
        &http,
        Provider::Facebook,
        &config(),
        &endpoints,
        "auth-code",
        "verifier",
    )
    .await
    .unwrap();
    assert_eq!(token.access_token, "user-token");
    let token = super::exchange_meta_short_token(&http, &config(), &endpoints, &token.access_token)
        .await
        .unwrap();
    assert_eq!(token.access_token, "long-user-token");
    let scopes = granted_scopes(&http, Provider::Facebook, &endpoints, &token)
        .await
        .unwrap();
    assert!(scopes_are_sufficient(Provider::Facebook, &scopes));
    assert!(!scopes.iter().any(|scope| scope == "email"));
    let facebook = discover_identities(&http, Provider::Facebook, &endpoints, &token)
        .await
        .unwrap();
    assert_eq!(facebook.len(), 1);
    assert_eq!(facebook[0].account_id, "page-1");
    let instagram = discover_identities(&http, Provider::Instagram, &endpoints, &token)
        .await
        .unwrap();
    assert_eq!(instagram[0].account_id, "ig-1");
    assert_eq!(instagram[0].display_name.as_deref(), Some("synthetic"));
}

#[tokio::test]
async fn threads_scopes_come_from_debug_token_and_youtube_from_its_scope_string() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "threads-short",
            "scope": "threads_basic,threads_content_publish,threads_manage_replies"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "threads-long",
            "expires_in": 5_184_000
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "youtube-token",
            "refresh_token": "youtube-refresh",
            "scope": "https://www.googleapis.com/auth/youtube.force-ssl https://www.googleapis.com/auth/youtube.upload"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "threads-user",
            "username": "synthetic-threads"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/debug_token"))
        .and(wiremock::matchers::query_param(
            "input_token",
            "threads-long",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "type": "USER",
                "is_valid": true,
                "scopes": ["threads_basic", "threads_content_publish", "threads_manage_replies"]
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"id": "yt-1", "snippet": {"title": "Synthetic Channel"}}]
        })))
        .mount(&server)
        .await;

    let http = reqwest::Client::new();
    let endpoints = endpoints(&server);
    let short = exchange_code(
        &http,
        Provider::Threads,
        &config(),
        &endpoints,
        "auth-code",
        "verifier",
    )
    .await
    .unwrap();
    let long = exchange_threads_short_token(&http, &config(), &endpoints, &short.access_token)
        .await
        .unwrap();
    assert_eq!(long.access_token, "threads-long");
    assert_eq!(long.scope, None);
    let threads_scopes = granted_scopes(&http, Provider::Threads, &endpoints, &long)
        .await
        .unwrap();
    assert!(scopes_are_sufficient(Provider::Threads, &threads_scopes));
    let threads = discover_identities(&http, Provider::Threads, &endpoints, &long)
        .await
        .unwrap();
    assert_eq!(threads[0].account_id, "threads-user");
    let youtube = exchange_code(
        &http,
        Provider::Youtube,
        &config(),
        &endpoints,
        "auth-code",
        "verifier",
    )
    .await
    .unwrap();
    let scopes = granted_scopes(&http, Provider::Youtube, &endpoints, &youtube)
        .await
        .unwrap();
    assert!(scopes_are_sufficient(Provider::Youtube, &scopes));
    let channels = discover_identities(&http, Provider::Youtube, &endpoints, &youtube)
        .await
        .unwrap();
    assert_eq!(channels[0].account_id, "yt-1");
}

#[tokio::test]
async fn callback_connects_a_facebook_page() {
    let db = mem_surreal().await;
    let (admin, brand_key) = practice(&db).await;
    let server = MockServer::start().await;
    mount_facebook_oauth(
        &server,
        facebook_permissions(),
        Some(serde_json::json!({
            "data": [{
                "id": "page-99",
                "name": "Connected Page",
                "access_token": "page-token"
            }, {"id": "other-brand-page", "name": "Other Brand", "access_token": "other-token"}]
        })),
    )
    .await;
    let mut state = crate::test_support::app_state(db.clone()).await;
    state.runtime_kms = std::sync::Arc::new(cloud::FakeKms::new("social-oauth"));
    let (status, location) = finish(
        state.clone(),
        CallbackQuery {
            auth_code: None,
            state: Some(begin_facebook(&db, admin, &brand_key).await),
            code: Some("auth-code".into()),
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(location.contains("social_connected=1"), "{location}");
    let accounts =
        store::social_accounts::for_provider_identity(&db, SocialProvider::Facebook, "page-99")
            .await
            .unwrap();
    assert_eq!(accounts.len(), 1);
    assert!(store::social_accounts::for_provider_identity(
        &db,
        SocialProvider::Facebook,
        "other-brand-page"
    )
    .await
    .unwrap()
    .is_empty());
    store::social_accounts::revoke(
        &db,
        Role::Admin,
        admin,
        &brand_key,
        SocialProvider::Facebook,
        "page-99",
    )
    .await
    .unwrap();
    let (_, location) = finish(
        state,
        CallbackQuery {
            auth_code: None,
            state: Some(begin_facebook(&db, admin, &brand_key).await),
            code: Some("reconnect-code".into()),
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert!(location.contains("social_connected=1"));
    let reconnected =
        store::social_accounts::for_provider_identity(&db, SocialProvider::Facebook, "page-99")
            .await
            .unwrap();
    assert_eq!(reconnected.len(), 1);
    assert_eq!(reconnected[0].id, accounts[0].id);
    assert_eq!(reconnected[0].status, "connected");
    assert!(reconnected[0].revoked_at.is_none());
}

#[tokio::test]
async fn callback_refuses_missing_state() {
    let db = mem_surreal().await;
    let server = MockServer::start().await;
    let missing = complete_authorization(
        crate::test_support::app_state(db).await,
        CallbackQuery {
            auth_code: None,
            state: None,
            code: None,
            error: None,
        },
        Some(config()),
        &endpoints(&server),
    )
    .await
    .into_response();
    assert_eq!(missing.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn callback_refuses_cancelled_authorization() {
    let db = mem_surreal().await;
    let (admin, brand_key) = practice(&db).await;
    let server = MockServer::start().await;
    let (status, location) = finish(
        crate::test_support::app_state(db.clone()).await,
        CallbackQuery {
            auth_code: None,
            state: Some(begin_facebook(&db, admin, &brand_key).await),
            code: None,
            error: Some("access_denied".into()),
        },
        &endpoints(&server),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(location.contains("cancelled"), "{location}");
}

#[tokio::test]
async fn callback_refuses_insufficient_scopes() {
    let db = mem_surreal().await;
    let (admin, brand_key) = practice(&db).await;
    let server = MockServer::start().await;
    mount_facebook_oauth(
        &server,
        serde_json::json!({"data": [{"permission": "pages_show_list", "status": "granted"}]}),
        None,
    )
    .await;
    let (status, location) = finish(
        crate::test_support::app_state(db.clone()).await,
        CallbackQuery {
            auth_code: None,
            state: Some(begin_facebook(&db, admin, &brand_key).await),
            code: Some("auth-code".into()),
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(location.contains("required%20permissions"), "{location}");
}

#[tokio::test]
async fn callback_refuses_missing_identities() {
    let db = mem_surreal().await;
    let (admin, brand_key) = practice(&db).await;
    let server = MockServer::start().await;
    mount_facebook_oauth(
        &server,
        facebook_permissions(),
        Some(serde_json::json!({"data": []})),
    )
    .await;
    let (status, location) = finish(
        crate::test_support::app_state(db.clone()).await,
        CallbackQuery {
            auth_code: None,
            state: Some(begin_facebook(&db, admin, &brand_key).await),
            code: Some("auth-code".into()),
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        location.contains("eligible%20provider%20account"),
        "{location}"
    );
}

#[tokio::test]
async fn cancelled_consent_keeps_the_saved_destination_and_replay_is_rejected() {
    let db = mem_surreal().await;
    let (admin, brand) = practice(&db).await;
    let raw_state = begin_facebook(&db, admin, &brand).await;
    let server = MockServer::start().await;
    let app = crate::test_support::app_state(db.clone()).await;
    let (_, location) = finish(
        app.clone(),
        CallbackQuery {
            state: Some(raw_state.clone()),
            code: None,
            auth_code: None,
            error: Some("access_denied".into()),
        },
        &endpoints(&server),
    )
    .await;
    assert!(location.contains("cancelled"));
    assert_eq!(
        store::social_account_selections::for_brand(&db, &brand)
            .await
            .unwrap()[0]
            .provider_account_id,
        "page-99"
    );
    let (status, _) = finish(
        app,
        CallbackQuery {
            state: Some(raw_state),
            code: Some("synthetic".into()),
            auth_code: None,
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn shared_meta_consent_connects_only_the_selected_linked_pair_and_rejects_replay() {
    for (linked, grant_instagram) in [("ig-99", true), ("wrong-link", true), ("ig-99", false)] {
        let db = mem_surreal().await;
        let (admin, brand) = practice(&db).await;
        let server = MockServer::start().await;
        let mut permissions = facebook_permissions();
        for scope in SocialProvider::Instagram
            .required_scopes()
            .iter()
            .filter(|_| grant_instagram)
        {
            permissions["data"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({"permission":scope,"status":"granted"}));
        }
        mount_facebook_oauth(&server, permissions, Some(serde_json::json!({"data":[
            {"id":"page-99","name":"Synthetic Page","access_token":"synthetic-page-token","instagram_business_account":{"id":linked,"username":"SyntheticBrand"}},
            {"id":"other-page","name":"Other Page","access_token":"other-token","instagram_business_account":{"id":"other-ig","username":"OtherBrand"}}
        ]}))).await;
        let attempt = store::social_oauth_attempts::begin(
            &db,
            Role::Admin,
            admin,
            &brand,
            "facebook",
            Utc::now(),
        )
        .await
        .unwrap();
        store::social_oauth_attempts::bind_account(&db, &attempt, "page-99")
            .await
            .unwrap();
        store::social_oauth_attempts::bind_instagram(&db, &attempt, "ig-99")
            .await
            .unwrap();
        let mut app = crate::test_support::app_state(db.clone()).await;
        app.runtime_kms = std::sync::Arc::new(cloud::FakeKms::new("shared-meta"));
        let query = || CallbackQuery {
            state: Some(attempt.state.clone()),
            code: Some("synthetic-code".into()),
            auth_code: None,
            error: None,
        };
        let (_, location) = finish(app.clone(), query(), &endpoints(&server)).await;
        let accounts = store::social_accounts::for_actor(&db, Role::Admin, admin)
            .await
            .unwrap();
        if linked == "ig-99" && grant_instagram {
            assert!(location.contains("social_connected=2"), "{location}");
            assert_eq!(accounts.len(), 2);
            assert!(accounts
                .iter()
                .any(|a| a.provider == SocialProvider::Facebook
                    && a.provider_account_id == "page-99"));
            assert!(accounts.iter().any(
                |a| a.provider == SocialProvider::Instagram && a.provider_account_id == "ig-99"
            ));
        } else {
            assert!(
                location.contains(if grant_instagram {
                    "not%20linked"
                } else {
                    "required%20permissions"
                }),
                "{location}"
            );
            assert!(accounts.is_empty());
        }
        assert_eq!(
            finish(app, query(), &endpoints(&server)).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|request| request.method == "POST"
                    && request.url.path() == "/oauth/access_token")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn usable_saved_connection_is_reused_and_selection_changes_do_not_retarget_it() {
    use axum::extract::{Path, State};
    use axum::Extension;
    let db = mem_surreal().await;
    let (admin, brand) = practice(&db).await;
    let kms = cloud::FakeKms::new("reuse");
    save_facebook_selection(&db, admin, &brand, "page-99").await;
    let account = store::social_accounts::connect(
        &db,
        store::social_accounts::ConnectSocialAccount {
            actor_role: Role::Admin,
            actor_person_id: admin,
            brand_key: &brand,
            provider: SocialProvider::Facebook,
            provider_account_id: "page-99",
            display_name: None,
            scopes: &SocialProvider::Facebook
                .required_scopes()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            expires_at: None,
            access_token: "synthetic-token",
            refresh_token: None,
        },
        &kms,
    )
    .await
    .unwrap();
    let mut session = crate::session::SessionData::fresh("admin@example.com", Role::Admin);
    session.person_id = Some(admin);
    let form = || {
        axum::Form(ConnectForm {
            provider_account_id: Some("ignored-unsaved-account".into()),
            _csrf: None,
        })
    };
    let response = connect_start(
        State(admin_state(db.clone()).await),
        Some(Extension(session.clone())),
        Path((brand.clone(), "facebook".into())),
        form(),
    )
    .await;
    assert_eq!(
        location_of(&response).1,
        format!("/app/admin/brands/{brand}/edit?social_reused=true")
    );
    save_facebook_selection(&db, admin, &brand, "new-page").await;
    assert_eq!(
        store::social_accounts::selected_for_brand(&db, &brand, std::slice::from_ref(&account.id))
            .await
            .unwrap()[0]
            .provider_account_id,
        "page-99"
    );
    store::social_accounts::revoke(
        &db,
        Role::Admin,
        admin,
        &brand,
        SocialProvider::Facebook,
        "page-99",
    )
    .await
    .unwrap();
    save_facebook_selection(&db, admin, &brand, "page-99").await;
    let response = super::connect_start_configured(
        State(admin_state(db).await),
        Some(Extension(session)),
        Path((brand.clone(), "facebook".into())),
        form(),
        Some(config()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn token_response_deserializes_optional_scope_and_refresh() {
    let token: TokenResponse = serde_json::from_value(serde_json::json!({
        "access_token": "tok",
        "refresh_token": "ref",
        "scope": "a b"
    }))
    .unwrap();
    assert_eq!(token.refresh_token.as_deref(), Some("ref"));
    assert_eq!(token.scope.as_deref(), Some("a b"));
}

async fn admin_state(db: store::surreal::SurrealDb) -> crate::admin::AdminState {
    let app = crate::test_support::app_state(db).await;
    crate::admin::AdminState {
        surreal: app.surreal,
        workflow_runtime: app.workflow_runtime,
        signature_provider: app.signature_provider,
        retainer_intake_questionnaire: workflows::retainer_intake_questionnaire(),
        questionnaire_runtime: app.questionnaire_runtime,
        storage: app.storage,
        assets_storage: app.assets_storage,
        forms_registry: app.forms_registry,
        email: app.email,
        billing_provider: app.billing_provider,
        contract_reviewer: app.contract_reviewer,
        bootstrap_owner_email: app.bootstrap_owner_email,
        on_call_lawyer_email: app.on_call_lawyer_email,
        bootstrap_company: crate::admin::bootstrap_company_from_env(),
        sessions: app.sessions,
        secure_cookies: false,
        attachment_scanner: app.attachment_scanner,
        runtime_kms: app.runtime_kms,
        twilio_registry: None,
    }
}

#[tokio::test]
async fn connect_and_disconnect_refuse_anonymous_or_unconfigured_attempts() {
    use axum::extract::{Path, State};
    use axum::Extension;

    let db = mem_surreal().await;
    let state = admin_state(db).await;
    let missing = connect_start(
        State(state.clone()),
        None,
        Path(("neon".into(), "facebook".into())),
        axum::Form(ConnectForm {
            _csrf: None,
            provider_account_id: None,
        }),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let mut session = crate::session::SessionData::fresh("admin@example.com", Role::Admin);
    session.person_id = Some(Uuid::now_v7());
    let (status, location) = location_of(
        &connect_start(
            State(state.clone()),
            Some(Extension(session.clone())),
            Path(("neon".into(), "carrier-pigeon".into())),
            axum::Form(ConnectForm {
                _csrf: None,
                provider_account_id: None,
            }),
        )
        .await
        .into_response(),
    );
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(location.contains("Unsupported"), "{location}");
    let (status, location) = location_of(
        &connect_start(
            State(state.clone()),
            Some(Extension(session.clone())),
            Path(("neon".into(), "facebook".into())),
            axum::Form(ConnectForm {
                _csrf: None,
                provider_account_id: None,
            }),
        )
        .await
        .into_response(),
    );
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        location.contains("Choose%20this%20Brand") || location.contains("configured"),
        "{location}"
    );
    assert_eq!(
        disconnect(
            State(state.clone()),
            None,
            Path(("neon".into(), "facebook".into(), "page-1".into())),
            axum::Form(DisconnectForm { _csrf: None }),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND
    );
    let (status, location) = location_of(
        &disconnect(
            State(state),
            Some(Extension(session)),
            Path(("neon".into(), "facebook".into(), "page-1".into())),
            axum::Form(DisconnectForm { _csrf: None }),
        )
        .await
        .into_response(),
    );
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        location.contains("disconnected") || location.contains("error="),
        "{location}"
    );
}

#[tokio::test]
async fn callback_refuses_an_unadmitted_actor_and_a_missing_code() {
    let db = mem_surreal().await;
    let (admin, brand_key) = practice(&db).await;
    store::persons::set_admitted(&db, admin, false)
        .await
        .unwrap();
    let server = MockServer::start().await;
    let (status, location) = finish(
        crate::test_support::app_state(db.clone()).await,
        CallbackQuery {
            auth_code: None,
            state: Some(begin_facebook(&db, admin, &brand_key).await),
            code: Some("auth-code".into()),
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(location.contains("not%20admitted"), "{location}");

    let (admin, brand_key) = practice(&db).await;
    let state = begin_facebook(&db, admin, &brand_key).await;
    let (status, location) = finish(
        crate::test_support::app_state(db).await,
        CallbackQuery {
            auth_code: None,
            state: Some(state),
            code: None,
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(location.contains("authorization%20code"), "{location}");
}

#[tokio::test]
async fn callback_refuses_a_failed_code_exchange() {
    let db = mem_surreal().await;
    let (admin, brand_key) = practice(&db).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/access_token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
            "error": "invalid_grant"
        })))
        .mount(&server)
        .await;
    let (status, location) = finish(
        crate::test_support::app_state(db.clone()).await,
        CallbackQuery {
            auth_code: None,
            state: Some(begin_facebook(&db, admin, &brand_key).await),
            code: Some("auth-code".into()),
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        location.contains("could%20not%20be%20completed"),
        "{location}"
    );
}

#[tokio::test]
async fn connect_start_redirects_when_facebook_is_configured() {
    use axum::extract::{Path, State};
    use axum::Extension;

    let db = mem_surreal().await;
    let (admin, brand_key) = practice(&db).await;
    let mut session = crate::session::SessionData::fresh("admin@example.com", Role::Admin);
    session.person_id = Some(admin);
    store::social_account_selections::save(
        &db,
        Role::Admin,
        admin,
        &brand_key,
        SocialProvider::Facebook,
        "page-99",
    )
    .await
    .unwrap();
    let response = super::connect_start_configured(
        State(admin_state(db.clone()).await),
        Some(Extension(session.clone())),
        Path((brand_key.clone(), "facebook".into())),
        axum::Form(ConnectForm {
            _csrf: None,
            provider_account_id: Some("page-99".into()),
        }),
        Some(config()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("http-equiv=\"refresh\""));
    assert!(body.contains("https://www.facebook.com/v26.0/dialog/oauth?"));
    assert!(body.contains("Continue to provider"));
    assert!(body.contains("Cancel and return to Brand"));

    let kms = cloud::FakeKms::new("social-disconnect");
    store::social_accounts::connect(
        &db,
        store::social_accounts::ConnectSocialAccount {
            actor_role: Role::Admin,
            actor_person_id: admin,
            brand_key: &brand_key,
            provider: SocialProvider::Facebook,
            provider_account_id: "page-disconnect",
            display_name: Some("Disconnect Page"),
            scopes: &["pages_manage_posts".into()],
            expires_at: None,
            access_token: "synthetic-access",
            refresh_token: None,
        },
        &kms,
    )
    .await
    .unwrap();
    let mut state = admin_state(db).await;
    state.runtime_kms = std::sync::Arc::new(kms);
    let (status, location) = location_of(
        &disconnect(
            State(state),
            Some(Extension(session)),
            Path((brand_key, "facebook".into(), "page-disconnect".into())),
            axum::Form(DisconnectForm { _csrf: None }),
        )
        .await
        .into_response(),
    );
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(location.contains("social_disconnected=1"), "{location}");
}

#[tokio::test]
async fn callback_connects_a_youtube_channel() {
    let db = mem_surreal().await;
    let (admin, brand_key) = practice(&db).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "youtube-token",
            "refresh_token": "youtube-refresh",
            "scope": "https://www.googleapis.com/auth/youtube.force-ssl https://www.googleapis.com/auth/youtube.upload"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/channels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [{"id": "yt-99", "snippet": {"title": "Connected Channel"}}]
        })))
        .mount(&server)
        .await;
    let state_token = begin_provider(&db, admin, &brand_key, "youtube").await;
    let mut state = crate::test_support::app_state(db.clone()).await;
    state.runtime_kms = std::sync::Arc::new(cloud::FakeKms::new("social-youtube"));
    let (status, location) = finish(
        state,
        CallbackQuery {
            auth_code: None,
            state: Some(state_token),
            code: Some("auth-code".into()),
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(location.contains("social_connected=1"), "{location}");
    let accounts =
        store::social_accounts::for_provider_identity(&db, SocialProvider::Youtube, "yt-99")
            .await
            .unwrap();
    assert_eq!(accounts.len(), 1);
}

async fn begin_provider(
    db: &store::surreal::SurrealDb,
    admin: Uuid,
    brand_key: &str,
    provider: &str,
) -> String {
    let attempt = store::social_oauth_attempts::begin(
        db,
        Role::Admin,
        admin,
        brand_key,
        provider,
        Utc::now(),
    )
    .await
    .unwrap();
    let target = match provider {
        "instagram" => "ig-99",
        "threads" => "threads-99",
        _ => "yt-99",
    };
    store::social_oauth_attempts::bind_account(db, &attempt, target)
        .await
        .unwrap();
    attempt.state
}

#[tokio::test]
async fn callback_connects_an_instagram_account() {
    let db = mem_surreal().await;
    let (admin, brand_key) = practice(&db).await;
    let server = MockServer::start().await;
    mount_facebook_oauth(
        &server,
        serde_json::json!({
            "data": [
                {"permission": "pages_show_list", "status": "granted"},
                {"permission": "pages_read_engagement", "status": "granted"},
                {"permission": "instagram_basic", "status": "granted"},
                {"permission": "instagram_content_publish", "status": "granted"},
                {"permission": "instagram_manage_comments", "status": "granted"},
                {"permission": "instagram_manage_messages", "status": "granted"}
            ]
        }),
        Some(serde_json::json!({
            "data": [{
                "id": "page-ig",
                "name": "IG Page",
                "access_token": "page-token",
                "instagram_business_account": {"id": "ig-99", "username": "connected-ig"}
            }]
        })),
    )
    .await;
    let mut state = crate::test_support::app_state(db.clone()).await;
    state.runtime_kms = std::sync::Arc::new(cloud::FakeKms::new("social-instagram"));
    let (status, location) = finish(
        state,
        CallbackQuery {
            auth_code: None,
            state: Some(begin_provider(&db, admin, &brand_key, "instagram").await),
            code: Some("auth-code".into()),
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(location.contains("social_connected=1"), "{location}");
    let accounts =
        store::social_accounts::for_provider_identity(&db, SocialProvider::Instagram, "ig-99")
            .await
            .unwrap();
    assert_eq!(accounts.len(), 1);
    // A Page token from a long-lived user token does not expire.
    assert_eq!(accounts[0].expires_at, None);
    assert!(SocialProvider::Instagram
        .missing_scopes(&accounts[0].scopes)
        .is_empty());
}

#[tokio::test]
async fn callback_connects_a_threads_identity() {
    let db = mem_surreal().await;
    let (admin, brand_key) = practice(&db).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "threads-short",
            "scope": "threads_basic,threads_content_publish,threads_manage_replies"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "threads-long",
            "token_type": "bearer",
            "expires_in": 5_184_000
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/debug_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "is_valid": true,
                "scopes": ["threads_basic", "threads_content_publish", "threads_manage_replies"]
            }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "threads-99",
            "username": "connected-threads"
        })))
        .mount(&server)
        .await;
    let mut state = crate::test_support::app_state(db.clone()).await;
    state.runtime_kms = std::sync::Arc::new(cloud::FakeKms::new("social-threads"));
    let (status, location) = finish(
        state,
        CallbackQuery {
            auth_code: None,
            state: Some(begin_provider(&db, admin, &brand_key, "threads").await),
            code: Some("auth-code".into()),
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(location.contains("social_connected=1"), "{location}");
    let accounts =
        store::social_accounts::for_provider_identity(&db, SocialProvider::Threads, "threads-99")
            .await
            .unwrap();
    assert_eq!(accounts.len(), 1);
    assert!(accounts[0].expires_at.unwrap() > Utc::now() + chrono::Duration::days(50));
}

#[tokio::test]
async fn callback_refuses_a_threads_token_missing_a_permission() {
    let db = mem_surreal().await;
    let (admin, brand_key) = practice(&db).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "threads-short"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/access_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "threads-long",
            "expires_in": 5_184_000
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/debug_token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {"is_valid": true, "scopes": ["threads_basic"]}
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1.0/me"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let state = crate::test_support::app_state(db.clone()).await;
    let (_, location) = finish(
        state,
        CallbackQuery {
            auth_code: None,
            state: Some(begin_provider(&db, admin, &brand_key, "threads").await),
            code: Some("auth-code".into()),
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert!(
        location.contains("did%20not%20grant%20the%20required%20permissions"),
        "{location}"
    );
}
#[tokio::test]
async fn linkedin_exchange_verifies_granted_scopes_and_expiry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "synthetic-token", "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "active": true, "client_id": "synthetic-client",
            "expires_at": Utc::now().timestamp() + 1800,
            "scope": "r_organization_admin,w_organization_social"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let token = exchange_code(
        &reqwest::Client::new(),
        Provider::Linkedin,
        &config(),
        &endpoints(&server),
        "synthetic-code",
        "unused",
    )
    .await
    .unwrap();
    let scopes = granted_scopes(
        &reqwest::Client::new(),
        Provider::Linkedin,
        &endpoints(&server),
        &token,
    )
    .await
    .unwrap();
    assert!(scopes_are_sufficient(Provider::Linkedin, &scopes));
    assert!(token.expires_in.unwrap() <= 1800);
    assert!(token.refresh_token.is_none());
    server.verify().await;
}

#[tokio::test]
async fn linkedin_discovery_accepts_only_approved_company_administrators() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/organizationAcls"))
        .and(wiremock::matchers::header("LinkedIn-Version", "202609"))
        .and(wiremock::matchers::header("X-Restli-Protocol-Version", "2.0.0"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "elements": [
                {"organizationTarget": "urn:li:organization:1001", "role": "ADMINISTRATOR", "state": "APPROVED"},
                {"organization": "urn:li:organization:1002", "role": "ADMINISTRATOR", "state": "REVOKED"},
                {"organization": "urn:li:organization:1003", "role": "ANALYST", "state": "APPROVED"}
            ]
        }))).expect(1).mount(&server).await;
    let token = TokenResponse {
        access_token: "synthetic-token".into(),
        refresh_token: None,
        expires_in: Some(1800),
        scope: None,
    };
    let identities = discover_identities(
        &reqwest::Client::new(),
        Provider::Linkedin,
        &endpoints(&server),
        &token,
    )
    .await
    .unwrap();
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].account_id, "1001");
    server.verify().await;
}

#[tokio::test]
async fn linkedin_callback_stores_only_the_company_chosen_for_the_brand() {
    let db = mem_surreal().await;
    let (actor, brand) = practice(&db).await;
    let attempt = store::social_oauth_attempts::begin(
        &db,
        Role::Admin,
        actor,
        &brand,
        "linkedin",
        Utc::now(),
    )
    .await
    .unwrap();
    store::social_oauth_attempts::bind_account(&db, &attempt, "1002")
        .await
        .unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "synthetic-token", "expires_in": 3600
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST")).and(path("/introspect"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "active": true, "client_id": "synthetic-client", "expires_at": Utc::now().timestamp() + 3600,
            "scope": "r_organization_admin,w_organization_social"
        }))).mount(&server).await;
    Mock::given(method("GET")).and(path("/organizationAcls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"elements": [
            {"organization": "urn:li:organization:1001", "role": "ADMINISTRATOR", "state": "APPROVED"},
            {"organization": "urn:li:organization:1002", "role": "ADMINISTRATOR", "state": "APPROVED"}
        ]}))).mount(&server).await;
    let mut state = crate::test_support::app_state(db.clone()).await;
    state.runtime_kms = std::sync::Arc::new(cloud::FakeKms::new("linkedin-oauth"));
    let response = complete_authorization(
        state,
        CallbackQuery {
            auth_code: None,
            state: Some(attempt.state),
            code: Some("synthetic-code".into()),
            error: None,
        },
        Some(config()),
        &endpoints(&server),
    )
    .await;
    assert!(response.headers()[LOCATION]
        .to_str()
        .unwrap()
        .contains("social_connected=1"));
    assert!(store::social_accounts::connected_account(
        &db,
        &brand,
        SocialProvider::Linkedin,
        "1001"
    )
    .await
    .unwrap()
    .is_none());
    assert!(store::social_accounts::connected_account(
        &db,
        &brand,
        SocialProvider::Linkedin,
        "1002"
    )
    .await
    .unwrap()
    .is_some());
}

#[tokio::test]
async fn x_pkce_exchange_and_identity_are_provider_verified() {
    let server = MockServer::start().await;
    let config = OAuthConfig {
        client_id: "x-id".into(),
        client_secret: "x-secret".into(),
        redirect_uri: "https://example.test/social/oauth/callback".into(),
    };
    let url = authorization_url(Provider::X, &config, "state", "verifier").unwrap();
    assert!(url
        .query_pairs()
        .any(|(key, value)| key == "code_challenge_method" && value == "S256"));
    Mock::given(method("POST")).and(path("/oauth2/token"))
        .and(wiremock::matchers::header("authorization", "Basic eC1pZDp4LXNlY3JldA=="))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "synthetic-token", "refresh_token": "synthetic-refresh", "expires_in": 7200,
            "scope": "tweet.read tweet.write users.read offline.access"
        }))).expect(1).mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/users/me"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data":{"id":"1002","username":"SyntheticBrand"}}),
            ),
        )
        .mount(&server)
        .await;
    let token = exchange_code(
        &reqwest::Client::new(),
        Provider::X,
        &config,
        &endpoints(&server),
        "code",
        "verifier",
    )
    .await
    .unwrap();
    let identities = discover_identities(
        &reqwest::Client::new(),
        Provider::X,
        &endpoints(&server),
        &token,
    )
    .await
    .unwrap();
    assert_eq!(identities[0].account_id, "1002");
    assert_eq!(
        identities[0].display_name.as_deref(),
        Some("SyntheticBrand")
    );
    assert!(scopes_are_sufficient(
        Provider::X,
        &granted_scopes(
            &reqwest::Client::new(),
            Provider::X,
            &endpoints(&server),
            &token
        )
        .await
        .unwrap()
    ));
    assert!(!scopes_are_sufficient(Provider::X, &["tweet.write".into()]));
    server.verify().await;
}

#[test]
fn x_selection_is_explicit_case_insensitive_and_cannot_import_another_brand() {
    let identity = ConnectedIdentity {
        provider: Provider::X,
        account_id: "1002".into(),
        display_name: Some("SyntheticBrand".into()),
        access_token: "synthetic-token".into(),
    };
    assert!(identity_matches(Provider::X, &identity, "syntheticbrand"));
    assert!(!identity_matches(Provider::X, &identity, "OtherBrand"));
    assert!(!identity_matches(
        Provider::Linkedin,
        &identity,
        "SyntheticBrand"
    ));
}

#[tokio::test]
async fn connections_without_checkbox_still_require_saved_destinations() {
    use axum::extract::{Path, State};
    use axum::Extension;
    let db = mem_surreal().await;
    let (admin, brand) = practice(&db).await;
    let mut session = crate::session::SessionData::fresh("admin@example.com", Role::Admin);
    session.person_id = Some(admin);
    for provider in ["x", "tiktok"] {
        let (_, location) = location_of(
            &connect_start(
                State(admin_state(db.clone()).await),
                Some(Extension(session.clone())),
                Path((brand.clone(), provider.into())),
                axum::Form(ConnectForm {
                    provider_account_id: Some("SyntheticBrand".into()),
                    _csrf: None,
                }),
            )
            .await
            .into_response(),
        );
        assert!(
            location.contains("save%20it%20before%20authorizing"),
            "{provider}: {location}"
        );
    }
}
#[tokio::test]
async fn missing_brand_target_refuses_start_and_legacy_callback_without_provider_requests() {
    use axum::extract::{Path, State};
    use axum::Extension;
    let db = mem_surreal().await;
    let (admin, brand) = practice(&db).await;
    let server = MockServer::start().await;
    let mut session = crate::session::SessionData::fresh("admin@example.com", Role::Admin);
    session.person_id = Some(admin);
    for target in [None, Some("   ".to_string())] {
        let (_, location) = location_of(
            &connect_start(
                State(admin_state(db.clone()).await),
                Some(Extension(session.clone())),
                Path((brand.clone(), "facebook".into())),
                axum::Form(ConnectForm {
                    provider_account_id: target,
                    _csrf: None,
                }),
            )
            .await,
        );
        assert!(location.contains("Choose%20this%20Brand"), "{location}");
    }
    let attempt = store::social_oauth_attempts::begin(
        &db,
        Role::Admin,
        admin,
        &brand,
        "facebook",
        Utc::now(),
    )
    .await
    .unwrap();
    let (_, location) = finish(
        crate::test_support::app_state(db.clone()).await,
        CallbackQuery {
            auth_code: None,
            code: Some("synthetic-code".into()),
            state: Some(attempt.state),
            error: None,
        },
        &endpoints(&server),
    )
    .await;
    assert!(location.contains("Choose%20this%20Brand"), "{location}");
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(store::social_accounts::for_provider_identity(
        &db,
        SocialProvider::Facebook,
        "page-99"
    )
    .await
    .unwrap()
    .is_empty());
}

#[test]
fn threads_target_matches_only_verified_id_or_exact_username() {
    let identity = ConnectedIdentity {
        provider: Provider::Threads,
        account_id: "threads-id".into(),
        display_name: Some("brand_username".into()),
        access_token: "synthetic".into(),
    };
    assert!(identity_matches(Provider::Threads, &identity, "threads-id"));
    assert!(identity_matches(
        Provider::Threads,
        &identity,
        "brand_username"
    ));
    assert!(!identity_matches(
        Provider::Threads,
        &identity,
        "other_brand"
    ));
    assert!(!identity_matches(
        Provider::Threads,
        &identity,
        "BRAND_USERNAME"
    ));
    assert!(!identity_matches(
        Provider::Facebook,
        &identity,
        "brand_username"
    ));
}
