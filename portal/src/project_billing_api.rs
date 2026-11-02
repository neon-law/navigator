//! Matter-level invoice generation — the first production caller of the
//! [`billing::BillingProvider`] seam beyond the self-serve purchases flow.
//!
//! One command composes a matter's unbilled work into one invoice and raises
//! it through the provider seam: unbilled billable Time Entries at their
//! snapshotted rates plus unbilled confirmed Project Costs (the same parts
//! `store::matter_financials` prices Work In Progress from), or — when the
//! matter carries an agreed flat fee — the fee itself plus those costs. The
//! raised invoice is mirrored locally (`store::xero_invoices::upsert`, the
//! same ingest path the nightly reconcile uses), and the consumed entries and
//! costs flip to invoiced under the Xero id through their own audited
//! transitions (`time_entries::mark_invoiced`, `project_costs::link_invoice`).
//!
//! **Xero is the books.** The only write across the provider boundary is
//! `create_invoice` — idempotent per matter id at the seam (Xero honours the
//! `Idempotency-Key` for 24h), so a retry after a lost response or a crash
//! mid-flow re-mints the same invoice instead of a second one. On a provider
//! refusal nothing is marked, nothing is mirrored, and the whole generation
//! is retryable. Every other write here is local state describing what Xero
//! now holds.
//!
//! A matter whose flat fee is agreed bills the fee rather than itemized time:
//! the fee is the agreed price of the matter's work, so its unbilled entries
//! are consumed by (marked under) the fee invoice and never billed again —
//! which is also what makes a second generation on a flat matter answer the
//! same invoice instead of raising another.
//!
//! Route admission is lawyer tier (the `LawyerSession` extractor plus an
//! embedded-Rego `route_allow` rule), exactly as [`crate::project_costs_api`];
//! the matter-scope question is asked once through
//! [`store::access::matter_viewer`], so a caller with no firm-side
//! participation row — a client, a clerk, an unassigned lawyer, Owner and
//! Admin included — gets the non-disclosing 404, never a refusal.
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use store::{
    attorney_rates, postgres_mirror, project_costs as costs, projects, time_entries, xero_invoices,
};
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};
use crate::billing::{self, InvoiceLine, InvoiceRequest};

/// The currency a generated invoice bills in — the deployment's receivables
/// default, the same one the mirror's ingest falls back to. A confirmed
/// unbilled cost in another currency refuses the whole generation (before
/// anything is raised) rather than silently dropping reimbursable money or
/// mixing currencies on one Xero invoice.
const INVOICE_CURRENCY: &str = "USD";

/// The Xero chart-of-accounts code the revenue posts to — the same sales code
/// the services catalog copy and the sandbox-reset fixture bill against.
const REVENUE_ACCOUNT_CODE: &str = "200";

/// How a Xero `Reference` names the matter an invoice bills — the documented
/// ingest convention (`store::xero_invoices::ProjectScopeRef`).
fn matter_reference(code: &str) -> String {
    format!("Matter {code}")
}

/// The composition of a matter's unbilled work into invoice lines: the pure
/// middle of the command, no store or provider on either side.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Compose {
    line_items: Vec<InvoiceLine>,
    total_cents: i64,
}

/// Compose the invoice lines for a matter: one line per unbilled billable
/// Time Entry at its snapshotted rate (the same per-entry amount Work In
/// Progress shows), one per unbilled confirmed Project Cost, or — when the
/// matter carries an agreed flat fee — the fee as one line covering the
/// entries. Money is integer cents throughout; quantity is always 1 because
/// each line is already priced in cents, never in fractional hours.
fn compose_lines(
    entries: &[time_entries::TimeEntry],
    unbilled_costs: &[costs::ProjectCost],
    flat_fee_cents: Option<i64>,
) -> Result<Compose, InvoiceGenerationError> {
    for cost in unbilled_costs {
        if cost.currency != INVOICE_CURRENCY {
            return Err(InvoiceGenerationError::CurrencyMismatch(
                cost.currency.clone(),
            ));
        }
    }

    let mut line_items = Vec::with_capacity(entries.len() + unbilled_costs.len() + 1);
    if let Some(fee_cents) = flat_fee_cents {
        // The fee is the agreed price of the matter's work: one line, and the
        // entries below are consumed under it rather than itemized.
        line_items.push(InvoiceLine {
            description: "Flat fee — legal services per the fee agreement".to_string(),
            quantity: 1,
            unit_amount_cents: fee_cents,
            account_code: REVENUE_ACCOUNT_CODE.to_string(),
        });
    } else {
        for entry in entries {
            line_items.push(InvoiceLine {
                description: format!("{} — {}", entry.description, entry.performed_on),
                quantity: 1,
                unit_amount_cents: time_entries::entry_amount_cents(
                    entry.minutes,
                    entry.rate_cents_per_hour,
                ),
                account_code: REVENUE_ACCOUNT_CODE.to_string(),
            });
        }
    }
    for cost in unbilled_costs {
        line_items.push(InvoiceLine {
            description: format!("{} — {}", cost.description, cost.payee),
            quantity: 1,
            unit_amount_cents: cost.amount_cents,
            account_code: REVENUE_ACCOUNT_CODE.to_string(),
        });
    }

    let total_cents = line_items
        .iter()
        .map(|line| i64::from(line.quantity) * line.unit_amount_cents)
        .sum();
    Ok(Compose {
        line_items,
        total_cents,
    })
}

/// What the matter owes a billed party: the client DRI when the matter names
/// one, else the first client-side participant. `None` when the matter has no
/// client-side participation at all — there is nobody to address the
/// invoice to, and the command refuses rather than guess.
async fn billed_party(
    surreal: &store::surreal::SurrealDb,
    project_id: Uuid,
) -> Result<Option<store::persons::Person>, InvoiceGenerationError> {
    let participations = projects::participations_for_project(surreal, project_id).await?;
    let client_side = participations
        .into_iter()
        .filter(|row| {
            projects::PARTICIPATION_CLIENT_SIDE.contains(&row.participation.as_str())
                || row.is_client_dri
        })
        .collect::<Vec<_>>();
    let party = client_side
        .iter()
        .find(|row| row.is_client_dri)
        .or_else(|| client_side.first());
    match party {
        Some(row) => Ok(store::persons::find_by_id(surreal, row.person_id).await?),
        None => Ok(None),
    }
}

/// One raised invoice as the command answers it: what was billed, the parts
/// that were consumed under it, and the Xero id every local flip carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct InvoiceReceipt {
    pub xero_invoice_id: String,
    pub reference: String,
    pub currency: String,
    pub total_cents: i64,
    pub line_items: Vec<InvoiceLine>,
    /// Time Entries consumed under the invoice. On a flat-fee matter these
    /// are covered by the fee line, not itemized on it.
    pub consumed_time_entries: usize,
    pub consumed_costs: usize,
    /// Whether the invoice billed the matter's agreed flat fee instead of
    /// itemized time.
    pub flat_fee: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InvoiceGenerationError {
    #[error("nothing is unbilled on this matter")]
    NothingToInvoice,
    #[error("an unbilled cost is in {0}; a matter invoice bills in USD")]
    CurrencyMismatch(String),
    #[error("the matter names no client to bill")]
    NoBilledParty,
    #[error("the matter is not on the store")]
    NoMatter,
    #[error(transparent)]
    Time(#[from] time_entries::TimeEntryError),
    #[error(transparent)]
    Fee(#[from] attorney_rates::FeeStructureError),
    #[error(transparent)]
    Cost(#[from] costs::ProjectCostError),
    #[error(transparent)]
    Billing(#[from] billing::BillingError),
    #[error(transparent)]
    Mirror(#[from] Box<xero_invoices::XeroInvoiceError>),
    #[error(transparent)]
    Project(#[from] projects::ProjectStoreError),
    #[error(transparent)]
    Person(#[from] store::persons::PersonError),
}

/// Generate the matter's invoice: compose the unbilled work, raise one
/// invoice through the provider seam, mirror it locally, and flip the
/// consumed entries and costs to invoiced under the Xero id. `now` prices the
/// mirror's issued date; the caller's authorization seam governs who may call.
///
/// # Errors
///
/// [`InvoiceGenerationError`] when the matter has nothing unbilled, names no
/// client, carries a cost in another currency, or the store or provider
/// refuses — in every failure case nothing is marked and nothing is mirrored,
/// so the command is safe to retry.
pub(crate) async fn generate_invoice(
    surreal: &store::surreal::SurrealDb,
    pg: &store::postgres::PostgresDb,
    provider: &dyn billing::BillingProvider,
    project_id: Uuid,
    actor_id: Uuid,
    now: DateTime<Utc>,
) -> Result<InvoiceReceipt, InvoiceGenerationError> {
    let project = projects::find_by_id(surreal, project_id)
        .await?
        .ok_or(InvoiceGenerationError::NoMatter)?;

    let entries = time_entries::unbilled_billable(pg, project_id).await?;
    let unbilled_costs = costs::list_for_project(pg, project_id)
        .await?
        .into_iter()
        .filter(|cost| cost.amount_status == "confirmed" && cost.billing_status == "unbilled")
        .collect::<Vec<_>>();
    let flat_fee_cents = attorney_rates::flat_fee_cents(pg, project_id).await?;

    // The zero-unbilled no-op: with no fee agreed and nothing unbilled there
    // is nothing to compose, and the provider is never called — a repeat of a
    // successful generation lands here, not on Xero. A fee agreement always
    // bills, even on a matter that tracks no time.
    if flat_fee_cents.is_none() && entries.is_empty() && unbilled_costs.is_empty() {
        return Err(InvoiceGenerationError::NothingToInvoice);
    }
    let compose = compose_lines(&entries, &unbilled_costs, flat_fee_cents)?;

    let Some(party) = billed_party(surreal, project_id).await? else {
        return Err(InvoiceGenerationError::NoBilledParty);
    };
    // Resolve the billed party first — the seam's documented order — then
    // raise the invoice idempotent per matter.
    provider
        .ensure_contact(&billing::ContactRequest {
            name: party.name.clone(),
            email: party.email.clone(),
        })
        .await?;
    let reference = matter_reference(&project.code);
    let invoice = provider
        .create_invoice(
            project_id,
            &InvoiceRequest {
                contact_name: party.name,
                contact_email: party.email,
                reference: reference.clone(),
                line_items: compose.line_items.clone(),
                due_date: None,
            },
        )
        .await?;

    // Mirror first (the costs' link validates against it), then flip the
    // consumed work through its own audited transitions. A refusal anywhere
    // below leaves the earlier local steps describing what Xero genuinely
    // holds, and every flip is replay-safe under the same invoice id.
    xero_invoices::upsert(
        surreal,
        &xero_invoices::UpsertXeroInvoice {
            project_id,
            xero_invoice_id: invoice.0.clone(),
            reference: reference.clone(),
            status: "AUTHORISED".to_string(),
            amount_cents: compose.total_cents,
            currency: INVOICE_CURRENCY.to_string(),
            issued_at: now,
            due_at: None,
        },
    )
    .await
    .map_err(Box::new)?;
    if !entries.is_empty() {
        let ids = entries.iter().map(|entry| entry.id).collect::<Vec<_>>();
        time_entries::mark_invoiced(pg, &ids, actor_id, &invoice.0).await?;
    }
    for cost in &unbilled_costs {
        costs::link_invoice(surreal, pg, cost.id, actor_id, &invoice.0).await?;
    }

    Ok(InvoiceReceipt {
        xero_invoice_id: invoice.0,
        reference,
        currency: INVOICE_CURRENCY.to_string(),
        total_cents: compose.total_cents,
        line_items: compose.line_items,
        consumed_time_entries: entries.len(),
        consumed_costs: unbilled_costs.len(),
        flat_fee: flat_fee_cents.is_some(),
    })
}

fn refused(status: StatusCode, reason: &'static str) -> Response {
    (
        status,
        Json(serde_json::json!({
            "error": "invoice_generation_refused",
            "reason": reason,
        })),
    )
        .into_response()
}

/// Map a generation refusal to its response: a 5xx logs first — the store or
/// the provider failed and the matter's state is untouched — while a 4xx is
/// the matter's own state speaking.
fn error(error: &InvoiceGenerationError, project_id: Uuid) -> Response {
    let (status, reason) = match error {
        InvoiceGenerationError::NothingToInvoice => (StatusCode::CONFLICT, "nothing_to_invoice"),
        InvoiceGenerationError::CurrencyMismatch(_) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "currency_mismatch")
        }
        InvoiceGenerationError::NoBilledParty => {
            (StatusCode::UNPROCESSABLE_ENTITY, "no_billed_party")
        }
        InvoiceGenerationError::NoMatter => (StatusCode::NOT_FOUND, "no_matter"),
        InvoiceGenerationError::Time(time_entries::TimeEntryError::Conflict)
        | InvoiceGenerationError::Cost(costs::ProjectCostError::Conflict) => {
            (StatusCode::CONFLICT, "work_moved_under_another_invoice")
        }
        InvoiceGenerationError::Billing(billing::BillingError::Provider(_)) => {
            // The provider refused: the caller retries the same command. The
            // failure names the upstream in the log, never in the response.
            tracing::warn!(
                event = "invoice_generation_provider_failed",
                outcome = "provider_error",
                project_id = %project_id,
                store_error = %error
            );
            return refused(StatusCode::BAD_GATEWAY, "provider_refused");
        }
        InvoiceGenerationError::Time(time_entries::TimeEntryError::Invalid(_))
        | InvoiceGenerationError::Time(time_entries::TimeEntryError::NotParticipant)
        | InvoiceGenerationError::Cost(costs::ProjectCostError::Invalid(_)) => {
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_work")
        }
        InvoiceGenerationError::Time(time_entries::TimeEntryError::NotFound)
        | InvoiceGenerationError::Cost(costs::ProjectCostError::NotFound) => {
            (StatusCode::NOT_FOUND, "work_not_found")
        }
        // Store outages and every remaining store fault: the generation is
        // down, not refused.
        InvoiceGenerationError::Time(_)
        | InvoiceGenerationError::Fee(_)
        | InvoiceGenerationError::Cost(_)
        | InvoiceGenerationError::Mirror(_)
        | InvoiceGenerationError::Project(_)
        | InvoiceGenerationError::Person(_) => {
            tracing::warn!(
                event = "invoice_generation_store_failed",
                outcome = "store_error",
                project_id = %project_id,
                store_error = %error
            );
            return refused(StatusCode::SERVICE_UNAVAILABLE, "store_error");
        }
    };
    if status.is_server_error() {
        tracing::warn!(
            event = "invoice_generation_failed",
            outcome = "store_error",
            project_id = %project_id,
            store_error = %error
        );
    }
    refused(status, reason)
}

async fn postgres(
    operation: &'static str,
    project_id: Uuid,
) -> Result<store::postgres::PostgresDb, Response> {
    store::postgres::PostgresDb::from_env().await.map_err(|_| {
        tracing::warn!(
            event = "invoice_generation_store_unavailable",
            operation,
            outcome = "unavailable",
            project_id = %project_id
        );
        refused(StatusCode::SERVICE_UNAVAILABLE, "postgres_unavailable")
    })
}

/// Open the billing tables with their dependency rows mirrored — the
/// generation checks the parent Project row and the participants in
/// PostgreSQL, so a matter or participant added before the mirror was enabled
/// would otherwise be wrongly refused there. The same preparation
/// [`crate::time_tracking_api`] applies to its doors.
async fn prepared_store(
    state: &ApiState,
    operation: &'static str,
    project_id: Uuid,
) -> Result<store::postgres::PostgresDb, Response> {
    let pg = postgres(operation, project_id).await?;
    let mirrored = async {
        postgres_mirror::mirror(
            &state.surreal,
            &pg,
            &postgres_mirror::MirrorKey::Project(project_id),
        )
        .await?;
        let participations = projects::participations_for_project(&state.surreal, project_id)
            .await
            .map_err(|_| postgres_mirror::MirrorError::Source)?;
        for role in participations {
            postgres_mirror::mirror(
                &state.surreal,
                &pg,
                &postgres_mirror::MirrorKey::ProjectParticipation(role.id),
            )
            .await?;
        }
        Ok::<_, postgres_mirror::MirrorError>(())
    }
    .await;
    if let Err(store_error) = mirrored {
        tracing::warn!(
            event = "invoice_generation_mirror_failed",
            operation,
            outcome = "store_error",
            project_id = %project_id,
            store_error = %store_error
        );
        return Err(refused(
            StatusCode::SERVICE_UNAVAILABLE,
            "postgres_unavailable",
        ));
    }
    Ok(pg)
}

/// `POST /app/api/projects/{id}/invoices` — generate the matter's invoice
/// from its unbilled work.
pub(crate) async fn generate(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(project_id): Path<Uuid>,
) -> Result<Response, ApiError> {
    let actor_id = session.person_id.ok_or(ApiError::Forbidden)?;
    let viewer =
        store::access::matter_viewer(&state.surreal, session.person_id, session.role, project_id)
            .await
            .map_err(ApiError::Db)?;
    if !viewer.is_some_and(|viewer| viewer.is_firm_side()) {
        return Err(ApiError::NotFound);
    }
    let pg = match prepared_store(&state, "generate", project_id).await {
        Ok(pg) => pg,
        Err(response) => return Ok(response),
    };
    match generate_invoice(
        &state.surreal,
        &pg,
        state.billing_provider.as_ref(),
        project_id,
        actor_id,
        Utc::now(),
    )
    .await
    {
        Ok(receipt) => Ok(Json(receipt).into_response()),
        Err(e) => Ok(error(&e, project_id)),
    }
}
