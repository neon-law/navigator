//! `navigator project reference add` and `navigator project adverse add` —
//! a matter's write paths into the Conflict-Check Graph.
//!
//! A reference names a counterparty Person or Entity the matter verified
//! without granting them any standing: it goes through `POST /app/api/seed`
//! as a one-record `project_reference` document, the same door a Project
//! repository's `seeds/ProjectReference.yaml` reconciles through. Adversity
//! is then asserted against a referenced party through
//! `POST /app/api/projects/{id}/relationships/adverse`.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::{
    projects::{manifest, surfaces},
    remote,
};

/// The party a verb names: exactly one of a Person by email or an Entity by
/// name, as the seed documents spell them.
pub struct PartyArgs {
    pub person_email: Option<String>,
    pub person_name: Option<String>,
    pub entity_name: Option<String>,
    pub entity_type: Option<String>,
    pub jurisdiction: Option<String>,
}

fn project_code(project: Option<&str>) -> Result<String> {
    let local = manifest::read(Path::new("."));
    project
        .map(str::to_string)
        .or_else(|| local.as_ref().and_then(|m| m.project.clone()))
        .ok_or_else(|| anyhow!("pass --project or run from a Project checkout"))
}

fn host_from_manifest(host: Option<&str>) -> Option<String> {
    host.map(str::to_string)
        .or_else(|| manifest::read(Path::new(".")).and_then(|m| m.host))
}

/// `navigator project reference add --reason <reason> (--person-email <email>
/// --person-name <name> | --entity-name <name> --entity-type <type>
/// --jurisdiction <name>)`.
pub async fn add_reference(
    host: Option<&str>,
    project: Option<&str>,
    reason: &str,
    party: PartyArgs,
) -> ExitCode {
    let result: Result<()> = async {
        let code = project_code(project)?;
        let host = host_from_manifest(host);
        let record = reference_record(&code, reason, &party)?;
        let yaml = serde_yaml::to_string(&serde_json::json!({
            "lookup_fields": ["project_id", "reason"],
            "records": [record],
        }))
        .context("encode project_reference seed")?;
        let session = remote::resolve(host.as_deref())?;
        let body = remote::send_seed_document(&session, "project_reference", &yaml).await?;
        println!("{body}");
        Ok(())
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator project reference add: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// One `project_reference` seed record, in the shape
/// `store::seed::SeedModel::ProjectReference` parses.
fn reference_record(code: &str, reason: &str, party: &PartyArgs) -> Result<serde_json::Value> {
    if !store::project_references::is_reason(reason) {
        return Err(anyhow!(
            "--reason must be one of {}",
            store::project_references::REASONS.join(", ")
        ));
    }
    let mut record = serde_json::json!({
        "reason": reason,
        "project": { "code": code },
    });
    match (&party.person_email, &party.entity_name) {
        (Some(email), None) => {
            let name = party
                .person_name
                .as_deref()
                .ok_or_else(|| anyhow!("--person-email needs --person-name"))?;
            record["person"] = serde_json::json!({ "email": email, "name": name });
        }
        (None, Some(name)) => {
            let entity_type = party
                .entity_type
                .as_deref()
                .ok_or_else(|| anyhow!("--entity-name needs --entity-type"))?;
            let jurisdiction = party
                .jurisdiction
                .as_deref()
                .ok_or_else(|| anyhow!("--entity-name needs --jurisdiction"))?;
            record["entity"] = serde_json::json!({
                "name": name,
                "entity_type": { "name": entity_type, "jurisdiction": { "name": jurisdiction } },
            });
        }
        _ => {
            return Err(anyhow!(
                "name exactly one party: --person-email or --entity-name"
            ))
        }
    }
    Ok(record)
}

/// `navigator project adverse add (--person-email <email> | --entity-name
/// <name>) [--subject entity|client_dri]`.
pub async fn add_adversity(
    host: Option<&str>,
    project: Option<&str>,
    party: PartyArgs,
    subject: &str,
) -> ExitCode {
    let result: Result<()> = async {
        let code = project_code(project)?;
        let host = host_from_manifest(host);
        let body = adversity_body(&party, subject)?;
        let (base, token) = remote::resolve(host.as_deref())?;
        let id = surfaces::resolve_project_id(&base, &token, &code).await?;
        let response = reqwest::Client::new()
            .post(format!(
                "{base}/app/api/projects/{id}/relationships/adverse"
            ))
            .timeout(Duration::from_secs(30))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .context("record adversity")?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status != reqwest::StatusCode::CREATED {
            return Err(anyhow!("adversity refused: {status}: {text}"));
        }
        println!("{text}");
        Ok(())
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("navigator project adverse add: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn adversity_body(party: &PartyArgs, subject: &str) -> Result<serde_json::Value> {
    if !matches!(subject, "entity" | "client_dri") {
        return Err(anyhow!("--subject must be `entity` or `client_dri`"));
    }
    let mut body = serde_json::json!({ "subject": subject });
    match (&party.person_email, &party.entity_name) {
        (Some(email), None) => body["person_email"] = serde_json::json!(email),
        (None, Some(name)) => body["entity_name"] = serde_json::json!(name),
        _ => {
            return Err(anyhow!(
                "name exactly one party: --person-email or --entity-name"
            ))
        }
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::{adversity_body, reference_record, PartyArgs};

    fn entity() -> PartyArgs {
        PartyArgs {
            person_email: None,
            person_name: None,
            entity_name: Some("Widget Works LLC".into()),
            entity_type: Some("LLC".into()),
            jurisdiction: Some("Nevada".into()),
        }
    }

    #[test]
    fn a_reference_record_carries_the_project_and_exactly_one_party() {
        let record = reference_record("acme", "counterparty_entity", &entity()).unwrap();
        assert_eq!(record["project"]["code"], "acme");
        assert_eq!(
            record["entity"]["entity_type"]["jurisdiction"]["name"],
            "Nevada"
        );
        assert!(record.get("person").is_none());

        let both = PartyArgs {
            person_email: Some("a@example.com".into()),
            person_name: Some("A".into()),
            ..entity()
        };
        assert!(reference_record("acme", "signatory", &both).is_err());
        assert!(reference_record("acme", "co_counsel", &entity()).is_err());
    }

    #[test]
    fn an_adversity_body_names_the_party_by_natural_key() {
        let body = adversity_body(&entity(), "entity").unwrap();
        assert_eq!(body["entity_name"], "Widget Works LLC");
        assert_eq!(body["subject"], "entity");
        assert!(adversity_body(&entity(), "client").is_err());
    }
}
