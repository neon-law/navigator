//! `navigator project envelope read|void` (LAW-501, LAW-430): reach any
//! envelope in the caller's Firm `DocuSign` account by its ID or a `DocuSign`
//! notification link, whether or not a Project records it.

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::remote;

/// Print the envelope's state and signers; read only.
pub async fn read(host: Option<&str>, envelope: &str) -> ExitCode {
    let result = async {
        let id = envelope_id_from(envelope)?;
        let (base, token) = remote::resolve(host)?;
        request(&base, &token, reqwest::Method::GET, &id.to_string(), None).await
    }
    .await;
    finish("read", result, |value| {
        let found = &value["navigator"];
        if let (false, Some(code)) = (found["recorded"] == true, found["project"].as_str()) {
            eprintln!(
                "Not recorded yet. To record it on {code}: navigator project envelope register {} --project {code} --draft <draft>",
                value["envelopeId"].as_str().unwrap_or(envelope)
            );
        }
    })
}

/// Void a sent envelope, or delete a held draft.
pub async fn void(host: Option<&str>, envelope: &str, reason: &str) -> ExitCode {
    let result = async {
        let id = envelope_id_from(envelope)?;
        let (base, token) = remote::resolve(host)?;
        let body = serde_json::json!({ "reason": reason });
        request(
            &base,
            &token,
            reqwest::Method::POST,
            &format!("{id}/void"),
            Some(body),
        )
        .await
    }
    .await;
    finish("void", result, |_| {})
}

fn finish(
    verb: &str,
    result: Result<serde_json::Value>,
    after: impl FnOnce(&serde_json::Value),
) -> ExitCode {
    match result {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
            after(&value);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("navigator project envelope {verb}: {error:#}");
            ExitCode::FAILURE
        }
    }
}

/// Accept a bare envelope ID or a `DocuSign` link carrying it as `m=` (the
/// "View Completed Document" link) or `envelopeId=`.
fn envelope_id_from(input: &str) -> Result<uuid::Uuid> {
    let input = input.trim();
    if let Ok(id) = uuid::Uuid::parse_str(input) {
        return Ok(id);
    }
    let url = reqwest::Url::parse(input)
        .map_err(|_| anyhow!("{input:?} is neither an envelope ID nor a DocuSign link"))?;
    url.query_pairs()
        .find(|(key, _)| key == "m" || key.eq_ignore_ascii_case("envelopeId"))
        .and_then(|(_, value)| uuid::Uuid::parse_str(&value).ok())
        .ok_or_else(|| anyhow!("the link carries no envelope ID (expected an m= value)"))
}

async fn request(
    base: &str,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let client = remote::authenticated_client_builder(base)?
        .timeout(Duration::from_secs(60))
        .build()?;
    let mut request = client
        .request(method, format!("{base}/app/api/envelopes/{path}"))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await?;
    let status = response.status();
    let value: serde_json::Value = response.json().await.unwrap_or_default();
    anyhow::ensure!(
        status.is_success(),
        "{status}: {}",
        value["message"].as_str().unwrap_or("refused")
    );
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn an_envelope_id_comes_from_the_bare_id_or_the_docusign_link() {
        let id = "5f0c7a52-3c8e-4a8e-9b1d-2a6f0e4c9d11";
        let expected = uuid::Uuid::parse_str(id).unwrap();
        for input in [
            id.to_string(),
            id.to_uppercase(),
            format!("https://na4.docusign.net/Member/EmailStart.aspx?a=x&m={id}&er=y"),
            format!("https://app.docusign.com/documents/details?envelopeId={id}"),
        ] {
            assert_eq!(envelope_id_from(&input).unwrap(), expected, "{input}");
        }
        for input in [
            "not-an-id",
            "https://na4.docusign.net/Member/EmailStart.aspx?a=x",
        ] {
            assert!(envelope_id_from(input).is_err(), "{input}");
        }
    }

    #[tokio::test]
    async fn a_refusal_carries_the_servers_reason() {
        let server = MockServer::start().await;
        let envelope = uuid::Uuid::new_v4();
        Mock::given(method("POST"))
            .and(path(format!("/app/api/envelopes/{envelope}/void")))
            .and(header("authorization", "Bearer test-token"))
            .and(body_json(serde_json::json!({"reason": "Declined"})))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "error": "envelope_closed",
                "message": "Envelope is completed; only a draft or an envelope out for signature can be voided",
            })))
            .expect(1)
            .mount(&server)
            .await;
        let error = request(
            &server.uri(),
            "test-token",
            reqwest::Method::POST,
            &format!("{envelope}/void"),
            Some(serde_json::json!({"reason": "Declined"})),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("409"), "{error}");
        assert!(error.to_string().contains("is completed"), "{error}");
    }
}
