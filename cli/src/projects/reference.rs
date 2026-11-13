//! `navigator project reference add` and `navigator project adverse add` —
//! a matter's write paths into the Conflict-Check Graph.
//!
//! A reference names a counterparty Person or Entity the matter verified
//! without granting them any standing: it goes through `POST /app/api/seed`
//! as a one-record `project_reference` document, the same door a Project
//! repository's `seeds/ProjectReference.yaml` reconciles through. Adversity
//! is then asserted against a referenced party through
//! `POST /app/api/projects/{id}/relationships/adverse`; `adverse add` given
//! what a reference needs records the `adverse_party` reference first.

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
        let session = remote::resolve(host.as_deref())?;
        let body = send_reference(&session, record).await?;
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

/// Send one reference record through the seed door, which creates a new
/// Person or Entity from it and leaves an existing reference unchanged.
async fn send_reference(session: &(String, String), record: serde_json::Value) -> Result<String> {
    let yaml = serde_yaml::to_string(&serde_json::json!({
        "lookup_fields": ["project_id", "reason"],
        "records": [record],
    }))
    .context("encode project_reference seed")?;
    remote::send_seed_document(session, "project_reference", &yaml).await
}

/// One `project_reference` seed record, in the shape
/// `model::seed::SeedModel::ProjectReference` parses.
fn reference_record(code: &str, reason: &str, party: &PartyArgs) -> Result<serde_json::Value> {
    if !model::project_references::is_reason(reason) {
        return Err(anyhow!(
            "--reason must be one of {}",
            model::project_references::REASONS.join(", ")
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

/// `navigator project adverse add (--person-email <email> [--person-name
/// <name>] | --entity-name <name> [--entity-type <type> --jurisdiction
/// <name>]) [--subject entity|client_dri]`.
pub async fn add_adversity(
    host: Option<&str>,
    project: Option<&str>,
    party: PartyArgs,
    subject: &str,
) -> ExitCode {
    let result: Result<String> = async {
        let code = project_code(project)?;
        let host = host_from_manifest(host);
        let body = adversity_body(&party, subject)?;
        let reference = adverse_reference(&code, &party)?;
        let session = remote::resolve(host.as_deref())?;
        record_adversity(&session, &code, reference, &body).await
    }
    .await;
    match result {
        Ok(edge) => {
            println!("{edge}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project adverse add: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// The `adverse_party` reference to record first, when the verb names what
/// one needs — `--person-name`, or `--entity-type` and `--jurisdiction` —
/// since adversity is asserted only against a party the matter references.
fn adverse_reference(code: &str, party: &PartyArgs) -> Result<Option<serde_json::Value>> {
    if party.person_name.is_none() && party.entity_type.is_none() && party.jurisdiction.is_none() {
        return Ok(None);
    }
    reference_record(code, model::project_references::REASON_ADVERSE_PARTY, party).map(Some)
}

/// Reference the party when asked, creating it if new, then assert the
/// client's adversity to it.
async fn record_adversity(
    session: &(String, String),
    code: &str,
    reference: Option<serde_json::Value>,
    body: &serde_json::Value,
) -> Result<String> {
    if let Some(record) = reference {
        send_reference(session, record).await?;
    }
    let (base, token) = session;
    let id = surfaces::resolve_project_id(base, token, code).await?;
    let response = reqwest::Client::new()
        .post(format!(
            "{base}/app/api/projects/{id}/relationships/adverse"
        ))
        .timeout(Duration::from_secs(30))
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .context("record adversity")?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status != reqwest::StatusCode::CREATED {
        return Err(anyhow!("adversity refused: {status}: {text}"));
    }
    Ok(text)
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
    use super::{adverse_reference, adversity_body, record_adversity, reference_record, PartyArgs};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    #[tokio::test]
    async fn a_new_person_is_referenced_as_an_adverse_party_before_the_edge() {
        let server = MockServer::start().await;
        let project = uuid::Uuid::nil();
        Mock::given(method("POST"))
            .and(path("/app/api/seed"))
            .and(body_partial_json(
                serde_json::json!({ "model": "project_reference" }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("created 1"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/app/api/projects"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{ "id": project, "code": "acme" }])),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!(
                "/app/api/projects/{project}/relationships/adverse"
            )))
            .and(body_partial_json(
                serde_json::json!({ "person_email": "counsel@example.com" }),
            ))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(serde_json::json!({"kind": "adverse_to"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let party = PartyArgs {
            person_email: Some("counsel@example.com".into()),
            person_name: Some("Synthetic Counsel".into()),
            entity_name: None,
            entity_type: None,
            jurisdiction: None,
        };
        let reference = adverse_reference("acme", &party).unwrap();
        let body = adversity_body(&party, "entity").unwrap();
        let session = (server.uri(), "token".to_string());
        let edge = record_adversity(&session, "acme", reference, &body)
            .await
            .unwrap();
        assert!(edge.contains("adverse_to"));
        let requests = server.received_requests().await.unwrap();
        let seed = requests
            .iter()
            .find(|request| request.url.path() == "/app/api/seed")
            .unwrap();
        let seed: serde_json::Value = serde_json::from_slice(&seed.body).unwrap();
        let yaml = seed["yaml"].as_str().unwrap();
        assert!(yaml.contains("reason: adverse_party"), "{yaml}");
        assert!(yaml.contains("name: Synthetic Counsel"), "{yaml}");

        // A party named by email alone is asserted against as it stands.
        let bare = PartyArgs {
            person_name: None,
            ..party
        };
        assert!(adverse_reference("acme", &bare).unwrap().is_none());
    }
}
