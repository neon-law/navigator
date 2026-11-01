//! `navigator site firm provision-forgejo <slug>`: ask a deployment to create
//! or reconcile a Firm's Forgejo organization.
//!
//! The deployment does the work with its own provisioning identity
//! (`POST /app/api/firms/{id}/forgejo-organization`); this verb only resolves
//! the Firm from the caller's authorized directory and reports the outcome.
//! Its output is built from the server's answer, never from the argument.

use std::process::ExitCode;

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
struct VisibleFirm {
    id: uuid::Uuid,
    slug: Option<String>,
}

#[derive(Deserialize)]
struct Provisioned {
    organization: String,
    owner_login: String,
    created: bool,
    made_private: bool,
    owner_added: bool,
}

fn refusal(status: reqwest::StatusCode, body: &str) -> String {
    let value = serde_json::from_str::<serde_json::Value>(body).ok();
    let message = value
        .as_ref()
        .and_then(|value| {
            value["message"]
                .as_str()
                .or_else(|| value["error"].as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.lines().next().unwrap_or_default().trim().to_string());
    format!("{status}: {message}")
}

/// Resolve the Firm whose slug is `slug` and provision its organization.
/// Returns the one-line report.
async fn provision_at(base: &str, token: &str, slug: &str) -> Result<String> {
    let http = reqwest::Client::new();
    let response = http
        .get(format!("{base}/app/api/firms"))
        .bearer_auth(token)
        .send()
        .await
        .context("GET /app/api/firms")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("list Firms failed: {}", refusal(status, &body));
    }
    let firms: Vec<VisibleFirm> =
        serde_json::from_str(&body).context("parse GET /app/api/firms")?;
    let needle = slug.trim();
    let firm = firms
        .into_iter()
        .find(|firm| firm.slug.as_deref() == Some(needle))
        .ok_or_else(|| anyhow!("no Firm with that slug is visible to this caller"))?;

    let response = http
        .post(format!(
            "{base}/app/api/firms/{}/forgejo-organization",
            firm.id
        ))
        .bearer_auth(token)
        .send()
        .await
        .context("POST /app/api/firms/{id}/forgejo-organization")?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!(
            "Forgejo organization provisioning refused: {}",
            refusal(status, &body)
        );
    }
    let report: Provisioned =
        serde_json::from_str(&body).context("parse the provisioning report")?;
    let organization = if report.created {
        "created"
    } else if report.made_private {
        "made private"
    } else {
        "already present"
    };
    let owner = if report.owner_added {
        "added to its owners team"
    } else {
        "already in its owners team"
    };
    Ok(format!(
        "organization {}: {organization}; {} {owner}",
        report.organization, report.owner_login
    ))
}

/// `navigator site firm provision-forgejo`.
pub async fn provision(host: Option<&str>, slug: &str) -> ExitCode {
    let result = async {
        let (base, token) = crate::remote::resolve(host)?;
        provision_at(&base, &token, slug).await
    }
    .await;
    match result {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator: {error:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::provision_at;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FIRM: &str = "0192f0a0-0000-7000-8000-000000000001";

    async fn directory(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/app/api/firms"))
            .and(header("authorization", "Bearer session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                { "id": FIRM, "name": "Example Law PC", "slug": "example-law-pc" }
            ])))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn the_firm_is_resolved_by_slug_and_the_server_report_is_printed() {
        let server = MockServer::start().await;
        directory(&server).await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/firms/{FIRM}/forgejo-organization")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "firm_id": FIRM, "organization": "example-law-pc", "forgejo_id": 7,
                "owner_login": "firm-account", "created": true, "made_private": false,
                "owner_added": true, "provisioned_at": "2026-11-01T00:00:00Z",
                "reconciled_at": "2026-11-01T00:00:00Z"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let report = provision_at(&server.uri(), "session", "example-law-pc")
            .await
            .unwrap();
        assert_eq!(
            report,
            "organization example-law-pc: created; firm-account added to its owners team"
        );
    }

    #[tokio::test]
    async fn an_unknown_slug_is_refused_without_a_write() {
        let server = MockServer::start().await;
        directory(&server).await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let error = provision_at(&server.uri(), "session", "another-firm")
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("no Firm with that slug"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn the_servers_refusal_is_reported() {
        let server = MockServer::start().await;
        directory(&server).await;
        Mock::given(method("POST"))
            .and(path(format!("/app/api/firms/{FIRM}/forgejo-organization")))
            .respond_with(ResponseTemplate::new(503).set_body_json(json!({
                "error": "forgejo_provisioning_unconfigured",
                "message": "Forgejo organization provisioning needs NAVIGATOR_FORGEJO_PROVISIONER_TOKEN"
            })))
            .mount(&server)
            .await;
        let error = provision_at(&server.uri(), "session", "example-law-pc")
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("NAVIGATOR_FORGEJO_PROVISIONER_TOKEN"),
            "{error}"
        );
    }
}
