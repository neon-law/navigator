//! Xero webhook signals for already-linked journal sources. Signatures are
//! verified on the original bytes before any parsing or PostgreSQL write.
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use sha2::Sha256;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    events: Vec<ProviderEvent>,
    first_event_sequence: u64,
    last_event_sequence: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProviderEvent {
    resource_id: Uuid,
    tenant_id: Uuid,
    tenant_type: String,
    event_category: String,
    event_type: String,
    event_date_utc: String,
}
struct Signal {
    scope: String,
    object_type: &'static str,
    external_id: String,
    event_id: String,
    at: String,
}
fn verify(key: &[u8], signature: &str, body: &[u8]) -> bool {
    if key.is_empty() {
        return false;
    }
    let Ok(signature) = STANDARD.decode(signature) else {
        return false;
    };
    let Ok(mut mac) = <Hmac<Sha256> as KeyInit>::new_from_slice(key) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}
fn parse(body: &[u8]) -> Result<Vec<Signal>, ()> {
    if body.len() > 1_048_576 {
        return Err(());
    }
    let envelope: Envelope = serde_json::from_slice(body).map_err(|_| ())?;
    if envelope.events.len() > 1000 || envelope.first_event_sequence > envelope.last_event_sequence
    {
        return Err(());
    }
    let mut result = Vec::new();
    for (offset, event) in envelope.events.into_iter().enumerate() {
        if event.tenant_type != "ORGANISATION"
            || !matches!(event.event_type.as_str(), "CREATE" | "UPDATE")
        {
            continue;
        }
        let object_type = match event.event_category.as_str() {
            "INVOICE" => "invoice",
            "CREDITNOTE" => "credit_note",
            _ => continue,
        };
        let at = chrono::DateTime::parse_from_rfc3339(&event.event_date_utc)
            .map(|time| time.to_rfc3339())
            .or_else(|_| {
                chrono::NaiveDateTime::parse_from_str(&event.event_date_utc, "%Y-%m-%dT%H:%M:%S%.f")
                    .map(|time| time.and_utc().to_rfc3339())
            })
            .map_err(|_| ())?;
        let sequence = envelope
            .first_event_sequence
            .checked_add(offset as u64)
            .filter(|seq| *seq <= envelope.last_event_sequence)
            .ok_or(())?;
        let event_id = store::documents::sha256_hex(
            &serde_json::to_vec(&(
                sequence,
                event.tenant_id,
                event.resource_id,
                &event.event_category,
                &event.event_type,
                &at,
            ))
            .map_err(|_| ())?,
        );
        result.push(Signal {
            scope: event.tenant_id.to_string(),
            object_type,
            external_id: event.resource_id.to_string(),
            event_id,
            at,
        });
    }
    Ok(result)
}
pub(crate) fn routes() -> Router<crate::AppState> {
    Router::new()
        .route("/webhook/journal/xero/{firm}", post(xero))
        .layer(axum::extract::DefaultBodyLimit::max(1_048_576))
}
async fn xero(
    State(state): State<crate::AppState>,
    Path(firm): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    use store::firm_secrets::{
        resolve_firm_credential, IntegrationProvider, IntegrationSecretKind,
    };
    let Some(signature) = headers
        .get("x-xero-signature")
        .and_then(|value| value.to_str().ok())
    else {
        return StatusCode::UNAUTHORIZED;
    };
    let kms = crate::integrations::runtime_kms_from_env().await;
    let Ok(key) = resolve_firm_credential(
        &state.surreal,
        firm,
        IntegrationProvider::Xero,
        IntegrationSecretKind::XeroWebhookKey,
        kms.as_ref(),
    )
    .await
    else {
        return StatusCode::UNAUTHORIZED;
    };
    if !verify(key.expose_for_provider_call().as_bytes(), signature, &body) {
        return StatusCode::UNAUTHORIZED;
    }
    let Ok(signals) = parse(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    // Xero's signed intent-to-receive handshake has no events and requires 200.
    if signals.is_empty() {
        return StatusCode::OK;
    }
    let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    for signal in signals {
        if store::firm_journal::signal(
            &pg,
            firm,
            "xero",
            &signal.scope,
            signal.object_type,
            &signal.external_id,
            &signal.event_id,
            &signal.at,
        )
        .await
        .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE;
        }
    }
    StatusCode::OK
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signature_covers_original_bytes_and_fails_closed() {
        let body = br#"{ "events": [] }"#;
        // Generated keys are confined to this verification test.
        let key: [u8; 32] = rand::random();
        let wrong_key: [u8; 32] = rand::random();
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&key).unwrap();
        mac.update(body);
        let signature = STANDARD.encode(mac.finalize().into_bytes());
        assert!(verify(&key, &signature, body));
        assert!(!verify(&key, &signature, br#"{"events":[]}"#));
        assert!(!verify(&wrong_key, &signature, body));
        assert!(!verify(b"", &signature, body));
        assert!(!verify(&key, "bad base64", body));
    }
    #[test]
    fn event_identity_is_stable_and_urls_are_not_fetch_targets() {
        let tenant = Uuid::now_v7();
        let invoice = Uuid::now_v7();
        let body = serde_json::json!({"firstEventSequence":42,"lastEventSequence":42,"entropy":"a","events":[{"tenantId":tenant,"tenantType":"ORGANISATION","resourceId":invoice,"resourceUrl":"http://127.0.0.1/private","eventType":"UPDATE","eventCategory":"INVOICE","eventDateUtc":"2026-10-11T01:00:00.123"}]});
        let one = parse(&serde_json::to_vec(&body).unwrap()).unwrap();
        let mut changed_entropy = body.clone();
        changed_entropy["entropy"] = serde_json::json!("b");
        let two = parse(&serde_json::to_vec(&changed_entropy).unwrap()).unwrap();
        assert_eq!(one[0].event_id, two[0].event_id);
        assert_eq!(one[0].scope, tenant.to_string());
        assert!(one[0].at.ends_with("+00:00"));
        let mut malformed = body;
        malformed["events"][0]["eventDateUtc"] = serde_json::json!("unknown");
        assert!(parse(&serde_json::to_vec(&malformed).unwrap()).is_err());
        assert!(
            parse(br#"{"events":[],"firstEventSequence":0,"lastEventSequence":0}"#)
                .unwrap()
                .is_empty()
        );
    }
}
