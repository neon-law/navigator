use super::{
    authorization_url, discover_identities, exchange_code, exchange_threads_short_token,
    granted_scopes, oauth_config_from, scopes_are_sufficient, OAuthConfig, Provider,
    ProviderEndpoints, TokenResponse,
};
use super::{identity_matches, ConnectedIdentity};
use chrono::Utc;
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
