//! Headless sign-in to Forgejo through Rauthy, for the KIND smoke.
//!
//! Drives the same Authorization Code exchange a browser does: Forgejo's
//! `/user/oauth2/<source>` redirect, Rauthy's login (CSRF token, session
//! cookie, proof of work, password), and Forgejo's callback. Only the
//! synthetic KIND fixture users ever sign in here. Network-bound, so it is
//! excluded from the coverage gate; its parsing helpers live in `mod.rs`.

use anyhow::{bail, ensure, Context, Result};
use reqwest::header::{HeaderMap, COOKIE, LOCATION, SET_COOKIE};
use reqwest::{Client, Response, StatusCode};

use super::{
    enrollment_secret, forgejo_csrf, path_and_query, rauthy_template, solve_pow, CookieJar,
    FIXTURE_PASSWORD,
};

/// The Forgejo `OAuth2` source name in the KIND identity wiring.
const SOURCE: &str = "rauthy";

/// Where Forgejo left the browser after its callback.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    /// Signed in: redirected home with a session.
    SignedIn,
    /// No account and no auto-registration: offered the link-account page.
    LinkAccount,
    /// The identity provider vouched for the user, and Forgejo now asks for
    /// the account's TOTP code before it opens a session.
    TwoFactor,
    /// Refused outright, with the HTTP status and page title.
    Refused(u16, String),
}

pub(super) fn client() -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("navigator-forgejo-smoke")
        // Solving Rauthy's proof of work can outlast its keep-alive, and a
        // reused connection the server already closed fails the login. A
        // fresh connection per request costs nothing at this volume.
        .pool_max_idle_per_host(0)
        .build()
        .context("build the smoke HTTP client")
}

fn keep(jar: &mut CookieJar, headers: &HeaderMap) {
    jar.store(
        headers
            .get_all(SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok()),
    );
}

fn location(response: &Response) -> Result<String> {
    response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .with_context(|| format!("{} carried no Location", response.url()))
}

async fn get(client: &Client, url: &str, jar: &mut CookieJar) -> Result<Response> {
    let mut request = client.get(url);
    if let Some(cookies) = jar.header() {
        request = request.header(COOKIE, cookies);
    }
    let response = request.send().await.with_context(|| format!("GET {url}"))?;
    keep(jar, response.headers());
    Ok(response)
}

fn title(page: &str) -> String {
    page.split_once("<title>")
        .and_then(|(_, rest)| rest.split_once("</title>"))
        .map(|(title, _)| title.trim().to_owned())
        .unwrap_or_default()
}

/// Sign `email` in to the Forgejo at `forge` (a port-forward origin) and
/// report where Forgejo left the session. `jar` carries Forgejo's cookies.
pub(super) async fn sign_in(
    client: &Client,
    forge: &str,
    email: &str,
    jar: &mut CookieJar,
) -> Result<Outcome> {
    let start = get(client, &format!("{forge}/user/oauth2/{SOURCE}"), jar).await?;
    ensure!(
        start.status().is_redirection(),
        "Forgejo did not redirect to its OIDC source ({})",
        start.status()
    );
    let authorize = url::Url::parse(&location(&start)?).context("parse the authorize URL")?;
    let rauthy = authorize.origin().ascii_serialization();
    let params: std::collections::BTreeMap<String, String> = authorize
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();

    let mut rauthy_jar = CookieJar::default();
    let page = get(client, authorize.as_str(), &mut rauthy_jar).await?;
    ensure!(
        page.status() == StatusCode::OK,
        "Rauthy's authorize page answered {}",
        page.status()
    );
    let html = page.text().await.context("read Rauthy's login page")?;
    let csrf = rauthy_template(&html, "tpl_csrf_token")
        .context("Rauthy's login page has no CSRF token")?;

    let challenge = client
        .post(format!("{rauthy}/auth/v1/pow"))
        .send()
        .await
        .context("request a Rauthy proof of work")?
        .text()
        .await
        .context("read the proof of work")?;
    let pow = solve_pow(challenge.trim())?;

    let param = |key: &str| params.get(key).cloned();
    let mut body = serde_json::json!({
        "email": email,
        "password": FIXTURE_PASSWORD,
        "client_id": param("client_id").context("authorize URL has no client_id")?,
        "redirect_uri": param("redirect_uri").context("authorize URL has no redirect_uri")?,
        "scopes": param("scope").unwrap_or_default().split(' ').collect::<Vec<_>>(),
        "state": param("state"),
        "nonce": param("nonce"),
        "pow": pow,
    });
    if let (Some(challenge), Some(method)) =
        (param("code_challenge"), param("code_challenge_method"))
    {
        body["code_challenge"] = challenge.into();
        body["code_challenge_method"] = method.into();
    }
    let login = client
        .post(format!("{rauthy}/auth/v1/oidc/authorize"))
        .header("x-csrf-token", csrf)
        .header(COOKIE, rauthy_jar.header().unwrap_or_default())
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await
        .context("submit the Rauthy login")?;
    if login.status() != StatusCode::ACCEPTED {
        bail!(
            "Rauthy refused {email}: {} {}",
            login.status(),
            login.text().await.unwrap_or_default()
        );
    }
    let callback = path_and_query(&location(&login)?)?;

    let finished = get(client, &format!("{forge}{callback}"), jar).await?;
    let status = finished.status();
    if status.is_redirection() {
        let next = location(&finished)?;
        let path = if next.starts_with('/') {
            next
        } else {
            path_and_query(&next)?
        };
        return Ok(match path {
            path if path == "/" => Outcome::SignedIn,
            path if path.starts_with("/user/link_account") => Outcome::LinkAccount,
            path if path.starts_with("/user/two_factor") => Outcome::TwoFactor,
            path => Outcome::Refused(status.as_u16(), format!("redirect to {path}")),
        });
    }
    let page = finished.text().await.unwrap_or_default();
    Ok(Outcome::Refused(status.as_u16(), title(&page)))
}

/// POST a same-origin form, as Forgejo's pages submit it.
async fn post_form(
    client: &Client,
    forge: &str,
    path: &str,
    jar: &mut CookieJar,
    form: &[(&str, &str)],
) -> Result<Response> {
    let response = client
        .post(format!("{forge}{path}"))
        .header(COOKIE, jar.header().unwrap_or_default())
        .header("origin", forge)
        .header("sec-fetch-site", "same-origin")
        .form(form)
        .send()
        .await
        .with_context(|| format!("POST {path}"))?;
    keep(jar, response.headers());
    Ok(response)
}

/// Where a session lands on `path`: the status and any redirect target.
pub(super) async fn landing(
    client: &Client,
    forge: &str,
    path: &str,
    jar: &mut CookieJar,
) -> Result<(u16, String)> {
    let response = get(client, &format!("{forge}{path}"), jar).await?;
    let status = response.status().as_u16();
    let target = response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    Ok((status, target))
}

/// From the link-account page, try to register the external identity as a
/// new local account. Returns the response status; the caller proves the
/// outcome in the database.
pub(super) async fn try_link_account_signup(
    client: &Client,
    forge: &str,
    email: &str,
    jar: &mut CookieJar,
) -> Result<u16> {
    let page = get(client, &format!("{forge}/user/link_account"), jar).await?;
    let html = page.text().await.context("read the link-account page")?;
    let csrf = forgejo_csrf(&html).unwrap_or_default();
    let user_name = email.split('@').next().unwrap_or(email);
    let password = "synthetic-link-attempt-password";
    let response = post_form(
        client,
        forge,
        "/user/link_account_signup",
        jar,
        &[
            ("_csrf", csrf.as_str()),
            ("user_name", user_name),
            ("email", email),
            ("password", password),
            ("retype", password),
        ],
    )
    .await?;
    Ok(response.status().as_u16())
}

/// Try local self-service registration. Returns the page and the status.
pub(super) async fn try_local_signup(client: &Client, forge: &str) -> Result<(String, u16)> {
    let mut jar = CookieJar::default();
    let page = get(client, &format!("{forge}/user/sign_up"), &mut jar).await?;
    let html = page.text().await.context("read the sign-up page")?;
    let csrf = forgejo_csrf(&html).unwrap_or_default();
    let password = "synthetic-signup-password";
    let response = post_form(
        client,
        forge,
        "/user/sign_up",
        &mut jar,
        &[
            ("_csrf", csrf.as_str()),
            ("user_name", "forge-local-signup"),
            ("email", "forge-local-signup@example.test"),
            ("password", password),
            ("retype", password),
        ],
    )
    .await?;
    Ok((html, response.status().as_u16()))
}

/// Read the TOTP secret Forgejo offers on its enrollment page.
pub(super) async fn enrollment_page_secret(
    client: &Client,
    forge: &str,
    jar: &mut CookieJar,
) -> Result<String> {
    let page = get(
        client,
        &format!("{forge}/user/settings/security/two_factor/enroll"),
        jar,
    )
    .await?;
    ensure!(
        page.status() == StatusCode::OK,
        "the TOTP enrollment page answered {}",
        page.status()
    );
    let html = page.text().await.context("read the enrollment page")?;
    enrollment_secret(&html).context("the enrollment page shows no TOTP secret")
}

/// Confirm TOTP enrollment with a code. Returns the status and redirect.
pub(super) async fn enroll(
    client: &Client,
    forge: &str,
    jar: &mut CookieJar,
    code: &str,
) -> Result<(u16, String)> {
    let response = post_form(
        client,
        forge,
        "/user/settings/security/two_factor/enroll",
        jar,
        &[("passcode", code)],
    )
    .await?;
    let status = response.status().as_u16();
    let target = response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    Ok((status, target))
}

/// Answer the sign-in TOTP prompt. Returns the status and redirect.
pub(super) async fn submit_two_factor(
    client: &Client,
    forge: &str,
    jar: &mut CookieJar,
    code: &str,
) -> Result<(u16, String)> {
    let response = post_form(
        client,
        forge,
        "/user/two_factor",
        jar,
        &[("passcode", code)],
    )
    .await?;
    let status = response.status().as_u16();
    let target = response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    Ok((status, target))
}
