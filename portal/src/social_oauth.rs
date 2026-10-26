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
        &state.surreal,
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
        store::social_account_selections::for_brand(&state.surreal, &brand_key).await
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
        store::social_accounts::for_actor(&state.surreal, session.role, actor_id).await
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
        &state.surreal,
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
    if store::social_oauth_attempts::bind_account(&state.surreal, &attempt, target)
        .await
        .is_err()
    {
        return redirect_error(&back, "Could not bind the provider account to this Brand");
    }
    if let Some(instagram) = linked_instagram {
        if store::social_oauth_attempts::bind_instagram(
            &state.surreal,
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
        store::social_oauth_attempts::consume(&state.surreal, raw_state, Utc::now()).await
    else {
        return Err(invalid_authorization());
    };
    let back = format!("/app/admin/brands/{}/edit", attempt.brand_key);
    let Some(person) = store::persons::find_by_id(&state.surreal, attempt.actor_person_id)
        .await
        .ok()
        .flatten()
    else {
        return Err(redirect_error(
            &back,
            "The authorizing person is no longer available",
        ));
    };
    if !store::persons::is_admitted(&state.surreal, person.id)
        .await
        .unwrap_or(false)
    {
        return Err(redirect_error(
            &back,
            "The authorizing person is not admitted",
        ));
    }
    let Some(brand) = store::brands::find_by_key(&state.surreal, &attempt.brand_key)
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
            &state.surreal,
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
) -> Result<(TokenResponse, Vec<String>, Vec<ConnectedIdentity>), Response> {
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
            &input.state.surreal,
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
                    &input.state.surreal,
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
    let (token, scopes, identities) = match exchange_and_discover(
        provider,
        &config,
        endpoints,
        code,
        &authorized.attempt.pkce_verifier,
        &authorized.back,
    )
    .await
    {
        Ok(discovered) => discovered,
        Err(response) => return response,
    };
    let identities = match selected_callback_identities(
        provider,
        authorized.attempt.instagram_account_id.as_deref(),
        target,
        &token,
        &scopes,
        endpoints,
        identities,
    )
    .await
    {
        Ok(identities) => identities,
        Err(message) => return redirect_error(&authorized.back, message),
    };
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

async fn selected_callback_identities(
    provider: Provider,
    instagram: Option<&str>,
    target: &str,
    token: &TokenResponse,
    scopes: &[String],
    endpoints: &ProviderEndpoints,
    identities: Vec<ConnectedIdentity>,
) -> Result<Vec<ConnectedIdentity>, &'static str> {
    let Some(instagram) = instagram else {
        return Ok(identities
            .into_iter()
            .filter(|identity| identity_matches(provider, identity, target))
            .collect());
    };
    if provider != Provider::Facebook || !scopes_are_sufficient(Provider::Instagram, scopes) {
        return Err(
            "Facebook and Instagram did not grant all required permissions. Reconnect Meta.",
        );
    }
    discover_selected_meta(&reqwest::Client::new(), endpoints, token, target, instagram).await.map_err(|()| "The selected Instagram account is not linked to the selected Facebook Page. Check both saved identities and reconnect Meta.")
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
) -> Result<Vec<ConnectedIdentity>, ()> {
    let pages: MetaPages = http
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
        .map_err(|_| ())?
        .error_for_status()
        .map_err(|_| ())?
        .json()
        .await
        .map_err(|_| ())?;
    let page = pages
        .data
        .into_iter()
        .find(|page| page.id == page_id)
        .ok_or(())?;
    let instagram = page
        .instagram_business_account
        .filter(|account| account.id == instagram_id)
        .ok_or(())?;
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
        &state.surreal,
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
mod tests {
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

    async fn begin_facebook(
        db: &store::surreal::SurrealDb,
        admin: Uuid,
        brand_key: &str,
    ) -> String {
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
        let url = super::selected_authorization_url(Provider::Facebook, &config(), &attempt, true)
            .unwrap();
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
            let url = authorization_url(provider, &config, "synthetic-state", "synthetic-verifier")
                .unwrap();
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
        let token =
            super::exchange_meta_short_token(&http, &config(), &endpoints, &token.access_token)
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
                assert!(accounts
                    .iter()
                    .any(|a| a.provider == SocialProvider::Instagram
                        && a.provider_account_id == "ig-99"));
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
            store::social_accounts::selected_for_brand(
                &db,
                &brand,
                std::slice::from_ref(&account.id)
            )
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
        let accounts = store::social_accounts::for_provider_identity(
            &db,
            SocialProvider::Threads,
            "threads-99",
        )
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
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data":{"id":"1002","username":"SyntheticBrand"}}),
            ))
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
}
