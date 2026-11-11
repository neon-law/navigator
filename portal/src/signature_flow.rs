//! Template-agnostic send-for-signature flow.
//!
//! One seam any notation whose workflow declares a `sent_for_signature`
//! state drives: gate on the rendered PDF, refuse an unfinished document,
//! fire `pdf_persisted`, build the manifest from the template's declared
//! signer set, send exactly one envelope, and persist the request id.
//! Extracted from the retainer walk (`retainer_walk::dispatch_signature`)
//! so a second signed template signs through the same path instead of
//! growing a second handler.

use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

use workflows::{StateMachineRuntime, StateName};

use crate::retainer_walk::WorkflowDriveError;

/// How the envelope is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendMode {
    /// Create and send the envelope now — today's behavior.
    Send,
    /// Create the envelope as a draft without sending. The provider seam
    /// does not carry a draft capability yet (LAW-270 lands it), so this
    /// is refused before anything is written or fired.
    Prepare,
}

/// The dependencies the send command core ([`send_notation_for_signature`])
/// borrows from whichever web state drives it — the lawyer `AdminState` or
/// the REST `ApiState`. The provider and the send mode are per-call
/// arguments; the actor rides along because every transition the send fires
/// is attributed to whoever sent, never the notation's client (#252).
/// Distinct from [`crate::retainer_walk::RenderDeps`]: send needs the
/// signature provider but not the government-form registry / assets bucket.
pub struct SendFlowContext<'a> {
    /// The other store — the notation's client is a `persons` row.
    pub surreal: &'a store::surreal::SurrealDb,
    pub runtime: &'a dyn StateMachineRuntime,
    pub storage: &'a Arc<dyn cloud::StorageService>,
    /// The authenticated actor attributed to the transitions this send fires.
    pub acting: Option<Uuid>,
}

/// Dispatch a notation's rendered document for signature — the deliberate,
/// authenticated "send" half of the pipeline, keyed off the notation's
/// template declarations rather than any one template. Confirms the
/// worker's PDF is present, fires `pdf_persisted` (→
/// `sent_for_signature__pending`), reads the persisted PDF back, builds
/// the manifest from the template's declared signer set, and sends exactly
/// one envelope, persisting the `signature_request_id`.
///
/// Idempotent (find-or-create keyed on `notation_id`): a notation that
/// already carries a `signature_request_id` reuses it and neither
/// re-fires the transition nor re-sends. When the PDF isn't present yet
/// (the worker hasn't rendered, or its storage is misconfigured), returns
/// [`WorkflowDriveError::DocumentNotReady`] so the caller can answer
/// "not yet — retry" (a `409`) instead of looping or 500-ing. The
/// provider ALSO sends an X-DocuSign-Idempotency-Key so a concurrent
/// double-send dedupes at DocuSign, not just on the id check here.
pub(crate) async fn send_notation_for_signature(
    ctx: &SendFlowContext<'_>,
    notation_id: Uuid,
    provider: &dyn crate::signature::SignatureProvider,
    mode: SendMode,
) -> Result<(StateName, crate::signature::SignatureRequestId), WorkflowDriveError> {
    // A draft envelope is a provider capability the seam does not carry
    // yet (LAW-270); refuse before any store write or workflow signal.
    if mode == SendMode::Prepare {
        return Err(WorkflowDriveError::PrepareUnsupported);
    }

    let notation_row = store::notations::find_by_id(ctx.surreal, notation_id)
        .await?
        .ok_or(WorkflowDriveError::TemplateMissing(notation_id))?;

    // Idempotency: this notation already has an envelope out. Reuse the
    // recorded id, fire nothing, send nothing — the post-state is
    // whatever the notation already records.
    if let Some(existing) =
        store::signatures::request_id_for_notation(ctx.surreal, notation_id).await?
    {
        return Ok((
            StateName::from(notation_row.state.as_str()),
            crate::signature::SignatureRequestId(existing),
        ));
    }

    // Readiness gate: the worker durably renders + persists the PDF on
    // entering `generate_pdf__*`. Confirm it landed before advancing —
    // against real Restate Cloud that render is a separate invocation, so
    // "approved fired" does not imply "PDF written."
    if !document_pdf_ready(ctx.storage.as_ref(), notation_id).await? {
        return Err(WorkflowDriveError::DocumentNotReady(notation_id));
    }

    // Resolve the template + its clauses before advancing. The manifest's
    // recipients and placed signature fields are built from them below, and
    // the fee-clause gate reads them here: the walk parks at
    // `sent_for_signature__pending` once `pdf_persisted` fires, so a refusal
    // has to land before that transition rather than unwind it.
    let template_row = store::templates::find_by_id(ctx.surreal, notation_row.template_id)
        .await?
        .ok_or(WorkflowDriveError::TemplateMissing(notation_id))?;
    let raw_template_body = store::templates::body(ctx.surreal, ctx.storage, &template_row).await?;
    let clauses = store::notation_clauses::for_notation(ctx.surreal, notation_id)
        .await
        .map_err(|error| WorkflowDriveError::Db(error.to_string()))?;

    // A template that carries the custom-clause slot expects its terms to
    // arrive as clauses — for the engagement agreement that is the fee
    // basis, which NRPC 1.5(b) and Cal. B&P § 6148 require the client be
    // told in writing. `splice` substitutes the marker with whatever exists,
    // including nothing, so an empty slot renders cleanly and would dispatch
    // an agreement with no fee terms in it. Refuse instead.
    if raw_template_body.contains(store::notation_clauses::CUSTOM_CLAUSES_MARKER)
        && clauses.is_empty()
    {
        return Err(WorkflowDriveError::ClausesRequired(notation_id));
    }

    // Final documents only: the text the worker rendered from this context
    // carries nothing a person is still meant to complete. Signing anchors
    // (`{{client.signature}}`) are the provider's and pass.
    let template_body = store::notation_clauses::splice(&raw_template_body, &clauses);
    let (keys, answer_ctx) =
        crate::retainer_walk::build_answer_context(ctx.surreal, Some(ctx.storage), notation_id)
            .await?;
    let unfinished = rules::final_document::scan(&crate::retainer_walk::substitute_template_body(
        &template_body,
        &keys,
        &answer_ctx,
    ));
    if !unfinished.is_empty() {
        return Err(WorkflowDriveError::Unfinished(
            unfinished.iter().map(ToString::to_string).collect(),
        ));
    }

    let s = crate::retainer_walk::signal_workflow(
        ctx.runtime,
        notation_id,
        "pdf_persisted",
        None,
        ctx.acting,
    )
    .await?;
    crate::retainer_walk::sync_notation_state(ctx.surreal, notation_id, s.as_str()).await?;

    // Now at sent_for_signature__pending; fire the signature seam.
    // Recipients follow the template's declared signer set in order.
    // The captive client's identity comes from the questionnaire answers
    // when present (the retainer asks `person__client`, exposing
    // `person__client.name`) and otherwise from the notation's bound
    // Person row — never hardcoded in the provider.
    let (_typst_source, signature_fields) = crate::signature_render::expand_signatures(
        &crate::retainer_walk::typst_body_from_template(
            &template_body,
            &keys,
            &answer_ctx,
            template_row.kind.as_deref(),
        ),
    );

    // Read the PDF the worker persisted back from storage so the bytes
    // sent are exactly the bytes stored (one renderer, no second
    // in-process copy to drift).
    let pdf_bytes = ctx
        .storage
        .get(&store::notations::document_pdf_storage_key(notation_id))
        .await?
        .bytes;
    let client = store::persons::find_by_id(ctx.surreal, notation_row.person_id).await?;
    // `emailed` delivery → non-captive client (DocuSign emails the signing
    // link); anything else (`embedded`, the default) keeps the captive
    // embedded-signing recipient. Read off the notation so the single send
    // path serves both without a second route.
    let captive = notation_row.delivery != store::notations::DELIVERY_EMAILED;
    let manifest = build_signature_manifest(
        notation_id,
        &signature_fields,
        &answer_ctx,
        client.as_ref(),
        captive,
        &raw_template_body,
    )?;
    let id = provider
        .send_for_signature(notation_id, &pdf_bytes, &manifest)
        .await?;
    // Persist the request id so the inbound completion webhook
    // (`crate::esignature_webhook`) can resolve its callback back to this
    // notation.
    persist_signature_request_id(ctx.surreal, notation_id, &id.0).await?;
    let notation_id = notation_id.to_string();
    let project_id = notation_row.project_id.to_string();
    telemetry::record_funnel_event(telemetry::FunnelEvent::Sent {
        notation_id: &notation_id,
        project_id: &project_id,
        channel: telemetry::FunnelChannel::Signature,
    });

    Ok((s, id))
}

/// Whether the worker has rendered + persisted the notation's document
/// PDF — a cheap existence probe on [`store::notations::
/// document_pdf_storage_key`] (a metadata-only HEAD on GCS).
/// [`send_notation_for_signature`] gates on this before advancing the
/// workflow and sending the envelope, and `notation status` surfaces it
/// as `document_ready`, so a misconfigured worker that never wrote the
/// PDF is visible rather than an opaque 500 at send time.
pub(crate) async fn document_pdf_ready(
    storage: &dyn cloud::StorageService,
    notation_id: Uuid,
) -> Result<bool, cloud::StorageError> {
    storage
        .exists(&store::notations::document_pdf_storage_key(notation_id))
        .await
}

/// Assemble the signature manifest from the placed fields and the
/// template's declared signer set. Recipients fan out one per declared
/// role, in declaration order, using the same routing-order mechanism
/// two-party envelopes use. `client` resolves from questionnaire answers
/// or the bound Person; `firm` from `DOCUSIGN_SIGNER_*`; any other role
/// from `person__<role>.name` / `.email`. Empty fields → empty manifest
/// (the provider's single-signer fallback). A declared role that lacks a
/// resolved name and email refuses the send.
pub(crate) fn build_signature_manifest(
    notation_id: Uuid,
    fields: &[crate::signature::SignatureField],
    ctx: &BTreeMap<String, String>,
    client: Option<&store::persons::Person>,
    captive: bool,
    template_source: &str,
) -> Result<crate::signature::SignatureManifest, WorkflowDriveError> {
    use crate::signature::{SignatureManifest, SignatureRecipient};
    if fields.is_empty() {
        return Ok(SignatureManifest::default());
    }
    let set = rules::signer_set(template_source)
        .map_err(|message| WorkflowDriveError::UnresolvedSigner { role: message })?;
    let role_present = |role: &str| fields.iter().any(|f| f.recipient_role == role);
    let env = |k: &str, default: &str| {
        std::env::var(k)
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default.to_string())
    };
    let answered = |key: &str| ctx.get(key).filter(|s| !s.is_empty()).cloned();

    let mut recipients = Vec::new();
    for (role, routing_order) in set.roles.iter().zip(1u32..) {
        if !role_present(role) {
            if set.explicit {
                return Err(WorkflowDriveError::UnresolvedSigner { role: role.clone() });
            }
            continue;
        }
        let (name, email, client_user_id) = match role.as_str() {
            // Shared with the view path so the triple sent and the triple
            // replayed cannot drift — see
            // [`crate::retainer_walk::resolve_client_recipient`].
            "client" => {
                let c = crate::retainer_walk::resolve_client_recipient(
                    notation_id,
                    ctx,
                    client,
                    captive,
                );
                (c.name, c.email, c.client_user_id)
            }
            "firm" => (
                env("DOCUSIGN_SIGNER_NAME", "Neon Law"),
                env("DOCUSIGN_SIGNER_EMAIL", "support@neonlaw.com"),
                None,
            ),
            other => (
                answered(&format!("person__{other}.name")).unwrap_or_default(),
                answered(&format!("person__{other}.email")).unwrap_or_default(),
                None,
            ),
        };
        if name.is_empty() || email.is_empty() {
            return Err(WorkflowDriveError::UnresolvedSigner { role: role.clone() });
        }
        recipients.push(SignatureRecipient {
            role: role.clone(),
            email,
            name,
            routing_order,
            client_user_id,
        });
    }
    Ok(crate::signature::SignatureManifest {
        recipients,
        fields: fields.to_vec(),
    })
}

/// Record the e-signature provider's request id in `signatures` so a
/// later completion webhook can resolve its callback by `(provider,
/// provider_id)`. See [`crate::esignature_webhook`].
async fn persist_signature_request_id(
    surreal: &store::surreal::SurrealDb,
    notation_id: Uuid,
    request_id: &str,
) -> Result<(), store::signatures::SignatureError> {
    store::signatures::record_request(
        surreal,
        notation_id,
        store::signatures::SignatureProvider::DocuSign,
        request_id,
    )
    .await?;
    Ok(())
}

