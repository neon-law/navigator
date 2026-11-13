//! Read-side envelope status (LAW-326) — one composer every status surface
//! shares.
//!
//! The send path ([`crate::signature_flow`]) writes the durable
//! `signature` row; the inbound webhook stamps its state and each signer's
//! own step ([`crate::esignature_webhook`]). Nothing on `main` reads that
//! record back per signer. This module is that read: the envelope-wide
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

/// One declared signer's own step on the envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EnvelopeSignerState {
    /// The declared role (`client`, `firm`, …) in routing order.
    pub role: String,
    /// The provider recorded this role's recipient completion (webhook
    /// receipt); on a completed envelope every declared role reports
    /// signed.
    pub signed: bool,
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

    let signed_roles = &signature.completed_signer_roles;
    let signers = match declared_roles(surreal, storage, notation).await {
        Ok(declared) => declared
            .into_iter()
            .map(|role| {
                let signed = signed_roles.contains(&role);
                EnvelopeSignerState { role, signed }
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

/// The template's declared signer roles in routing order — the same
/// resolution the send path's manifest used, so status and envelope
/// never disagree about who signs.
async fn declared_roles(
    surreal: &store::surreal::SurrealDb,
    storage: &Arc<dyn cloud::StorageService>,
    notation: &store::notations::Notation,
) -> Result<Vec<String>, String> {
    let template = store::templates::find_by_id(surreal, notation.template_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "template row missing".to_string())?;
    let source = store::templates::body(surreal, storage, &template)
        .await
        .map_err(|e| e.to_string())?;
    let set = crate::signature_flow::signer_set_for_template(&template.code, &source)?;
    Ok(set.signers.into_iter().map(|s| s.role).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

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
                },
                EnvelopeSignerState {
                    role: "firm".to_string(),
                    signed: false,
                },
            ],
            updated_at: Some("2026-11-12T00:00:00+00:00".to_string()),
        };
        let json = serde_json::to_value(&report).expect("report serializes");
        assert_eq!(json["signers"][0]["role"], "client");
        assert_eq!(json["signers"][0]["signed"], true);
        assert_eq!(json["signers"][1]["role"], "firm");
        assert_eq!(json["signers"][1]["signed"], false);
        assert_eq!(json["state"], "requested");
        assert_eq!(
            json["sender_view_href"],
            "/app/lawyer/notations/n1/sender-view"
        );
    }
}
