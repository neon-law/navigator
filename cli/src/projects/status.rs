//! Read a Project's live lifecycle through its authenticated, matter-scoped API.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::{
    projects::{manifest, surfaces},
    remote,
};

mod readiness;

pub async fn run(host: Option<&str>, project: Option<&str>, json: bool, all: bool) -> ExitCode {
    if all {
        return readiness::run(host, json).await;
    }
    match read(host, project).await {
        Ok(mut value) => {
            if matches!(value["project_status"].as_str(), Some("pitch" | "open")) {
                value["workspace"] = super::workspace::inspect(&value).await;
            }
            if json {
                println!("{value}");
            } else {
                print_status(&value);
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project status: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Refresh the matter's already-linked envelopes through the Firm-scoped provider.
pub async fn refresh_envelopes(
    host: Option<&str>,
    project: &str,
    envelope_id: Option<uuid::Uuid>,
) -> ExitCode {
    let result: Result<()> = async {
        let (base, token) = remote::resolve(host)?;
        let id = surfaces::resolve_project_id(&base, &token, project).await?;
        let value = read_envelopes(&base, &token, id, envelope_id).await?;
        println!("{}", serde_json::to_string_pretty(&value)?);
        Ok(())
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator project envelope status: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn read_envelopes(
    base: &str,
    token: &str,
    id: uuid::Uuid,
    envelope_id: Option<uuid::Uuid>,
) -> Result<serde_json::Value> {
    let client = remote::authenticated_client_builder(base)?
        .timeout(Duration::from_secs(120))
        .build()?;
    let url = format!("{base}/app/api/projects/{id}/envelopes");
    let request = match envelope_id {
        Some(envelope) => client.get(format!("{url}/{envelope}")),
        None => client
            .post(format!("{url}/refresh"))
            .json(&serde_json::json!({})),
    };
    let response = request.bearer_auth(token).send().await?;
    anyhow::ensure!(
        response.status().is_success(),
        "envelope status failed: {}",
        response.status()
    );
    let value: serde_json::Value = response.json().await?;
    Ok(value)
}

pub(super) async fn read(host: Option<&str>, project: Option<&str>) -> Result<serde_json::Value> {
    let local = manifest::read(Path::new("."));
    let project = project
        .or_else(|| local.as_ref().and_then(|m| m.project.as_deref()))
        .ok_or_else(|| anyhow!("pass a Project code or run from a Project checkout"))?;
    let host = host.or_else(|| local.as_ref().and_then(|m| m.host.as_deref()));
    let (base, token) = remote::resolve(host)?;
    let visible = surfaces::resolve_visible_project(&base, &token, project).await?;
    let id = visible.id;
    let response = reqwest::Client::new()
        .get(format!("{base}/app/api/projects/{id}/status"))
        .timeout(Duration::from_secs(30))
        .bearer_auth(&token)
        .send()
        .await
        .context("read Project status")?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("Project status read failed: {status}"));
    }
    let mut value: serde_json::Value = response.json().await.context("parse Project status")?;
    // The live ownership the manifest's Firm and brand are checked against.
    if let Some(firm) = visible.firm_id {
        value["firm_id"] = serde_json::json!(firm);
    }
    if let Some(brand) = &visible.brand {
        value["brand"] = serde_json::json!(brand);
    }
    if let Some(firm) = visible.firm_id {
        if let Some(name) = firm_name(&base, &token, firm).await {
            value["firm_name"] = serde_json::json!(name);
        }
    }
    Ok(value)
}

/// The Firm's name from the Firm directory, when this caller may list it.
async fn firm_name(base: &str, token: &str, firm: uuid::Uuid) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Firm {
        id: uuid::Uuid,
        name: String,
    }
    let response = reqwest::Client::new()
        .get(format!("{base}/app/api/firms"))
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let firms: Vec<Firm> = response.json().await.ok()?;
    firms.into_iter().find(|f| f.id == firm).map(|f| f.name)
}

/// The live Firm and brand as one status line.
fn ownership_line(value: &serde_json::Value) -> String {
    let brand = value["brand"].as_str().unwrap_or("none");
    let firm = match (value["firm_id"].as_str(), value["firm_name"].as_str()) {
        (Some(id), Some(name)) => format!("{name} ({id})"),
        (Some(id), None) => format!("{id} (name not visible to this role)"),
        _ => "none".to_owned(),
    };
    format!("Firm: {firm}; brand: {brand}")
}

/// The owning Firm's Google Calendar and Google Drive switches as one status line.
fn integrations_line(value: &serde_json::Value) -> String {
    let switch = |key: &str| match value["firm_integrations"][key].as_bool() {
        Some(true) => "on",
        Some(false) => "off",
        None => "unknown",
    };
    format!(
        "Firm Integrations: Google Calendar {}; Google Drive {}",
        switch("calendar"),
        switch("drive")
    )
}

fn print_onboarding(onboarding: &serde_json::Value) {
    println!("onboarding filed: {}", onboarding["filed"]);
    println!(
        "onboarding status: {}",
        onboarding["status"].as_str().unwrap_or("unknown")
    );
    for (label, key) in [("signed PDF", "signed_pdf"), ("certificate", "certificate")] {
        if let Some(name) = onboarding[key].as_str() {
            println!("onboarding {label}: {name}");
        }
    }
    if let Some(documents) = onboarding["documents"].as_array() {
        for doc in documents {
            println!(
                "  {} (visibility: {})",
                doc["id"],
                doc["visibility"].as_str().unwrap_or("unknown")
            );
        }
    }
}

fn print_status(value: &serde_json::Value) {
    for field in [
        "code",
        "status",
        "project_status",
        "opened_at",
        "closed_at",
        "source_state",
    ] {
        println!("{field}: {}", value[field].as_str().unwrap_or("none"));
    }
    println!("{}\n{}", ownership_line(value), integrations_line(value));
    match value["drive_folder_id"].as_str() {
        Some(id) => println!("drive folder: https://drive.google.com/drive/folders/{id}"),
        None => println!("drive folder: none linked"),
    }
    if let Some(history) = value["history"].as_array() {
        println!("status history:");
        for entry in history {
            let field = |key: &str| entry[key].as_str().unwrap_or("none");
            println!(
                "  {} -> {} (effective: {}, recorded: {}, reason: {}, attester: {})",
                field("from_status"),
                field("to_status"),
                field("effective_at"),
                field("recorded_at"),
                field("reason"),
                field("attesting_person_id")
            );
        }
    }
    print_onboarding(&value["onboarding"]);
    if let Some(adoptions) = value["envelopes"]
        .as_array()
        .or_else(|| value["adopted_envelopes"].as_array())
    {
        for adoption in adoptions {
            let envelope = &adoption["envelope"];
            let notation = adoption["notation_id"]
                .as_str()
                .map_or_else(String::new, |id| format!(" (Notation {id})"));
            println!(
                "DocuSign envelope {}: {}{notation}",
                envelope["envelopeId"].as_str().unwrap_or("unknown"),
                envelope["status"].as_str().unwrap_or("unknown"),
            );
            if let Some(signers) = envelope["signers"].as_array() {
                for signer in signers {
                    let signed = signer["signedDateTime"]
                        .as_str()
                        .map_or_else(String::new, |at| format!(" ({at})"));
                    println!(
                        "  {}: {}{signed}",
                        signer["name"].as_str().unwrap_or("unknown"),
                        signer["status"].as_str().unwrap_or("unknown")
                    );
                }
            }
            if let Some(sent) = envelope["sentDateTime"].as_str() {
                println!("  sent: {sent}");
            }
            for document in envelope["envelopeDocuments"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if let Some(name) = document["name"].as_str() {
                    println!("  document: {name}");
                }
            }
        }
    }
    if let Some(workspace) = value["workspace"].as_object() {
        for (step, outcome) in workspace {
            println!("workspace {step}: {}", outcome.as_str().unwrap_or_default());
        }
    }
    if let Some(detail) =
        super::mirror_repair::issue_detail(&value["repository_remotes"]["forgejo_issue"])
    {
        println!("Forgejo mirror: {detail}");
    }
    if let Some(missing) = value["missing"].as_array() {
        for item in missing {
            println!("missing: {}", item.as_str().unwrap_or_default());
        }
    }
}

#[cfg(test)]
mod envelope_tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn lookup_reads_one_envelope_without_registering_or_refreshing() {
        let server = MockServer::start().await;
        let project = uuid::Uuid::new_v4();
        let envelope = uuid::Uuid::new_v4();
        Mock::given(method("GET"))
            .and(path(format!(
                "/app/api/projects/{project}/envelopes/{envelope}"
            )))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "sent"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let result = read_envelopes(&server.uri(), "test-token", project, Some(envelope))
            .await
            .unwrap();
        assert_eq!(result["status"], "sent");
    }

    #[tokio::test]
    async fn matter_envelope_status_refreshes_the_scoped_record_and_reports_refusals() {
        for status in [200, 403] {
            let server = MockServer::start().await;
            let id = uuid::Uuid::nil();
            Mock::given(method("POST"))
                .and(path(format!("/app/api/projects/{id}/envelopes/refresh")))
                .and(header("authorization", "Bearer test-token"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .set_body_json(serde_json::json!({"envelopes": []})),
                )
                .expect(1)
                .mount(&server)
                .await;
            let result = read_envelopes(&server.uri(), "test-token", id, None).await;
            assert_eq!(result.is_ok(), status == 200);
        }
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::projects::test_login::{answer, site};
    use serde_json::json;

    const FIRM: &str = "11111111-1111-4111-8111-111111111111";

    /// ENG-1417: the live Firm and brand are readable, so a gate refusal that
    /// names both sides has a verb to confirm the live one.
    #[tokio::test]
    async fn the_read_adds_the_live_firm_id_firm_name_and_brand() {
        let id = uuid::Uuid::new_v4();
        let (server, _login) =
            site(json!([{"id": id, "code": "acme", "firm_id": FIRM, "brand": "vesta"}])).await;
        let host = server.uri();
        answer(
            &server,
            "GET",
            &format!("/app/api/projects/{id}/status"),
            200,
            json!({"code": "acme"}),
        )
        .await;
        answer(
            &server,
            "GET",
            "/app/api/firms",
            200,
            json!([
                {"id": "22222222-2222-4222-8222-222222222222", "name": "Other Firm", "slug": "other", "brands": []},
                {"id": FIRM, "name": "Sample Firm", "slug": "sample", "brands": ["vesta"]},
            ]),
        )
        .await;
        let value = read(Some(&host), Some("acme")).await.unwrap();
        assert_eq!(value["firm_id"], FIRM);
        assert_eq!(value["firm_name"], "Sample Firm");
        assert_eq!(value["brand"], "vesta");
        assert_eq!(
            ownership_line(&value),
            format!("Firm: Sample Firm ({FIRM}); brand: vesta")
        );
    }

    /// A role that cannot list Firms still sees the id and brand.
    #[tokio::test]
    async fn a_caller_who_cannot_list_firms_still_sees_the_id_and_brand() {
        let id = uuid::Uuid::new_v4();
        let (server, _login) =
            site(json!([{"id": id, "code": "acme", "firm_id": FIRM, "brand": "vesta"}])).await;
        let host = server.uri();
        answer(
            &server,
            "GET",
            &format!("/app/api/projects/{id}/status"),
            200,
            json!({"code": "acme"}),
        )
        .await;
        answer(
            &server,
            "GET",
            "/app/api/firms",
            403,
            json!({"message": "not admin"}),
        )
        .await;
        let value = read(Some(&host), Some("acme")).await.unwrap();
        assert!(value["firm_name"].is_null());
        assert_eq!(
            ownership_line(&value),
            format!("Firm: {FIRM} (name not visible to this role); brand: vesta")
        );
    }

    #[test]
    fn firm_integrations_print_on_or_off_and_unknown_from_an_older_server() {
        let on = json!({"firm_integrations": {"calendar": true, "drive": false}});
        assert_eq!(
            integrations_line(&on),
            "Firm Integrations: Google Calendar on; Google Drive off"
        );
        assert_eq!(
            integrations_line(&json!({})),
            "Firm Integrations: Google Calendar unknown; Google Drive unknown"
        );
    }

    #[test]
    fn a_project_with_no_owner_says_so() {
        assert_eq!(ownership_line(&json!({})), "Firm: none; brand: none");
    }
}
