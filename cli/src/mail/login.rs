//! `navigator mail login`: the installed-app OAuth flow that mints the Gmail
//! refresh token `navigator mail` redeems.
//!
//! The flow is Google's loopback redirect for desktop clients: a one-shot
//! listener on `127.0.0.1` at a random port, PKCE (S256), and a `state`
//! nonce the callback must echo. The authorization request names exactly
//! the two scopes the CLI is built around, and the granted scopes are
//! checked again after the exchange, because the consent screen lets a user
//! (or an earlier grant to the same client) change what the token carries.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use reqwest::Response;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use url::Url;

use super::{
    keychain_delete, keychain_read, keychain_write, CLIENT_SECRET_SERVICE, GOOGLE_TOKEN_API,
    REFRESH_TOKEN_SERVICE,
};

const GOOGLE_AUTHORIZATION_API: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_REVOKE_API: &str = "https://oauth2.googleapis.com/revoke";

/// The only scopes `navigator mail` requests or accepts: read mail, and
/// create or update drafts. `gmail.compose` can send at Google's level; the
/// guard against that is the endpoint allow-list, which has no send path.
pub(super) const GMAIL_SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.compose",
];

/// How long the loopback listener waits for the browser redirect.
const CALLBACK_TIMEOUT: Duration = Duration::from_mins(5);

/// A PKCE code verifier: 32 random bytes, base64url without padding, which
/// is 43 characters from the RFC 7636 unreserved set.
fn pkce_verifier() -> String {
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The S256 code challenge for a verifier (RFC 7636 §4.2).
fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A 128-bit hex nonce the callback must echo back unchanged.
fn random_state() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes
        .iter()
        .fold(String::with_capacity(32), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// The Google authorization URL the browser opens.
fn authorization_url(
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
    account: &str,
) -> Result<Url> {
    let scope = GMAIL_SCOPES.join(" ");
    Url::parse_with_params(
        GOOGLE_AUTHORIZATION_API,
        &[
            ("client_id", client_id),
            ("redirect_uri", redirect_uri),
            ("response_type", "code"),
            ("scope", scope.as_str()),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("login_hint", account),
            ("state", state),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ],
    )
    .context("build the Google authorization URL")
}

/// Refuses a grant that carries any scope beyond the two, or that is
/// missing either of them. Google reports granted scopes space-separated
/// and in any order.
pub(super) fn check_granted_scopes(granted: Option<&str>) -> Result<()> {
    let granted = granted
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Google did not report the token's granted scopes"))?;
    let scopes: Vec<&str> = granted.split_whitespace().collect();
    let extra = scopes
        .iter()
        .filter(|scope| !GMAIL_SCOPES.contains(scope))
        .copied()
        .collect::<Vec<_>>();
    if !extra.is_empty() {
        return Err(anyhow!(
            "the token carries scopes beyond gmail.readonly and gmail.compose ({}); refusing it",
            extra.join(", ")
        ));
    }
    let missing = GMAIL_SCOPES
        .iter()
        .filter(|scope| !scopes.contains(scope))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(anyhow!(
            "the token is missing {}; grant both gmail.readonly and gmail.compose on the consent screen",
            missing.join(", ")
        ));
    }
    Ok(())
}

/// What the loopback callback carried.
#[derive(Debug, Eq, PartialEq)]
enum Callback {
    /// The authorization code, after the echoed `state` matched.
    Code(String),
    /// A request that is not the OAuth redirect (a favicon fetch, a
    /// speculative preconnect); keep listening.
    Ignored,
}

/// Reads one HTTP request off the socket and classifies it. A `state`
/// mismatch or an `error` parameter ends the login: the code is never
/// redeemed.
async fn handle_callback(socket: &mut TcpStream, expected_state: &str) -> Result<Callback> {
    let mut buffer = [0u8; 8192];
    let read = socket
        .read(&mut buffer)
        .await
        .context("read the callback")?;
    if read == 0 {
        return Ok(Callback::Ignored);
    }
    let request = String::from_utf8_lossy(&buffer[..read]);
    let Some(target) = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
    else {
        respond(socket, 400, "Bad Request", "Malformed request.").await;
        return Ok(Callback::Ignored);
    };
    let url = Url::parse(&format!("http://127.0.0.1{target}")).context("parse the callback")?;
    let (mut state, mut code, mut error) = (None, None, None);
    for (name, value) in url.query_pairs() {
        match name.as_ref() {
            "state" => state = Some(value.into_owned()),
            "code" => code = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if state.is_none() && code.is_none() && error.is_none() {
        respond(socket, 404, "Not Found", "Not found.").await;
        return Ok(Callback::Ignored);
    }
    if state.as_deref() != Some(expected_state) {
        respond(
            socket,
            400,
            "Bad Request",
            "State mismatch; nothing was stored.",
        )
        .await;
        return Err(anyhow!(
            "the callback's state did not match; refusing the authorization code"
        ));
    }
    if let Some(error) = error {
        respond(socket, 400, "Bad Request", "Authorization was not granted.").await;
        // Google's error values are fixed codes such as access_denied; keep
        // only the code's own characters.
        let error: String = error
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(64)
            .collect();
        return Err(anyhow!("Google refused the authorization: {error}"));
    }
    let code = code.ok_or_else(|| anyhow!("the callback carried no authorization code"))?;
    respond(
        socket,
        200,
        "OK",
        "Gmail authorization received. You can close this tab and return to the terminal.",
    )
    .await;
    Ok(Callback::Code(code))
}

async fn respond(socket: &mut TcpStream, status: u16, reason: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;
}

/// Accepts connections until the OAuth redirect arrives or the timeout
/// passes.
async fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    tokio::time::timeout(CALLBACK_TIMEOUT, async {
        loop {
            let (mut socket, _) = listener
                .accept()
                .await
                .context("accept the callback connection")?;
            if let Callback::Code(code) = handle_callback(&mut socket, expected_state).await? {
                return Ok(code);
            }
        }
    })
    .await
    .map_err(|_| anyhow!("timed out waiting for the browser redirect"))?
}

/// The token endpoint's grant. Never printed or logged.
#[derive(Deserialize)]
pub(super) struct TokenGrant {
    pub(super) access_token: String,
    #[serde(default)]
    pub(super) refresh_token: Option<String>,
    #[serde(default)]
    pub(super) scope: Option<String>,
}

#[derive(Deserialize)]
struct TokenError {
    error: Option<String>,
}

/// Parses a token-endpoint response. A failure names Google's fixed error
/// code (`invalid_client`, `invalid_grant`, ...) and nothing else from the
/// body, which can echo request values.
pub(super) async fn token_response(response: Response) -> Result<TokenGrant> {
    let status = response.status();
    if status.is_success() {
        return response
            .json()
            .await
            .context("parse Google token endpoint response");
    }
    let code = response
        .json::<TokenError>()
        .await
        .ok()
        .and_then(|body| body.error)
        .filter(|code| {
            code.len() <= 64 && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
    Err(match code {
        Some(code) => anyhow!("Google token endpoint request failed: {status} ({code})"),
        None => anyhow!("Google token endpoint request failed: {status}"),
    })
}

/// Redeems the authorization code with the PKCE verifier.
async fn exchange_code(
    http: &reqwest::Client,
    token_url: &str,
    form: &AuthorizationCodeForm<'_>,
) -> Result<TokenGrant> {
    let response = http
        .post(token_url)
        .form(&form.pairs())
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .context("exchange the authorization code")?;
    token_response(response).await
}

struct AuthorizationCodeForm<'a> {
    code: &'a str,
    client_id: &'a str,
    client_secret: &'a str,
    redirect_uri: &'a str,
    code_verifier: &'a str,
}

impl AuthorizationCodeForm<'_> {
    fn pairs(&self) -> [(&str, &str); 6] {
        [
            ("code", self.code),
            ("client_id", self.client_id),
            ("client_secret", self.client_secret),
            ("redirect_uri", self.redirect_uri),
            ("grant_type", "authorization_code"),
            ("code_verifier", self.code_verifier),
        ]
    }
}

/// Best-effort revocation of a token the CLI refuses to keep. Revoking a
/// refresh token also revokes its access tokens.
pub(super) async fn revoke(http: &reqwest::Client, token: &str) {
    let _ = http
        .post(GOOGLE_REVOKE_API)
        .form(&[("token", token)])
        .timeout(Duration::from_secs(30))
        .send()
        .await;
}

/// The stored client secret, or one read from the terminal and stored.
///
/// With `--client-secret-file`, the secret comes from Google's downloaded
/// client JSON and replaces any stored one. Otherwise the Keychain item is
/// used, and the terminal is prompted only when the item is missing.
fn client_secret(client_id: &str, secret_file: Option<&Path>) -> Result<String> {
    let secret = if let Some(path) = secret_file {
        let bytes = std::fs::read(path)
            .with_context(|| format!("read the client secret file {}", path.display()))?;
        secret_from_client_json(&bytes, client_id)?
    } else {
        if let Some(secret) = keychain_read(CLIENT_SECRET_SERVICE, client_id)? {
            return Ok(secret);
        }
        prompt_hidden("Google OAuth client secret (input hidden): ")?
            .trim()
            .to_owned()
    };
    if secret.is_empty() {
        return Err(anyhow!("no client secret entered; nothing was stored"));
    }
    keychain_write(CLIENT_SECRET_SERVICE, client_id, &secret)?;
    eprintln!("Stored the client secret in the macOS Keychain, service {CLIENT_SECRET_SERVICE}.");
    Ok(secret)
}

#[derive(Deserialize)]
struct ClientJson {
    installed: Option<InstalledClient>,
}

#[derive(Deserialize)]
struct InstalledClient {
    client_id: Option<String>,
    client_secret: Option<String>,
}

/// `.installed.client_secret` from a Google `client_secret_*.json`. Refuses
/// a file for a different client id, or one that is not a Desktop-app
/// (`installed`) client. Errors never quote the file's contents.
fn secret_from_client_json(bytes: &[u8], client_id: &str) -> Result<String> {
    let parsed: ClientJson = serde_json::from_slice(bytes)
        .map_err(|_| anyhow!("the client secret file is not Google client JSON"))?;
    let installed = parsed.installed.ok_or_else(|| {
        anyhow!("the client secret file has no `installed` client; download a Desktop-app client's JSON")
    })?;
    if installed
        .client_id
        .as_deref()
        .is_some_and(|id| id.trim() != client_id.trim())
    {
        return Err(anyhow!(
            "the client secret file is for a different OAuth client than the configured client id; nothing was stored"
        ));
    }
    installed
        .client_secret
        .map(|secret| secret.trim().to_owned())
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| anyhow!("the client secret file has no installed.client_secret"))
}

/// Reads one line from the controlling terminal with echo off. Refuses when
/// there is no terminal, so a secret is never read from a pipe or a file.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn prompt_hidden(prompt: &str) -> Result<String> {
    use rustix::termios::{tcgetattr, tcsetattr, LocalModes, OptionalActions};
    use std::io::{BufRead as _, Write as _};

    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("no terminal to prompt for the client secret on; run navigator mail login interactively")?;
    let original = tcgetattr(&tty).context("read the terminal settings")?;
    let mut hidden = original.clone();
    hidden.local_modes.remove(LocalModes::ECHO);
    hidden.local_modes.insert(LocalModes::ECHONL);
    tcsetattr(&tty, OptionalActions::Flush, &hidden).context("turn off terminal echo")?;
    let result = (|| {
        let mut writer = &tty;
        writer.write_all(prompt.as_bytes())?;
        writer.flush()?;
        let mut line = String::new();
        std::io::BufReader::new(&tty).read_line(&mut line)?;
        Ok::<_, std::io::Error>(line)
    })();
    let restored = tcsetattr(&tty, OptionalActions::Now, &original);
    let line = result.context("read the client secret from the terminal")?;
    restored.context("restore terminal echo")?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn prompt_hidden(_prompt: &str) -> Result<String> {
    Err(anyhow!(
        "prompting for the client secret is supported only on macOS"
    ))
}

/// `navigator mail login`.
pub(super) async fn login_command(secret_file: Option<&Path>) -> Result<()> {
    let (account, client_id) = super::configured_setup()?;
    let client_secret = client_secret(&client_id, secret_file)?;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind the loopback listener on 127.0.0.1")?;
    let port = listener
        .local_addr()
        .context("read the loopback port")?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/");
    let state = random_state();
    let verifier = pkce_verifier();
    let url = authorization_url(
        &client_id,
        &redirect_uri,
        &state,
        &pkce_challenge(&verifier),
        &account,
    )?;
    eprintln!("Opening Google sign-in in your browser. If it does not open, visit:\n\n  {url}\n");
    crate::login::open_in_browser(url.as_str());

    let code = wait_for_code(&listener, &state).await?;
    let http = reqwest::Client::new();
    let grant = exchange_code(
        &http,
        GOOGLE_TOKEN_API,
        &AuthorizationCodeForm {
            code: &code,
            client_id: &client_id,
            client_secret: &client_secret,
            redirect_uri: &redirect_uri,
            code_verifier: &verifier,
        },
    )
    .await?;
    let refresh_token = grant.refresh_token.clone().ok_or_else(|| {
        anyhow!(
            "Google returned no refresh token; remove the app's access in your Google Account settings and rerun navigator mail login"
        )
    })?;
    if let Err(error) = check_granted_scopes(grant.scope.as_deref()) {
        revoke(&http, &refresh_token).await;
        return Err(error.context("the new token was revoked and not stored"));
    }
    let profile = super::GmailClient::new(grant.access_token.clone())?
        .profile()
        .await
        .context("read the signed-in Gmail profile")?;
    if !profile.email_address.eq_ignore_ascii_case(account.trim()) {
        revoke(&http, &refresh_token).await;
        return Err(anyhow!(
            "Google signed in a different account than the configured Gmail address; the token was revoked and not stored. Rerun navigator mail login and choose the configured account"
        ));
    }
    keychain_write(REFRESH_TOKEN_SERVICE, &account, &refresh_token)?;
    println!(
        "Stored the Gmail refresh token in the macOS Keychain, service {REFRESH_TOKEN_SERVICE}. Run navigator mail setup to check."
    );
    Ok(())
}

/// Deletes the stored refresh token when the refresh exchange reports a
/// scope beyond the two, so the over-broad token is not redeemed again.
pub(super) fn refuse_stored_token(account: &str, error: anyhow::Error) -> anyhow::Error {
    match keychain_delete(REFRESH_TOKEN_SERVICE, account) {
        Ok(()) => error.context(
            "the stored refresh token was deleted from the Keychain; run navigator mail login",
        ),
        Err(delete) => error.context(format!(
            "deleting the stored refresh token also failed ({delete:#})"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn pkce_challenge_is_unpadded_base64url_sha256_of_the_verifier() {
        // Computed independently: base64url(sha256(verifier)) with the `=`
        // padding stripped.
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mJ92K9qRDJ_ZkaPzeqyCl2KeKdfvi0"),
            "a3Y1UP8ov5P-4PFcdafxli32Z4Mgd_LRbL4Mbt59ckc"
        );
    }

    #[test]
    fn pkce_verifier_is_43_unreserved_characters_and_fresh() {
        let verifier = pkce_verifier();
        assert_eq!(verifier.len(), 43);
        assert!(verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(verifier, pkce_verifier());
    }

    #[test]
    fn state_is_32_hex_characters_and_fresh() {
        let state = random_state();
        assert_eq!(state.len(), 32);
        assert!(state.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(state, random_state());
    }

    #[test]
    fn authorization_url_requests_exactly_the_two_scopes_offline_with_pkce() {
        let url = authorization_url(
            "client.apps.googleusercontent.com",
            "http://127.0.0.1:53000/",
            "nonce",
            "challenge",
            "lawyer@example.test",
        )
        .unwrap();
        assert_eq!(url.host_str(), Some("accounts.google.com"));
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            pairs["scope"],
            "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/gmail.compose"
        );
        assert_eq!(pairs["access_type"], "offline");
        assert_eq!(pairs["prompt"], "consent");
        assert_eq!(pairs["login_hint"], "lawyer@example.test");
        assert_eq!(pairs["state"], "nonce");
        assert_eq!(pairs["code_challenge"], "challenge");
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(pairs["redirect_uri"], "http://127.0.0.1:53000/");
        assert_eq!(pairs["response_type"], "code");
        assert!(!pairs.contains_key("client_secret"));
        assert!(!pairs.contains_key("include_granted_scopes"));
    }

    #[test]
    fn granted_scopes_must_be_exactly_the_two_in_any_order() {
        let readonly = GMAIL_SCOPES[0];
        let compose = GMAIL_SCOPES[1];
        assert!(check_granted_scopes(Some(&format!("{compose} {readonly}"))).is_ok());
        assert!(check_granted_scopes(Some(&format!("{readonly} {compose}"))).is_ok());

        let broad = check_granted_scopes(Some(&format!(
            "{readonly} {compose} https://www.googleapis.com/auth/gmail.send"
        )))
        .unwrap_err();
        assert!(format!("{broad}").contains("gmail.send"));
        assert!(
            check_granted_scopes(Some(&format!("{readonly} https://mail.google.com/"))).is_err()
        );
        assert!(check_granted_scopes(Some(readonly)).is_err());
        assert!(check_granted_scopes(Some("")).is_err());
        assert!(check_granted_scopes(None).is_err());
    }

    async fn send_to(addr: SocketAddr, target: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response).await;
        response
    }

    #[tokio::test]
    async fn callback_returns_the_code_when_state_matches_after_ignoring_a_favicon() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let waiter = tokio::spawn(async move { wait_for_code(&listener, "nonce").await });
        let favicon = send_to(addr, "/favicon.ico").await;
        assert!(favicon.starts_with("HTTP/1.1 404"));
        let page = send_to(addr, "/?state=nonce&code=4%2Fauth-code&scope=x").await;
        assert!(page.starts_with("HTTP/1.1 200"));
        assert!(!page.contains("auth-code"));
        assert_eq!(waiter.await.unwrap().unwrap(), "4/auth-code");
    }

    #[tokio::test]
    async fn callback_refuses_a_state_mismatch() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let waiter = tokio::spawn(async move { wait_for_code(&listener, "nonce").await });
        send_to(addr, "/?state=forged&code=stolen").await;
        let error = waiter.await.unwrap().unwrap_err();
        assert!(format!("{error}").contains("state did not match"));
    }

    #[tokio::test]
    async fn callback_refuses_an_error_without_echoing_markup() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let waiter = tokio::spawn(async move { wait_for_code(&listener, "nonce").await });
        send_to(addr, "/?state=nonce&error=access_denied%3Cscript%3E").await;
        let error = format!("{}", waiter.await.unwrap().unwrap_err());
        assert!(error.contains("access_denied"));
        assert!(!error.contains('<'));
    }

    #[tokio::test]
    async fn code_exchange_sends_the_secret_verifier_and_redirect() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("client_secret=shh"))
            .and(body_string_contains("code_verifier=verifier"))
            .and(body_string_contains(
                "redirect_uri=http%3A%2F%2F127.0.0.1%3A53000%2F",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access",
                "refresh_token": "refresh",
                "scope": GMAIL_SCOPES.join(" "),
                "token_type": "Bearer",
                "expires_in": 3599,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let grant = exchange_code(
            &reqwest::Client::new(),
            &format!("{}/token", server.uri()),
            &AuthorizationCodeForm {
                code: "code",
                client_id: "client",
                client_secret: "shh",
                redirect_uri: "http://127.0.0.1:53000/",
                code_verifier: "verifier",
            },
        )
        .await
        .unwrap();
        assert_eq!(grant.refresh_token.as_deref(), Some("refresh"));
        assert!(check_granted_scopes(grant.scope.as_deref()).is_ok());
    }

    #[tokio::test]
    async fn token_errors_name_googles_code_but_never_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": "invalid_client",
                "error_description": "echoed secret shh",
            })))
            .mount(&server)
            .await;
        let error = exchange_code(
            &reqwest::Client::new(),
            &format!("{}/token", server.uri()),
            &AuthorizationCodeForm {
                code: "code",
                client_id: "client",
                client_secret: "shh",
                redirect_uri: "http://127.0.0.1:53000/",
                code_verifier: "verifier",
            },
        )
        .await
        .err()
        .unwrap();
        let text = format!("{error:#}");
        assert!(text.contains("invalid_client"));
        assert!(!text.contains("shh"));
    }

    #[test]
    fn the_secret_comes_from_installed_client_secret_for_the_configured_client() {
        let json = br#"{"installed":{"client_id":"client.apps.googleusercontent.com","client_secret":"shh","redirect_uris":["http://localhost"]}}"#;
        assert_eq!(
            secret_from_client_json(json, "client.apps.googleusercontent.com").unwrap(),
            "shh"
        );
        let other = secret_from_client_json(json, "other.apps.googleusercontent.com")
            .err()
            .unwrap();
        assert!(!format!("{other}").contains("shh"));
        let web = br#"{"web":{"client_id":"client","client_secret":"shh"}}"#;
        assert!(secret_from_client_json(web, "client").is_err());
        assert!(
            secret_from_client_json(br#"{"installed":{"client_id":"client"}}"#, "client").is_err()
        );
        let garbage = secret_from_client_json(b"shh not json", "client")
            .err()
            .unwrap();
        assert!(!format!("{garbage}").contains("shh"));
    }

    #[test]
    fn login_never_prints_the_secret_or_tokens() {
        let source = include_str!("login.rs");
        let body = source
            .split("pub(super) async fn login_command(")
            .nth(1)
            .unwrap()
            .split("pub(super) fn refuse_stored_token")
            .next()
            .unwrap();
        for forbidden in [
            "{client_secret",
            "{refresh_token",
            "{code}",
            "{grant",
            "{secret",
        ] {
            assert!(!body.contains(forbidden), "login prints `{forbidden}`");
        }
    }
}
