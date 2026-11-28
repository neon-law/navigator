use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::{
    firm_capability::FirmCapability,
    plan_claims::{self, Change, ClaimError},
};
use uuid::Uuid;

use super::{legal_plans_api, participates_as_firm, ApiError, ApiState, LawyerSession};

async fn scope(
    state: &ApiState,
    lawyer: &LawyerSession,
    project: Uuid,
) -> Result<(store::projects::Project, Uuid), ApiError> {
    if !participates_as_firm(state, lawyer, project).await {
        return Err(ApiError::NotFound);
    }
    legal_plans_api::firm_project(state, &lawyer.0, project).await
}

fn response<T: serde::Serialize>(result: Result<T, ClaimError>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => {
            let status = match error {
                ClaimError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
                ClaimError::NotFound => StatusCode::NOT_FOUND,
                ClaimError::Duplicate => StatusCode::CONFLICT,
                ClaimError::Database(_) | ClaimError::Deadline(_) => {
                    StatusCode::INTERNAL_SERVER_ERROR
                }
            };
            (
                status,
                Json(serde_json::json!({"error": error.to_string()})),
            )
                .into_response()
        }
    }
}

pub(super) async fn list(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(project): Path<Uuid>,
) -> Result<Response, ApiError> {
    let (_, firm) = scope(&state, &lawyer, project).await?;
    let pg = match legal_plans_api::plan_store().await {
        Ok(pg) => pg,
        Err(unavailable) => return Ok(unavailable),
    };
    Ok(response(plan_claims::list(&pg, project, firm).await))
}

pub(super) async fn record(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path(project): Path<Uuid>,
    Json(change): Json<Change>,
) -> Result<Response, ApiError> {
    let (matter, firm) = scope(&state, &lawyer, project).await?;
    let timezone = store::projects::calendar_timezone(&state.application_store, &matter)
        .await
        .map_err(|_| ApiError::Db("read the Project calendar timezone".to_owned()))?;
    let actor = lawyer.0.person_id.ok_or(ApiError::Forbidden)?;
    let pg = match legal_plans_api::plan_store().await {
        Ok(pg) => pg,
        Err(unavailable) => return Ok(unavailable),
    };
    Ok(response(
        plan_claims::record(&pg, project, firm, actor, timezone, change).await,
    ))
}

#[derive(Debug, Deserialize)]
pub(super) struct WorklistQuery {
    #[serde(default)]
    totals_only: bool,
}

pub(super) async fn worklist(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Query(query): Query<WorklistQuery>,
) -> Result<Response, ApiError> {
    let projects = if query.totals_only {
        legal_plans_api::authorize(&state, &session, firm, FirmCapability::ViewPlanMetrics).await?;
        None
    } else {
        legal_plans_api::authorize(&state, &session, firm, FirmCapability::ViewDirectory).await?;
        let projects = store::access::visible_projects_as_lawyer(
            &state.application_store,
            session.person_id,
            session.role,
        )
        .await
        .map_err(ApiError::Db)?;
        Some(
            projects
                .into_iter()
                .filter(|project| project.firm_id == Some(firm))
                .map(|project| project.id)
                .collect::<Vec<_>>(),
        )
    };
    let pg = match legal_plans_api::plan_store().await {
        Ok(pg) => pg,
        Err(unavailable) => return Ok(unavailable),
    };
    Ok(response(
        plan_claims::worklist(&pg, firm, projects.as_deref()).await,
    ))
}

#[cfg(test)]
#[path = "plan_claims_api/tests.rs"]
mod tests;

fn dollars(cents: Option<i64>) -> String {
    cents.map_or_else(
        || "Not verified".to_owned(),
        |cents| format!("${}.{:02}", cents / 100, (cents % 100).abs()),
    )
}

fn packet_params(packet: &store::claim_packets::ClaimPacket) -> pdf::ClaimPacketParams {
    use pdf::{PacketInvoice, PacketPage};
    use store::legal_plans::ClaimProof;
    let optional = |value: &Option<String>| value.clone().unwrap_or_else(|| "Not recorded".into());
    let dates = if packet.first_service_on == packet.last_service_on {
        packet.first_service_on.to_string()
    } else {
        format!("{} to {}", packet.first_service_on, packet.last_service_on)
    };
    let mut lead = Vec::new();
    if let Some(file_with) = &packet.file_with {
        lead.push(format!("File with: {file_with}"));
    }
    if let Some(filer) = &packet.filer {
        lead.push(format!("Filed by: {filer}"));
    }
    if let Some(subject) = &packet.subject {
        lead.push(format!("Subject: {subject}"));
    }
    if let Some(invoice_to) = packet
        .filing
        .as_ref()
        .and_then(|filing| filing.invoice_to.as_ref())
    {
        lead.push(format!("Send the itemized invoice to: {invoice_to}"));
    }
    let proof_name = |proof: &ClaimProof| match proof {
        ClaimProof::EngagementLetter => "Signed engagement letter",
        ClaimProof::FiledDocument => "Filed document",
    };
    let mut proof_lead = packet
        .proofs
        .iter()
        .map(|proof| format!("{}: {}", proof_name(&proof.proof), proof.filename))
        .collect::<Vec<_>>();
    proof_lead.extend(
        packet
            .missing_proof
            .iter()
            .map(|proof| format!("{}: not filed on the Project", proof_name(proof))),
    );
    let billed_by = packet
        .billing_entity
        .clone()
        .unwrap_or_else(|| packet.firm_name.clone());
    pdf::ClaimPacketParams {
        claim_form: PacketPage {
            title: packet.form_title.clone(),
            lead,
            rows: vec![
                ("Plan".into(), packet.plan_name.clone()),
                ("Plan type".into(), optional(&packet.plan_type)),
                ("Plan Case Number".into(), packet.plan_case_number.clone()),
                ("Member ID".into(), optional(&packet.member_id)),
                ("Member".into(), optional(&packet.member_name)),
                ("Service".into(), packet.service.to_owned()),
                ("Benefit".into(), packet.benefit.clone()),
                ("Service dates".into(), dates.clone()),
                ("Attorney".into(), optional(&packet.responsible_lawyer)),
                ("Firm".into(), packet.firm_name.clone()),
                (
                    "Attorney minutes".into(),
                    packet
                        .attorney_minutes
                        .map_or_else(|| "Not recorded".into(), |minutes| minutes.to_string()),
                ),
                (
                    "Amount claimed".into(),
                    dollars(packet.claimed_cents.map(i64::from).or(packet.covered_cents)),
                ),
                ("Claim".into(), packet.claim_id.to_string()),
            ],
        },
        invoice: PacketInvoice {
            page: PacketPage {
                title: "Itemized invoice".into(),
                lead: Vec::new(),
                rows: vec![
                    ("Billed by".into(), billed_by),
                    (
                        "Matter".into(),
                        format!("{} ({})", packet.matter, packet.matter_code),
                    ),
                    ("Plan Case Number".into(), packet.plan_case_number.clone()),
                    ("Service dates".into(), dates),
                    (
                        "Fee terms".into(),
                        match (packet.rate_cents_per_hour, packet.fixed_fee_cents) {
                            (Some(rate), _) => {
                                format!("{} per hour", dollars(Some(i64::from(rate))))
                            }
                            (None, Some(fixed)) => {
                                format!("Fixed fee {}", dollars(Some(i64::from(fixed))))
                            }
                            (None, None) => "Not verified".into(),
                        },
                    ),
                ],
            },
            lines: packet
                .lines
                .iter()
                .map(|line| {
                    [
                        line.performed_on.to_string(),
                        line.timekeeper.clone(),
                        line.minutes.to_string(),
                        line.narrative.clone(),
                        line.amount_cents
                            .map_or_else(|| "Included".into(), |cents| dollars(Some(cents))),
                    ]
                })
                .collect(),
            totals: vec![
                ("Total fees".into(), dollars(packet.total_cents)),
                ("Covered by the plan".into(), dollars(packet.covered_cents)),
                ("Member share".into(), dollars(packet.member_share_cents)),
            ],
        },
        proof: PacketPage {
            title: "Proof".into(),
            lead: if proof_lead.is_empty() {
                vec!["The plan asks for no proof.".into()]
            } else {
                proof_lead
            },
            rows: Vec::new(),
        },
    }
}

fn packet_refusal(message: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(serde_json::json!({ "error": message })),
    )
        .into_response()
}

/// `POST .../legal-plan-claims/{claim_id}/packet` — render the claim form,
/// itemized invoice, and proof as one PDF, file it on the Project as an
/// internal document, and link it to the claim. Filing the same packet
/// again changes nothing.
pub(super) async fn packet(
    State(state): State<ApiState>,
    lawyer: LawyerSession,
    Path((project, claim)): Path<(Uuid, Uuid)>,
) -> Result<Response, ApiError> {
    use base64::Engine as _;
    if lawyer.0.viewing_as_dri.is_some() {
        return Err(ApiError::Forbidden);
    }
    let (matter, firm) = scope(&state, &lawyer, project).await?;
    let actor = lawyer.0.person_id.ok_or(ApiError::Forbidden)?;
    let timezone = store::projects::calendar_timezone(&state.application_store, &matter)
        .await
        .map_err(|_| ApiError::Db("read the Project calendar timezone".to_owned()))?;
    let pg = match legal_plans_api::plan_store().await {
        Ok(pg) => pg,
        Err(unavailable) => return Ok(unavailable),
    };
    let data = match store::claim_packets::read(&pg, project, firm, claim).await {
        Ok(data) => data,
        Err(error) => return Ok(response::<()>(Err(error))),
    };
    if matches!(data.claim_stage, "approved" | "denied") {
        return Ok(packet_refusal("the claim is decided; no packet is needed"));
    }
    let mut proofs = Vec::with_capacity(data.proofs.len());
    for proof in &data.proofs {
        match state.storage.get(&proof.storage_key).await {
            Ok(object) => proofs.push(object.bytes),
            Err(error) => {
                tracing::warn!(%project, %claim, asset = %proof.asset_id, %error, "claim packet proof unreadable");
                return Ok(packet_refusal(
                    "a proof document's bytes are unavailable; repair it before filing the packet",
                ));
            }
        }
    }
    let params = packet_params(&data);
    let rendered =
        tokio::task::spawn_blocking(move || pdf::render_claim_packet(&params, &proofs)).await;
    let bytes = match rendered {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => {
            tracing::warn!(%project, %claim, %error, "claim packet render failed");
            return Ok(packet_refusal("the claim packet could not be rendered"));
        }
        Err(error) => return Err(ApiError::Db(error.to_string())),
    };
    let filename = format!(
        "{}_legal-plan-claim-{}.pdf",
        chrono::Utc::now().date_naive().format("%Y.%m.%d"),
        &claim.simple().to_string()[..8]
    );
    let description = format!("Legal Plan Claim packet for claim {claim}");
    let filed = store::documents::ingest_bytes_exactly_once(
        &state.application_store,
        &state.storage,
        &store::documents::IngestArgs {
            project_id: project,
            source: store::documents::source::GENERATED,
            filename: &filename,
            kind: "invoice",
            content_type: "application/pdf",
            description: Some(&description),
            visibility: store::documents::visibility::INTERNAL,
            secondary_storage_key: None,
        },
        &bytes,
    )
    .await
    .map_err(|error| ApiError::Db(error.to_string()))?;
    let claim_row = match plan_claims::record_packet(
        &pg,
        project,
        firm,
        actor,
        claim,
        (filed.asset_id, &filed.sha256_hex),
        timezone,
    )
    .await
    {
        Ok(claim_row) => claim_row,
        Err(error) => return Ok(response::<()>(Err(error))),
    };
    Ok(Json(serde_json::json!({
        "claim": claim_row,
        "asset_id": filed.asset_id,
        "filename": filename,
        "sha256": filed.sha256_hex,
        "file_with": data.file_with,
        "filer": data.filer,
        "subject": data.subject,
        "invoice_to": data.filing.as_ref().and_then(|filing| filing.invoice_to.clone()),
        "proofs": data.proofs,
        "missing_proof": data.missing_proof,
        "pdf_base64": base64::engine::general_purpose::STANDARD.encode(&bytes),
    }))
    .into_response())
}
