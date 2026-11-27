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
use crate::signature::{SignatureError, SignatureProvider, SignatureRecipient};

pub use crate::signature::SendMode;

/// The dependencies the send command core ([`send_notation_for_signature`])
/// borrows from whichever web state drives it — the lawyer `AdminState` or
/// the REST `ApiState`. The provider and the send mode are per-call
/// arguments; the actor rides along because every transition the send fires
/// is attributed to whoever sent, never the notation's client (#252).
/// Distinct from [`crate::retainer_walk::RenderDeps`]: send needs the
/// signature provider but not the government-form registry / assets bucket.
pub struct SendFlowContext<'a> {
    /// The other store — the notation's client is a `persons` row.
    pub application_store: &'a store::application_store::ApplicationStore,
    pub runtime: &'a dyn StateMachineRuntime,
    pub storage: &'a Arc<dyn cloud::StorageService>,
    /// The authenticated actor attributed to the transitions this send fires.
    pub acting: Option<Uuid>,
}

/// Advance an existing draft when the Project's provider confirms dispatch.
/// This records the send without issuing a provider write.
pub(crate) async fn record_observed_dispatch(
    db: &store::application_store::ApplicationStore,
    runtime: &dyn StateMachineRuntime,
    notation: &store::notations::Notation,
) -> Result<(), WorkflowDriveError> {
    use workflows::MachineKind;

    let current = runtime
        .current_state(MachineKind::Workflow, notation.id)
        .await;
    let next = match current.as_ref().map(StateName::prefix) {
        Some("generate_pdf") => {
            match crate::retainer_walk::signal_workflow(
                runtime,
                notation.id,
                "pdf_persisted",
                None,
                None,
            )
            .await
            {
                Ok(next) => next,
                Err(error) => {
                    // An overlapping return or dispatch may already have advanced the workflow.
                    runtime
                        .current_state(MachineKind::Workflow, notation.id)
                        .await
                        .filter(|state| state.prefix() == "sent_for_signature")
                        .ok_or(error)?
                }
            }
        }
        Some("sent_for_signature") => current.ok_or(
            workflows::WorkflowRuntimeError::UnknownNotation(notation.id),
        )?,
        _ => return Ok(()),
    };
    if next.prefix() == "sent_for_signature"
        && (notation.state == "lawyer_review" || notation.state.starts_with("generate_pdf"))
    {
        store::notations::reconcile_state(db, notation.id, &notation.state, next.as_str()).await?;
    }
    Ok(())
}

/// Dispatch a notation's rendered document for signature — the deliberate,
/// authenticated "send" half of the pipeline, keyed off the notation's
/// template declarations rather than any one template. Confirms the
/// worker's PDF is present, builds the manifest from the template's
/// declared signer set, and creates exactly one envelope, persisting the
/// `signature_request_id`.
///
/// Preparation records the provider's draft ID before advancing the
/// workflow. Dispatch advances to the signing state and sends that draft
/// by ID. A retry in the signing state checks the same envelope rather than
/// creating a new one.
pub(crate) async fn send_notation_for_signature(
    ctx: &SendFlowContext<'_>,
    notation_id: Uuid,
    provider: &dyn crate::signature::SignatureProvider,
    mode: SendMode,
) -> Result<(StateName, crate::signature::SignatureRequestId), WorkflowDriveError> {
    let notation_row = store::notations::find_by_id(ctx.application_store, notation_id)
        .await?
        .ok_or(WorkflowDriveError::TemplateMissing(notation_id))?;
    let current_state = ctx
        .runtime
        .current_state(workflows::MachineKind::Workflow, notation_id)
        .await;
    check_signature_readiness(ctx, &notation_row, current_state.as_ref(), mode).await?;
    if crate::signature_adoption::evidence(ctx.storage.as_ref(), notation_id)
        .await
        .map_err(|_| WorkflowDriveError::Db("reading envelope evidence failed".into()))?
        .is_some()
    {
        return Err(WorkflowDriveError::ApprovalNotReady {
            state: "externally sent envelope".into(),
        });
    }
    let current_state = current_state.ok_or(workflows::WorkflowRuntimeError::UnknownNotation(
        notation_id,
    ))?;
    let scoped = provider
        .for_project(ctx.application_store, notation_row.project_id)
        .await?;
    let provider = crate::firm_docusign::resolved(scoped.as_ref(), provider);
    let existing = store::signatures::request_id_for_notation(ctx.application_store, notation_id)
        .await?
        .map(crate::signature::SignatureRequestId);

    if mode == SendMode::Prepare {
        if let Some(id) = existing {
            return Ok((current_state, id));
        }
    } else if current_state.prefix() == "sent_for_signature" {
        // A prior send may have reached DocuSign before the HTTP response
        // was lost. Reconcile the recorded envelope by ID.
        let existing = existing.ok_or(WorkflowDriveError::DispatchNeedsReconciliation)?;
        let (id, firm, onboarding) =
            record_signature_draft(ctx, &notation_row, provider, Some(existing)).await?;
        provider
            .dispatch_notation_draft(&id, firm.as_ref(), onboarding)
            .await?;
        if notation_row.state == "lawyer_review" || notation_row.state.starts_with("generate_pdf") {
            store::notations::reconcile_state(
                ctx.application_store,
                notation_id,
                &notation_row.state,
                current_state.as_str(),
            )
            .await?;
        }
        return Ok((current_state, id));
    }

    if current_state.prefix() != "generate_pdf"
        || !document_pdf_ready(ctx.storage.as_ref(), notation_id).await?
    {
        return Err(WorkflowDriveError::DocumentNotReady(notation_id));
    }

    let (id, firm, onboarding) =
        record_signature_draft(ctx, &notation_row, provider, existing).await?;
    if mode == SendMode::Send {
        provider
            .verify_notation_draft(&id, firm.as_ref(), onboarding)
            .await?;
        let next = crate::retainer_walk::signal_workflow(
            ctx.runtime,
            notation_id,
            "pdf_persisted",
            None,
            ctx.acting,
        )
        .await?;
        crate::retainer_walk::sync_notation_state(
            ctx.application_store,
            notation_id,
            next.as_str(),
        )
        .await?;
        provider
            .dispatch_notation_draft(&id, firm.as_ref(), onboarding)
            .await?;
        let notation_id = notation_id.to_string();
        let project_id = notation_row.project_id.to_string();
        telemetry::record_funnel_event(telemetry::FunnelEvent::Sent {
            notation_id: &notation_id,
            project_id: &project_id,
            channel: telemetry::FunnelChannel::Signature,
        });
        return Ok((next, id));
    }

    Ok((current_state, id))
}

async fn check_signature_readiness(
    ctx: &SendFlowContext<'_>,
    notation: &store::notations::Notation,
    current_state: Option<&StateName>,
    mode: SendMode,
) -> Result<(), WorkflowDriveError> {
    let state = current_state.map_or(notation.state.as_str(), StateName::as_str);
    if mode == SendMode::Prepare && state == StateName::BEGIN {
        return Err(WorkflowDriveError::SignatureStepRequired {
            state: state.to_owned(),
            next_step:
                "Complete the questionnaire and approve the notation before preparing its envelope.",
        });
    }
    if notation.state == "lawyer_review" {
        match ctx
            .runtime
            .approval_progress(notation.id, notation.updated_at.timestamp_micros())
            .await?
        {
            Some(workflows::ApprovalProgress::Pending) => {
                return Err(WorkflowDriveError::DocumentNotReady(notation.id));
            }
            Some(workflows::ApprovalProgress::Failed) => {
                return Err(WorkflowDriveError::ApprovalRunFailed);
            }
            _ => {}
        }
    }
    if mode == SendMode::Prepare
        && (state == "lawyer_review" || state.starts_with("lawyer_review__"))
    {
        return Err(WorkflowDriveError::SignatureStepRequired {
            state: state.to_owned(),
            next_step: "Approve the notation before preparing its envelope.",
        });
    }
    Ok(())
}

async fn record_signature_draft(
    ctx: &SendFlowContext<'_>,
    notation_row: &store::notations::Notation,
    provider: &dyn crate::signature::SignatureProvider,
    existing: Option<crate::signature::SignatureRequestId>,
) -> Result<
    (
        crate::signature::SignatureRequestId,
        Option<SignatureRecipient>,
        bool,
    ),
    WorkflowDriveError,
> {
    let notation_id = notation_row.id;
    // Resolve the template + its clauses before advancing. The manifest's
    // recipients and placed signature fields are built from them below, and
    // the fee-clause gate reads them here: the walk parks at
    // `sent_for_signature__pending` once `pdf_persisted` fires, so a refusal
    // has to land before that transition rather than unwind it.
    let template_row =
        store::templates::find_by_id(ctx.application_store, notation_row.template_id)
            .await?
            .ok_or(WorkflowDriveError::TemplateMissing(notation_id))?;
    let raw_template_body =
        store::templates::body(ctx.application_store, ctx.storage, &template_row).await?;
    let clauses = store::notation_clauses::for_notation(ctx.application_store, notation_id)
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
    let (keys, answer_ctx) = crate::retainer_walk::build_answer_context(
        ctx.application_store,
        Some(ctx.storage),
        notation_id,
    )
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

    // Fire the signature seam. Recipients follow the template's declared
    // signer set in order.
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

    let set = signer_set(&template_row.code, &raw_template_body)?;
    let firm = resolve_firm_recipient(
        ctx.application_store,
        notation_row.project_id,
        provider,
        firm_routing_order(
            &set,
            signature_fields.is_empty(),
            signature_fields
                .iter()
                .any(|field| field.recipient_role == "firm"),
        ),
    )
    .await?;
    let onboarding = template_row.kind.as_deref() == Some("onboarding");
    if let Some(id) = existing {
        return Ok((id, firm, onboarding));
    }

    // Read the PDF the worker persisted back from storage so the bytes
    // sent are exactly the bytes stored (one renderer, no second
    // in-process copy to drift).
    let pdf_bytes = ctx
        .storage
        .get(&store::notations::document_pdf_storage_key(notation_id))
        .await?
        .bytes;
    let client = store::persons::find_by_id(ctx.application_store, notation_row.person_id).await?;
    // `emailed` delivery → non-captive client (DocuSign emails the signing
    // link); anything else (`embedded`, the default) keeps the captive
    // embedded-signing recipient. Read off the notation so the single send
    // path serves both without a second route.
    let captive = notation_row.delivery != store::notations::DELIVERY_EMAILED;
    let manifest = assemble_signature_manifest(
        notation_id,
        &signature_fields,
        &answer_ctx,
        client.as_ref(),
        captive,
        &set,
        firm.as_ref(),
    )?;
    let id = if onboarding {
        provider
            .prepare_onboarding(notation_id, &pdf_bytes, &manifest)
            .await?
    } else {
        provider
            .send_for_signature(notation_id, &pdf_bytes, &manifest, SendMode::Prepare)
            .await?
    };
    persist_signature_request_id(ctx.application_store, notation_id, &id.0).await?;
    Ok((id, firm, onboarding))
}

fn firm_routing_order(set: &rules::SignerSet, no_fields: bool, firm_field: bool) -> Option<u32> {
    if no_fields && !set.explicit {
        return Some(1);
    }
    set.signers
        .iter()
        .find(|signer| signer.role == "firm" && (set.explicit || firm_field))
        .map(|signer| signer.routing_order)
}

async fn resolve_firm_recipient(
    db: &store::application_store::ApplicationStore,
    project_id: Uuid,
    provider: &dyn SignatureProvider,
    order: Option<u32>,
) -> Result<Option<SignatureRecipient>, WorkflowDriveError> {
    let Some(order) = order else { return Ok(None) };
    let dri = store::projects::lawyer_dri_people(db, project_id)
        .await
        .map_err(|error| WorkflowDriveError::Db(error.to_string()))?
        .into_iter()
        .next();
    let configured = if dri.is_none() {
        let (name, email) = provider
            .configured_signer()
            .ok_or(SignatureError::FirmSigner)?;
        if name.trim().is_empty() {
            let person = store::persons::find_by_email_ci(db, email)
                .await?
                .filter(|person| person.role.is_lawyer_tier())
                .ok_or(SignatureError::FirmSigner)?;
            Some((person.name, person.email))
        } else {
            Some((name.to_owned(), email.to_owned()))
        }
    } else {
        None
    };
    let project = store::projects::find_by_id(db, project_id)
        .await
        .map_err(|error| WorkflowDriveError::Db(error.to_string()))?
        .ok_or(SignatureError::FirmSigner)?;
    let mut trade_names = Vec::new();
    if let Some(brand) = store::brands::find_by_key(db, &project.brand)
        .await
        .map_err(|error| WorkflowDriveError::Db(error.to_string()))?
    {
        trade_names.push(brand.name);
        trade_names.extend(brand.legal_entity);
    }
    if let Some(firm_id) = project.firm_id {
        if let Some(firm) = store::firms::find_by_id(db, firm_id)
            .await
            .map_err(|error| WorkflowDriveError::Db(error.to_string()))?
        {
            trade_names.push(firm.name);
            if let Some(entity_id) = firm.entity_id {
                let entity = store::entities::find_by_id(db, entity_id)
                    .await
                    .map_err(|error| WorkflowDriveError::Db(error.to_string()))?;
                trade_names.extend(entity.map(|entity| entity.name));
            }
        }
        for brand in store::brands::for_firm(db, firm_id)
            .await
            .map_err(|error| WorkflowDriveError::Db(error.to_string()))?
        {
            trade_names.push(brand.name);
            trade_names.extend(brand.legal_entity);
        }
    }
    choose_firm_recipient(dri.as_ref(), configured, &trade_names, order)
        .map(Some)
        .map_err(Into::into)
}

fn choose_firm_recipient(
    dri: Option<&store::persons::Person>,
    configured: Option<(String, String)>,
    trade_names: &[String],
    routing_order: u32,
) -> Result<SignatureRecipient, SignatureError> {
    let (name, email) = match dri {
        Some(person) if person.role.is_lawyer_tier() => (person.name.clone(), person.email.clone()),
        Some(_) => return Err(SignatureError::FirmSigner),
        None => configured.ok_or(SignatureError::FirmSigner)?,
    };
    let normalized = |value: &str| {
        value
            .chars()
            .filter(|character| character.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let name_key = normalized(&name);
    if name_key.is_empty()
        || name.contains('@')
        || name.chars().any(char::is_control)
        || trade_names
            .iter()
            .any(|trade_name| normalized(trade_name) == name_key)
        || !store::deployment::is_signer_mailbox(&email)
    {
        return Err(SignatureError::FirmSigner);
    }
    Ok(SignatureRecipient {
        role: "firm".into(),
        name,
        email,
        routing_order,
        client_user_id: None,
    })
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
/// questionnaire answers or the bound Person; `firm` from the Project's
/// lawyer DRI or configured signing lawyer; other roles from declared `name`/`email`
/// answer-key bindings, falling back to `person__<role>.name` /
/// `.email`. Templates with no declared signers and no fields route only to
/// the Firm signer. Explicit signers require placed fields
/// and resolved identities.
///
/// Resolve the signer set for a template from the bytes the send path
/// loads.
///
/// A template stored with its frontmatter — a Project-scoped Draft's
/// persisted composition — parses directly. A **seeded** template's stored
/// body carries no frontmatter (`seed_templates` splits it off), so the
/// declarations recover from the bundled source the seed canonically
/// mirrors — the same recovery shape
/// `workflows::notation_session::questionnaire_definition_for` applies to
/// a seeded questionnaire. Without the recovery, an explicit signer set
/// would silently collapse into the implicit client/firm default at send
/// time for every seeded template that declares one.
///
/// `code` is the template's catalog code (`template_row.code`); only
/// frontmatter-less sources consult the bundled seed, so a Project-scoped
/// body that deliberately drops a `signers:` block keeps its (implicit)
/// choice.
pub(crate) fn signer_set_for_template(
    code: &str,
    stored_source: &str,
) -> Result<rules::SignerSet, String> {
    if rules::frontmatter::extract(stored_source).is_some() {
        return rules::signer_set(stored_source);
    }
    match store::seed::seeded_template_markdown(code) {
        Some(bundled) => rules::signer_set(bundled),
        None => rules::signer_set(stored_source),
    }
}

fn signer_set(code: &str, source: &str) -> Result<rules::SignerSet, WorkflowDriveError> {
    signer_set_for_template(code, source)
        .map_err(|message| WorkflowDriveError::UnresolvedSigner { role: message })
}

#[cfg(test)]
pub(crate) fn build_signature_manifest(
    notation_id: Uuid,
    fields: &[crate::signature::SignatureField],
    ctx: &BTreeMap<String, String>,
    client: Option<&store::persons::Person>,
    captive: bool,
    template_code: &str,
    template_source: &str,
) -> Result<crate::signature::SignatureManifest, WorkflowDriveError> {
    let set = signer_set(template_code, template_source)?;
    let firm = set
        .signers
        .iter()
        .find(|signer| signer.role == "firm")
        .map(|signer| SignatureRecipient {
            role: "firm".into(),
            name: "Example Signing Lawyer".into(),
            email: "firm-signer@example.com".into(),
            routing_order: signer.routing_order,
            client_user_id: None,
        });
    assemble_signature_manifest(
        notation_id,
        fields,
        ctx,
        client,
        captive,
        &set,
        firm.as_ref(),
    )
}

fn assemble_signature_manifest(
    notation_id: Uuid,
    fields: &[crate::signature::SignatureField],
    ctx: &BTreeMap<String, String>,
    client: Option<&store::persons::Person>,
    captive: bool,
    set: &rules::SignerSet,
    firm: Option<&SignatureRecipient>,
) -> Result<crate::signature::SignatureManifest, WorkflowDriveError> {
    use crate::signature::{SignatureManifest, SignatureRecipient};
    if fields.is_empty() && !set.explicit {
        return Ok(SignatureManifest {
            recipients: firm.cloned().into_iter().collect(),
            fields: Vec::new(),
        });
    }
    let role_present = |role: &str| fields.iter().any(|f| f.recipient_role == role);
    let answered = |key: &str| signer_answer(ctx, key);

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
            "firm" => {
                recipients.push(firm.cloned().ok_or(SignatureError::FirmSigner)?);
                continue;
            }
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

/// Resolve a scalar binding or a field in an ordered People answer.
fn signer_answer(ctx: &BTreeMap<String, String>, key: &str) -> Option<String> {
    if let Some(value) = ctx.get(key) {
        return (!value.trim().is_empty()).then(|| value.trim().to_owned());
    }
    let (state, row, field) = rules::f107::people_signer_binding(key)?;
    let rows: serde_json::Value = serde_json::from_str(ctx.get(state)?).ok()?;
    let value = rows.get(row)?.get(field)?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Record the e-signature provider's request id in `signatures` so a
/// later completion webhook can resolve its callback by `(provider,
/// provider_id)`. See [`crate::esignature_webhook`].
async fn persist_signature_request_id(
    application_store: &store::application_store::ApplicationStore,
    notation_id: Uuid,
    request_id: &str,
) -> Result<(), store::signatures::SignatureError> {
    store::signatures::record_request(
        application_store,
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
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
            build_signature_manifest(Uuid::now_v7(), &fields, &ctx, Some(&client), true, "", "")
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
            build_signature_manifest(Uuid::now_v7(), &fields, &ctx, Some(&client), true, "", "")
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
            "",
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
            "",
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
            "",
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

        let manifest = build_signature_manifest(
            Uuid::now_v7(),
            &fields,
            &ctx,
            Some(&client),
            true,
            "",
            template,
        )
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

    fn mutual_nda_answers() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("entity__company.name".into(), "Example Research LLC".into()),
            ("entity__other_party.name".into(), "Jordan Example".into()),
            (
                "custom_datetime__effective_date".into(),
                "2026-01-01".into(),
            ),
            (
                "custom_text__business_purpose".into(),
                "a software prototype".into(),
            ),
            ("custom_text__business_term".into(), "one year".into()),
            (
                "custom_text__business_survival".into(),
                "three years".into(),
            ),
            ("jurisdiction__law".into(), "Nevada".into()),
            ("jurisdiction__seat".into(), "Reno, Nevada".into()),
            (
                "people__notices".into(),
                serde_json::json!([
                    {"name": "Avery Example", "email": "avery@example.com"},
                    {"name": "Jordan Example", "email": "jordan@example.com"},
                ])
                .to_string(),
            ),
            (
                "people__signatories".into(),
                serde_json::json!([
                    {"name": "Avery Example", "email": "avery@example.com"},
                    {"name": "Jordan Example", "email": "jordan@example.com"},
                ])
                .to_string(),
            ),
        ])
    }

    #[tokio::test]
    async fn shared_mutual_nda_prepares_two_signers_without_dispatch() {
        let template =
            store::seed::seeded_template_markdown("mutual_nda").expect("shared template");
        let composed = rules::clauses::compile(template).expect("shared clauses resolve");
        let answers = mutual_nda_answers();
        let typst = crate::retainer_walk::typst_body_from_template(
            &composed,
            &answers,
            &answers,
            Some("agreement"),
        );
        let (typst, fields) = crate::signature_render::expand_signatures(&typst);
        assert!(!typst.contains("{{"), "all NDA placeholders are filled");
        let pdf = pdf::render(&typst).expect("the NDA renders");
        let manifest = build_signature_manifest(
            Uuid::now_v7(),
            &fields,
            &answers,
            None,
            true,
            "mutual_nda",
            "",
        )
        .expect("both named signers resolve without Project participation");

        assert!(build_signature_manifest(
            Uuid::now_v7(),
            &fields,
            &BTreeMap::new(),
            None,
            true,
            "mutual_nda",
            "",
        )
        .is_err());
        assert!(build_signature_manifest(
            Uuid::now_v7(),
            &[],
            &answers,
            None,
            true,
            "mutual_nda",
            "",
        )
        .is_err());
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v2.1/accounts/example/envelopes"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "envelopeId": "draft-nda",
                "status": "created"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let provider = crate::signature::DocuSignSignatureProvider::new(
            server.uri(),
            "example",
            "test-token",
            "sender@example.com",
            "Sender",
        );
        let id = crate::signature::SignatureProvider::send_for_signature(
            &provider,
            Uuid::now_v7(),
            &pdf,
            &manifest,
            super::SendMode::Prepare,
        )
        .await
        .expect("DocuSign draft prepares");
        assert_eq!(id.0, "draft-nda");
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(body["status"], "created");
        let signers = body["recipients"]["signers"].as_array().unwrap();
        assert_eq!(signers.len(), 2);
        for (email, signer) in ["avery@example.com", "jordan@example.com"]
            .into_iter()
            .zip(signers)
        {
            assert_eq!(signer["email"], email);
            assert!(signer["clientUserId"].is_null());
            assert_eq!(signer["tabs"]["signHereTabs"].as_array().unwrap().len(), 1);
            assert_eq!(
                signer["tabs"]["dateSignedTabs"].as_array().unwrap().len(),
                1
            );
        }
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
            build_signature_manifest(Uuid::now_v7(), &fields, &ctx, Some(&client), false, "", "")
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
    fn prepare_mode_creates_a_distinct_envelope_kind() {
        // A prepare is not a send: the flow treats the modes differently
        // (prepare never fires `pdf_persisted`), so the values must stay
        // distinct and the mode must survive the provider call boundary.
        assert_ne!(SendMode::Prepare, SendMode::Send);
    }
}
