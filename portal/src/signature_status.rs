//! Read-side envelope status (LAW-326) — one composer every status surface
//! shares.
//!
//! The send path ([`crate::signature_flow`]) writes the durable
//! `signature` row; the inbound webhook stamps its state and each signer's
//! own step ([`crate::esignature_webhook`]). This module reads the envelope-wide
//! lifecycle state plus per-declared-signer progress, composed from
//! [`store::signatures::latest_for_notation`] and the template's declared
//! signer set — the same declaration the send path built the manifest
//! from, so the status surface can never name a role the envelope does
//! not carry.
//!
//! Consumers: the REST door (`GET /app/api/notations/{id}/signature`),
//! the CLI's `navigator notation envelope status`, and the notation
//! review page's status card.

use std::sync::Arc;

use serde::Serialize;

pub(crate) async fn project_envelope_json(
    db: &store::surreal::SurrealDb,
    storage: &Arc<dyn cloud::StorageService>,
    notation: Option<&store::notations::Notation>,
    envelope: &crate::signature::ExternalEnvelope,
) -> Result<serde_json::Value, serde_json::Error> {
    let roles = match notation {
        Some(notation) => declared_roles(db, storage, notation)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    envelope_with_roles(envelope, &roles)
}

fn envelope_with_roles(
    envelope: &crate::signature::ExternalEnvelope,
    roles: &[rules::f107::SignerRole],
) -> Result<serde_json::Value, serde_json::Error> {
    let mut value = serde_json::to_value(envelope)?;
    let order = |signer: &crate::signature::ExternalSigner| {
        signer
            .routing_order
            .as_deref()
            .and_then(|order| order.parse::<u32>().ok())
    };
    let Some(signers) = value["signers"].as_array_mut() else {
        return Ok(value);
    };
    for (signer, recipient) in signers.iter_mut().zip(&envelope.signers) {
        let Some(routing_order) = order(recipient) else {
            continue;
        };
        if envelope
            .signers
            .iter()
            .filter(|other| order(other) == Some(routing_order))
            .count()
            != 1
        {
            continue;
        }
        if let Some(role) = roles
            .iter()
            .find(|role| role.routing_order == routing_order)
        {
            signer["role"] = serde_json::Value::String(role.role.clone());
        }
    }
    Ok(value)
}

/// One declared signer's own step on the envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvelopeSignerState {
    /// The declared role (`client`, `firm`, …) in routing order.
    pub role: String,
    /// The provider recorded this role's recipient completion (webhook
    /// receipt); on a completed envelope every declared role reports
    /// signed.
    pub signed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip)]
    routing_order: Option<u32>,
}

/// One notation's envelope, read off the durable signature row.
///
/// Every field is optional: a notation that has never been prepared or
/// sent has no envelope, and a status read answers that emptiness
/// structurally rather than with a 404 — "no envelope yet" is the
/// state a lawyer preparing one expects to see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct EnvelopeStatusReport {
    /// The provider's envelope id — `None` until a prepare or send
    /// persisted one.
    pub signature_request_id: Option<String>,
    /// The provider that executed the request (stored string form).
    pub provider: Option<String>,
    /// Envelope-wide lifecycle state: `requested`, `completed`,
    /// `declined`, `voided`, or `expired`.
    pub state: Option<String>,
    /// The portal route that mints the lawyer's sender-view window on
    /// this envelope — the one-click "open in DocuSign" link. `None`
    /// until an envelope exists.
    pub sender_view_href: Option<String>,
    /// Every declared signer in routing order, with their own step.
    pub signers: Vec<EnvelopeSignerState>,
    /// When the durable record last moved — the freshness a lawyer
    /// needs before trusting "signed".
    pub updated_at: Option<String>,
}

/// Compose one notation's envelope status. Infallible by contract: a
/// status read must never 500 on the surfaces it cannot resolve, so a
/// template whose body cannot be read degrades to reporting the roles
/// the webhook has already recorded (a `tracing::warn` names the gap)
/// and an absent envelope reports structurally empty.
pub async fn status_for_notation(
    surreal: &store::surreal::SurrealDb,
    storage: &Arc<dyn cloud::StorageService>,
    notation: &store::notations::Notation,
) -> EnvelopeStatusReport {
    let Some(signature) =
        store::signatures::latest_for_notation(surreal, notation.id).await.unwrap_or_else(|error| {
            tracing::warn!(notation_id = %notation.id, %error, "signature status: latest_for_notation failed");
            None
        })
    else {
        return EnvelopeStatusReport::default();
    };

    let completed = signature.state == store::signatures::SignatureState::Completed;
    let signed_roles = &signature.completed_signer_roles;
    let signers = match declared_roles(surreal, storage, notation).await {
        Ok(declared) => declared
            .into_iter()
            .map(|signer| {
                let signed = completed || signed_roles.contains(&signer.role);
                EnvelopeSignerState {
                    role: signer.role,
                    signed,
                    name: None,
                    email: None,
                    routing_order: Some(signer.routing_order),
                }
            })
            .collect(),
        // The declaration could not be read back; the recorded roles
        // still tell the truth about who has signed.
        Err(error) => {
            tracing::warn!(notation_id = %notation.id, %error, "signature status: declared signer set unavailable");
            signed_roles
                .iter()
                .map(|role| EnvelopeSignerState {
                    role: role.clone(),
                    signed: true,
                    name: None,
                    email: None,
                    routing_order: None,
                })
                .collect()
        }
    };

    EnvelopeStatusReport {
        signature_request_id: Some(signature.provider_id),
        provider: Some(signature.provider),
        state: Some(signature.state.as_str().to_string()),
        sender_view_href: Some(format!("/app/lawyer/notations/{}/sender-view", notation.id)),
        signers,
        updated_at: Some(signature.updated_at.to_rfc3339()),
    }
}

pub(crate) async fn read_signer_identities(
    report: &mut EnvelopeStatusReport,
    db: &store::surreal::SurrealDb,
    project_id: uuid::Uuid,
    provider: &dyn crate::signature::SignatureProvider,
) {
    let Some(request_id) = &report.signature_request_id else {
        return;
    };
    let Ok(scoped) = provider.for_project(db, project_id).await else {
        return;
    };
    let provider = crate::firm_docusign::resolved(scoped.as_ref(), provider);
    let Ok(recipients) = provider
        .notation_signers(&crate::signature::SignatureRequestId(request_id.clone()))
        .await
    else {
        return;
    };
    for signer in &mut report.signers {
        let Some(order) = signer.routing_order else {
            continue;
        };
        let mut matching = recipients.iter().filter(|recipient| {
            recipient
                .routing_order
                .as_deref()
                .and_then(|value| value.parse().ok())
                == Some(order)
        });
        if let Some(recipient) = matching.next().filter(|_| matching.next().is_none()) {
            signer.name = Some(recipient.name.clone());
            signer.email = Some(recipient.email.clone());
        }
    }
}

/// The template's declared signer roles in routing order — the same
/// resolution the send path's manifest used, so status and envelope
/// never disagree about who signs.
async fn declared_roles(
    surreal: &store::surreal::SurrealDb,
    storage: &Arc<dyn cloud::StorageService>,
    notation: &store::notations::Notation,
) -> Result<Vec<rules::f107::SignerRole>, String> {
    let template = store::templates::find_by_id(surreal, notation.template_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "template row missing".to_string())?;
    let source = store::templates::body(surreal, storage, &template)
        .await
        .map_err(|e| e.to_string())?;
    let set = crate::signature_flow::signer_set_for_template(&template.code, &source)?;
    let fields = rules::f107::signature_placeholders(&source);
    let fallback = !set.explicit && fields.is_empty();
    Ok(set
        .signers
        .into_iter()
        .filter_map(|mut signer| {
            if fallback && signer.role == "firm" {
                signer.routing_order = 1;
                Some(signer)
            } else if set.explicit || fields.iter().any(|field| field.signer == signer.role) {
                Some(signer)
            } else {
                None
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_envelope_labels_only_unambiguous_slots_and_keeps_provider_identity() {
        let envelope = serde_json::from_value(serde_json::json!({
            "envelopeId": uuid::Uuid::nil(),
            "status": "sent",
            "signers": [
                {"name": "Ada Example", "email": "ada@example.com", "status": "sent", "routingOrder": "2"},
                {"name": "Client Example", "email": "client@example.com", "status": "sent", "routingOrder": "1"},
                {"name": "Other Example", "email": "other@example.com", "status": "sent", "routingOrder": "1"}
            ]
        })).unwrap();
        let roles = crate::signature_flow::signer_set_for_template("test", "").unwrap();
        let json = envelope_with_roles(&envelope, &roles.signers).unwrap();
        assert_eq!(json["signers"][0]["role"], "firm");
        assert_eq!(json["signers"][0]["name"], "Ada Example");
        assert_eq!(json["signers"][0]["email"], "ada@example.com");
        assert!(json["signers"][1].get("role").is_none());
        assert!(json["signers"][2].get("role").is_none());
        assert!(envelope_with_roles(&envelope, &[]).unwrap()["signers"][0]
            .get("role")
            .is_none());
    }

    // status_for_notation needs an engine, so its integration behavior is
    // covered by the REST-door tests (`server/tests/api_notation_send.rs`)
    // and the webhook cucumber runner. What is testable here, without one,
    // is the report's serializable shape: the exact field names the CLI
    // and the status card read.

    #[test]
    fn an_absent_envelope_serializes_with_nulls_and_no_signers() {
        let report = EnvelopeStatusReport::default();
        let json = serde_json::to_value(&report).expect("report serializes");
        assert_eq!(
            json,
            serde_json::json!({
                "signature_request_id": null,
                "provider": null,
                "state": null,
                "sender_view_href": null,
                "signers": [],
                "updated_at": null,
            })
        );
    }

    #[test]
    fn per_signer_progress_serializes_as_role_plus_signed() {
        let report = EnvelopeStatusReport {
            signature_request_id: Some("env-1".to_string()),
            provider: Some("docusign".to_string()),
            state: Some("requested".to_string()),
            sender_view_href: Some("/app/lawyer/notations/n1/sender-view".to_string()),
            signers: vec![
                EnvelopeSignerState {
                    role: "client".to_string(),
                    signed: true,
                    name: None,
                    email: None,
                    routing_order: Some(1),
                },
                EnvelopeSignerState {
                    role: "firm".to_string(),
                    signed: false,
                    name: Some("Ada Example".into()),
                    email: Some("ada@example.com".into()),
                    routing_order: Some(2),
                },
            ],
            updated_at: Some("2026-11-12T00:00:00+00:00".to_string()),
        };
        let json = serde_json::to_value(&report).expect("report serializes");
        assert_eq!(json["signers"][0]["role"], "client");
        assert_eq!(json["signers"][0]["signed"], true);
        assert_eq!(json["signers"][1]["role"], "firm");
        assert_eq!(json["signers"][1]["signed"], false);
        assert_eq!(json["signers"][1]["name"], "Ada Example");
        assert_eq!(json["signers"][1]["email"], "ada@example.com");
        assert_eq!(json["state"], "requested");
        assert_eq!(
            json["sender_view_href"],
            "/app/lawyer/notations/n1/sender-view"
        );
    }
}
