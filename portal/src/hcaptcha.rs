//! hCaptcha on the public lead forms.
//!
//! A deployment turns the challenge on by naming its public sitekey in
//! `HCAPTCHA_SITEKEY`; the secret that redeems a solved challenge then rides
//! the Secret rail as `HCAPTCHA_SECRET`, and boot refuses a sitekey without
//! it (`store::deployment::WEB_REQUIREMENTS`). Neither set leaves every form
//! exactly as it was, which is what KIND and local development run.
//!
//! The browser posts the solved token as `h-captcha-response`; [`HCaptcha::verify`]
//! redeems it once at `siteverify`, pinned to this sitekey so a token solved
//! on someone else's site cannot be replayed here.

use std::net::IpAddr;
use std::sync::LazyLock;

use serde::Deserialize;

/// The public sitekey. Inline Deployment env, not a secret.
pub const SITEKEY_ENV: &str = "HCAPTCHA_SITEKEY";
/// The account secret that redeems a token. Secret rail only.
pub const SECRET_ENV: &str = "HCAPTCHA_SECRET";
/// The form field the hCaptcha widget writes the solved token into.
pub const RESPONSE_FIELD: &str = "h-captcha-response";
/// The origins hCaptcha asks a CSP to admit in `script-src`, `frame-src`,
/// `style-src`, and `connect-src`. The page CSP admits them only on a page
/// that renders the widget (`webapp::lead_capture::HCaptchaWidget`).
pub const CSP_ORIGINS: &str = "https://hcaptcha.com https://*.hcaptcha.com";

const VERIFY_URL: &str = "https://api.hcaptcha.com/siteverify";

/// A deployment's hCaptcha keys. No `Debug`: the secret must never reach a log.
#[derive(Clone)]
pub struct HCaptcha {
    sitekey: String,
    secret: String,
    verify_url: String,
    client: reqwest::Client,
}

/// A sitekey without its secret: the deployment declared the challenge but
/// cannot redeem it, so boot stops rather than reject every lead.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{SECRET_ENV} is required once {SITEKEY_ENV} is set")]
pub struct MissingSecret;

/// The process's keys, read once. Pages read the sitekey from here; the lead
/// route reads [`crate::AppState::hcaptcha`], which `hosting` fills from it.
pub static CONFIGURED: LazyLock<Option<HCaptcha>> =
    LazyLock::new(|| HCaptcha::from_lookup(|key| std::env::var(key).ok()).unwrap_or(None));

impl HCaptcha {
    /// Read the keys. An unset or blank sitekey is "off".
    ///
    /// # Errors
    ///
    /// [`MissingSecret`] when the sitekey is set and the secret is not.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, MissingSecret> {
        let value = |key| {
            lookup(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let Some(sitekey) = value(SITEKEY_ENV) else {
            return Ok(None);
        };
        let secret = value(SECRET_ENV).ok_or(MissingSecret)?;
        Ok(Some(Self::new(sitekey, secret, VERIFY_URL)))
    }

    /// Keys pointed at an explicit `siteverify` URL — a test's mock server.
    #[must_use]
    pub fn new(
        sitekey: impl Into<String>,
        secret: impl Into<String>,
        verify_url: impl Into<String>,
    ) -> Self {
        Self {
            sitekey: sitekey.into(),
            secret: secret.into(),
            verify_url: verify_url.into(),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default(),
        }
    }

    /// The public sitekey the widget renders with.
    #[must_use]
    pub fn sitekey(&self) -> &str {
        &self.sitekey
    }

    /// Redeem a solved token. Fails closed: a blank token, a transport error,
    /// or any answer but `success: true` is a refusal.
    pub async fn verify(&self, token: &str, remote_ip: Option<IpAddr>) -> bool {
        let token = token.trim();
        if token.is_empty() {
            return false;
        }
        let remote_ip = remote_ip
            .filter(|ip| !ip.is_unspecified())
            .map(|ip| ip.to_string());
        let mut form = vec![
            ("secret", self.secret.as_str()),
            ("response", token),
            ("sitekey", self.sitekey.as_str()),
        ];
        if let Some(ip) = remote_ip.as_deref() {
            form.push(("remoteip", ip));
        }
        let answer = match self.client.post(&self.verify_url).form(&form).send().await {
            Ok(response) => response.json::<SiteVerify>().await,
            Err(error) => Err(error),
        };
        match answer {
            Ok(answer) => {
                if !answer.success {
                    tracing::info!(error_codes = ?answer.error_codes, "hcaptcha refused a token");
                }
                answer.success
            }
            Err(error) => {
                tracing::warn!(error = %error, "hcaptcha siteverify unreachable");
                false
            }
        }
    }
}

#[derive(Deserialize)]
struct SiteVerify {
    success: bool,
    #[serde(default, rename = "error-codes")]
    error_codes: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn an_unset_or_blank_sitekey_leaves_the_challenge_off() {
        assert!(HCaptcha::from_lookup(lookup(&[])).unwrap().is_none());
        assert!(
            HCaptcha::from_lookup(lookup(&[(SITEKEY_ENV, "  "), (SECRET_ENV, "s")]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_sitekey_without_its_secret_refuses_to_boot() {
        assert_eq!(
            HCaptcha::from_lookup(lookup(&[(SITEKEY_ENV, "site")])).err(),
            Some(MissingSecret)
        );
    }

    #[test]
    fn both_keys_turn_the_challenge_on() {
        let keys = HCaptcha::from_lookup(lookup(&[(SITEKEY_ENV, "site"), (SECRET_ENV, "secret")]))
            .unwrap()
            .expect("configured");
        assert_eq!(keys.sitekey(), "site");
    }

    #[tokio::test]
    async fn a_solved_token_is_redeemed_against_this_sitekey() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("response=solved"))
            .and(body_string_contains("sitekey=site"))
            .and(body_string_contains("secret=secret"))
            .and(body_string_contains("remoteip=203.0.113.9"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"success": true})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let keys = HCaptcha::new("site", "secret", server.uri());
        assert!(
            keys.verify("solved", Some("203.0.113.9".parse().unwrap()))
                .await
        );
    }

    #[tokio::test]
    async fn a_refused_blank_or_unreachable_token_fails_closed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"success": false, "error-codes": ["invalid-input-response"]}),
            ))
            .mount(&server)
            .await;
        let keys = HCaptcha::new("site", "secret", server.uri());
        assert!(!keys.verify("forged", None).await);
        assert!(!keys.verify("  ", None).await);
        let unreachable = HCaptcha::new("site", "secret", "http://127.0.0.1:9/siteverify");
        assert!(!unreachable.verify("solved", None).await);
    }
}
