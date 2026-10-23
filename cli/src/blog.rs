//! Authenticated Brand Post publication and provider links.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::Method;
use serde_json::{json, Value};

async fn send(
    base: &str,
    token: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut request = client
        .request(method, format!("{base}{path}"))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.context("contact Navigator blog API")?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .context("read Navigator blog response")?;
    if !status.is_success() {
        let message = serde_json::from_slice::<Value>(&bytes).map_or_else(
            |_| {
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .take(300)
                    .collect::<String>()
            },
            |body| body.to_string(),
        );
        bail!("HTTP {status}: {message}");
    }
    serde_json::from_slice(&bytes).context("decode Navigator blog response")
}

async fn request(
    host: Option<&str>,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let (base, token) = crate::remote::resolve(host)?;
    send(&base, &token, method, path, body).await
}

fn report(result: Result<Value>) -> ExitCode {
    match result {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator: blog: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn segment(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

#[derive(serde::Deserialize)]
struct PreflightAccount {
    brand_key: String,
    provider: store::social_accounts::SocialProvider,
    provider_account_id: String,
    display_name: Option<String>,
    scopes: Vec<String>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    status: String,
}

fn preflight(
    base: &str,
    brand: &str,
    providers: &[String],
    status: &Value,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(Value, bool)> {
    if providers.is_empty() {
        bail!("choose at least one provider");
    }
    let accounts: Vec<PreflightAccount> = serde_json::from_value(status["accounts"].clone())
        .context("decode Social Accounts from status")?;
    let mut blockers = Vec::new();
    let mut destinations = Vec::new();
    let mut account_ids = std::collections::BTreeMap::new();
    if status["publisher_configured"] != true {
        blockers.push("Configure the durable social publisher; deploy the preflight-capable release if configuration is absent from status.".to_string());
    }
    if let Some(post) = status.get("post").filter(|value| !value.is_null()) {
        let due = post["publish_at"]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .is_some_and(|at| at <= now);
        if post["status"] == "published" || (post["status"] == "scheduled" && due) {
            blockers.push("This Post is already public or due; reconcile its existing receipts before sending again.".into());
        }
    }
    for name in providers {
        let provider: store::social_accounts::SocialProvider = serde_json::from_value(json!(name))
            .with_context(|| format!("unknown social provider `{name}`"))?;
        if destinations
            .iter()
            .any(|destination: &Value| destination["provider"] == *name)
        {
            bail!("each provider may appear only once");
        }
        if status["connection_setup"][name] != true {
            blockers.push(format!("{name}: configure provider OAuth and its callback on this deployment; inspect the account onboarding guide."));
        }
        let connected = accounts
            .iter()
            .filter(|account| {
                account.brand_key == brand
                    && account.provider == provider
                    && account.status == "connected"
            })
            .collect::<Vec<_>>();
        let mut destination = json!({"provider":name});
        if let [account] = connected.as_slice() {
            destination["account_id"] = json!(account.provider_account_id);
            destination["display_name"] = json!(account.display_name);
            let missing = provider.missing_scopes(&account.scopes);
            if !missing.is_empty() {
                blockers.push(format!(
                    "{name}: reconnect with missing permissions: {}",
                    missing.join(", ")
                ));
            }
            if provider.token_renewal(account.expires_at, now)
                == store::social_accounts::TokenRenewal::Reconnect
            {
                blockers.push(format!(
                    "{name}: authorization expired; reconnect this Brand's account."
                ));
            }
            account_ids.insert(name, &account.provider_account_id);
        } else {
            blockers.push(format!(
                "{name}: connect exactly one account belonging to this Brand in Navigator."
            ));
        }
        destinations.push(destination);
    }
    let ready = blockers.is_empty();
    Ok((
        json!({
            "brand":brand,"ready_to_prepare":ready,"blockers":blockers,
            "setup_url":format!("{base}/app/admin/brands/{}/edit", segment(brand)),
            "destinations":destinations,"account_ids":account_ids,
            "verification":"Stored connections and configuration only. Verify destination identities before approval; media, consent revocation, provider approval and delivery are checked when publishing."
        }),
        ready,
    ))
}

pub async fn run_preflight(
    host: Option<&str>,
    brand: &str,
    slug: &str,
    providers: &[String],
) -> ExitCode {
    let result = async {
        let (base, token) = crate::remote::resolve(host)?;
        let status = send(
            &base,
            &token,
            Method::GET,
            &format!("/app/api/blog/{}/{}", segment(brand), segment(slug)),
            None,
        )
        .await?;
        preflight(&base, brand, providers, &status, chrono::Utc::now())
    }
    .await;
    match result {
        Ok((value, ready)) => {
            println!("{value}");
            if ready {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => report(Err(error)),
    }
}

async fn run_source(host: Option<&str>, file: &Path, path: &str) -> ExitCode {
    report(
        async {
            let source = std::fs::read_to_string(file)
                .with_context(|| format!("read {}", file.display()))?;
            let body: Value = serde_yaml::from_str(&source).context("parse Post YAML")?;
            request(host, Method::POST, path, Some(body)).await
        }
        .await,
    )
}

pub async fn run_record(host: Option<&str>, file: &Path) -> ExitCode {
    run_source(host, file, "/app/api/blog/record").await
}

pub async fn run_publish(host: Option<&str>, file: &Path) -> ExitCode {
    run_source(host, file, "/app/api/blog").await
}

pub async fn run_status(host: Option<&str>, brand: &str, slug: &str) -> ExitCode {
    report(
        request(
            host,
            Method::GET,
            &format!("/app/api/blog/{}/{}", segment(brand), segment(slug)),
            None,
        )
        .await,
    )
}

pub async fn run_handle(host: Option<&str>, brand: &str, provider: &str, handle: &str) -> ExitCode {
    report(
        request(
            host,
            Method::PUT,
            &format!(
                "/app/api/blog/{}/handles/{}",
                segment(brand),
                segment(provider)
            ),
            Some(json!({"handle":handle})),
        )
        .await,
    )
}

fn link_body(provider: &str, permalink: &str) -> Result<Value> {
    let mut url = url::Url::parse(permalink).context("parse provider URL")?;
    let markers: &[&str] = match provider {
        "facebook" => &["/reel/", "/videos/", "/posts/"],
        "instagram" => &["/reel/", "/p/"],
        "threads" => &["/post/"],
        "tiktok" => &["/video/"],
        "youtube" => &["/shorts/", "/watch"],
        "x" => &["/status/"],
        "linkedin" => &["/posts/", "/feed/update/"],
        _ => bail!("unknown social provider `{provider}`"),
    };
    if !markers.iter().any(|marker| url.path().contains(marker)) {
        bail!("provide a public post permalink, rather than a profile or insights URL");
    }
    let video_id = (provider == "youtube")
        .then(|| {
            url.query_pairs()
                .find(|(key, _)| key == "v")
                .map(|(_, value)| value.into_owned())
        })
        .flatten();
    url.set_query(None);
    url.set_fragment(None);
    if let Some(id) = &video_id {
        url.query_pairs_mut().append_pair("v", id);
    }
    let post_id = video_id
        .or_else(|| {
            url.path_segments()
                .into_iter()
                .flatten()
                .rfind(|part| !part.is_empty())
                .map(str::to_string)
        })
        .context("provider URL has no post identifier")?;
    store::posts::validate_social_link(&store::posts::SocialLink {
        provider: provider.into(),
        account: "validation".into(),
        post_id: Some(post_id.clone()),
        permalink: Some(url.to_string()),
    })
    .map_err(anyhow::Error::msg)?;
    Ok(json!({"post_id":post_id,"permalink":url.as_str()}))
}

pub async fn run_link(
    host: Option<&str>,
    brand: &str,
    slug: &str,
    provider: &str,
    permalink: &str,
) -> ExitCode {
    report(
        async {
            request(
                host,
                Method::PUT,
                &format!(
                    "/app/api/blog/{}/{}/links/{}",
                    segment(brand),
                    segment(slug),
                    segment(provider)
                ),
                Some(link_body(provider, permalink)?),
            )
            .await
        }
        .await,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use wiremock::{
        matchers::{bearer_token, body_json, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn preflight_status() -> Value {
        json!({"accounts":[],"post":null,"publications":[],
            "publisher_configured":true,"connection_setup":{"facebook":true,"instagram":true,"tiktok":true}})
    }

    fn account(provider: &str) -> Value {
        let provider_kind: store::social_accounts::SocialProvider =
            serde_json::from_value(json!(provider)).unwrap();
        json!({"brand_key":"vesta","provider":provider,"provider_account_id":"synthetic-account",
            "display_name":"Sample Brand","status":"connected","scopes":provider_kind.required_scopes(),
            "expires_at":null})
    }

    #[test]
    fn preflight_missing_connections_reports_setup_without_sending() {
        let (value, ready) = preflight(
            "https://publisher.example",
            "vesta",
            &["facebook".into(), "instagram".into(), "tiktok".into()],
            &preflight_status(),
            chrono::Utc::now(),
        )
        .unwrap();
        assert!(!ready);
        assert_eq!(value["blockers"].as_array().unwrap().len(), 3);
        assert_eq!(
            value["setup_url"],
            "https://publisher.example/app/admin/brands/vesta/edit"
        );
        assert_eq!(value["account_ids"], json!({}));
    }

    #[test]
    fn preflight_checks_only_requested_brand_and_destinations() {
        let mut status = preflight_status();
        status["accounts"] = json!([account("facebook")]);
        let (value, ready) = preflight(
            "https://publisher.example",
            "vesta",
            &["facebook".into()],
            &status,
            chrono::Utc::now(),
        )
        .unwrap();
        assert!(ready);
        assert_eq!(
            value["account_ids"],
            json!({"facebook":"synthetic-account"})
        );
        status["accounts"][0]["brand_key"] = json!("neon");
        assert!(
            !preflight(
                "https://publisher.example",
                "vesta",
                &["facebook".into()],
                &status,
                chrono::Utc::now()
            )
            .unwrap()
            .1
        );
    }

    #[test]
    fn preflight_blocks_missing_permissions_expiry_duplicates_and_old_server() {
        let mut status = preflight_status();
        status["accounts"] = json!([account("facebook")]);
        status["accounts"][0]["scopes"] = json!([]);
        status["accounts"][0]["expires_at"] = json!("2020-01-01T00:00:00Z");
        let (value, ready) = preflight(
            "https://publisher.example",
            "vesta",
            &["facebook".into()],
            &status,
            chrono::Utc::now(),
        )
        .unwrap();
        assert!(!ready);
        assert_eq!(value["blockers"].as_array().unwrap().len(), 2);
        assert!(preflight(
            "https://publisher.example",
            "vesta",
            &["facebook".into(), "facebook".into()],
            &status,
            chrono::Utc::now()
        )
        .is_err());
        assert!(preflight(
            "https://publisher.example",
            "vesta",
            &["unknown".into()],
            &status,
            chrono::Utc::now()
        )
        .is_err());
        let (_, ready) = preflight(
            "https://publisher.example",
            "vesta",
            &["facebook".into()],
            &json!({"accounts":[account("facebook")]}),
            chrono::Utc::now(),
        )
        .unwrap();
        assert!(!ready);
    }

    #[test]
    fn preflight_refuses_due_posts_even_with_connected_destinations() {
        let mut status = preflight_status();
        status["accounts"] = json!([account("facebook")]);
        status["post"] = json!({"status":"scheduled","publish_at":"2020-01-01T00:00:00Z"});
        assert!(
            !preflight(
                "https://publisher.example",
                "vesta",
                &["facebook".into()],
                &status,
                chrono::Utc::now()
            )
            .unwrap()
            .1
        );
    }

    #[test]
    fn preflight_revoked_accounts_block_but_refreshable_tokens_can_proceed() {
        let mut status = preflight_status();
        status["accounts"] = json!([account("tiktok")]);
        status["accounts"][0]["expires_at"] = json!("2020-01-01T00:00:00Z");
        assert!(
            preflight(
                "https://publisher.example",
                "vesta",
                &["tiktok".into()],
                &status,
                chrono::Utc::now()
            )
            .unwrap()
            .1
        );
        status["accounts"][0]["status"] = json!("revoked");
        assert!(
            !preflight(
                "https://publisher.example",
                "vesta",
                &["tiktok".into()],
                &status,
                chrono::Utc::now()
            )
            .unwrap()
            .1
        );
    }

    #[test]
    fn preflight_parses_explicit_provider_selection() {
        let cli = crate::Cli::try_parse_from([
            "navigator",
            "site",
            "blog",
            "preflight",
            "vesta",
            "example",
            "--providers",
            "facebook,instagram,tiktok",
            "--host",
            "https://publisher.example",
        ])
        .unwrap();
        assert!(
            matches!(cli.command, Some(crate::Command::Site { action: crate::SiteCmd::Blog {
            action: crate::BlogCmd::Preflight { providers, .. }, .. } }) if providers.len() == 3)
        );
    }

    #[test]
    fn publication_needs_only_a_source_file_and_site_selection() {
        let cli = crate::Cli::try_parse_from([
            "navigator",
            "site",
            "blog",
            "publish",
            "post.yaml",
            "--host",
            "https://publisher.example",
        ])
        .unwrap();
        assert!(
            matches!(cli.command, Some(crate::Command::Site { action: crate::SiteCmd::Blog { host: Some(host), action: crate::BlogCmd::Publish { .. } } }) if host == "https://publisher.example")
        );
    }

    #[test]
    fn provider_links_drop_tracking_and_reject_wrong_hosts() {
        assert_eq!(
            link_body(
                "instagram",
                "https://www.instagram.com/reel/example/?utm_source=share"
            )
            .unwrap(),
            json!({"post_id":"example","permalink":"https://www.instagram.com/reel/example/"})
        );
        assert!(link_body("tiktok", "https://example.com/video/123").is_err());
        assert!(link_body("threads", "http://www.threads.com/@example/post/123").is_err());
        assert!(link_body("instagram", "https://www.instagram.com/example/").is_err());
        assert!(link_body("facebook", "https://business.facebook.com/latest/insights/").is_err());
    }

    #[tokio::test]
    async fn publication_uses_site_bearer_and_reports_provider_receipts() {
        let server = MockServer::start().await;
        let payload = json!({"brand":"vesta","slug":"example","providers":["threads"]});
        Mock::given(method("POST"))
            .and(path("/app/api/blog"))
            .and(bearer_token("test-session"))
            .and(body_json(&payload))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({"outcome":"queued"})))
            .expect(1)
            .mount(&server)
            .await;
        let result = send(
            &server.uri(),
            "test-session",
            Method::POST,
            "/app/api/blog",
            Some(payload),
        )
        .await
        .unwrap();
        assert_eq!(result["outcome"], "queued");
    }

    #[tokio::test]
    async fn missing_account_is_an_error_not_publication_success() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(422)
                    .set_body_json(json!({"error":"instagram requires a connected account"})),
            )
            .mount(&server)
            .await;
        let error = send(
            &server.uri(),
            "test-session",
            Method::POST,
            "/app/api/blog",
            Some(json!({})),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("connected account"));
    }
}
