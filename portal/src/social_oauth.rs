//! OAuth entry and callback for registered Brand social accounts.

mod navigation;
mod tiktok;
use navigation::authorization_navigation;

use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::session::SessionData;

const META_GRAPH: &str = "https://graph.facebook.com/v26.0";
const THREADS_GRAPH: &str = "https://graph.threads.net";
const YOUTUBE_TOKEN: &str = "https://oauth2.googleapis.com/token";
const YOUTUBE_CHANNELS: &str = "https://www.googleapis.com/youtube/v3/channels";

#[derive(Clone)]
struct ProviderEndpoints {
    meta_graph: String,
    threads_graph: String,
    youtube_token: String,
    youtube_channels: String,
    x_api: String,
    tiktok: String,
    linkedin_token: String,
    linkedin_introspect: String,
    linkedin_rest: String,
}

impl Default for ProviderEndpoints {
    fn default() -> Self {
        Self {
            meta_graph: META_GRAPH.into(),
            threads_graph: THREADS_GRAPH.into(),
            youtube_token: YOUTUBE_TOKEN.into(),
            youtube_channels: YOUTUBE_CHANNELS.into(),
            x_api: "https://api.x.com/2".into(),
            tiktok: "https://business-api.tiktok.com/open_api/v1.3".into(),
            linkedin_token: "https://www.linkedin.com/oauth/v2/accessToken".into(),
            linkedin_introspect: "https://www.linkedin.com/oauth/v2/introspectToken".into(),
            linkedin_rest: "https://api.linkedin.com/rest".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    Facebook,
    Instagram,
    Threads,
    Youtube,
    Linkedin,
    X,
    Tiktok,
}

impl Provider {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "facebook" => Some(Self::Facebook),
            "instagram" => Some(Self::Instagram),
            "threads" => Some(Self::Threads),
            "youtube" => Some(Self::Youtube),
            "linkedin" => Some(Self::Linkedin),
            "x" => Some(Self::X),
            "tiktok" => Some(Self::Tiktok),
            _ => None,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Facebook => "facebook",
            Self::Instagram => "instagram",
            Self::Threads => "threads",
            Self::Youtube => "youtube",
            Self::Linkedin => "linkedin",
            Self::X => "x",
            Self::Tiktok => "tiktok",
        }
    }

    const fn client_id_env(self) -> &'static str {
        match self {
            Self::Facebook | Self::Instagram => "NAVIGATOR_META_APP_ID",
            Self::Threads => "NAVIGATOR_THREADS_APP_ID",
            Self::Youtube => "NAVIGATOR_YOUTUBE_CLIENT_ID",
            Self::Linkedin => "NAVIGATOR_LINKEDIN_CLIENT_ID",
            Self::X => "NAVIGATOR_X_CLIENT_ID",
            Self::Tiktok => "NAVIGATOR_TIKTOK_BUSINESS_APP_ID",
        }
    }

    const fn client_secret_env(self) -> &'static str {
        match self {
            Self::Facebook | Self::Instagram => "NAVIGATOR_META_APP_SECRET",
            Self::Threads => "NAVIGATOR_THREADS_APP_SECRET",
            Self::Youtube => "NAVIGATOR_YOUTUBE_CLIENT_SECRET",
            Self::Linkedin => "NAVIGATOR_LINKEDIN_CLIENT_SECRET",
            Self::X => "NAVIGATOR_X_CLIENT_SECRET",
            Self::Tiktok => "NAVIGATOR_TIKTOK_BUSINESS_APP_SECRET",
        }
    }

    const fn authorization_endpoint(self) -> &'static str {
        match self {
            Self::Facebook | Self::Instagram => "https://www.facebook.com/v26.0/dialog/oauth",
            Self::Threads => "https://threads.net/oauth/authorize",
            Self::Youtube => "https://accounts.google.com/o/oauth2/v2/auth",
            Self::Linkedin => "https://www.linkedin.com/oauth/v2/authorization",
            Self::X => "https://x.com/i/oauth2/authorize",
            Self::Tiktok => "https://www.tiktok.com/v2/auth/authorize",
        }
    }

    /// The authorization request's `scope`: the connection's required
    /// permissions, comma-separated for Meta and space-separated for Google and `LinkedIn`.
    fn scopes(self) -> String {
        let separator = if matches!(self, Self::Youtube | Self::Linkedin | Self::X) {
            " "
        } else {
            ","
        };
        store_provider(self).required_scopes().join(separator)
    }

    /// Google and X document PKCE for this flow; Meta's Facebook Login and Threads
    /// authorization windows do not, and authenticate the exchange with the
    /// app secret instead.
    const fn uses_pkce(self) -> bool {
        matches!(self, Self::Youtube | Self::X)
    }
}

#[derive(Debug, Deserialize)]
pub struct ConnectForm {
    #[serde(default)]
    provider_account_id: Option<String>,
    #[serde(rename = "_csrf")]
    _csrf: Option<String>,
}

/// Save a Brand destination independently of provider consent.
async fn save_selection(
    State(state): State<crate::admin::AdminState>,
    session: Option<Extension<SessionData>>,
    Path((brand_key, provider_name)): Path<(String, String)>,
    Form(form): Form<ConnectForm>,
) -> Response {
    let back = format!("/app/admin/brands/{brand_key}/edit");
    let Some(Extension(session)) = session else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(actor) = session.person_id else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(provider) = Provider::parse(&provider_name) else {
        return redirect_error(&back, "Unsupported social provider");
    };
    match store::social_account_selections::save(
        &state.application_store,
        session.role,
        actor,
        &brand_key,
        store_provider(provider),
        form.provider_account_id.as_deref().unwrap_or(""),
    )
    .await
    {
        Ok(()) => Redirect::to(&back).into_response(),
        Err(_) => redirect_error(
            &back,
            "Could not save the destination. Check the account identity and Brand access.",
        ),
    }
}

/// `POST /app/admin/brands/{brand_key}/social/{provider}/connect`.
pub async fn connect_start(
    State(state): State<crate::admin::AdminState>,
    session: Option<Extension<SessionData>>,
    Path((brand_key, provider_name)): Path<(String, String)>,
    Form(form): Form<ConnectForm>,
) -> Response {
    connect_start_configured(
        State(state),
        session,
        Path((brand_key, provider_name)),
        Form(form),
        None,
    )
    .await
}

async fn connect_start_configured(
    State(state): State<crate::admin::AdminState>,
    session: Option<Extension<SessionData>>,
    Path((brand_key, provider_name)): Path<(String, String)>,
    Form(_form): Form<ConnectForm>,
    config: Option<OAuthConfig>,
) -> Response {
    let back = format!("/app/admin/brands/{brand_key}/edit");
    let Some(session) = session.map(|Extension(session)| session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(actor_id) = session.person_id else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(provider) = Provider::parse(&provider_name) else {
        return redirect_error(&back, "Unsupported social provider");
    };
    let Ok(selections) =
        store::social_account_selections::for_brand(&state.application_store, &brand_key).await
    else {
        return redirect_error(&back, "Could not load the saved destination");
    };
    let Some(selection) = selections
        .iter()
        .find(|selection| selection.provider == provider.as_str())
    else {
        return redirect_error(
            &back,
            "Choose this Brand's provider account: save it before authorizing",
        );
    };
    let target = selection.provider_account_id.as_str();
    let linked_instagram = (provider == Provider::Facebook)
        .then(|| {
            selections
                .iter()
                .find(|selection| selection.provider == "instagram")
        })
        .flatten();
    if let Ok(accounts) =
        store::social_accounts::for_actor(&state.application_store, session.role, actor_id).await
    {
        let usable = |selection| selection_is_usable(&accounts, &brand_key, provider, selection);
        if usable(selection) && linked_instagram.is_none_or(usable) {
            return Redirect::to(&format!("{back}?social_reused=true")).into_response();
        }
    }
    let Some(config) = config.or_else(|| oauth_config(provider)) else {
        return redirect_error(&back, "Social account connection is not configured");
    };
    let attempt = match store::social_oauth_attempts::begin(
        &state.application_store,
        session.role,
        actor_id,
        &brand_key,
        provider.as_str(),
        Utc::now(),
    )
    .await
    {
        Ok(attempt) => attempt,
        Err(error) => return redirect_error(&back, &error.to_string()),
    };
    if let Some(error) = target_error(provider, target) {
        return redirect_error(&back, error);
    }
    if store::social_oauth_attempts::bind_account(&state.application_store, &attempt, target)
        .await
        .is_err()
    {
        return redirect_error(&back, "Could not bind the provider account to this Brand");
    }
    if let Some(instagram) = linked_instagram {
        if store::social_oauth_attempts::bind_instagram(
            &state.application_store,
            &attempt,
            &instagram.provider_account_id,
        )
        .await
        .is_err()
        {
            return redirect_error(&back, "Could not bind the selected Instagram account");
        }
    }
    match selected_authorization_url(provider, &config, &attempt, linked_instagram.is_some()) {
        Ok(url) => authorization_navigation(&url, &back),
        Err(()) => redirect_error(&back, "Could not start social account authorization"),
    }
}

fn selection_is_usable(
    accounts: &[store::social_accounts::SocialAccount],
    brand_key: &str,
    provider: Provider,
    selection: &store::social_account_selections::SocialAccountSelection,
) -> bool {
    accounts.iter().any(|account| {
        account.brand_key == brand_key
            && account.provider.as_str() == selection.provider
            && identity_matches(
                Provider::parse(&selection.provider).unwrap_or(provider),
                &ConnectedIdentity {
                    provider,
                    account_id: account.provider_account_id.clone(),
                    display_name: account.display_name.clone(),
                    access_token: String::new(),
                },
                &selection.provider_account_id,
            )
            && account.status == "connected"
            && account.provider.missing_scopes(&account.scopes).is_empty()
            && account
                .provider
                .token_renewal(account.expires_at, Utc::now())
                != store::social_accounts::TokenRenewal::Reconnect
    })
}

fn target_error(provider: Provider, target: &str) -> Option<&'static str> {
    match provider {
        Provider::Tiktok
            if target.len() > 32
                || !target
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_')) =>
        {
            Some("Enter this Brand's TikTok username without @")
        }
        Provider::Linkedin if !target.bytes().all(|byte| byte.is_ascii_digit()) => {
            Some("Enter this Brand's numeric provider account ID")
        }
        Provider::X
            if target.is_empty()
                || target.len() > 20
                || !target
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') =>
        {
            Some("Enter this Brand's X username, without @ (for example NeonLawUSA)")
        }
        _ => None,
    }
}

fn selected_authorization_url(
    provider: Provider,
    config: &OAuthConfig,
    attempt: &store::social_oauth_attempts::SocialOAuthAttempt,
    linked_instagram: bool,
) -> Result<Url, ()> {
    let mut url = authorization_url(provider, config, &attempt.state, &attempt.pkce_verifier)?;
    if linked_instagram {
        let mut scopes = store_provider(Provider::Facebook)
            .required_scopes()
            .to_vec();
        scopes.extend(store_provider(Provider::Instagram).required_scopes());
        scopes.sort_unstable();
        scopes.dedup();
        let pairs: Vec<_> = url
            .query_pairs()
            .filter(|(key, _)| key != "scope")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.set_query(None);
        url.query_pairs_mut()
            .extend_pairs(pairs)
            .append_pair("scope", &scopes.join(","));
    }
    Ok(url)
}

#[derive(Clone)]
struct OAuthConfig {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

/// Configuration presence only; provider consent and live delivery need separate verification.
pub(crate) fn connection_configured(provider_name: &str) -> bool {
    let Some(provider) = Provider::parse(provider_name) else {
        return false;
    };
    let Some(config) = oauth_config(provider) else {
        return false;
    };
    authorization_url(
        provider,
        &config,
        "configuration-check",
        "configuration-check",
    )
    .is_ok()
}

fn oauth_config(provider: Provider) -> Option<OAuthConfig> {
    let mut config = oauth_config_from(
        std::env::var(provider.client_id_env()).ok(),
        std::env::var(provider.client_secret_env()).ok(),
        std::env::var("NAVIGATOR_SOCIAL_OAUTH_REDIRECT_URI").ok(),
    )?;
    if provider == Provider::Tiktok {
        let mut url = Url::parse(&config.redirect_uri).ok()?;
        if url.scheme() != "https" || url.port().is_some() || url.query().is_some() {
            return None;
        }
        if !url.path().ends_with('/') {
            url.set_path(&format!("{}/", url.path()));
        }
        config.redirect_uri = url.to_string();
    }
    Some(config)
}

fn oauth_config_from(
    client_id: Option<String>,
    client_secret: Option<String>,
    redirect_uri: Option<String>,
) -> Option<OAuthConfig> {
    let client_id = client_id?;
    let client_secret = client_secret?;
    let redirect_uri = redirect_uri?;
    if client_id.trim().is_empty() || client_secret.trim().is_empty() {
        return None;
    }
    let parsed = Url::parse(&redirect_uri).ok()?;
    if !matches!(parsed.scheme(), "https" | "http")
        || parsed.host_str().is_none()
        || parsed.fragment().is_some()
        || (parsed.scheme() == "http"
            && !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "::1")))
    {
        return None;
    }
    Some(OAuthConfig {
        client_id,
        client_secret,
        redirect_uri,
    })
}

fn authorization_url(
    provider: Provider,
    config: &OAuthConfig,
    state: &str,
    verifier: &str,
) -> Result<Url, ()> {
    if provider == Provider::Tiktok {
        let generated =
            std::env::var("NAVIGATOR_TIKTOK_BUSINESS_AUTHORIZATION_URL").map_err(|_| ())?;
        return tiktok::authorization_url(&generated, &config.redirect_uri, state);
    }
    let mut url = Url::parse(provider.authorization_endpoint()).map_err(|_| ())?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("client_id", &config.client_id)
            .append_pair("redirect_uri", &config.redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &provider.scopes())
            .append_pair("state", state);
        if provider.uses_pkce() {
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
            query
                .append_pair("code_challenge", &challenge)
                .append_pair("code_challenge_method", "S256");
        }
        if provider == Provider::Threads {
            query.append_pair("enable_fb_login", "0");
        }
        if provider == Provider::Youtube {
            query
                .append_pair("access_type", "offline")
                .append_pair("prompt", "consent");
        }
    }
    Ok(url)
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    state: Option<String>,
    code: Option<String>,
    #[serde(default)]
    auth_code: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Deserialize)]
struct MetaPermissions {
    #[serde(default)]
    data: Vec<MetaPermission>,
}

#[derive(Deserialize)]
struct MetaPermission {
    permission: String,
    status: String,
}

#[derive(Deserialize)]
struct MetaPages {
    #[serde(default)]
    data: Vec<MetaPage>,
    paging: Option<MetaPaging>,
}

#[derive(Deserialize)]
struct MetaPaging {
    next: Option<String>,
    cursors: Option<MetaCursors>,
}

#[derive(Deserialize)]
struct MetaCursors {
    after: Option<String>,
}

#[derive(Deserialize)]
struct MetaPage {
    id: String,
    name: Option<String>,
    access_token: String,
    instagram_business_account: Option<InstagramAccount>,
}

#[derive(Deserialize)]
struct InstagramAccount {
    id: String,
    username: Option<String>,
}

#[derive(Deserialize)]
struct ThreadsIdentity {
    id: String,
    username: Option<String>,
}

#[derive(Deserialize)]
struct YoutubeChannels {
    #[serde(default)]
    items: Vec<YoutubeChannel>,
}

#[derive(Deserialize)]
struct YoutubeChannel {
    id: String,
    snippet: YoutubeSnippet,
}

#[derive(Deserialize)]
struct YoutubeSnippet {
    title: String,
}

struct ConnectedIdentity {
    provider: Provider,
    account_id: String,
    display_name: Option<String>,
    access_token: String,
}

fn identity_matches(provider: Provider, identity: &ConnectedIdentity, target: &str) -> bool {
    (provider == Provider::Tiktok
        && identity
            .display_name
            .as_deref()
            .is_some_and(|name| name.ends_with(&format!(" (@{target})"))))
        || (!matches!(provider, Provider::X | Provider::Tiktok) && identity.account_id == target)
        || (matches!(provider, Provider::X | Provider::Threads)
            && identity.display_name.as_deref().is_some_and(|name| {
                if provider == Provider::Threads {
                    name == target
                } else {
                    name.eq_ignore_ascii_case(target)
                }
            }))
}

/// Provider callbacks are anonymous ingress, authenticated by the one-time
/// state token and PKCE where the provider supports it.
async fn callback(
    State(state): State<crate::AppState>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    complete_authorization(state, query, None, &ProviderEndpoints::default()).await
}

fn invalid_authorization() -> Response {
    (StatusCode::BAD_REQUEST, [(axum::http::header::CACHE_CONTROL, "no-store"), (axum::http::header::REFERRER_POLICY, "no-referrer")], axum::response::Html("<!doctype html><html lang=\"en\"><title>Authorization needs restarting</title><h1>Authorization expired or was already used</h1><p>Return to your Brand and connect again. Saved destinations are unchanged.</p><a href=\"/app/admin/brands\">Return to Brands</a></html>")).into_response()
}

struct AuthorizedCallback {
    attempt: store::social_oauth_attempts::SocialOAuthAttempt,
    person: store::persons::Person,
    back: String,
}

async fn authorize_callback(
    state: &crate::AppState,
    query: &CallbackQuery,
) -> Result<AuthorizedCallback, Response> {
    let Some(raw_state) = query.state.as_deref() else {
        return Err(invalid_authorization());
    };
    let Ok(attempt) =
        store::social_oauth_attempts::consume(&state.application_store, raw_state, Utc::now())
            .await
    else {
        return Err(invalid_authorization());
    };
    let back = format!("/app/admin/brands/{}/edit", attempt.brand_key);
    let Some(person) =
        store::persons::find_by_id(&state.application_store, attempt.actor_person_id)
            .await
            .ok()
            .flatten()
    else {
        return Err(redirect_error(
            &back,
            "The authorizing person is no longer available",
        ));
    };
    if !store::persons::is_admitted(&state.application_store, person.id)
        .await
        .unwrap_or(false)
    {
        return Err(redirect_error(
            &back,
            "The authorizing person is not admitted",
        ));
    }
    let Some(brand) = store::brands::find_by_key(&state.application_store, &attempt.brand_key)
        .await
        .ok()
        .flatten()
    else {
        return Err(redirect_error(&back, "The Brand is no longer available"));
    };
    if brand.firm_id != Some(attempt.firm_id) {
        return Err(redirect_error(
            &back,
            "The Brand's Firm changed during authorization",
        ));
    }
    let decision = store::firm_capability::FirmCapability::ManageBrandContent
        .resolve(
            &state.application_store,
            person.role,
            Some(person.id),
            attempt.firm_id,
        )
        .await;
    if !matches!(
        decision,
        Ok(store::firm_capability::FirmCapabilityDecision::Allowed)
    ) {
        return Err(redirect_error(
            &back,
            "The authorizing person no longer manages this Brand",
        ));
    }
    Ok(AuthorizedCallback {
        attempt,
        person,
        back,
    })
}

async fn exchange_and_discover(
    provider: Provider,
    config: &OAuthConfig,
    endpoints: &ProviderEndpoints,
    code: &str,
    verifier: &str,
    back: &str,
    meta_selection: Option<(&str, &str)>,
) -> Result<(TokenResponse, Vec<String>, Vec<ConnectedIdentity>), Response> {
    if meta_selection.is_some() && provider != Provider::Facebook {
        return Err(redirect_error(
            back,
            "Facebook and Instagram did not grant all required permissions. Reconnect Meta.",
        ));
    }
    let http = reqwest::Client::new();
    if provider == Provider::Tiktok {
        return tiktok::exchange(&http, config, &endpoints.tiktok, code)
            .await
            .map_err(|_| redirect_error(back, "TikTok authorization could not be verified"));
    }
    let token = match exchange_code(&http, provider, config, endpoints, code, verifier).await {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(provider = provider.as_str(), error = %error, "social OAuth code exchange failed");
            return Err(redirect_error(
                back,
                "The provider authorization could not be completed",
            ));
        }
    };
    let long_lived = match provider {
        Provider::Threads => {
            exchange_threads_short_token(&http, config, endpoints, &token.access_token).await
        }
        Provider::Facebook | Provider::Instagram => {
            exchange_meta_short_token(&http, config, endpoints, &token.access_token).await
        }
        Provider::Youtube | Provider::Linkedin | Provider::X | Provider::Tiktok => Ok(token),
    };
    let token = match long_lived {
        Ok(token) => token,
        Err(error) => {
            tracing::warn!(provider = provider.as_str(), error = %error, "social long-lived token exchange failed");
            return Err(redirect_error(
                back,
                "The provider authorization could not be completed",
            ));
        }
    };
    let scopes = match granted_scopes(&http, provider, endpoints, &token).await {
        Ok(scopes) if scopes_are_sufficient(provider, &scopes) => scopes,
        Ok(_) => {
            return Err(redirect_error(
                back,
                "The provider did not grant the required permissions",
            ));
        }
        Err(error) => {
            tracing::warn!(provider = provider.as_str(), error = %error, "social OAuth scope verification failed");
            return Err(redirect_error(
                back,
                "The provider permissions could not be verified",
            ));
        }
    };
    if let Some((page_id, instagram_id)) = meta_selection {
        if !scopes_are_sufficient(Provider::Instagram, &scopes) {
            return Err(redirect_error(
                back,
                "Facebook and Instagram did not grant all required permissions. Reconnect Meta.",
            ));
        }
        let identities = discover_selected_meta(&http, endpoints, &token, page_id, instagram_id)
            .await
            .map_err(|message| redirect_error(back, message))?;
        return Ok((token, scopes, identities));
    }
    let identities = match discover_identities(&http, provider, endpoints, &token).await {
        Ok(identities) if !identities.is_empty() => identities,
        Ok(_) => {
            return Err(redirect_error(
                back,
                "No eligible provider account was found",
            ));
        }
        Err(error) => {
            tracing::warn!(provider = provider.as_str(), error = %error, "social OAuth account lookup failed");
            return Err(redirect_error(
                back,
                "The provider account could not be verified",
            ));
        }
    };
    Ok((token, scopes, identities))
}

const fn store_provider(provider: Provider) -> store::social_accounts::SocialProvider {
    match provider {
        Provider::Facebook => store::social_accounts::SocialProvider::Facebook,
        Provider::Instagram => store::social_accounts::SocialProvider::Instagram,
        Provider::Threads => store::social_accounts::SocialProvider::Threads,
        Provider::Youtube => store::social_accounts::SocialProvider::Youtube,
        Provider::Linkedin => store::social_accounts::SocialProvider::Linkedin,
        Provider::X => store::social_accounts::SocialProvider::X,
        Provider::Tiktok => store::social_accounts::SocialProvider::Tiktok,
    }
}

struct PersistIdentities<'a> {
    state: &'a crate::AppState,
    person: &'a store::persons::Person,
    brand_key: &'a str,
    provider: Provider,
    scopes: &'a [String],
    expires_at: Option<DateTime<Utc>>,
    refresh_token: Option<&'a str>,
    identities: Vec<ConnectedIdentity>,
    back: &'a str,
}

async fn persist_identities(input: PersistIdentities<'_>) -> Result<usize, Response> {
    let mut connected = 0;
    let mut connected_rows: Vec<(store::social_accounts::SocialProvider, String)> = Vec::new();
    for identity in input.identities {
        let identity_provider = store_provider(identity.provider);
        let result = store::social_accounts::connect(
            &input.state.application_store,
            store::social_accounts::ConnectSocialAccount {
                actor_role: input.person.role,
                actor_person_id: input.person.id,
                brand_key: input.brand_key,
                provider: identity_provider,
                provider_account_id: &identity.account_id,
                display_name: identity.display_name.as_deref(),
                scopes: input.scopes,
                expires_at: input.expires_at,
                access_token: &identity.access_token,
                refresh_token: input.refresh_token,
            },
            input.state.runtime_kms.as_ref(),
        )
        .await;
        if result.is_err() {
            tracing::warn!(
                provider = input.provider.as_str(),
                "social account credential save failed"
            );
            for (saved_provider, saved_id) in connected_rows {
                let _ = store::social_accounts::revoke(
                    &input.state.application_store,
                    input.person.role,
                    input.person.id,
                    input.brand_key,
                    saved_provider,
                    &saved_id,
                )
                .await;
            }
            return Err(redirect_error(
                input.back,
                "A provider account could not be saved",
            ));
        }
        connected_rows.push((identity_provider, identity.account_id));
        connected += 1;
    }
    Ok(connected)
}

async fn complete_authorization(
    state: crate::AppState,
    query: CallbackQuery,
    config: Option<OAuthConfig>,
    endpoints: &ProviderEndpoints,
) -> Response {
    let authorized = match authorize_callback(&state, &query).await {
        Ok(authorized) => authorized,
        Err(response) => return response,
    };
    let Some(target) = authorized
        .attempt
        .provider_account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return redirect_error(
            &authorized.back,
            "Choose this Brand's provider account before authorizing again",
        );
    };
    if let Some(error) = query.error.as_deref() {
        return redirect_error(
            &authorized.back,
            if error == "access_denied" {
                "Social account authorization was cancelled"
            } else {
                "The provider could not complete authorization. Your saved destination is unchanged; try Connect again."
            },
        );
    }
    let Some(provider) = Provider::parse(&authorized.attempt.provider) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let callback_code = if provider == Provider::Tiktok {
        query.auth_code.as_deref()
    } else {
        query.code.as_deref()
    };
    let Some(code) = callback_code else {
        return redirect_error(
            &authorized.back,
            "The provider did not return an authorization code",
        );
    };
    let Some(config) = config.or_else(|| oauth_config(provider)) else {
        return redirect_error(
            &authorized.back,
            "Social account connection is not configured",
        );
    };
    let meta_selection = authorized
        .attempt
        .instagram_account_id
        .as_deref()
        .map(|instagram| (target, instagram));
    let (token, scopes, mut identities) = match exchange_and_discover(
        provider,
        &config,
        endpoints,
        code,
        &authorized.attempt.pkce_verifier,
        &authorized.back,
        meta_selection,
    )
    .await
    {
        Ok(discovered) => discovered,
        Err(response) => return response,
    };
    if meta_selection.is_none() {
        identities.retain(|identity| identity_matches(provider, identity, target));
    }
    if identities.is_empty() {
        return redirect_error(
            &authorized.back,
            "The chosen account was not authorized for this Brand",
        );
    }
    let expires_at = authorization_expiry(provider, token.expires_in);
    match persist_identities(PersistIdentities {
        state: &state,
        person: &authorized.person,
        brand_key: &authorized.attempt.brand_key,
        provider,
        scopes: &scopes,
        expires_at,
        refresh_token: token.refresh_token.as_deref(),
        identities,
        back: &authorized.back,
    })
    .await
    {
        Ok(connected) => Redirect::to(&format!("{}?social_connected={connected}", authorized.back))
            .into_response(),
        Err(response) => response,
    }
}

// Page tokens issued from a long-lived Meta user token carry no expiry;
// the user token's own expiry does not apply to those Page credentials.
fn authorization_expiry(provider: Provider, expires_in: Option<i64>) -> Option<DateTime<Utc>> {
    if matches!(provider, Provider::Facebook | Provider::Instagram) {
        return None;
    }
    expires_in
        .filter(|seconds| *seconds > 0)
        .map(|seconds| Utc::now() + Duration::seconds(seconds))
}

async fn exchange_code(
    http: &reqwest::Client,
    provider: Provider,
    config: &OAuthConfig,
    endpoints: &ProviderEndpoints,
    code: &str,
    verifier: &str,
) -> Result<TokenResponse, String> {
    if provider == Provider::Linkedin {
        return exchange_linkedin(http, config, endpoints, code).await;
    }
    let response = match provider {
        Provider::Linkedin | Provider::Tiktok => return Err("unsupported token exchange".into()),
        Provider::X => {
            http.post(format!("{}/oauth2/token", endpoints.x_api))
                .basic_auth(&config.client_id, Some(&config.client_secret))
                .form(&[
                    ("grant_type", "authorization_code"),
                    ("redirect_uri", config.redirect_uri.as_str()),
                    ("code", code),
                    ("code_verifier", verifier),
                ])
                .send()
                .await
        }
        Provider::Facebook | Provider::Instagram => {
            http.post(format!("{}/oauth/access_token", endpoints.meta_graph))
                .form(&[
                    ("client_id", config.client_id.as_str()),
                    ("client_secret", config.client_secret.as_str()),
                    ("redirect_uri", config.redirect_uri.as_str()),
                    ("code", code),
                ])
                .send()
                .await
        }
        Provider::Threads => {
            http.post(format!("{}/oauth/access_token", endpoints.threads_graph))
                .query(&[
                    ("client_id", config.client_id.as_str()),
                    ("client_secret", config.client_secret.as_str()),
                    ("grant_type", "authorization_code"),
                    ("redirect_uri", config.redirect_uri.as_str()),
                    ("code", code),
                ])
                .body("")
                .send()
                .await
        }
        Provider::Youtube => {
            http.post(&endpoints.youtube_token)
                .form(&[
                    ("client_id", config.client_id.as_str()),
                    ("client_secret", config.client_secret.as_str()),
                    ("grant_type", "authorization_code"),
                    ("redirect_uri", config.redirect_uri.as_str()),
                    ("code", code),
                    ("code_verifier", verifier),
                ])
                .send()
                .await
        }
    }
    .map_err(|_| "provider token endpoint could not be reached".to_string())?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|_| "provider token response could not be read".to_string())?;
    if !status.is_success() || body.len() > 64 * 1024 {
        return Err(format!("provider token endpoint returned {status}"));
    }
    let token: TokenResponse = serde_json::from_slice(&body)
        .map_err(|_| "provider token response was invalid".to_string())?;
    if token.access_token.trim().is_empty() {
        return Err("provider returned an empty access token".into());
    }
    if provider == Provider::X
        && (token
            .expires_in
            .is_none_or(|seconds| !(1..=86_400).contains(&seconds))
            || token
                .refresh_token
                .as_deref()
                .is_none_or(|value| value.trim().is_empty()))
    {
        return Err("X authorization requires an expiry and offline refresh token".into());
    }
    Ok(token)
}

async fn exchange_threads_short_token(
    http: &reqwest::Client,
    config: &OAuthConfig,
    endpoints: &ProviderEndpoints,
    short_lived_token: &str,
) -> Result<TokenResponse, String> {
    let response = http
        .get(format!("{}/access_token", endpoints.threads_graph))
        .query(&[
            ("grant_type", "th_exchange_token"),
            ("client_secret", config.client_secret.as_str()),
            ("access_token", short_lived_token),
        ])
        .send()
        .await
        .map_err(|_| "Threads token exchange endpoint could not be reached".to_string())?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "Threads token response could not be read".to_string())?;
    if !status.is_success() || bytes.len() > 64 * 1024 {
        return Err(format!("Threads token exchange returned {status}"));
    }
    let token: TokenResponse = serde_json::from_slice(&bytes)
        .map_err(|_| "Threads token response was invalid".to_string())?;
    if token.access_token.trim().is_empty() || token.expires_in.is_none() {
        return Err("Threads token response was incomplete".into());
    }
    Ok(token)
}

/// Exchange a short-lived Facebook Login user token for a long-lived one, so
/// the Page tokens discovered with it do not expire.
async fn exchange_meta_short_token(
    http: &reqwest::Client,
    config: &OAuthConfig,
    endpoints: &ProviderEndpoints,
    short_lived_token: &str,
) -> Result<TokenResponse, String> {
    let response = http
        .get(format!("{}/oauth/access_token", endpoints.meta_graph))
        .query(&[
            ("grant_type", "fb_exchange_token"),
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("fb_exchange_token", short_lived_token),
        ])
        .send()
        .await
        .map_err(|_| "Meta token exchange endpoint could not be reached".to_string())?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| "Meta token response could not be read".to_string())?;
    if !status.is_success() || bytes.len() > 64 * 1024 {
        return Err(format!("Meta token exchange returned {status}"));
    }
    let token: TokenResponse = serde_json::from_slice(&bytes)
        .map_err(|_| "Meta token response was invalid".to_string())?;
    if token.access_token.trim().is_empty() {
        return Err("Meta token response was incomplete".into());
    }
    Ok(token)
}

#[derive(Deserialize)]
struct ThreadsDebugToken {
    data: ThreadsTokenData,
}

#[derive(Deserialize)]
struct ThreadsTokenData {
    #[serde(default)]
    is_valid: bool,
    #[serde(default)]
    scopes: Vec<String>,
}

async fn granted_scopes(
    http: &reqwest::Client,
    provider: Provider,
    endpoints: &ProviderEndpoints,
    token: &TokenResponse,
) -> Result<Vec<String>, String> {
    if matches!(provider, Provider::Facebook | Provider::Instagram) {
        let permissions = http
            .get(format!("{}/me/permissions", endpoints.meta_graph))
            .query(&[("access_token", token.access_token.as_str())])
            .send()
            .await
            .map_err(|_| "Meta permissions endpoint could not be reached".to_string())?;
        if !permissions.status().is_success() {
            return Err("Meta permissions could not be verified".into());
        }
        let permissions: MetaPermissions = permissions
            .json()
            .await
            .map_err(|_| "Meta permissions response was invalid".to_string())?;
        return Ok(permissions
            .data
            .into_iter()
            .filter(|permission| permission.status == "granted")
            .map(|permission| permission.permission)
            .collect());
    }
    if provider == Provider::Threads {
        // Threads token responses carry no scope; its debug endpoint reports
        // the permissions the token actually holds.
        let response = http
            .get(format!("{}/v1.0/debug_token", endpoints.threads_graph))
            .query(&[
                ("input_token", token.access_token.as_str()),
                ("access_token", token.access_token.as_str()),
            ])
            .send()
            .await
            .map_err(|_| "Threads token debug endpoint could not be reached".to_string())?;
        if !response.status().is_success() {
            return Err("Threads permissions could not be verified".into());
        }
        let debug: ThreadsDebugToken = response
            .json()
            .await
            .map_err(|_| "Threads token debug response was invalid".to_string())?;
        if !debug.data.is_valid {
            return Err("Threads reported the token as invalid".into());
        }
        return Ok(debug.data.scopes);
    }
    let scope = token
        .scope
        .as_deref()
        .ok_or_else(|| "provider did not return granted scopes".to_string())?;
    Ok(scope
        .split([',', ' '])
        .filter(|scope| !scope.is_empty())
        .map(str::to_string)
        .collect())
}

fn scopes_are_sufficient(provider: Provider, scopes: &[String]) -> bool {
    store_provider(provider).missing_scopes(scopes).is_empty()
}

async fn discover_selected_meta(
    http: &reqwest::Client,
    endpoints: &ProviderEndpoints,
    token: &TokenResponse,
    page_id: &str,
    instagram_id: &str,
) -> Result<Vec<ConnectedIdentity>, &'static str> {
    let mut seen_cursors = std::collections::HashSet::new();
    let mut after = None;
    let page = loop {
        let mut request = http
            .get(format!("{}/me/accounts", endpoints.meta_graph))
            .query(&[
                (
                    "fields",
                    "id,name,access_token,instagram_business_account{id,username}",
                ),
                ("access_token", token.access_token.as_str()),
            ]);
        if let Some(cursor) = &after {
            request = request.query(&[("after", cursor)]);
        }
        let pages: MetaPages = request
            .send()
            .await
            .map_err(|_| "Meta could not be reached for the Page lookup")?
            .error_for_status()
            .map_err(|_| "Meta rejected the Page lookup")?
            .json()
            .await
            .map_err(|_| "Meta returned an invalid Page response")?;
        if let Some(page) = pages.data.into_iter().find(|page| page.id == page_id) {
            break page;
        }
        let Some(paging) = pages.paging.filter(|paging| paging.next.is_some()) else {
            return Err("The selected Facebook Page was not returned by Meta");
        };
        // Only send a cursor back to the configured endpoint. A provider-supplied
        // next URL may contain credentials and is never followed or logged.
        let cursor = paging
            .cursors
            .and_then(|cursors| cursors.after)
            .filter(|cursor| !cursor.is_empty() && seen_cursors.insert(cursor.clone()))
            .ok_or("Meta returned invalid Page pagination")?;
        after = Some(cursor);
    };
    let instagram = page
        .instagram_business_account
        .filter(|account| account.id == instagram_id)
        .ok_or("The selected Instagram account is not linked to the selected Facebook Page")?;
    Ok(vec![
        ConnectedIdentity {
            provider: Provider::Facebook,
            account_id: page.id,
            display_name: page.name,
            access_token: page.access_token.clone(),
        },
        ConnectedIdentity {
            provider: Provider::Instagram,
            account_id: instagram.id,
            display_name: instagram.username,
            access_token: page.access_token,
        },
    ])
}

async fn discover_x(
    http: &reqwest::Client,
    endpoints: &ProviderEndpoints,
    token: &TokenResponse,
) -> Result<Vec<ConnectedIdentity>, String> {
    let response = http
        .get(format!("{}/users/me", endpoints.x_api))
        .bearer_auth(&token.access_token)
        .send()
        .await
        .map_err(|_| "X identity endpoint could not be reached".to_string())?;
    if !response.status().is_success() {
        return Err("X identity could not be verified".into());
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "X identity response was invalid".to_string())?;
    let id = body
        .pointer("/data/id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| "X identity response was incomplete".to_string())?;
    Ok(vec![ConnectedIdentity {
        provider: Provider::X,
        account_id: id.into(),
        display_name: body
            .pointer("/data/username")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        access_token: token.access_token.clone(),
    }])
}

async fn discover_identities(
    http: &reqwest::Client,
    provider: Provider,
    endpoints: &ProviderEndpoints,
    token: &TokenResponse,
) -> Result<Vec<ConnectedIdentity>, String> {
    match provider {
        Provider::Tiktok => Err("TikTok requires its verified account exchange".into()),
        Provider::Linkedin => discover_linkedin(http, endpoints, token).await,
        Provider::X => discover_x(http, endpoints, token).await,
        Provider::Facebook | Provider::Instagram => {
            let response = http
                .get(format!("{}/me/accounts", endpoints.meta_graph))
                .query(&[
                    (
                        "fields",
                        "id,name,access_token,instagram_business_account{id,username}",
                    ),
                    ("access_token", token.access_token.as_str()),
                ])
                .send()
                .await
                .map_err(|_| "Meta pages could not be reached".to_string())?;
            if !response.status().is_success() {
                return Err("Meta pages could not be verified".into());
            }
            let pages: MetaPages = response
                .json()
                .await
                .map_err(|_| "Meta pages response was invalid".to_string())?;
            Ok(pages
                .data
                .into_iter()
                .filter_map(|page| match provider {
                    Provider::Facebook => Some(ConnectedIdentity {
                        provider,
                        account_id: page.id,
                        display_name: page.name,
                        access_token: page.access_token,
                    }),
                    Provider::Instagram => {
                        page.instagram_business_account
                            .map(|account| ConnectedIdentity {
                                provider,
                                account_id: account.id,
                                display_name: account.username,
                                access_token: page.access_token,
                            })
                    }
                    _ => None,
                })
                .collect())
        }
        Provider::Threads => {
            let response = http
                .get(format!("{}/v1.0/me", endpoints.threads_graph))
                .query(&[
                    ("fields", "id,username"),
                    ("access_token", token.access_token.as_str()),
                ])
                .send()
                .await
                .map_err(|_| "Threads identity endpoint could not be reached".to_string())?;
            if !response.status().is_success() {
                return Err("Threads identity could not be verified".into());
            }
            let account: ThreadsIdentity = response
                .json()
                .await
                .map_err(|_| "Threads identity response was invalid".to_string())?;
            Ok(vec![ConnectedIdentity {
                provider,
                account_id: account.id,
                display_name: account.username,
                access_token: token.access_token.clone(),
            }])
        }
        Provider::Youtube => {
            let response = http
                .get(&endpoints.youtube_channels)
                .bearer_auth(&token.access_token)
                .query(&[("part", "id,snippet"), ("mine", "true")])
                .send()
                .await
                .map_err(|_| "YouTube channels endpoint could not be reached".to_string())?;
            if !response.status().is_success() {
                return Err("YouTube channel could not be verified".into());
            }
            let channels: YoutubeChannels = response
                .json()
                .await
                .map_err(|_| "YouTube channels response was invalid".to_string())?;
            Ok(channels
                .items
                .into_iter()
                .map(|channel| ConnectedIdentity {
                    provider,
                    account_id: channel.id,
                    display_name: Some(channel.snippet.title),
                    access_token: token.access_token.clone(),
                })
                .collect())
        }
    }
}

#[derive(Deserialize)]
struct LinkedInIntrospection {
    active: bool,
    client_id: Option<String>,
    scope: Option<String>,
    expires_at: Option<i64>,
}

async fn exchange_linkedin(
    http: &reqwest::Client,
    config: &OAuthConfig,
    endpoints: &ProviderEndpoints,
    code: &str,
) -> Result<TokenResponse, String> {
    let response = http
        .post(&endpoints.linkedin_token)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", config.redirect_uri.as_str()),
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
        ])
        .send()
        .await
        .map_err(|_| "LinkedIn token exchange could not be reached".to_string())?;
    if !response.status().is_success() {
        return Err("LinkedIn token exchange was refused".into());
    }
    let mut token: TokenResponse = response
        .json()
        .await
        .map_err(|_| "LinkedIn token response was invalid".to_string())?;
    let response = http
        .post(&endpoints.linkedin_introspect)
        .form(&[
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("token", token.access_token.as_str()),
        ])
        .send()
        .await
        .map_err(|_| "LinkedIn permissions could not be reached".to_string())?;
    if !response.status().is_success() {
        return Err("LinkedIn permissions could not be verified".into());
    }
    let introspection: LinkedInIntrospection = response
        .json()
        .await
        .map_err(|_| "LinkedIn permission response was invalid".to_string())?;
    let lifetime = introspection
        .expires_at
        .map(|expiry| expiry - Utc::now().timestamp());
    if !introspection.active
        || introspection.client_id.as_deref() != Some(&config.client_id)
        || token.access_token.trim().is_empty()
        || lifetime.is_none_or(|seconds| seconds <= 0)
    {
        return Err("LinkedIn authorization is not valid".into());
    }
    token.scope = introspection.scope;
    token.expires_in = lifetime;
    // Programmatic refresh requires separate provider approval. Reconnect on expiry.
    token.refresh_token = None;
    Ok(token)
}

#[derive(Deserialize)]
struct LinkedInAcls {
    elements: Vec<LinkedInAcl>,
}

#[derive(Deserialize)]
struct LinkedInAcl {
    #[serde(alias = "organizationTarget")]
    organization: String,
    role: String,
    state: String,
}

/// Paginate approved administrator roles; never import unrelated roles.
async fn discover_linkedin(
    http: &reqwest::Client,
    endpoints: &ProviderEndpoints,
    token: &TokenResponse,
) -> Result<Vec<ConnectedIdentity>, String> {
    let mut identities = Vec::new();
    for page in 0..100 {
        let response = http
            .get(format!("{}/organizationAcls", endpoints.linkedin_rest))
            .bearer_auth(&token.access_token)
            .header("LinkedIn-Version", "202609")
            .header("X-Restli-Protocol-Version", "2.0.0")
            .query(&[
                ("q", "roleAssignee"),
                ("role", "ADMINISTRATOR"),
                ("state", "APPROVED"),
                ("count", "100"),
                ("start", &(page * 100).to_string()),
            ])
            .send()
            .await
            .map_err(|_| "LinkedIn Page roles could not be reached".to_string())?;
        if !response.status().is_success() {
            return Err("LinkedIn Page roles could not be verified".into());
        }
        let acls: LinkedInAcls = response
            .json()
            .await
            .map_err(|_| "LinkedIn Page roles response was invalid".to_string())?;
        let count = acls.elements.len();
        for acl in acls.elements {
            if acl.role != "ADMINISTRATOR" || acl.state != "APPROVED" {
                continue;
            }
            let Some(id) = acl.organization.strip_prefix("urn:li:organization:") else {
                continue;
            };
            if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            identities.push(ConnectedIdentity {
                provider: Provider::Linkedin,
                account_id: id.to_string(),
                display_name: None,
                access_token: token.access_token.clone(),
            });
        }
        if count < 100 {
            return Ok(identities);
        }
    }
    Err("LinkedIn Page role lookup exceeded its limit".into())
}

fn redirect_error(back: &str, message: &str) -> Response {
    Redirect::to(&format!(
        "{back}?error={}",
        crate::admin::encode_query_value(message)
    ))
    .into_response()
}

pub fn public_routes() -> Router<crate::AppState> {
    Router::new()
        .route("/social/oauth/callback", get(callback))
        .route("/social/oauth/callback/", get(callback))
}

pub fn admin_routes() -> Router<crate::admin::AdminState> {
    Router::new()
        .route(
            "/app/admin/brands/{brand_key}/social/{provider}/selection",
            post(save_selection),
        )
        .route(
            "/app/admin/brands/{brand_key}/social/{provider}/connect",
            post(connect_start),
        )
        .route(
            "/app/admin/brands/{brand_key}/social/{provider}/{account_id}/disconnect",
            post(disconnect),
        )
}

#[derive(Debug, Deserialize)]
pub struct DisconnectForm {
    #[serde(rename = "_csrf")]
    _csrf: Option<String>,
}

async fn disconnect(
    State(state): State<crate::admin::AdminState>,
    session: Option<Extension<SessionData>>,
    Path((brand_key, provider_name, account_id)): Path<(String, String, String)>,
    Form(_form): Form<DisconnectForm>,
) -> Response {
    let back = format!("/app/admin/brands/{brand_key}/edit");
    let Some(session) = session.map(|Extension(session)| session) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (Some(actor_id), Some(provider)) = (session.person_id, Provider::parse(&provider_name))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let provider = match provider {
        Provider::Facebook => store::social_accounts::SocialProvider::Facebook,
        Provider::Instagram => store::social_accounts::SocialProvider::Instagram,
        Provider::Threads => store::social_accounts::SocialProvider::Threads,
        Provider::Youtube => store::social_accounts::SocialProvider::Youtube,
        Provider::Linkedin => store::social_accounts::SocialProvider::Linkedin,
        Provider::X => store::social_accounts::SocialProvider::X,
        Provider::Tiktok => store::social_accounts::SocialProvider::Tiktok,
    };
    match store::social_accounts::revoke(
        &state.application_store,
        session.role,
        actor_id,
        &brand_key,
        provider,
        &account_id,
    )
    .await
    {
        Ok(()) => Redirect::to(&format!("{back}?social_disconnected=1")).into_response(),
        Err(_) => redirect_error(&back, "The social account could not be disconnected"),
    }
}

#[cfg(test)]
#[path = "social_oauth/compatibility_tests.rs"]
mod tests;
