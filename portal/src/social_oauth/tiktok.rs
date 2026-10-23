//! Accounts API authorization is verified against the provider's token
//! inspector before one selected Brand identity can be stored.

use serde::Deserialize;

use super::{ConnectedIdentity, OAuthConfig, Provider, TokenResponse};

#[derive(Deserialize)]
struct Envelope<T> {
    code: i64,
    data: Option<T>,
}

#[derive(Deserialize)]
struct GrantResponse {
    access_token: String,
    refresh_token: String,
    expires_in: i64,
    open_id: String,
    scope: String,
}

#[derive(Deserialize)]
struct Inspection {
    app_id: String,
    creator_id: String,
    scope: String,
}

#[derive(Deserialize)]
struct Profile {
    username: String,
    display_name: String,
}

async fn read<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> Result<T, String> {
    if !response.status().is_success() {
        return Err("TikTok could not verify the authorization".into());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "TikTok response could not be read")?;
    if bytes.len() > 64 * 1024 {
        return Err("TikTok response is too large".into());
    }
    let envelope: Envelope<T> =
        serde_json::from_slice(&bytes).map_err(|_| "TikTok response is invalid")?;
    if envelope.code != 0 {
        return Err("TikTok refused the authorization verification".into());
    }
    envelope
        .data
        .ok_or_else(|| "TikTok authorization data is missing".into())
}

pub(super) async fn exchange(
    http: &reqwest::Client,
    config: &OAuthConfig,
    base: &str,
    code: &str,
) -> Result<(TokenResponse, Vec<String>, Vec<ConnectedIdentity>), String> {
    let token: GrantResponse = read(
        http.post(format!("{base}/tt_user/oauth2/token/"))
            .json(&serde_json::json!({
                "client_id": config.client_id, "client_secret": config.client_secret,
                "grant_type": "authorization_code", "auth_code": code,
                "redirect_uri": config.redirect_uri,
            }))
            .send()
            .await
            .map_err(|_| "TikTok token endpoint could not be reached")?,
    )
    .await?;
    if token.access_token.trim().is_empty()
        || token.refresh_token.trim().is_empty()
        || token.open_id.trim().is_empty()
        || token.open_id.len() > 256
        || token.expires_in <= 0
        || token.expires_in > 86_400
    {
        return Err("TikTok token response is incomplete".into());
    }
    let inspection: Inspection = read(http.post(format!("{base}/tt_user/token_info/get/"))
        .json(&serde_json::json!({"app_id": config.client_id, "access_token": token.access_token}))
        .send().await.map_err(|_| "TikTok permissions could not be reached")?).await?;
    if inspection.app_id != config.client_id || inspection.creator_id != token.open_id {
        return Err("TikTok returned a different app or account identity".into());
    }
    let scopes: Vec<String> = inspection
        .scope
        .split(',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(str::to_string)
        .collect();
    if [
        "video.publish",
        "video.list",
        "user.info.basic",
        "user.info.username",
    ]
    .iter()
    .any(|scope| !scopes.iter().any(|granted| granted == scope))
    {
        return Err("TikTok did not grant publishing and account identity permissions".into());
    }
    let profile: Profile = read(
        http.get(format!("{base}/business/get/"))
            .header("Access-Token", &token.access_token)
            .query(&[
                ("business_id", token.open_id.as_str()),
                ("fields", "[\"username\",\"display_name\"]"),
            ])
            .send()
            .await
            .map_err(|_| "TikTok profile could not be reached")?,
    )
    .await?;
    if profile.username.trim().is_empty() || profile.username.len() > 32 {
        return Err("TikTok did not return the authorized account username".into());
    }
    let identity = ConnectedIdentity {
        provider: Provider::Tiktok,
        account_id: token.open_id,
        display_name: Some(format!("{} (@{})", profile.display_name, profile.username)),
        access_token: token.access_token.clone(),
    };
    Ok((
        TokenResponse {
            access_token: token.access_token,
            refresh_token: Some(token.refresh_token),
            expires_in: Some(token.expires_in),
            scope: Some(token.scope),
        },
        scopes,
        vec![identity],
    ))
}

pub(super) fn authorization_url(
    generated: &str,
    redirect_uri: &str,
    state: &str,
) -> Result<url::Url, ()> {
    let mut url = url::Url::parse(generated).map_err(|_| ())?;
    if url.scheme() != "https"
        || url.host_str() != Some("www.tiktok.com")
        || url.path() != "/v2/auth/authorize"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let redirect_values = pairs
        .iter()
        .filter(|(key, _)| key == "redirect_uri")
        .collect::<Vec<_>>();
    let duplicate_critical_key = ["redirect_uri", "client_id", "client_key", "scope"]
        .iter()
        .any(|name| pairs.iter().filter(|(key, _)| key == name).count() > 1);
    if redirect_values.len() != 1
        || redirect_values[0].1 != redirect_uri
        || duplicate_critical_key
        || pairs.iter().any(|(key, _)| {
            matches!(
                key.as_str(),
                "client_secret"
                    | "secret"
                    | "access_token"
                    | "refresh_token"
                    | "code"
                    | "auth_code"
            )
        })
    {
        return Err(());
    }
    url.set_query(None);
    let mut query = url.query_pairs_mut();
    for (key, value) in pairs {
        if !matches!(key.as_str(), "state" | "disable_auto_auth") {
            query.append_pair(&key, &value);
        }
    }
    query
        .append_pair("state", state)
        .append_pair("disable_auto_auth", "1");
    drop(query);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

    #[test]
    fn generated_url_keeps_console_parameters_and_replaces_one_time_state() {
        let url=authorization_url("https://www.tiktok.com/v2/auth/authorize?redirect_uri=https%3A%2F%2Fexample.test%2Fsocial%2Foauth%2Fcallback%2F&scope=video.publish&state=old&disable_auto_auth=0","https://example.test/social/oauth/callback/","new-state").unwrap();
        let pairs = url.query_pairs().collect::<Vec<_>>();
        assert_eq!(pairs.iter().filter(|(key, _)| key == "state").count(), 1);
        assert!(pairs
            .iter()
            .any(|(key, value)| key == "state" && value == "new-state"));
        assert!(pairs
            .iter()
            .any(|(key, value)| key == "disable_auto_auth" && value == "1"));
        for invalid in [
            "https://www.tiktok.com/v2/auth/authorize?redirect_uri=https%3A%2F%2Fexample.test%2Fsocial%2Foauth%2Fcallback%2F&redirect_uri=https%3A%2F%2Fevil.test%2F",
            "http://www.tiktok.com/v2/auth/authorize",
            "https://evil.example/v2/auth/authorize",
            "https://www.tiktok.com/v2/auth/authorize?client_secret=secret",
            "https://user:secret@www.tiktok.com/v2/auth/authorize",
        ] {
            assert!(authorization_url(
                invalid,
                "https://example.test/social/oauth/callback/",
                "state"
            )
            .is_err());
        }
    }

    async fn token_mock(server: &MockServer) {
        token_mock_with_scopes(
            server,
            "video.publish,video.list,user.info.basic,user.info.username",
        )
        .await;
    }

    async fn token_mock_with_scopes(server: &MockServer, scopes: &str) {
        Mock::given(matchers::method("POST")).and(matchers::path("/tt_user/oauth2/token/"))
            .and(matchers::body_partial_json(serde_json::json!({"grant_type":"authorization_code","auth_code":"synthetic-code"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{
                "access_token":"synthetic-access","refresh_token":"synthetic-refresh","expires_in":86400,
                "open_id":"chosen-account","scope":scopes
            }}))).mount(server).await;
    }

    fn config() -> OAuthConfig {
        OAuthConfig {
            client_id: "synthetic-app".into(),
            client_secret: "synthetic-secret".into(),
            redirect_uri: "https://example.test/social/oauth/callback/".into(),
        }
    }

    #[tokio::test]
    async fn exchange_requires_matching_app_account_and_granted_permissions() {
        let server = MockServer::start().await;
        token_mock(&server).await;
        Mock::given(matchers::method("POST")).and(matchers::path("/tt_user/token_info/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{
                "app_id":"synthetic-app","creator_id":"chosen-account","scope":"video.publish,video.list,user.info.basic,user.info.username"
            }}))).mount(&server).await;
        Mock::given(matchers::method("GET")).and(matchers::path("/business/get/"))
            .and(matchers::query_param("business_id","chosen-account"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"username":"synthetic.brand","display_name":"Synthetic Brand"}}))).mount(&server).await;
        let (tokens, scopes, identities) = exchange(
            &reqwest::Client::new(),
            &config(),
            &server.uri(),
            "synthetic-code",
        )
        .await
        .unwrap();
        assert_eq!(tokens.refresh_token.as_deref(), Some("synthetic-refresh"));
        assert_eq!(scopes.len(), 4);
        assert_eq!(identities[0].account_id, "chosen-account");
        assert!(super::super::identity_matches(
            Provider::Tiktok,
            &identities[0],
            "synthetic.brand"
        ));
        assert!(!super::super::identity_matches(
            Provider::Tiktok,
            &identities[0],
            "other.brand"
        ));
    }

    #[tokio::test]
    async fn exchange_rejects_inspector_identity_mismatch_before_profile_lookup() {
        let server = MockServer::start().await;
        token_mock(&server).await;
        Mock::given(matchers::method("POST")).and(matchers::path("/tt_user/token_info/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{
                "app_id":"other-app","creator_id":"chosen-account","scope":"video.publish,video.list,user.info.basic,user.info.username"
            }}))).mount(&server).await;
        assert!(exchange(
            &reqwest::Client::new(),
            &config(),
            &server.uri(),
            "synthetic-code"
        )
        .await
        .is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
    #[test]
    fn generated_account_holder_url_preserves_approved_comment_scopes() {
        let url = authorization_url(
            "https://www.tiktok.com/v2/auth/authorize?redirect_uri=https%3A%2F%2Fexample.test%2Fsocial%2Foauth%2Fcallback%2F&scope=video.publish%2Cvideo.list%2Cuser.info.basic%2Cuser.info.username%2Ccomment.list%2Ccomment.list.manage",
            "https://example.test/social/oauth/callback/", "synthetic-state").unwrap();
        let scopes = url
            .query_pairs()
            .find(|(name, _)| name == "scope")
            .unwrap()
            .1;
        assert!(scopes.split(',').any(|scope| scope == "comment.list"));
        assert!(scopes
            .split(',')
            .any(|scope| scope == "comment.list.manage"));
        assert!(scopes.split(',').any(|scope| scope == "video.publish"));
    }

    #[tokio::test]
    async fn reconnect_retains_all_inspector_verified_comment_grants() {
        let server = MockServer::start().await;
        let granted = "video.publish,video.list,user.info.basic,user.info.username,comment.list,comment.list.manage";
        token_mock_with_scopes(&server, granted).await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/tt_user/token_info/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"code":0,"data":{
                    "app_id":"synthetic-app","creator_id":"chosen-account","scope":granted
                }}),
            ))
            .mount(&server)
            .await;
        Mock::given(matchers::method("GET")).and(matchers::path("/business/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"username":"synthetic.brand","display_name":"Synthetic Brand"}}))).mount(&server).await;
        let (_, scopes, identities) = exchange(
            &reqwest::Client::new(),
            &config(),
            &server.uri(),
            "synthetic-code",
        )
        .await
        .unwrap();
        assert!(scopes.iter().any(|scope| scope == "comment.list"));
        assert!(scopes.iter().any(|scope| scope == "comment.list.manage"));
        assert_eq!(scopes.len(), 6);
        assert_eq!(identities[0].account_id, "chosen-account");
    }
}
