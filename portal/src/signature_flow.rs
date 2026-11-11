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
/// role, in the set's routing order — declaration order unless the
/// template spells `routing_order` out. `client` resolves from
/// questionnaire answers or the bound Person; `firm` from
/// `DOCUSIGN_SIGNER_*`; any other role from its declared `name`/`email`
/// answer-key bindings, falling back to `person__<role>.name` /
/// `.email`. Empty fields → empty manifest (the provider's
/// single-signer fallback). A declared role that lacks a resolved name
/// and email refuses the send.
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
    for signer in &set.signers {
        let role = signer.role.as_str();
        if !role_present(role) {
            if set.explicit {
                return Err(WorkflowDriveError::UnresolvedSigner {
                    role: role.to_string(),
                });
            }
            continue;
        }
        let (name, email, client_user_id) = match role {
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
            other => {
                // The template's declared bindings win; the person-state
                // defaults back them up. Non-client roles are always
                // emailed (validation refuses anything else), so there is
                // no embedded view to mint for them.
                let default_name = format!("person__{other}.name");
                let default_email = format!("person__{other}.email");
                let name_key = signer.name.as_deref().unwrap_or(&default_name);
                let email_key = signer.email.as_deref().unwrap_or(&default_email);
                (
                    answered(name_key).unwrap_or_default(),
                    answered(email_key).unwrap_or_default(),
                    None,
                )
            }
        };
        if name.is_empty() || email.is_empty() {
            return Err(WorkflowDriveError::UnresolvedSigner {
                role: role.to_string(),
            });
        }
        recipients.push(SignatureRecipient {
            role: role.to_string(),
            email,
            name,
            routing_order: signer.routing_order,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A bound Person carrying a distinct name/email, so a fixture can tell
    /// the answered value apart from the Person-row fallback.
    fn person_named(name: &str, email: &str) -> store::persons::Person {
        store::persons::Person {
            id: Uuid::now_v7(),
            name: name.to_string(),
            given_name: None,
            family_name: None,
            middle_name: None,
            email: email.to_string(),
            oidc_subject: None,
            microsoft_subject: None,
            apple_subject: None,
            role: store::persons::Role::Client,
            title: None,
            phone: None,
            xero_contact_id: None,
            profile_image_url: None,
            linkedin_url: None,
            email_confirmed: false,
            inserted_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn client_signature_field() -> crate::signature::SignatureField {
        crate::signature::SignatureField {
            recipient_role: "client".into(),
            kind: crate::signature::SignatureFieldKind::Signature,
            anchor: "{{client.signature}}".into(),
        }
    }

    fn assignor_signature_field() -> crate::signature::SignatureField {
        crate::signature::SignatureField {
            recipient_role: "confirming_assignor".into(),
            kind: crate::signature::SignatureFieldKind::Signature,
            anchor: "{{confirming_assignor.signature}}".into(),
        }
    }

    fn firm_signature_field() -> crate::signature::SignatureField {
        crate::signature::SignatureField {
            recipient_role: "firm".into(),
            kind: crate::signature::SignatureFieldKind::Signature,
            anchor: "{{firm.signature}}".into(),
        }
    }

    fn three_party_template() -> &'static str {
        "---\nsigners:\n  - client\n  - role: firm\n    delivery: emailed\n  - role: \
         confirming_assignor\n    delivery: emailed\n---\n"
    }

    fn employee_signature_field() -> crate::signature::SignatureField {
        crate::signature::SignatureField {
            recipient_role: "employee".into(),
            kind: crate::signature::SignatureFieldKind::Signature,
            anchor: "{{employee.signature}}".into(),
        }
    }

    #[test]
    fn signature_manifest_prefers_answered_client_name_over_person_row() {
        // The migration renamed the client-identity state to
        // `person__client`, whose `.name`/`.email` fields land in the render
        // context. The manifest must read those — not the removed
        // `custom_text__client_*` keys — so the questionnaire-confirmed
        // legal name reaches the DocuSign recipient. In production the bound
        // Person is created with `name = <email>` until the questionnaire
        // fills it, so a fallback silently signs the client under their
        // email address. The fixture name differs from the answer to prove
        // the answered value wins.
        let ctx = BTreeMap::from([
            ("person__client.name".to_string(), "Libra Prime".to_string()),
            (
                "person__client.email".to_string(),
                "libra@example.com".to_string(),
            ),
        ]);
        let client = person_named("libra@example.com", "libra@example.com");
        let fields = [client_signature_field()];

        let manifest =
            build_signature_manifest(Uuid::now_v7(), &fields, &ctx, Some(&client), true, "")
                .expect("client identity resolves from answers");

        let recipient = manifest
            .recipients
            .iter()
            .find(|r| r.role == "client")
            .expect("client recipient present");
        assert_eq!(recipient.name, "Libra Prime");
        assert_eq!(recipient.email, "libra@example.com");
    }

    #[test]
    fn signature_manifest_falls_back_to_person_row_when_unanswered() {
        // The trust questionnaire never captures an email and other flows
        // capture it out-of-band, so with no `person__client.*` in context
        // the recipient resolves from the bound Person row.
        let ctx = BTreeMap::new();
        let client = person_named("Libra Prime", "libra@example.com");
        let fields = [client_signature_field()];

        let manifest =
            build_signature_manifest(Uuid::now_v7(), &fields, &ctx, Some(&client), true, "")
                .expect("client identity resolves from the Person row");

        let recipient = manifest
            .recipients
            .iter()
            .find(|r| r.role == "client")
            .expect("client recipient present");
        assert_eq!(recipient.name, "Libra Prime");
        assert_eq!(recipient.email, "libra@example.com");
    }

    #[test]
    fn signature_manifest_fans_out_declared_roles_in_declaration_order() {
        let ctx = BTreeMap::from([
            ("person__client.name".to_string(), "Libra Prime".to_string()),
            (
                "person__client.email".to_string(),
                "libra@example.com".to_string(),
            ),
            (
                "person__confirming_assignor.name".to_string(),
                "Pat Assignor".to_string(),
            ),
            (
                "person__confirming_assignor.email".to_string(),
                "assignor@example.com".to_string(),
            ),
        ]);
        let client = person_named("Libra Prime", "libra@example.com");
        let fields = [
            client_signature_field(),
            firm_signature_field(),
            assignor_signature_field(),
        ];

        let manifest = build_signature_manifest(
            Uuid::now_v7(),
            &fields,
            &ctx,
            Some(&client),
            true,
            three_party_template(),
        )
        .expect("every declared role is resolved");

        let roles: Vec<&str> = manifest
            .recipients
            .iter()
            .map(|r| r.role.as_str())
            .collect();
        assert_eq!(roles, ["client", "firm", "confirming_assignor"]);
        assert_eq!(manifest.recipients[0].routing_order, 1);
        assert_eq!(manifest.recipients[1].routing_order, 2);
        assert_eq!(manifest.recipients[2].routing_order, 3);
        assert_eq!(manifest.recipients[2].email, "assignor@example.com");
        assert_eq!(manifest.recipients[2].name, "Pat Assignor");
    }

    /// A provider recipient event names its recipient by the id the
    /// manifest sent, so reading it back must land on the same declared
    /// role the manifest assigned that routing order.
    #[test]
    fn a_recipient_id_reads_back_as_the_role_the_manifest_sent_it_to() {
        let ctx = BTreeMap::from([
            (
                "person__confirming_assignor.name".to_string(),
                "Pat Assignor".to_string(),
            ),
            (
                "person__confirming_assignor.email".to_string(),
                "assignor@example.com".to_string(),
            ),
        ]);
        let client = person_named("Libra Prime", "libra@example.com");
        let fields = [
            client_signature_field(),
            firm_signature_field(),
            assignor_signature_field(),
        ];
        let manifest = build_signature_manifest(
            Uuid::now_v7(),
            &fields,
            &ctx,
            Some(&client),
            true,
            three_party_template(),
        )
        .expect("every declared role is resolved");
        for recipient in &manifest.recipients {
            assert_eq!(
                crate::retainer_walk::signer_role_for_recipient(
                    three_party_template(),
                    &recipient.routing_order.to_string()
                )
                .as_deref(),
                Some(recipient.role.as_str()),
            );
        }

        // No `signers:` key: the implicit client-then-firm default.
        assert_eq!(
            crate::retainer_walk::signer_role_for_recipient("no front matter", "1").as_deref(),
            Some("client")
        );
        assert_eq!(
            crate::retainer_walk::signer_role_for_recipient("no front matter", "2").as_deref(),
            Some("firm")
        );
        for unknown in ["0", "3", "", "client"] {
            assert_eq!(
                crate::retainer_walk::signer_role_for_recipient("no front matter", unknown),
                None,
                "{unknown:?} names no declared signer"
            );
        }
    }

    #[test]
    fn signature_manifest_refuses_an_unresolved_declared_role() {
        let ctx = BTreeMap::from([
            ("person__client.name".to_string(), "Libra Prime".to_string()),
            (
                "person__client.email".to_string(),
                "libra@example.com".to_string(),
            ),
        ]);
        let client = person_named("Libra Prime", "libra@example.com");
        let fields = [
            client_signature_field(),
            firm_signature_field(),
            assignor_signature_field(),
        ];

        let err = build_signature_manifest(
            Uuid::now_v7(),
            &fields,
            &ctx,
            Some(&client),
            true,
            three_party_template(),
        )
        .expect_err("confirming_assignor has no name or email");
        let message = err.to_string();
        assert!(
            message.contains("confirming_assignor"),
            "refusal must name the unresolved role: {message}"
        );
    }

    /// The template owns the envelope's shape: declared routing orders
    /// reorder recipients (the employee signs before the client
    /// countersigns, whatever the list order), a declared `email:` binding
    /// resolves from its questionnaire answer key, and the delivery default
    /// holds — the client signs embedded only on an embedded notation,
    /// every other declared role is emailed (no embedded view exists to
    /// serve them).
    #[test]
    fn signature_manifest_builds_from_the_declarations() {
        let template = "---\nquestionnaire:\n  BEGIN:\n    _: person__client\n  \
             person__client:\n    _: END\n  END: {{}}\n  custom_text__employee_email: {{}}\nsigners:\n  - \
             role: client\n    routing_order: 2\n  - role: employee\n    delivery: emailed\n    \
             routing_order: 1\n    email: custom_text__employee_email\n---\n";
        let ctx = BTreeMap::from([
            ("person__client.name".to_string(), "Libra Prime".to_string()),
            (
                "person__client.email".to_string(),
                "libra@example.com".to_string(),
            ),
            (
                "person__employee.name".to_string(),
                "Pat Employee".to_string(),
            ),
            (
                "custom_text__employee_email".to_string(),
                "pat@example.com".to_string(),
            ),
        ]);
        let client = person_named("Libra Prime", "libra@example.com");
        let fields = [employee_signature_field(), client_signature_field()];

        let manifest =
            build_signature_manifest(Uuid::now_v7(), &fields, &ctx, Some(&client), true, template)
                .expect("every declared role resolves");

        let roles: Vec<&str> = manifest
            .recipients
            .iter()
            .map(|r| r.role.as_str())
            .collect();
        assert_eq!(roles, ["employee", "client"], "declared routing order wins");
        assert_eq!(manifest.recipients[0].routing_order, 1);
        assert_eq!(manifest.recipients[1].routing_order, 2);
        // The employee's email came from the declared binding, not a
        // `person__employee.email` default the questionnaire never answers.
        assert_eq!(manifest.recipients[0].email, "pat@example.com");
        assert_eq!(manifest.recipients[0].client_user_id, None);
        // Embedded notation: the client keeps the captive ceremony.
        assert!(manifest.recipients[1].client_user_id.is_some());
    }

    /// The client's embedded posture is the notation's, not the manifest
    /// builder's: on an emailed notation the same template sends the client
    /// a link instead, so the view path and the envelope cannot disagree.
    #[test]
    fn signature_manifest_keeps_the_client_delivery_on_the_notation_row() {
        let ctx = BTreeMap::from([
            ("person__client.name".to_string(), "Libra Prime".to_string()),
            (
                "person__client.email".to_string(),
                "libra@example.com".to_string(),
            ),
        ]);
        let client = person_named("Libra Prime", "libra@example.com");
        let fields = [client_signature_field(), firm_signature_field()];

        let manifest =
            build_signature_manifest(Uuid::now_v7(), &fields, &ctx, Some(&client), false, "")
                .expect("the default set resolves");

        let client_recipient = manifest
            .recipients
            .iter()
            .find(|r| r.role == "client")
            .expect("client recipient present");
        assert_eq!(
            client_recipient.client_user_id, None,
            "emailed notation, emailed client"
        );
        let firm_recipient = manifest
            .recipients
            .iter()
            .find(|r| r.role == "firm")
            .expect("firm recipient present");
        assert_eq!(firm_recipient.client_user_id, None);
    }

    #[test]
    fn prepare_mode_refuses_before_anything_is_sent() {
        // LAW-270 lands draft envelopes; until the provider seam carries
        // that capability, prepare mode is a typed refusal — never a
        // silent send.
        assert_ne!(SendMode::Prepare, SendMode::Send);
        let refusal = WorkflowDriveError::PrepareUnsupported.to_string();
        assert!(
            refusal.contains("prepare"),
            "the refusal must name the unsupported mode: {refusal}"
        );
    }
}
