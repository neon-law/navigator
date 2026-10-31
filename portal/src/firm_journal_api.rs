//! Firm capability-gated accounting commands. Provider evidence is read-only.
use super::{ApiError, ApiState, LawyerSession};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use store::firm_journal::{
    self as journal, JournalEntry, ReconciliationRequest, SourceReference, SourceSnapshot,
    SourceStatus, Unit, Valuation,
};
use uuid::Uuid;

async fn authorize(
    state: &ApiState,
    session: &crate::SessionData,
    firm: Uuid,
) -> Result<(), ApiError> {
    let allowed = store::firm_capability::FirmCapability::ManageJournal
        .resolve(&state.surreal, session.role, session.person_id, firm)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?;
    if !allowed.is_allowed() {
        return Err(ApiError::NotFound);
    }
    Ok(())
}
/// The configured journal store. Without a PostgreSQL host this is SurrealDB;
/// with one, an unreachable PostgreSQL fails the request rather than splitting
/// the journal across stores.
async fn journal_store(state: &ApiState) -> Result<journal::JournalStore, ApiError> {
    journal::JournalStore::from_env(&state.surreal)
        .await
        .map_err(|_| ApiError::Db("PostgreSQL journal unavailable".into()))
}
fn journal_error(error: &journal::JournalError) -> Response {
    if matches!(error, journal::JournalError::RequiresPostgres) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"journal_requires_postgresql"})),
        )
            .into_response();
    }
    let status = match error {
        journal::JournalError::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
        journal::JournalError::Conflict => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (
        status,
        Json(serde_json::json!({"error":"journal_command_refused"})),
    )
        .into_response()
}
pub(super) async fn list(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    Ok(match journal_store(&state).await?.entries(firm).await {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => journal_error(&e),
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StatementQuery {
    project_id: Option<Uuid>,
}
pub(super) async fn statement(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Query(query): Query<StatementQuery>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    if let Some(project) = query.project_id {
        let project = store::projects::find_by_id(&state.surreal, project)
            .await
            .map_err(|e| ApiError::Db(e.to_string()))?
            .ok_or(ApiError::NotFound)?;
        if project.firm_id != Some(firm) {
            return Err(ApiError::NotFound);
        }
    }
    let store = journal_store(&state).await?;
    let result = async {
        let entries = store.entries(firm).await?;
        Ok::<_, journal::JournalError>((
            journal::statement(&entries, query.project_id)?,
            journal::cost_positions(&entries, query.project_id)?,
        ))
    }
    .await;
    Ok(match result {
        Ok((lines, costs)) => Json(serde_json::json!({
            "firm_id":firm,"project_id":query.project_id,"lines":lines,"costs":costs,
            "complete_account_reconciliation":false
        }))
        .into_response(),
        Err(e) => journal_error(&e),
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuditQuery {
    job_id: Option<Uuid>,
}
pub(super) async fn audits(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Query(query): Query<AuditQuery>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    let store = journal_store(&state).await?;
    let audits = match store.postgres() {
        Ok(pg) => journal::audits(pg, firm, query.job_id).await,
        Err(error) => Err(error),
    };
    Ok(match audits {
        Ok(audits) => Json(audits).into_response(),
        Err(error) => journal_error(&error),
    })
}
pub(super) async fn migration_triage(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    Ok(
        match store::journal_startup_migration::triage(&state.surreal, firm).await {
            Ok(rows) => Json(rows).into_response(),
            Err(error) => journal_error(&error),
        },
    )
}
pub(super) async fn post(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Json(entry): Json<JournalEntry>,
) -> Result<Response, ApiError> {
    apply_command(state, session, firm, entry, false).await
}
pub(super) async fn preview(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Json(entry): Json<JournalEntry>,
) -> Result<Response, ApiError> {
    apply_command(state, session, firm, entry, true).await
}
async fn apply_command(
    state: ApiState,
    session: crate::SessionData,
    firm: Uuid,
    entry: JournalEntry,
    preview: bool,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    if entry.firm_id != firm || Some(entry.actor_id) != session.person_id {
        return Err(ApiError::Forbidden);
    }
    let store = journal_store(&state).await?;
    let result = if preview {
        store.preview(&state.surreal, &entry).await
    } else {
        store.post(&state.surreal, &entry).await
    };
    Ok(match result {
        Ok(id) => (
            if preview {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            },
            Json(serde_json::json!({"id":id,"preview":preview})),
        )
            .into_response(),
        Err(error) => journal_error(&error),
    })
}

pub(super) async fn reconcile(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Json(request): Json<ReconciliationRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    if request.scope.firm_id != firm
        || request
            .approvals
            .iter()
            .any(|a| Some(a.actor_id) != session.person_id)
        || request
            .proposed_entries
            .iter()
            .any(|e| Some(e.actor_id) != session.person_id)
    {
        return Err(ApiError::Forbidden);
    }
    let store = journal_store(&state).await?;
    let audit = match store.postgres() {
        Ok(pg) => journal::reconcile(&state.surreal, pg, &request).await,
        Err(error) => Err(error),
    };
    Ok(match audit {
        Ok(audit) => Json(audit).into_response(),
        Err(e) => journal_error(&e),
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MercuryRequest {
    project_id: Uuid,
    account_id: Uuid,
    transaction_id: Uuid,
    journal_account: Option<String>,
}
pub(super) async fn mercury(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Json(request): Json<MercuryRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    let project = store::projects::find_by_id(&state.surreal, request.project_id)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?
        .ok_or(ApiError::NotFound)?;
    if project.firm_id != Some(firm) {
        return Err(ApiError::NotFound);
    }
    let kms = crate::integrations::runtime_kms_from_env().await;
    match mercury_snapshot(
        &state.surreal,
        &state.storage,
        kms.as_ref(),
        request.project_id,
        request.account_id,
        request.transaction_id,
    )
    .await
    {
        Ok(mut snapshot) => {
            snapshot.journal_account = request.journal_account;
            Ok(Json(snapshot).into_response())
        }
        Err(_) => Ok((
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error":"bank_evidence_unavailable"})),
        )
            .into_response()),
    }
}
/// Download evidence into private Documents. Refresh expired signed attachment
/// URLs by rereading only the same transaction; never enumerate the account.
pub async fn mercury_snapshot(
    db: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    kms: &dyn cloud::RuntimeKms,
    project: Uuid,
    account: Uuid,
    id: Uuid,
) -> anyhow::Result<SourceSnapshot> {
    use store::firm_secrets::{
        resolve_project_credential, IntegrationProvider, IntegrationSecretKind,
    };
    let credential = resolve_project_credential(
        db,
        project,
        IntegrationProvider::Mercury,
        IntegrationSecretKind::MercuryReadToken,
        kms,
    )
    .await?;
    let client = cloud::mercury::Mercury::new(credential.expose_for_provider_call())?;
    let (transaction, bytes) = client.transaction(account, id).await?;
    let digest = store::documents::sha256_hex(&bytes);
    let evidence_name = format!("accounting-source-{digest}.json");
    let evidence = private_evidence(
        db,
        storage,
        project,
        &evidence_name,
        "application/json",
        &bytes,
    )
    .await?;
    let mut receipts = Vec::new();
    for attachment in &transaction.attachments {
        let bytes = if let Ok(bytes) = client.receipt(attachment).await {
            bytes
        } else {
            let (refreshed, _) = client.transaction(account, id).await?;
            if refreshed.amount.get() != transaction.amount.get()
                || refreshed.status != transaction.status
            {
                anyhow::bail!("transaction changed while refreshing expired receipt");
            }
            let matches: Vec<_> = refreshed
                .attachments
                .iter()
                .filter(|candidate| {
                    candidate.file_name == attachment.file_name
                        && candidate.attachment_type == attachment.attachment_type
                })
                .collect();
            if matches.len() != 1 {
                anyhow::bail!("receipt identity is unavailable or ambiguous");
            }
            client.receipt(matches[0]).await?
        };
        let receipt_digest = store::documents::sha256_hex(&bytes);
        let name = format!("accounting-receipt-{receipt_digest}");
        receipts.push(
            private_evidence(
                db,
                storage,
                project,
                &name,
                "application/octet-stream",
                &bytes,
            )
            .await?,
        );
    }
    let status = match transaction.status.as_str() {
        "sent" => SourceStatus::Completed,
        "pending" => SourceStatus::Pending,
        "reversed" => SourceStatus::Reversed,
        "cancelled" | "failed" | "blocked" => SourceStatus::Failed,
        _ => return Err(anyhow::anyhow!("unrecognized source status")),
    };
    let mut snapshot = SourceSnapshot {
        occurred_at: transaction.posted_at.clone(),
        journal_account: None,
        adapter_version: "mercury-account-transaction-v1".into(),
        facts: journal::SourceFacts::Manual,
        source: SourceReference {
            provider: "mercury".into(),
            scope: store::projects::find_by_id(db, project)
                .await?
                .and_then(|p| p.firm_id)
                .ok_or_else(|| anyhow::anyhow!("Project has no Firm"))?
                .to_string(),
            account: account.to_string(),
            object_type: "transaction".into(),
            external_id: id.to_string(),
            locator: None,
        },
        revision: digest.clone(),
        retrieved_at: chrono::Utc::now().to_rfc3339(),
        digest,
        unit: Unit::fiat("USD", 2),
        quantity: journal::parse_decimal(transaction.amount.get(), 2)?,
        status,
        receipt_document_ids: receipts,
        evidence_document_ids: vec![evidence],
        valuation: Valuation::Unknown,
    };
    snapshot.revision = journal::source_revision(&snapshot)?;
    Ok(snapshot)
}
async fn private_evidence(
    db: &store::surreal::SurrealDb,
    storage: &std::sync::Arc<dyn cloud::StorageService>,
    project: Uuid,
    name: &str,
    content_type: &str,
    bytes: &[u8],
) -> anyhow::Result<Uuid> {
    Ok(store::documents::ingest_bytes_exactly_once(
        db,
        storage,
        &store::documents::IngestArgs {
            project_id: project,
            source: "upload",
            filename: name,
            kind: "unclassified",
            content_type,
            description: Some("Accounting source evidence"),
            visibility: "internal",
            secondary_storage_key: None,
        },
        bytes,
    )
    .await?
    .asset_id)
}

/// The firm-level cap a single bulk import may touch. A window past it is
/// refused rather than partially imported; narrow the window.
const MERCURY_IMPORT_CAP: usize = 5_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MercuryImportRequest {
    account_id: Uuid,
    /// Inclusive posted-date window start.
    start: chrono::NaiveDate,
    /// Inclusive posted-date window end.
    end: chrono::NaiveDate,
    dry_run: bool,
}

/// One transaction's disposition in a bulk import.
#[derive(serde::Serialize)]
struct MercuryImportOutcome {
    transaction_id: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_id: Option<Uuid>,
    /// `planned` (dry run), `imported`, or `skipped`.
    disposition: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// Everything one import transaction needs, grouped so the per-transaction
/// step stays readable.
struct MercuryImportContext<'a> {
    surreal: &'a store::surreal::SurrealDb,
    storage: &'a std::sync::Arc<dyn cloud::StorageService>,
    pg: &'a store::postgres::PostgresDb,
    /// The Firm's Projects as `(code, id)`, sorted by code, so allocation is
    /// deterministic and first-match wins on an exact duplicate.
    projects: &'a [(String, Uuid)],
    client: &'a cloud::mercury::Mercury,
    actor_id: Uuid,
    dry_run: bool,
}

/// Bulk-import one bounded posted-date window: list, allocate each
/// transaction to at most one Firm Project by code token in its free text,
/// and create one confirmed, receipt-backed Project cost per allocated
/// transaction. Dry runs list and allocate but file no evidence and create
/// no rows. Every created cost carries the transaction's Mercury source
/// reference, so a rerun skips instead of double-charging.
pub(super) async fn mercury_import(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Json(request): Json<MercuryImportRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    if request.end < request.start {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({"error":"window_end_precedes_start"})),
        )
            .into_response());
    }
    let refused = |error: &'static str| {
        (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": error })),
        )
            .into_response()
    };
    let kms = crate::integrations::runtime_kms_from_env().await;
    let credential = store::firm_secrets::resolve_firm_credential(
        &state.surreal,
        firm,
        store::firm_secrets::IntegrationProvider::Mercury,
        store::firm_secrets::IntegrationSecretKind::MercuryReadToken,
        kms.as_ref(),
    )
    .await;
    let Ok(credential) = credential else {
        return Ok(refused("bank_credential_unavailable"));
    };
    let Ok(client) = cloud::mercury::Mercury::new(credential.expose_for_provider_call()) else {
        return Ok(refused("bank_credential_unavailable"));
    };
    let window = match client
        .transactions_window(
            request.account_id,
            request.start,
            request.end,
            MERCURY_IMPORT_CAP,
        )
        .await
    {
        Ok(window) => window,
        Err(cloud::mercury::MercuryError::Overflow) => {
            return Ok((
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(serde_json::json!({"error":"window_exceeds_import_cap"})),
            )
                .into_response());
        }
        Err(_) => return Ok(refused("bank_evidence_unavailable")),
    };
    let mut projects: Vec<(String, Uuid)> = store::projects::all(&state.surreal)
        .await
        .map_err(|error| ApiError::Db(error.to_string()))?
        .into_iter()
        .filter(|project| project.firm_id == Some(firm))
        .map(|project| (project.code, project.id))
        .collect();
    projects.sort();
    let pg = store::postgres::PostgresDb::from_env()
        .await
        .map_err(|_| ApiError::Db("PostgreSQL cost store unavailable".to_string()))?;
    let context = MercuryImportContext {
        surreal: &state.surreal,
        storage: &state.storage,
        pg: &pg,
        projects: &projects,
        client: &client,
        actor_id: session.person_id.ok_or(ApiError::Forbidden)?,
        dry_run: request.dry_run,
    };
    let mut outcomes = Vec::with_capacity(window.len());
    for transaction in &window {
        outcomes.push(import_one(&context, transaction).await);
    }
    let imported = outcomes
        .iter()
        .filter(|outcome| outcome.disposition == "imported")
        .count();
    let skipped = outcomes
        .iter()
        .filter(|outcome| outcome.disposition == "skipped")
        .count();
    Ok(Json(serde_json::json!({
        "account_id": request.account_id,
        "start": request.start,
        "end": request.end,
        "dry_run": request.dry_run,
        "total_transactions": window.len(),
        "imported": imported,
        "skipped": skipped,
        "outcomes": outcomes,
    }))
    .into_response())
}

/// Allocate, evidence, and create the cost for one listed transaction.
#[expect(
    clippy::too_many_lines,
    reason = "one transaction's eligibility, allocation, evidence, and creation stay together"
)]
async fn import_one(
    context: &MercuryImportContext<'_>,
    transaction: &cloud::mercury::Transaction,
) -> MercuryImportOutcome {
    use store::project_costs as costs;
    let mut outcome = MercuryImportOutcome {
        transaction_id: transaction.id,
        project_code: None,
        cost_id: None,
        disposition: "skipped".into(),
        reason: None,
    };
    let skip = |outcome: &mut MercuryImportOutcome, reason: &str| {
        outcome.reason = Some(reason.to_string());
    };
    let Some(posted_at) = transaction.posted_at.as_deref() else {
        skip(&mut outcome, "unposted");
        return outcome;
    };
    let Some(occurred_on) =
        chrono::NaiveDate::parse_from_str(&posted_at[..posted_at.len().min(10)], "%Y-%m-%d").ok()
    else {
        skip(&mut outcome, "unreadable_posted_at");
        return outcome;
    };
    if transaction.status != "sent" {
        skip(&mut outcome, "status");
        return outcome;
    }
    let Ok(amount_cents) = costs::cents_from_exact_decimal(transaction.amount.get()) else {
        skip(&mut outcome, "unreadable_amount");
        return outcome;
    };
    if amount_cents <= 0 {
        skip(&mut outcome, "non_positive_amount");
        return outcome;
    }
    let text = [
        transaction.counterparty_name.as_deref(),
        transaction.source_description.as_deref(),
        transaction.note.as_deref(),
        transaction.category.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ");
    let matching: Vec<&(String, Uuid)> = context
        .projects
        .iter()
        .filter(|(code, _)| costs::project_code_token_match(code, &text))
        .collect();
    let (code, project_id) = match matching.len() {
        0 => {
            skip(&mut outcome, "unallocated");
            return outcome;
        }
        1 => matching[0].clone(),
        _ => {
            skip(&mut outcome, "ambiguous");
            return outcome;
        }
    };
    outcome.project_code = Some(code);
    let source_reference = costs::mercury_source_reference(transaction.id);
    if let Some(existing) =
        costs::find_by_source_reference(context.pg, project_id, &source_reference)
            .await
            .ok()
            .flatten()
    {
        outcome.cost_id = Some(existing.id);
        skip(&mut outcome, "already_imported");
        return outcome;
    }
    if context.dry_run {
        outcome.disposition = "planned".into();
        return outcome;
    }
    let evidence_bytes = serde_json::to_vec_pretty(transaction).unwrap_or_default();
    let Ok(evidence_document) = private_evidence(
        context.surreal,
        context.storage,
        project_id,
        &format!("accounting-source-{}.json", transaction.id),
        "application/json",
        &evidence_bytes,
    )
    .await
    else {
        skip(&mut outcome, "evidence_unavailable");
        return outcome;
    };
    let mut receipt_document = None;
    for attachment in &transaction.attachments {
        let Ok(bytes) = context.client.receipt(attachment).await else {
            skip(&mut outcome, "receipt_unavailable");
            return outcome;
        };
        let digest = store::documents::sha256_hex(&bytes);
        let Ok(document) = private_evidence(
            context.surreal,
            context.storage,
            project_id,
            &format!("accounting-receipt-{digest}"),
            "application/octet-stream",
            &bytes,
        )
        .await
        else {
            skip(&mut outcome, "receipt_unavailable");
            return outcome;
        };
        receipt_document.get_or_insert(document);
    }
    let draft = costs::MercuryCostDraft {
        transaction_id: transaction.id,
        occurred_on,
        payee: transaction
            .counterparty_name
            .clone()
            .unwrap_or_else(|| "Mercury".into()),
        description: transaction
            .note
            .clone()
            .or_else(|| transaction.source_description.clone())
            .unwrap_or_else(|| "Mercury transaction".into()),
        amount_cents,
    };
    let Ok(cost_input) =
        costs::draft_mercury_cost(&draft, receipt_document.unwrap_or(evidence_document))
    else {
        skip(&mut outcome, "cost_rejected");
        return outcome;
    };
    match costs::create(
        context.surreal,
        context.pg,
        project_id,
        context.actor_id,
        &cost_input,
    )
    .await
    {
        Ok(cost) => {
            outcome.disposition = "imported".into();
            outcome.cost_id = Some(cost.id);
        }
        Err(_) => skip(&mut outcome, "cost_rejected"),
    }
    outcome
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum XeroKind {
    Invoice,
    CreditNote,
    Payment,
    BankTransaction,
    BankTransfer,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct XeroRequest {
    project_id: Uuid,
    account_id: Option<Uuid>,
    object_id: Uuid,
    object_type: XeroKind,
    journal_account: Option<String>,
}
pub(super) async fn xero(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Json(request): Json<XeroRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    let project = store::projects::find_by_id(&state.surreal, request.project_id)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?
        .ok_or(ApiError::NotFound)?;
    if project.firm_id != Some(firm) {
        return Err(ApiError::NotFound);
    }
    match xero_snapshot(&state, &request).await {
        Ok(snapshot) => Ok(Json(snapshot).into_response()),
        Err(_) => Ok((
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error":"accounting_evidence_unavailable"})),
        )
            .into_response()),
    }
}
#[expect(
    clippy::too_many_lines,
    reason = "one selected-source adapter maps distinct Xero accounting object types"
)]
async fn xero_snapshot(state: &ApiState, request: &XeroRequest) -> anyhow::Result<SourceSnapshot> {
    use store::firm_secrets::{
        resolve_project_credential, IntegrationProvider, IntegrationSecretKind,
    };
    let kms = crate::integrations::runtime_kms_from_env().await;
    let mut credentials = Vec::new();
    for kind in [
        IntegrationSecretKind::XeroClientId,
        IntegrationSecretKind::XeroClientSecret,
        IntegrationSecretKind::XeroTenantId,
    ] {
        credentials.push(
            resolve_project_credential(
                &state.surreal,
                request.project_id,
                IntegrationProvider::Xero,
                kind,
                kms.as_ref(),
            )
            .await?,
        );
    }
    let tenant = credentials[2].expose_for_provider_call().parse::<Uuid>()?;
    let token=crate::xero_auth::XeroClientCredentials::new(credentials[0].expose_for_provider_call(),credentials[1].expose_for_provider_call(),"https://identity.xero.com/connect/token","accounting.invoices.read accounting.creditnotes.read accounting.payments.read accounting.banktransactions.read accounting.settings.read accounting.attachments.read").mint_access_token().await?;
    let client = cloud::xero_evidence::XeroEvidence::new(&token, tenant)?;
    let mut supporting_evidence = Vec::new();
    let attachment_parent;
    let occurred_at;
    let (account, currency, quantity, status, object_type, facts, bytes) = match request.object_type
    {
        XeroKind::Invoice => {
            let (invoice, bytes) = client.invoice(request.object_id).await?;
            occurred_at = invoice.date.as_deref().and_then(xero_timestamp);
            attachment_parent = Some((
                cloud::xero_evidence::AttachmentParent::Invoice,
                request.object_id,
            ));
            let precision = currency_precision(&invoice.currency_code)?;
            let status = match invoice.status.as_str() {
                "AUTHORISED" | "PAID" => SourceStatus::Completed,
                "DRAFT" | "SUBMITTED" => SourceStatus::Pending,
                "VOIDED" | "DELETED" => SourceStatus::Reversed,
                _ => anyhow::bail!("unknown invoice status"),
            };
            let due = journal::parse_decimal(invoice.amount_due.get(), precision)?;
            let facts = journal::SourceFacts::Invoice {
                invoice_id: invoice.invoice_id,
                number: invoice.invoice_number,
                total: journal::parse_decimal(invoice.total.get(), precision)?,
                paid: journal::parse_decimal(invoice.amount_paid.get(), precision)?,
                due: due.clone(),
                payment_ids: invoice.payments.iter().map(|p| p.payment_id).collect(),
            };
            (
                format!("receivable:{}", invoice.invoice_id),
                invoice.currency_code,
                due,
                status,
                "invoice",
                facts,
                bytes,
            )
        }
        XeroKind::CreditNote => {
            let (note, bytes) = client.credit_note(request.object_id).await?;
            occurred_at = note.date.as_deref().and_then(xero_timestamp);
            attachment_parent = Some((
                cloud::xero_evidence::AttachmentParent::CreditNote,
                request.object_id,
            ));
            let precision = currency_precision(&note.currency_code)?;
            let status = match note.status.as_str() {
                "AUTHORISED" | "PAID" => SourceStatus::Completed,
                "DRAFT" | "SUBMITTED" => SourceStatus::Pending,
                "VOIDED" | "DELETED" => SourceStatus::Reversed,
                _ => anyhow::bail!("unknown credit note status"),
            };
            let remaining = journal::parse_decimal(note.remaining_credit.get(), precision)?;
            let facts = journal::SourceFacts::CreditNote {
                credit_note_id: note.credit_note_id,
                number: note.credit_note_number,
                total: journal::parse_decimal(note.total.get(), precision)?,
                remaining_credit: remaining.clone(),
                payment_ids: note.payments.iter().map(|p| p.payment_id).collect(),
            };
            (
                format!("credit:{}", note.credit_note_id),
                note.currency_code,
                remaining,
                status,
                "credit_note",
                facts,
                bytes,
            )
        }
        XeroKind::Payment => {
            let account = request
                .account_id
                .ok_or_else(|| anyhow::anyhow!("payment requires bank account identity"))?;
            let (bank, _) = client.account(account).await?;
            let (payment, bytes) = client.payment(request.object_id, account).await?;
            occurred_at = payment.date.as_deref().and_then(xero_timestamp);
            if payment.invoice.is_some() && payment.credit_note.is_some() {
                anyhow::bail!("ambiguous payment accounting document");
            }
            attachment_parent = payment
                .invoice
                .as_ref()
                .map(|invoice| {
                    (
                        cloud::xero_evidence::AttachmentParent::Invoice,
                        invoice.invoice_id,
                    )
                })
                .or_else(|| {
                    payment.credit_note.as_ref().map(|note| {
                        (
                            cloud::xero_evidence::AttachmentParent::CreditNote,
                            note.credit_note_id,
                        )
                    })
                });
            let bank_amount = payment.bank_amount.as_ref().ok_or_else(|| {
                anyhow::anyhow!("bank-currency amount missing; conversion requires review")
            })?;
            let amount = journal::parse_decimal(
                bank_amount.get(),
                currency_precision(&bank.currency_code)?,
            )?;
            let signed = payment_quantity(&payment.payment_type, &amount)?;
            let status = match payment.status.as_str() {
                "AUTHORISED" => SourceStatus::Completed,
                "DELETED" | "VOIDED" => SourceStatus::Reversed,
                _ => anyhow::bail!("unknown payment status"),
            };
            let facts = journal::SourceFacts::Payment {
                credit_note_id: payment.credit_note.as_ref().map(|note| note.credit_note_id),
                payment_id: payment.payment_id,
                invoice_id: payment.invoice.as_ref().map(|i| i.invoice_id),
                invoice_number: payment
                    .invoice
                    .as_ref()
                    .and_then(|i| i.invoice_number.clone()),
                original_currency: payment
                    .invoice
                    .as_ref()
                    .and_then(|i| i.currency_code.clone())
                    .or_else(|| {
                        payment
                            .credit_note
                            .as_ref()
                            .and_then(|note| note.currency_code.clone())
                    }),
                original_quantity: payment.amount.get().into(),
                bank_quantity: signed.clone(),
                currency_rate: payment.currency_rate.as_ref().map(|n| n.get().into()),
                reconciled: payment.is_reconciled,
            };
            (
                account.to_string(),
                bank.currency_code,
                signed,
                status,
                "payment",
                facts,
                bytes,
            )
        }
        XeroKind::BankTransaction => {
            let account = request
                .account_id
                .ok_or_else(|| anyhow::anyhow!("transaction requires bank account"))?;
            let (bank, _) = client.account(account).await?;
            let (transaction, bytes) = client.bank_transaction(request.object_id, account).await?;
            occurred_at = transaction.date.as_deref().and_then(xero_timestamp);
            attachment_parent = Some((
                cloud::xero_evidence::AttachmentParent::BankTransaction,
                request.object_id,
            ));
            if bank.currency_code != transaction.currency_code {
                anyhow::bail!("cross-currency bank transaction requires explicit bank amount");
            }
            let amount = journal::parse_decimal(
                transaction.total.get(),
                currency_precision(&bank.currency_code)?,
            )?;
            let quantity = bank_transaction_quantity(&transaction.transaction_type, &amount)?;
            let status = match transaction.status.as_str() {
                "AUTHORISED" => SourceStatus::Completed,
                "DELETED" | "VOIDED" => SourceStatus::Reversed,
                _ => anyhow::bail!("unknown transaction status"),
            };
            let facts = journal::SourceFacts::BankTransaction {
                transaction_id: transaction.bank_transaction_id,
                transaction_type: transaction.transaction_type,
                currency_rate: transaction.currency_rate.as_ref().map(|n| n.get().into()),
                reconciled: transaction.is_reconciled,
            };
            (
                account.to_string(),
                bank.currency_code,
                quantity,
                status,
                "bank_transaction",
                facts,
                bytes,
            )
        }
        XeroKind::BankTransfer => {
            let account = request
                .account_id
                .ok_or_else(|| anyhow::anyhow!("transfer requires selected account"))?;
            let (transfer, bytes) = client.bank_transfer(request.object_id).await?;
            occurred_at = transfer.date.as_deref().and_then(xero_timestamp);
            attachment_parent = Some((
                cloud::xero_evidence::AttachmentParent::BankTransfer,
                request.object_id,
            ));
            let movement = if account == transfer.from_bank_account.account_id {
                transfer.from_bank_transaction_id
            } else if account == transfer.to_bank_account.account_id {
                transfer.to_bank_transaction_id
            } else {
                anyhow::bail!("account is not a transfer leg")
            };
            let (bank, account_bytes) = client.account(account).await?;
            let (transaction, movement_bytes) = client.bank_transaction(movement, account).await?;
            supporting_evidence.push(account_bytes);
            supporting_evidence.push(movement_bytes);
            let status = match transaction.status.as_str() {
                "AUTHORISED" if transfer.status.as_deref() != Some("DELETED") => {
                    SourceStatus::Completed
                }
                "DELETED" | "VOIDED" | "AUTHORISED" => SourceStatus::Reversed,
                _ => anyhow::bail!("unknown transfer leg status"),
            };
            if bank.currency_code != transaction.currency_code {
                anyhow::bail!("transfer leg requires explicit bank-currency quantity");
            }
            let quantity = bank_transaction_quantity(
                &transaction.transaction_type,
                &journal::parse_decimal(
                    transaction.total.get(),
                    currency_precision(&bank.currency_code)?,
                )?,
            )?;
            let facts = journal::SourceFacts::BankTransfer {
                reconciled: if account == transfer.from_bank_account.account_id {
                    transfer.from_is_reconciled
                } else {
                    transfer.to_is_reconciled
                },
                transfer_id: transfer.bank_transfer_id,
                from_account_id: transfer.from_bank_account.account_id,
                to_account_id: transfer.to_bank_account.account_id,
                from_transaction_id: transfer.from_bank_transaction_id,
                to_transaction_id: transfer.to_bank_transaction_id,
            };
            (
                account.to_string(),
                bank.currency_code,
                quantity,
                status,
                "bank_transfer",
                facts,
                bytes,
            )
        }
    };
    let digest = store::documents::sha256_hex(&bytes);
    let name = format!("accounting-source-{digest}.json");
    let evidence = private_evidence(
        &state.surreal,
        &state.storage,
        request.project_id,
        &name,
        "application/json",
        &bytes,
    )
    .await?;
    let mut receipts = Vec::new();
    let mut source_evidence = vec![evidence];
    for bytes in supporting_evidence {
        let hash = store::documents::sha256_hex(&bytes);
        source_evidence.push(
            private_evidence(
                &state.surreal,
                &state.storage,
                request.project_id,
                &format!("xero-support-{hash}.json"),
                "application/json",
                &bytes,
            )
            .await?,
        );
    }
    if let Some((parent, object_id)) = attachment_parent {
        let (attachments, listing) = client.attachments(parent, object_id).await?;
        let hash = store::documents::sha256_hex(&listing);
        source_evidence.push(
            private_evidence(
                &state.surreal,
                &state.storage,
                request.project_id,
                &format!("xero-attachment-list-{hash}.json"),
                "application/json",
                &listing,
            )
            .await?,
        );
        for attachment in attachments {
            let bytes = client.attachment(parent, object_id, &attachment).await?;
            let hash = store::documents::sha256_hex(&bytes);
            receipts.push(
                private_evidence(
                    &state.surreal,
                    &state.storage,
                    request.project_id,
                    &format!("accounting-receipt-{hash}"),
                    &attachment.mime_type,
                    &bytes,
                )
                .await?,
            );
        }
    }
    let mut snapshot = SourceSnapshot {
        occurred_at,
        journal_account: request.journal_account.clone(),
        adapter_version: "xero-accounting-v2".into(),
        facts,
        source: SourceReference {
            provider: "xero".into(),
            scope: tenant.to_string(),
            account,
            object_type: object_type.into(),
            external_id: request.object_id.to_string(),
            locator: None,
        },
        revision: digest.clone(),
        retrieved_at: chrono::Utc::now().to_rfc3339(),
        digest,
        unit: Unit::fiat(&currency, currency_precision(&currency)?),
        quantity,
        status,
        receipt_document_ids: receipts,
        evidence_document_ids: source_evidence,
        valuation: Valuation::Unknown,
    };
    snapshot.revision = journal::source_revision(&snapshot)?;
    Ok(snapshot)
}
// Xero JSON dates contain Unix milliseconds. Preserve missing/unsupported dates
// as unknown, so reconciliation requires review instead of inventing a date.
fn xero_timestamp(raw: &str) -> Option<String> {
    if let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(raw) {
        return Some(timestamp.to_rfc3339());
    }
    let inner = raw.strip_prefix("/Date(")?.strip_suffix(")/")?;
    let length = inner
        .char_indices()
        .skip(1)
        .find(|(_, c)| matches!(c, '+' | '-'))
        .map_or(inner.len(), |(index, _)| index);
    let milliseconds = inner[..length].parse::<i64>().ok()?;
    chrono::DateTime::from_timestamp_millis(milliseconds).map(|time| time.to_rfc3339())
}
fn currency_precision(currency: &str) -> anyhow::Result<u8> {
    Ok(match currency {
        "USD" | "EUR" | "GBP" | "CAD" | "AUD" | "CHF" => 2,
        "JPY" | "KRW" => 0,
        "KWD" | "BHD" => 3,
        _ => anyhow::bail!("currency precision requires verified metadata"),
    })
}
fn payment_quantity(kind: &str, amount: &str) -> anyhow::Result<String> {
    let sign = match kind {
        "ACCRECPAYMENT" | "APCREDITPAYMENT" | "APPREPAYMENTPAYMENT" | "APOVERPAYMENTPAYMENT" => 1,
        "ACCPAYPAYMENT" | "ARCREDITPAYMENT" | "ARPREPAYMENTPAYMENT" | "AROVERPAYMENTPAYMENT" => -1,
        _ => anyhow::bail!("unsupported payment direction"),
    };
    Ok(journal::quantity(amount)?
        .checked_mul(sign)
        .ok_or_else(|| anyhow::anyhow!("quantity overflow"))?
        .to_string())
}
fn bank_transaction_quantity(kind: &str, amount: &str) -> anyhow::Result<String> {
    let sign = match kind {
        "RECEIVE" | "RECEIVE-OVERPAYMENT" | "RECEIVE-PREPAYMENT" | "RECEIVE-TRANSFER" => 1,
        "SPEND" | "SPEND-OVERPAYMENT" | "SPEND-PREPAYMENT" | "SPEND-TRANSFER" => -1,
        _ => anyhow::bail!("unsupported bank transaction direction"),
    };
    Ok(journal::quantity(amount)?
        .checked_mul(sign)
        .ok_or_else(|| anyhow::anyhow!("quantity overflow"))?
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xero_dates_preserve_known_occurrence_and_leave_unsupported_dates_unknown() {
        assert_eq!(
            xero_timestamp("2026-10-11T00:00:00Z").as_deref(),
            Some("2026-10-11T00:00:00+00:00")
        );
        assert_eq!(
            xero_timestamp("/Date(0+0000)/").as_deref(),
            Some("1970-01-01T00:00:00+00:00")
        );
        assert_eq!(
            xero_timestamp("/Date(-1000-0500)/").as_deref(),
            Some("1969-12-31T23:59:59+00:00")
        );
        for raw in ["", "unknown", "/Date(nope)/", "/Date(9223372036854775807)/"] {
            assert!(xero_timestamp(raw).is_none());
        }
    }

    #[test]
    fn bank_movements_use_exact_currency_precision_and_direction() {
        for currency in ["USD", "EUR", "GBP", "CAD", "AUD", "CHF"] {
            assert_eq!(currency_precision(currency).unwrap(), 2);
        }
        for currency in ["JPY", "KRW"] {
            assert_eq!(currency_precision(currency).unwrap(), 0);
        }
        for currency in ["KWD", "BHD"] {
            assert_eq!(currency_precision(currency).unwrap(), 3);
        }
        assert!(currency_precision("UNKNOWN").is_err());
        for kind in [
            "RECEIVE",
            "RECEIVE-OVERPAYMENT",
            "RECEIVE-PREPAYMENT",
            "RECEIVE-TRANSFER",
        ] {
            assert_eq!(bank_transaction_quantity(kind, "123").unwrap(), "123");
        }
        for kind in [
            "SPEND",
            "SPEND-OVERPAYMENT",
            "SPEND-PREPAYMENT",
            "SPEND-TRANSFER",
        ] {
            assert_eq!(bank_transaction_quantity(kind, "123").unwrap(), "-123");
        }
        assert!(bank_transaction_quantity("UNKNOWN", "123").is_err());
        assert!(bank_transaction_quantity("SPEND", "1.5").is_err());
        assert!(payment_quantity("ACCRECPAYMENT", "1.5").is_err());
        let minimum = i128::MIN.to_string();
        assert!(bank_transaction_quantity("SPEND", &minimum).is_err());
        assert!(payment_quantity("ACCPAYPAYMENT", &minimum).is_err());
    }

    #[test]
    fn solana_normalization_keeps_transfer_fee_finality_and_source_identity_separate() {
        let document = Uuid::now_v7();
        let mut request = SolanaRequest {
            project_id: Uuid::now_v7(),
            network: "devnet".into(),
            wallet: "synthetic-wallet".into(),
            signature: "synthetic-signature".into(),
            locator: Some("instruction:0".into()),
            journal_account: Some("firm-wallet".into()),
        };
        for (locator, failed, payer, block_time) in [
            (Some("instruction:0"), false, "synthetic-wallet", Some(0)),
            (Some("instruction:0"), true, "another-wallet", None),
            (None, true, "synthetic-wallet", Some(i64::MAX)),
        ] {
            request.locator = locator.map(str::to_owned);
            let movement = cloud::solana_evidence::Movement {
                contract: "native".into(),
                decimals: 9,
                quantity: "10000".into(),
                network_fee: "5000".into(),
                fee_payer: payer.into(),
                block_time,
                failed,
            };
            let snapshot =
                normalized_solana_snapshot(&request, movement, "synthetic-digest".into(), document)
                    .unwrap();
            assert_eq!(snapshot.source.provider, "solana");
            assert_eq!(snapshot.source.scope, "devnet");
            assert_eq!(snapshot.source.account, "synthetic-wallet");
            assert_eq!(snapshot.source.external_id, "synthetic-signature");
            assert_eq!(snapshot.source.locator, request.locator);
            assert_eq!(snapshot.journal_account.as_deref(), Some("firm-wallet"));
            assert_eq!(snapshot.evidence_document_ids, vec![document]);
            assert!(snapshot.receipt_document_ids.is_empty());
            assert_eq!(snapshot.quantity, "10000");
            assert_eq!(
                snapshot.status,
                if locator.is_some() && failed {
                    SourceStatus::Failed
                } else {
                    SourceStatus::Completed
                }
            );
            assert_eq!(snapshot.occurred_at.is_some(), block_time == Some(0));
            assert_eq!(
                snapshot.revision,
                journal::source_revision(&snapshot).unwrap()
            );
            if let journal::SourceFacts::Crypto {
                fee_quantity,
                finality,
                transfer_locator,
                ..
            } = snapshot.facts
            {
                assert_eq!(
                    fee_quantity,
                    if payer == request.wallet { "5000" } else { "0" }
                );
                assert!(finality.satisfied());
                assert_eq!(transfer_locator, "instruction:0");
                assert_eq!(snapshot.source.object_type, "transfer");
            } else {
                assert!(matches!(
                    snapshot.facts,
                    journal::SourceFacts::NetworkFee { .. }
                ));
                assert_eq!(snapshot.source.object_type, "network_fee");
            }
        }
    }

    #[test]
    fn xero_payment_directions_follow_customer_and_supplier_refunds() {
        for kind in [
            "ACCRECPAYMENT",
            "APCREDITPAYMENT",
            "APPREPAYMENTPAYMENT",
            "APOVERPAYMENTPAYMENT",
        ] {
            assert_eq!(payment_quantity(kind, "123").unwrap(), "123");
        }
        for kind in [
            "ACCPAYPAYMENT",
            "ARCREDITPAYMENT",
            "ARPREPAYMENTPAYMENT",
            "AROVERPAYMENTPAYMENT",
        ] {
            assert_eq!(payment_quantity(kind, "123").unwrap(), "-123");
        }
        assert!(payment_quantity("UNKNOWN", "123").is_err());
    }

    // --- bulk Mercury import ---

    /// One listed transaction, with optional JSON-field overrides; a `null`
    /// override removes the field. `amount` stays a raw JSON numeral because
    /// the import must read the provider's exact decimal text.
    fn mercury_tx(id: Uuid, overrides: &serde_json::Value) -> cloud::mercury::Transaction {
        let mut base = serde_json::json!({
            "id": id,
            "accountId": Uuid::now_v7(),
            "amount": 12.34,
            "status": "sent",
            "postedAt": "2026-10-01T00:00:00+00:00",
            "counterpartyName": "Acme Supplies",
            "attachments": []
        });
        if let (Some(target), Some(patch)) = (base.as_object_mut(), overrides.as_object()) {
            for (key, value) in patch {
                if value.is_null() {
                    target.remove(key);
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        serde_json::from_value(base).unwrap()
    }

    /// The disposable fixture database, created and migrated once per process.
    static FIXTURE_DB: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();

    /// The Postgres handle the import writes through, against a disposable
    /// database on the synthetic fixture. `None` where the fixture port is
    /// absent — CI sets `NAV_REQUIRE_POSTGRES=1` so its tests never skip.
    async fn import_pg() -> Option<store::postgres::PostgresDb> {
        let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
            assert!(
                std::env::var("NAV_REQUIRE_POSTGRES").as_deref() != Ok("1"),
                "required PostgreSQL fixture lacks its disposable port"
            );
            return None;
        };
        let port: u16 = port.parse().expect("test PostgreSQL port is numeric");
        let options = |database: &str| {
            sqlx::postgres::PgConnectOptions::new()
                .host("127.0.0.1")
                .port(port)
                .username("postgres")
                .password("synthetic")
                .database(database)
                .ssl_mode(sqlx::postgres::PgSslMode::Disable)
        };
        let database = FIXTURE_DB
            .get_or_init(|| async {
                let database = format!("navigator_firm_journal_{}", uuid::Uuid::new_v4().simple());
                let admin = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(1)
                    .connect_with(options("postgres"))
                    .await
                    .expect("connect to the synthetic PostgreSQL fixture");
                let create = format!("CREATE DATABASE \"{database}\"");
                sqlx::query(sqlx::AssertSqlSafe(create))
                    .execute(&admin)
                    .await
                    .expect("create the disposable fixture database");
                let migrated = store::postgres::PostgresDb::connect(options(&database))
                    .await
                    .expect("connect the disposable fixture database");
                migrated
                    .migrate_embedded(None)
                    .await
                    .expect("migrate the disposable fixture database");
                database
            })
            .await
            .clone();
        Some(
            store::postgres::PostgresDb::connect(options(&database))
                .await
                .unwrap(),
        )
    }

    /// A synthetic Project whose hyphenated code appears verbatim as one
    /// token of a provider free-text field.
    async fn import_project(surreal: &store::surreal::SurrealDb) -> (String, Uuid) {
        let jurisdiction = store::jurisdictions::create(
            surreal,
            &store::jurisdictions::NewJurisdiction::new(
                "Mercury import jurisdiction",
                format!("MERCURY-{}", Uuid::new_v4()),
                "state",
            ),
        )
        .await
        .unwrap();
        let entity_type = store::entity_types::create(
            surreal,
            &format!("Mercury import entity type {}", Uuid::new_v4()),
        )
        .await
        .unwrap();
        let entity = store::entities::create(
            surreal,
            &store::entities::NewEntity {
                name: "Mercury import matter entity".into(),
                entity_type_id: entity_type.id,
                jurisdiction_id: jurisdiction.id,
                phone: None,
                url: None,
                xero_id: None,
                firm_anchor_key: None,
            },
        )
        .await
        .unwrap();
        let code = format!("mercury-import-{}", Uuid::new_v4().simple());
        let project = store::projects::create(
            surreal,
            &store::projects::NewProject {
                code: code.clone(),
                name: "Mercury import matter".into(),
                status: "open".into(),
                entity_id: entity.id,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        (code, project.id)
    }

    async fn import_context<'a>(
        surreal: &'a store::surreal::SurrealDb,
        pg: &'a store::postgres::PostgresDb,
        storage: &'a std::sync::Arc<dyn cloud::StorageService>,
        projects: &'a [(String, Uuid)],
        client: &'a cloud::mercury::Mercury,
        dry_run: bool,
    ) -> MercuryImportContext<'a> {
        MercuryImportContext {
            surreal,
            storage,
            pg,
            projects,
            client,
            // The actor must exist as a Person so the import's mirror step
            // can write it before the creation trigger's foreign key needs
            // it — in production the actor is always the session's person.
            actor_id: store::persons::create(
                surreal,
                &store::persons::NewPerson::new(
                    "Mercury import actor",
                    &format!("mercury-import-{}@example.test", Uuid::new_v4()),
                ),
            )
            .await
            .unwrap()
            .id,
            dry_run,
        }
    }

    #[tokio::test]
    async fn import_one_skips_each_ineligible_transaction_for_its_named_reason() {
        let Some(pg) = import_pg().await else {
            return;
        };
        let surreal = store::test_support::mem_surreal().await;
        let state = crate::test_support::app_state(surreal.clone()).await;
        let (code, project_id) = import_project(&surreal).await;
        let (other, other_id) = import_project(&surreal).await;
        let projects = vec![(code, project_id), (other, other_id)];
        let client = cloud::mercury::Mercury::new("synthetic-read-token").unwrap();
        let context =
            import_context(&surreal, &pg, &state.storage, &projects, &client, false).await;
        let cases = [
            (serde_json::json!({ "postedAt": null }), "unposted"),
            (
                serde_json::json!({ "postedAt": "01-02-2026" }),
                "unreadable_posted_at",
            ),
            (serde_json::json!({ "status": "pending" }), "status"),
            (serde_json::json!({ "amount": "abc" }), "unreadable_amount"),
            (serde_json::json!({ "amount": 1.005 }), "unreadable_amount"),
            (serde_json::json!({ "amount": 0 }), "non_positive_amount"),
            (
                serde_json::json!({ "counterpartyName": "Unrelated Vendor" }),
                "unallocated",
            ),
            (
                serde_json::json!({
                    "counterpartyName": format!("{} and {}", projects[0].0, projects[1].0)
                }),
                "ambiguous",
            ),
        ];
        for (patch, reason) in cases {
            let outcome = import_one(&context, &mercury_tx(Uuid::now_v7(), &patch)).await;
            assert_eq!(outcome.disposition, "skipped", "case {reason}");
            assert_eq!(outcome.reason.as_deref(), Some(reason), "case {reason}");
            assert_eq!(outcome.project_code, None, "case {reason}");
            assert_eq!(outcome.cost_id, None, "case {reason}");
        }
    }

    #[tokio::test]
    async fn import_one_plans_a_dry_run_without_creating_a_cost() {
        let Some(pg) = import_pg().await else {
            return;
        };
        let surreal = store::test_support::mem_surreal().await;
        let state = crate::test_support::app_state(surreal.clone()).await;
        let (code, project_id) = import_project(&surreal).await;
        let projects = vec![(code.clone(), project_id)];
        let client = cloud::mercury::Mercury::new("synthetic-read-token").unwrap();
        let context = import_context(&surreal, &pg, &state.storage, &projects, &client, true).await;
        let transaction = mercury_tx(
            Uuid::now_v7(),
            &serde_json::json!({ "counterpartyName": format!("Acme Supplies {code}") }),
        );
        let outcome = import_one(&context, &transaction).await;
        assert_eq!(outcome.disposition, "planned");
        assert_eq!(outcome.project_code.as_deref(), Some(code.as_str()));
        assert!(outcome.cost_id.is_none());
    }

    #[tokio::test]
    async fn import_one_files_evidence_creates_the_cost_and_skips_the_rerun() {
        let Some(pg) = import_pg().await else {
            return;
        };
        let surreal = store::test_support::mem_surreal().await;
        let state = crate::test_support::app_state(surreal.clone()).await;
        let (code, project_id) = import_project(&surreal).await;
        let projects = vec![(code.clone(), project_id)];
        let client = cloud::mercury::Mercury::new("synthetic-read-token").unwrap();
        let context =
            import_context(&surreal, &pg, &state.storage, &projects, &client, false).await;
        let transaction = mercury_tx(
            Uuid::now_v7(),
            &serde_json::json!({ "counterpartyName": format!("Acme Supplies {code}") }),
        );
        let first = import_one(&context, &transaction).await;
        assert_eq!(first.disposition, "imported");
        assert_eq!(first.project_code.as_deref(), Some(code.as_str()));
        let cost_id = first.cost_id.expect("an imported cost carries its id");
        let second = import_one(&context, &transaction).await;
        assert_eq!(second.disposition, "skipped");
        assert_eq!(second.reason.as_deref(), Some("already_imported"));
        assert_eq!(second.cost_id, Some(cost_id));
        // A payee the cost store refuses still files the evidence first,
        // then reports the rejection instead of charging the card.
        let refused = mercury_tx(
            Uuid::now_v7(),
            &serde_json::json!({
                "counterpartyName": "",
                "note": format!("Acme Supplies {code}")
            }),
        );
        let outcome = import_one(&context, &refused).await;
        assert_eq!(outcome.disposition, "skipped");
        assert_eq!(outcome.reason.as_deref(), Some("cost_rejected"));
    }

    #[tokio::test]
    async fn import_one_skips_a_transaction_whose_receipt_cannot_be_fetched() {
        let Some(pg) = import_pg().await else {
            return;
        };
        let surreal = store::test_support::mem_surreal().await;
        let state = crate::test_support::app_state(surreal.clone()).await;
        let (code, project_id) = import_project(&surreal).await;
        let projects = vec![(code.clone(), project_id)];
        let client = cloud::mercury::Mercury::new("synthetic-read-token").unwrap();
        let context =
            import_context(&surreal, &pg, &state.storage, &projects, &client, false).await;
        let transaction = mercury_tx(
            Uuid::now_v7(),
            &serde_json::json!({
                "counterpartyName": format!("Acme Supplies {code}"),
                "attachments": [{
                    "fileName": "receipt.pdf",
                    "attachmentType": "receipt",
                    "url": "http://127.0.0.1/private"
                }]
            }),
        );
        let outcome = import_one(&context, &transaction).await;
        assert_eq!(outcome.disposition, "skipped");
        assert_eq!(outcome.reason.as_deref(), Some("receipt_unavailable"));
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SolanaRequest {
    project_id: Uuid,
    network: String,
    wallet: String,
    signature: String,
    locator: Option<String>,
    journal_account: Option<String>,
}
/// Read one explicitly linked instruction, or its separately identified fee.
pub(super) async fn solana(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Json(request): Json<SolanaRequest>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    let project = store::projects::find_by_id(&state.surreal, request.project_id)
        .await
        .map_err(|e| ApiError::Db(e.to_string()))?
        .ok_or(ApiError::NotFound)?;
    if project.firm_id != Some(firm) {
        return Err(ApiError::NotFound);
    }
    match solana_snapshot(&state, &request).await {
        Ok(snapshot) => Ok(Json(snapshot).into_response()),
        Err(_) => Ok((
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error":"chain_evidence_unavailable"})),
        )
            .into_response()),
    }
}
async fn solana_snapshot(
    state: &ApiState,
    request: &SolanaRequest,
) -> anyhow::Result<SourceSnapshot> {
    let (movement, bytes) = cloud::solana_evidence::fetch(
        cloud::solana_evidence::Network::parse(&request.network)?,
        &request.signature,
        &request.wallet,
        request.locator.as_deref(),
    )
    .await?;
    let digest = store::documents::sha256_hex(&bytes);
    let document = private_evidence(
        &state.surreal,
        &state.storage,
        request.project_id,
        &format!("accounting-chain-{digest}.json"),
        "application/json",
        &bytes,
    )
    .await?;
    normalized_solana_snapshot(request, movement, digest, document)
}

fn normalized_solana_snapshot(
    request: &SolanaRequest,
    movement: cloud::solana_evidence::Movement,
    digest: String,
    document: Uuid,
) -> anyhow::Result<SourceSnapshot> {
    let fee_unit = Unit::Crypto {
        network: request.network.clone(),
        contract: "native".into(),
        decimals: 9,
    };
    let facts = if let Some(locator) = &request.locator {
        journal::SourceFacts::Crypto {
            transaction_hash: request.signature.clone(),
            transfer_locator: locator.clone(),
            wallet: request.wallet.clone(),
            finality_evidence_document_id: document,
            finality: journal::CryptoFinality::Finalized,
            fee_unit,
            fee_quantity: if movement.fee_payer == request.wallet {
                movement.network_fee.clone()
            } else {
                "0".into()
            },
        }
    } else {
        journal::SourceFacts::NetworkFee {
            transaction_hash: request.signature.clone(),
            wallet: request.wallet.clone(),
            finality_evidence_document_id: document,
            finality: journal::CryptoFinality::Finalized,
        }
    };
    let mut snapshot = SourceSnapshot {
        source: SourceReference {
            provider: "solana".into(),
            scope: request.network.clone(),
            account: request.wallet.clone(),
            object_type: if request.locator.is_some() {
                "transfer"
            } else {
                "network_fee"
            }
            .into(),
            external_id: request.signature.clone(),
            locator: request.locator.clone(),
        },
        occurred_at: movement
            .block_time
            .and_then(|time| chrono::DateTime::from_timestamp(time, 0))
            .map(|time| time.to_rfc3339()),
        journal_account: request.journal_account.clone(),
        adapter_version: "solana-finalized-jsonparsed-v1".into(),
        facts,
        revision: digest.clone(),
        retrieved_at: chrono::Utc::now().to_rfc3339(),
        digest,
        unit: Unit::Crypto {
            network: request.network.clone(),
            contract: movement.contract,
            decimals: movement.decimals,
        },
        quantity: movement.quantity,
        status: if request.locator.is_some() && movement.failed {
            SourceStatus::Failed
        } else {
            SourceStatus::Completed
        },
        receipt_document_ids: vec![],
        evidence_document_ids: vec![document],
        valuation: Valuation::Unknown,
    };
    snapshot.revision = journal::source_revision(&snapshot)?;
    Ok(snapshot)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SignalQuery {
    #[serde(default)]
    after: i64,
}
pub(super) async fn signals(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(firm): Path<Uuid>,
    Query(query): Query<SignalQuery>,
) -> Result<Response, ApiError> {
    authorize(&state, &session, firm).await?;
    let store = journal_store(&state).await?;
    let signals = match store.postgres() {
        Ok(pg) => journal::signals(pg, firm, query.after).await,
        Err(error) => Err(error),
    };
    Ok(match signals {
        Ok(signals) => Json(signals).into_response(),
        Err(error) => journal_error(&error),
    })
}
