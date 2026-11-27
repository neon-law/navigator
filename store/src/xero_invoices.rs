//! Xero invoice mirror helpers.
//!
//! A matter's invoice is raised in Xero by a lawyer at the price agreed
//! with the client. Navigator mirrors the invoice so the portal can show it
//! without calling Xero live. A matter may have many invoices.
//!
//! [`upsert`] refreshes invoice identity, status, total, and dates while
//! preserving `amount_paid_cents`; [`record_reconcile`] records Xero's
//! status and paid amount. The Xero invoice id is the row's stable primary
//! key, so concurrent upserts address the same row. PostgreSQL is the
//! accounting authority; Source access remains in the explicit compatibility
//! path.

use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::accounting as authority;

/// How a Xero `Reference` names the Project an invoice bills.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectScopeRef {
    /// `Matter <uuid>` — the Project's store id.
    Id(Uuid),
    /// `Matter <code>` — the Project's immutable code.
    Code(String),
}

/// Parse the documented Xero `Reference` (`Matter <project uuid or code>`).
///
/// `None` when the prefix is missing or the remainder is neither a UUID nor
/// a valid Project code — ingest counts those as unscoped and writes no row.
#[must_use]
pub fn project_scope_from_reference(reference: &str) -> Option<ProjectScopeRef> {
    let token = reference.trim().strip_prefix("Matter ")?.trim();
    if token.is_empty() {
        return None;
    }
    if let Ok(id) = Uuid::parse_str(token) {
        return Some(ProjectScopeRef::Id(id));
    }
    crate::projects::is_valid_code(token).then(|| ProjectScopeRef::Code(token.to_string()))
}

/// Resolve [`project_scope_from_reference`] against the store. `None` when
/// the reference does not parse or no live Project carries that id or code.
///
/// # Errors
///
/// [`XeroInvoiceError::Projects`] when the lookup itself fails.
pub async fn resolve_project_scope(
    db: &ApplicationStore,
    reference: &str,
) -> Result<Option<Uuid>, XeroInvoiceError> {
    match project_scope_from_reference(reference) {
        Some(ProjectScopeRef::Id(id)) => Ok(crate::projects::find_by_id(db, id)
            .await?
            .map(|project| project.id)),
        Some(ProjectScopeRef::Code(code)) => Ok(crate::projects::find_by_code(db, &code)
            .await?
            .map(|project| project.id)),
        None => Ok(None),
    }
}

/// One local mirror of a Xero invoice.
///
/// The application-facing shape: plain Rust types, no engine handles. The
/// Xero invoice id doubles as this row's own record id (see the module
/// docs), so it is the row's identity — there is no separate mirror-local
/// id. [`XeroInvoiceRow`] is the seam that turns it into what the SDK reads
/// and writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct XeroInvoice {
    /// The matter this invoice bills. Several invoices may share one.
    pub project_id: Uuid,
    /// Xero `InvoiceID` (GUID) returned on create, and this row's own record
    /// id. Internal — never surfaced on a client-facing response.
    pub xero_invoice_id: String,
    /// The invoice-level `Reference` carried into Xero (`Matter <project_id>`).
    pub reference: String,
    /// Xero invoice status mirror (`AUTHORISED`, `PAID`, `VOIDED`, …).
    pub status: String,
    /// Invoice total in minor units (cents). Avoids float.
    pub amount_cents: i64,
    /// Amount paid in minor units (cents); `0` until reconciled.
    pub amount_paid_cents: i64,
    /// ISO 4217 currency code (for example, `USD`).
    pub currency: String,
    /// When Xero raised the invoice (Xero `Date`).
    pub issued_at: DateTime<Utc>,
    /// When it falls due, if Xero recorded one (Xero `DueDate`).
    pub due_at: Option<DateTime<Utc>>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The invoice mirror row as PostgreSQL's `to_jsonb` serves it. The row's
/// `id` is the Xero invoice id itself, so it adds nothing to decode.
#[derive(serde::Deserialize)]
struct PgXeroInvoiceRow {
    project_id: Uuid,
    xero_invoice_id: String,
    reference: String,
    status: String,
    amount_cents: i64,
    amount_paid_cents: i64,
    currency: String,
    issued_at: DateTime<Utc>,
    due_at: Option<DateTime<Utc>>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgXeroInvoiceRow> for XeroInvoice {
    fn from(row: PgXeroInvoiceRow) -> Self {
        XeroInvoice {
            project_id: row.project_id,
            xero_invoice_id: row.xero_invoice_id,
            reference: row.reference,
            status: row.status,
            amount_cents: row.amount_cents,
            amount_paid_cents: row.amount_paid_cents,
            currency: row.currency,
            issued_at: row.issued_at,
            due_at: row.due_at,
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

/// Errors reading or writing a Xero invoice mirror.
///
/// This is deliberately not a bare [`surrealdb::Error`]: `portal`, `webapp`,
/// and `billing-workflows` consume this module without depending on the
/// SurrealDB crate.
#[derive(Debug, Error)]
pub enum XeroInvoiceError {
    /// A database operation failed.
    /// Reading the Projects a Firm owns, or their DRIs, failed —
    /// [`for_firm_since`] only.
    #[error(transparent)]
    Projects(#[from] crate::projects::ProjectStoreError),
    /// Resolving a brand key to its display name failed —
    /// [`firm_thirty_day_rollup`] only.
    #[error(transparent)]
    Brands(#[from] crate::brands::BrandError),
    /// Another writer created this Xero invoice's mirror row after this
    /// writer checked. [`upsert`] resolves this race by applying the
    /// metadata update to that row directly, since its id is the Xero
    /// invoice id.
    #[error("that Xero invoice is already mirrored")]
    InvoiceTaken,
    /// A write claimed success but returned no usable row.
    #[error("writing a Xero invoice mirror returned no usable row")]
    WriteReturnedNothing,
    /// PostgreSQL, under the accounting authority, failed.
    #[error(transparent)]
    Postgres(#[from] crate::postgres::accounting::AccountingAuthorityError),
}

/// The fields captured when a Xero invoice is mirrored locally. `currency`
/// defaults to `USD` at the call site; amounts are minor units (cents).
#[derive(Clone, Debug)]
pub struct UpsertXeroInvoice {
    pub project_id: Uuid,
    pub xero_invoice_id: String,
    pub reference: String,
    /// Xero invoice status at create time (`AUTHORISED`).
    pub status: String,
    pub amount_cents: i64,
    pub currency: String,
    /// When Xero raised the invoice (Xero `Date`).
    pub issued_at: DateTime<Utc>,
    /// When it falls due, if Xero recorded one (Xero `DueDate`).
    pub due_at: Option<DateTime<Utc>>,
}

/// The mirror row for one Xero invoice, addressed by the record id its own
/// Xero invoice id doubles as.
async fn for_xero_id(
    _db: &ApplicationStore,
    xero_invoice_id: &str,
) -> Result<Option<XeroInvoice>, XeroInvoiceError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(xero_invoice) AS row FROM xero_invoice \
         WHERE xero_invoice_id = $1 LIMIT 1",
    )
    .bind(xero_invoice_id);
    Ok(authority::maybe::<PgXeroInvoiceRow>(&pg, query)
        .await?
        .map(Into::into))
}

/// Idempotently mirror a raised Xero invoice, keyed on its own Xero invoice
/// id.
///
/// Inserts a fresh row, or — when one already exists for that Xero invoice —
/// updates the reference / status / total / dates in place while
/// **preserving** the reconciled `amount_paid_cents` (the reconcile workflow
/// owns that field). Two different Xero invoices for the same matter are two
/// rows; the create path settles a race on the mirror's own record id (see
/// the module docs): a competing writer's `CREATE` under the same
/// Xero-invoice-id-derived id is the row this one applies its metadata-only
/// update to, with no extra read needed to find it.
///
/// # Errors
///
/// [`XeroInvoiceError::Db`] when SurrealDB cannot read or write the mirror.
pub async fn upsert(
    _db: &ApplicationStore,
    input: &UpsertXeroInvoice,
) -> Result<XeroInvoice, XeroInvoiceError> {
    let pg = authority::authority().await?;
    // `amount_paid_cents` is deliberately absent from the DO UPDATE set:
    // the reconcile workflow owns that field, exactly as the SurrealDB
    // update path below preserves it.
    let row = serde_json::json!({
        "id": input.xero_invoice_id, "project_id": input.project_id,
        "xero_invoice_id": input.xero_invoice_id, "reference": input.reference,
        "status": input.status, "amount_cents": input.amount_cents,
        "amount_paid_cents": 0, "currency": input.currency,
        "issued_at": input.issued_at, "due_at": input.due_at,
    });
    let query = sqlx::query(
        "INSERT INTO xero_invoice SELECT * FROM \
         jsonb_populate_record(NULL::xero_invoice, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         ON CONFLICT (id) DO UPDATE SET project_id = EXCLUDED.project_id, \
         reference = EXCLUDED.reference, status = EXCLUDED.status, \
         amount_cents = EXCLUDED.amount_cents, currency = EXCLUDED.currency, \
         issued_at = EXCLUDED.issued_at, due_at = EXCLUDED.due_at, \
         updated_at = now() RETURNING to_jsonb(xero_invoice) AS row",
    )
    .bind(row);
    authority::maybe::<PgXeroInvoiceRow>(&pg, query)
        .await?
        .map(Into::into)
        .ok_or(XeroInvoiceError::WriteReturnedNothing)
}

/// Fold a reconcile result (Xero `Status` + `AmountPaid`) onto one invoice's
/// mirror row. No-op (returns `None`) when no mirror row exists yet for that
/// Xero invoice id.
///
/// # Errors
///
/// [`XeroInvoiceError::Db`] when SurrealDB cannot update the mirror.
pub async fn record_reconcile(
    _db: &ApplicationStore,
    xero_invoice_id: &str,
    status: &str,
    amount_paid_cents: i64,
) -> Result<Option<XeroInvoice>, XeroInvoiceError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE xero_invoice SET status = $1, amount_paid_cents = $2, \
         updated_at = now() WHERE xero_invoice_id = $3 \
         RETURNING to_jsonb(xero_invoice) AS row",
    )
    .bind(status)
    .bind(amount_paid_cents)
    .bind(xero_invoice_id);
    Ok(authority::maybe::<PgXeroInvoiceRow>(&pg, query)
        .await?
        .map(Into::into))
}

/// Resolve the invoice an IOLTA withdrawal line names.
///
/// A Xero withdrawal line references the invoice it settles either by the
/// `InvoiceID` — which is this mirror's own record id — or by the human
/// reference the firm put on the invoice. The id is tried first because it
/// is unique by construction; the reference is a fallback and is honoured
/// **only when exactly one** mirrored invoice carries it. Two invoices
/// sharing a reference make the line ambiguous, and an ambiguous allocation
/// is answered `None` so the caller refuses the whole withdrawal rather than
/// crediting a coin-flip matter.
///
/// # Errors
///
/// [`XeroInvoiceError::Db`] when a lookup fails.
pub async fn find_for_allocation(
    db: &ApplicationStore,
    reference: &str,
) -> Result<Option<XeroInvoice>, XeroInvoiceError> {
    if let Some(by_id) = for_xero_id(db, reference).await? {
        return Ok(Some(by_id));
    }
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(xero_invoice) AS row FROM xero_invoice \
         WHERE reference = $1 LIMIT 2",
    )
    .bind(reference);
    let mut matches = authority::rows::<PgXeroInvoiceRow>(&pg, query).await?;
    if matches.len() == 1 {
        Ok(Some(matches.remove(0).into()))
    } else {
        Ok(None)
    }
}

/// Fetch every invoice mirrored for a set of matters, newest first, for the
/// project-scoped portal invoice list. Empty input short-circuits to an
/// empty vec.
///
/// # Errors
///
/// [`XeroInvoiceError::Db`] if the lookup fails.
pub async fn for_projects(
    _db: &ApplicationStore,
    project_ids: &[Uuid],
) -> Result<Vec<XeroInvoice>, XeroInvoiceError> {
    if project_ids.is_empty() {
        return Ok(Vec::new());
    }
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(xero_invoice) AS row FROM xero_invoice \
         WHERE project_id = ANY($1) ORDER BY issued_at DESC",
    )
    .bind(project_ids.to_vec());
    Ok(authority::rows::<PgXeroInvoiceRow>(&pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

/// One invoice as the Firm-wide invoice graphs (ENG-591) consume it: the
/// mirror's own fields plus the Project's brand and lawyer DRI names, so
/// that caller never re-joins per row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FirmInvoice {
    pub project_id: Uuid,
    pub xero_invoice_id: String,
    pub status: String,
    pub amount_cents: i64,
    pub amount_paid_cents: i64,
    pub issued_at: DateTime<Utc>,
    /// ISO 4217 currency code. The rollup ([`firm_thirty_day_rollup`]) groups
    /// on this rather than assuming every invoice shares one currency.
    pub currency: String,
    /// The Project's brand key, for grouping by brand.
    pub brand: String,
    /// The Project's lawyer DRI names, alphabetical, or empty when
    /// unassigned — mirrors `store::projects::MatterDirectoryEntry`.
    pub lawyer_dris: Vec<String>,
}

/// Every invoice issued on or after `since`, for every Project a Firm owns —
/// the read the Firm show page's invoice graphs (ENG-591) consume.
///
/// # Errors
///
/// [`XeroInvoiceError::Db`] if the lookup fails; [`XeroInvoiceError::Projects`]
/// if reading the Firm's Projects or their lawyer DRIs fails.
pub async fn for_firm_since(
    db: &ApplicationStore,
    firm_id: Uuid,
    since: DateTime<Utc>,
) -> Result<Vec<FirmInvoice>, XeroInvoiceError> {
    let brand_by_project: std::collections::HashMap<Uuid, String> = crate::projects::all(db)
        .await?
        .into_iter()
        .filter(|project| project.firm_id == Some(firm_id))
        .map(|project| (project.id, project.brand))
        .collect();
    if brand_by_project.is_empty() {
        return Ok(Vec::new());
    }
    let project_ids: Vec<Uuid> = brand_by_project.keys().copied().collect();
    let lawyer_dris =
        crate::projects::dri_names_by_project(db, crate::projects::DriSide::Lawyer).await?;

    Ok(for_projects(db, &project_ids)
        .await?
        .into_iter()
        .filter(|invoice| invoice.issued_at >= since)
        .filter_map(|invoice| {
            let brand = brand_by_project.get(&invoice.project_id)?.clone();
            Some(FirmInvoice {
                lawyer_dris: lawyer_dris
                    .get(&invoice.project_id)
                    .cloned()
                    .unwrap_or_default(),
                brand,
                project_id: invoice.project_id,
                xero_invoice_id: invoice.xero_invoice_id,
                status: invoice.status,
                amount_cents: invoice.amount_cents,
                amount_paid_cents: invoice.amount_paid_cents,
                issued_at: invoice.issued_at,
                currency: invoice.currency,
            })
        })
        .collect())
}

/// One label's (a brand's, or a lawyer DRI's) summed cents within a
/// [`CurrencyInvoiceRollup`] — one bar-pair in the Firm show page's graphs
/// (ENG-591).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InvoiceGroupTotal {
    /// A brand's display name, a lawyer DRI's display name, or
    /// `"Unassigned"` — never a Project code, matter name, or Xero id.
    pub label: String,
    pub invoiced_cents: i64,
    pub paid_cents: i64,
}

/// The trailing-window rollup for one currency. Kept separate per currency
/// because summing across currencies would silently misreport a total —
/// [`firm_thirty_day_rollup`] never does that.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CurrencyInvoiceRollup {
    pub currency: String,
    pub total_invoiced_cents: i64,
    pub total_paid_cents: i64,
    /// Sorted by label for a deterministic render.
    pub by_brand: Vec<InvoiceGroupTotal>,
    /// Sorted by label for a deterministic render.
    pub by_lawyer_dri: Vec<InvoiceGroupTotal>,
}

/// The Firm show page's trailing-30-day invoice rollup (ENG-591): one
/// [`CurrencyInvoiceRollup`] per currency that appeared in the window. Empty
/// when no invoice on any of the Firm's Projects falls in the window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Default)]
pub struct FirmInvoiceRollup {
    pub currencies: Vec<CurrencyInvoiceRollup>,
}

const UNASSIGNED_DRI: &str = "Unassigned";

/// `label -> (invoiced_cents, paid_cents)`, accumulated across every invoice
/// seen so far for one group axis (brand, or lawyer DRI).
type GroupTotals = std::collections::BTreeMap<String, (i64, i64)>;

/// Fold one invoice's cents into a `(label -> totals)` map, creating the
/// entry on first sight.
fn accumulate(totals: &mut GroupTotals, label: &str, invoiced_cents: i64, paid_cents: i64) {
    let entry = totals.entry(label.to_string()).or_insert((0, 0));
    entry.0 = entry.0.saturating_add(invoiced_cents);
    entry.1 = entry.1.saturating_add(paid_cents);
}

fn totals_into_groups(totals: GroupTotals) -> Vec<InvoiceGroupTotal> {
    totals
        .into_iter()
        .map(|(label, (invoiced_cents, paid_cents))| InvoiceGroupTotal {
            label,
            invoiced_cents,
            paid_cents,
        })
        .collect()
}

/// The Firm show page's trailing-30-day invoice rollup (ENG-591): every
/// invoice issued in `[now - 30 days, now]` on one of the Firm's Projects,
/// grouped by brand and by lawyer DRI, summed in cents, and never summed
/// across currencies.
///
/// # Errors
///
/// [`XeroInvoiceError::Db`] or [`XeroInvoiceError::Projects`] if the
/// underlying read fails ([`for_firm_since`]); [`XeroInvoiceError::Brands`]
/// if resolving the Firm's brand names fails.
pub async fn firm_thirty_day_rollup(
    db: &ApplicationStore,
    firm_id: Uuid,
    now: DateTime<Utc>,
) -> Result<FirmInvoiceRollup, XeroInvoiceError> {
    let since = now - chrono::Duration::days(30);
    let invoices = for_firm_since(db, firm_id, since).await?;
    if invoices.is_empty() {
        return Ok(FirmInvoiceRollup::default());
    }

    // A Project's brand may be a system-wide compiled key or one of this
    // Firm's own scoped brands (ENG-587 validates `project.brand` against
    // either), so each key is resolved individually via `find_by_key` rather
    // than assumed to be one or the other.
    let mut brand_name_by_key: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for key in invoices
        .iter()
        .map(|invoice| invoice.brand.as_str())
        .collect::<std::collections::BTreeSet<_>>()
    {
        if let Some(brand) = crate::brands::find_by_key(db, key).await? {
            brand_name_by_key.insert(key.to_string(), brand.name);
        }
    }

    let mut by_currency: std::collections::BTreeMap<String, (GroupTotals, GroupTotals)> =
        std::collections::BTreeMap::new();

    for invoice in &invoices {
        let (by_brand, by_lawyer_dri) = by_currency.entry(invoice.currency.clone()).or_default();
        let brand_label = brand_name_by_key
            .get(&invoice.brand)
            .cloned()
            .unwrap_or_else(|| invoice.brand.clone());
        accumulate(
            by_brand,
            &brand_label,
            invoice.amount_cents,
            invoice.amount_paid_cents,
        );
        if invoice.lawyer_dris.is_empty() {
            accumulate(
                by_lawyer_dri,
                UNASSIGNED_DRI,
                invoice.amount_cents,
                invoice.amount_paid_cents,
            );
        } else {
            for name in &invoice.lawyer_dris {
                accumulate(
                    by_lawyer_dri,
                    name,
                    invoice.amount_cents,
                    invoice.amount_paid_cents,
                );
            }
        }
    }

    Ok(FirmInvoiceRollup {
        currencies: by_currency
            .into_iter()
            .map(|(currency, (by_brand, by_lawyer_dri))| {
                let total_invoiced_cents = by_brand.values().map(|(i, _)| *i).sum();
                let total_paid_cents = by_brand.values().map(|(_, p)| *p).sum();
                CurrencyInvoiceRollup {
                    currency,
                    total_invoiced_cents,
                    total_paid_cents,
                    by_brand: totals_into_groups(by_brand),
                    by_lawyer_dri: totals_into_groups(by_lawyer_dri),
                }
            })
            .collect(),
    })
}

/// The mirror rows that the nightly reconcile should re-check: anything not
/// already in a terminal state (`PAID` / `VOIDED`). A settled invoice is never
/// polled again.
///
/// # Errors
///
/// [`XeroInvoiceError::Db`] if the lookup fails.
pub async fn needing_reconcile(
    _db: &ApplicationStore,
) -> Result<Vec<XeroInvoice>, XeroInvoiceError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(xero_invoice) AS row FROM xero_invoice \
         WHERE status NOT IN ('PAID', 'VOIDED')",
    );
    Ok(authority::rows::<PgXeroInvoiceRow>(&pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{
        firm_thirty_day_rollup, for_firm_since, for_projects, needing_reconcile,
        project_scope_from_reference, record_reconcile, upsert, ProjectScopeRef, UpsertXeroInvoice,
    };
    use crate::application_store::ApplicationStore;
    use chrono::{Duration, TimeZone, Utc};

    async fn seed_project(application_store: &ApplicationStore, name: &str) -> uuid::Uuid {
        crate::test_support::seed_compiled_brands_postgres(application_store).await;
        crate::test_support::seed_project_id(application_store, name).await
    }

    fn input(project_id: uuid::Uuid, xero_id: &str, cents: i64) -> UpsertXeroInvoice {
        UpsertXeroInvoice {
            project_id,
            xero_invoice_id: xero_id.into(),
            reference: format!("Matter {project_id}"),
            status: "AUTHORISED".into(),
            amount_cents: cents,
            currency: "USD".into(),
            issued_at: Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(),
            due_at: None,
        }
    }

    #[test]
    fn project_scope_from_reference_reads_uuid_code_or_nothing() {
        let id = uuid::Uuid::from_u128(1);
        assert_eq!(
            project_scope_from_reference(&format!("Matter {id}")),
            Some(ProjectScopeRef::Id(id))
        );
        assert_eq!(
            project_scope_from_reference("Matter sample-matter"),
            Some(ProjectScopeRef::Code("sample-matter".into()))
        );
        assert_eq!(
            project_scope_from_reference("  Matter sample-matter  "),
            Some(ProjectScopeRef::Code("sample-matter".into()))
        );
        assert_eq!(project_scope_from_reference("sample-matter"), None);
        assert_eq!(project_scope_from_reference("Invoice 12"), None);
        assert_eq!(project_scope_from_reference("Matter "), None);
        assert_eq!(project_scope_from_reference("Matter not a code!"), None);
    }

    #[tokio::test]
    async fn upsert_inserts_one_row() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = seed_project(&application_store, "sample-matter").await;

            let row = upsert(&application_store, &input(project_id, "xero-1", 333_300))
                .await
                .unwrap();
            assert_eq!(row.project_id, project_id);
            assert_eq!(row.xero_invoice_id, "xero-1");
            assert_eq!(row.amount_cents, 333_300);
            assert_eq!(row.amount_paid_cents, 0);

            assert_eq!(
                for_projects(&application_store, &[project_id])
                    .await
                    .unwrap()
                    .len(),
                1
            );
        })
        .await;
    }

    /// ENG-588: a matter carries many invoices over time — two different
    /// Xero ids for one Project must land as two rows, not a collision.
    #[tokio::test]
    async fn two_different_invoices_for_one_matter_are_two_rows() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = seed_project(&application_store, "sample-matter").await;

            upsert(&application_store, &input(project_id, "xero-1", 10_000))
                .await
                .unwrap();
            upsert(&application_store, &input(project_id, "xero-2", 20_000))
                .await
                .unwrap();

            let rows = for_projects(&application_store, &[project_id])
                .await
                .unwrap();
            assert_eq!(rows.len(), 2, "two Xero ids must mirror as two rows");
            let ids: std::collections::BTreeSet<&str> =
                rows.iter().map(|r| r.xero_invoice_id.as_str()).collect();
            assert_eq!(ids, ["xero-1", "xero-2"].into_iter().collect());
        })
        .await;
    }

    #[tokio::test]
    async fn invoice_id_index_refuses_a_second_row_for_the_same_invoice() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = seed_project(&application_store, "sample-matter").await;
            upsert(&application_store, &input(project_id, "xero-1", 333_300))
                .await
                .unwrap();

            let pg = crate::postgres::PostgresDb::application().await.unwrap();
            let error = sqlx::query(
                "INSERT INTO xero_invoice (id, project_id, xero_invoice_id, reference, status, \
             amount_cents, amount_paid_cents, currency, issued_at, inserted_at, updated_at) \
             VALUES ($1, $2, $1, 'duplicate', 'AUTHORISED', 333300, 0, 'USD', now(), now(), now())",
            )
            .bind("xero-1")
            .bind(project_id)
            .execute(pg.pool())
            .await
            .expect_err("a second row with the same Xero invoice key must be refused");
            assert!(
                matches!(error, sqlx::Error::Database(ref database)
                if database.is_unique_violation()
                    && database.constraint() == Some("xero_invoice_pkey")),
                "expected the xero_invoice primary key to refuse a duplicate, got: {error}"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn upsert_is_idempotent_on_xero_invoice_id() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = seed_project(&application_store, "sample-matter").await;

            upsert(&application_store, &input(project_id, "xero-1", 333_300))
                .await
                .unwrap();
            let updated = upsert(&application_store, &input(project_id, "xero-1", 444_400))
                .await
                .unwrap();
            assert_eq!(updated.amount_cents, 444_400, "the repeat updates the row");

            assert_eq!(
                for_projects(&application_store, &[project_id])
                    .await
                    .unwrap()
                    .len(),
                1,
                "a repeated Xero id must update in place, not insert a second row"
            );
        })
        .await;
    }

    /// The mirror's own record id is the concurrency boundary (see the
    /// module docs), not merely an invariant observed after sequential
    /// calls. Every mirror replay must settle on the one winner rather than
    /// surface its create race to Restate.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_upserts_for_one_invoice_settle_on_one_row() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = seed_project(&application_store, "sample-matter").await;
            let input = input(project_id, "xero-1", 333_300);
            let pg = crate::postgres::PostgresDb::application().await.unwrap();

            let racers: Vec<_> = (0..8)
                .map(|_| {
                    let application_store = application_store.clone();
                    let input = input.clone();
                    let pg = pg.clone();
                    tokio::spawn(crate::postgres::PostgresDb::with_test_db(pg, async move {
                        upsert(&application_store, &input).await
                    }))
                })
                .collect();

            let mut ids = std::collections::BTreeSet::new();
            for (number, racer) in racers.into_iter().enumerate() {
                let row = racer.await.expect("racer task").unwrap_or_else(|error| {
                    panic!("racer {number} was refused instead of settling: {error:?}")
                });
                ids.insert(row.xero_invoice_id);
            }

            assert_eq!(ids.len(), 1, "the racers disagreed about which row won");
            assert_eq!(
                for_projects(&application_store, &[project_id])
                    .await
                    .unwrap()
                    .len(),
                1,
                "a race must not leave a second mirror row behind",
            );
        })
        .await;
    }

    #[tokio::test]
    async fn upsert_preserves_reconciled_amount_paid() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = seed_project(&application_store, "sample-matter").await;

            upsert(&application_store, &input(project_id, "xero-1", 333_300))
                .await
                .unwrap();
            record_reconcile(&application_store, "xero-1", "PAID", 333_300)
                .await
                .unwrap();
            let row = upsert(&application_store, &input(project_id, "xero-1", 333_300))
                .await
                .unwrap();
            assert_eq!(row.amount_paid_cents, 333_300);
            assert_eq!(row.status, "AUTHORISED", "raise resets the create-status");
        })
        .await;
    }

    #[tokio::test]
    async fn record_reconcile_updates_status_and_paid() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = seed_project(&application_store, "sample-matter").await;
            upsert(&application_store, &input(project_id, "xero-1", 333_300))
                .await
                .unwrap();

            let row = record_reconcile(&application_store, "xero-1", "PAID", 333_300)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.status, "PAID");
            assert_eq!(row.amount_paid_cents, 333_300);
        })
        .await;
    }

    /// Two invoices on one matter reconcile independently — folding a result
    /// onto one must not touch the other's status or paid amount.
    #[tokio::test]
    async fn record_reconcile_only_touches_its_own_invoice() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let project_id = seed_project(&application_store, "sample-matter").await;
            upsert(&application_store, &input(project_id, "xero-1", 10_000))
                .await
                .unwrap();
            upsert(&application_store, &input(project_id, "xero-2", 20_000))
                .await
                .unwrap();

            record_reconcile(&application_store, "xero-1", "PAID", 10_000)
                .await
                .unwrap();

            let rows = for_projects(&application_store, &[project_id])
                .await
                .unwrap();
            let one = rows.iter().find(|r| r.xero_invoice_id == "xero-1").unwrap();
            let two = rows.iter().find(|r| r.xero_invoice_id == "xero-2").unwrap();
            assert_eq!(one.status, "PAID");
            assert_eq!(two.status, "AUTHORISED", "the other invoice is untouched");
            assert_eq!(two.amount_paid_cents, 0);
        })
        .await;
    }

    #[tokio::test]
    async fn record_reconcile_is_noop_without_a_row() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            assert!(
                record_reconcile(&application_store, "no-such-invoice", "PAID", 100)
                    .await
                    .unwrap()
                    .is_none()
            );
        })
        .await;
    }

    #[tokio::test]
    async fn needing_reconcile_excludes_settled_invoices() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let open = seed_project(&application_store, "open").await;
            let paid = seed_project(&application_store, "paid").await;
            let void = seed_project(&application_store, "void").await;
            upsert(&application_store, &input(open, "x-open", 100))
                .await
                .unwrap();
            upsert(&application_store, &input(paid, "x-paid", 200))
                .await
                .unwrap();
            upsert(&application_store, &input(void, "x-void", 300))
                .await
                .unwrap();
            record_reconcile(&application_store, "x-paid", "PAID", 200)
                .await
                .unwrap();
            record_reconcile(&application_store, "x-void", "VOIDED", 0)
                .await
                .unwrap();

            let rows = needing_reconcile(&application_store).await.unwrap();
            assert_eq!(rows.len(), 1, "only the AUTHORISED invoice is re-checked");
            assert_eq!(rows[0].project_id, open);
        })
        .await;
    }

    #[tokio::test]
    async fn for_projects_filters_to_the_requested_matters_and_orders_newest_first() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let a = seed_project(&application_store, "a").await;
            let b = seed_project(&application_store, "b").await;
            let c = seed_project(&application_store, "c").await;
            let mut older = input(a, "xero-a-older", 100);
            older.issued_at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
            let mut newer = input(a, "xero-a-newer", 150);
            newer.issued_at = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
            upsert(&application_store, &older).await.unwrap();
            upsert(&application_store, &newer).await.unwrap();
            upsert(&application_store, &input(b, "xero-b", 200))
                .await
                .unwrap();
            upsert(&application_store, &input(c, "xero-c", 300))
                .await
                .unwrap();

            let rows = for_projects(&application_store, &[a, c]).await.unwrap();
            assert_eq!(rows.len(), 3);
            assert!(rows
                .iter()
                .all(|row| row.project_id == a || row.project_id == c));
            assert_eq!(
                rows[0].xero_invoice_id, "xero-a-newer",
                "newest issued invoice sorts first"
            );
            assert!(for_projects(&application_store, &[])
                .await
                .unwrap()
                .is_empty());
        })
        .await;
    }

    /// ENG-588: the read `for_firm_since` exists for (ENG-591's Firm invoice
    /// graphs) scopes to one Firm's Projects and excludes anything issued
    /// before the window, joining in each invoice's brand and lawyer DRI.
    #[tokio::test]
    async fn for_firm_since_scopes_to_the_firm_and_the_window() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            crate::test_support::seed_compiled_brands_postgres(&application_store).await;
            let admin = crate::test_support::ensure_person(
                &application_store,
                &crate::persons::NewPerson::with_role(
                    "Firm Admin",
                    "firm-admin@example.com",
                    crate::persons::Role::Admin,
                ),
            )
            .await;
            let firm = crate::firms::create(
                &application_store,
                &crate::firms::NewFirm {
                    name: "Invoice Firm".to_string(),
                    status: "active".to_string(),
                    entity_id: crate::test_support::seed_entity(&application_store).await,
                    admin_dri_person_id: admin.id,
                },
            )
            .await
            .unwrap();
            let entity_id = crate::test_support::seed_entity(&application_store).await;
            let owned = crate::projects::create(
                &application_store,
                &crate::projects::NewProject {
                    code: "firm-owned-matter".to_string(),
                    name: "Firm Owned Matter".to_string(),
                    status: "open".to_string(),
                    entity_id,
                    firm_id: Some(firm.id),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let other_entity = crate::test_support::seed_entity(&application_store).await;
            let unowned = crate::projects::create(
                &application_store,
                &crate::projects::NewProject {
                    code: "other-firm-matter".to_string(),
                    name: "Other Firm Matter".to_string(),
                    status: "open".to_string(),
                    entity_id: other_entity,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            crate::projects::designate_dri(
                &application_store,
                owned.id,
                admin.id,
                crate::projects::DriSide::Lawyer,
            )
            .await
            .unwrap();

            let since = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
            let mut too_old = input(owned.id, "xero-too-old", 100);
            too_old.issued_at = since - Duration::days(1);
            let mut in_window = input(owned.id, "xero-in-window", 200);
            in_window.issued_at = since + Duration::days(1);
            upsert(&application_store, &too_old).await.unwrap();
            upsert(&application_store, &in_window).await.unwrap();
            upsert(
                &application_store,
                &input(unowned.id, "xero-other-firm", 300),
            )
            .await
            .unwrap();

            let rows = for_firm_since(&application_store, firm.id, since)
                .await
                .unwrap();
            assert_eq!(rows.len(), 1, "only the in-window, firm-owned invoice");
            assert_eq!(rows[0].xero_invoice_id, "xero-in-window");
            assert_eq!(rows[0].brand, owned.brand);
            assert_eq!(rows[0].lawyer_dris, vec!["Firm Admin".to_string()]);
            assert_eq!(rows[0].currency, "USD");
        })
        .await;
    }

    /// One Firm, one entity, ready for a Project. Shared by the
    /// [`firm_thirty_day_rollup`] tests below.
    async fn seed_firm(application_store: &ApplicationStore) -> (crate::firms::Firm, uuid::Uuid) {
        crate::test_support::seed_compiled_brands_postgres(application_store).await;
        let admin = crate::test_support::ensure_person(
            application_store,
            &crate::persons::NewPerson::with_role(
                "Firm Admin",
                "firm-admin@example.com",
                crate::persons::Role::Admin,
            ),
        )
        .await;
        let firm = crate::firms::create(
            application_store,
            &crate::firms::NewFirm {
                name: format!("Rollup Firm {}", uuid::Uuid::now_v7()),
                status: "active".to_string(),
                entity_id: crate::test_support::seed_entity(application_store).await,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap();
        (firm, admin.id)
    }

    async fn seed_project_on_firm(
        application_store: &ApplicationStore,
        firm_id: uuid::Uuid,
        code: &str,
        brand: &str,
    ) -> crate::projects::Project {
        let entity_id = crate::test_support::seed_entity(application_store).await;
        crate::projects::create(
            application_store,
            &crate::projects::NewProject {
                code: code.to_string(),
                name: code.to_string(),
                status: "open".to_string(),
                entity_id,
                firm_id: Some(firm_id),
                brand: brand.to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
    }

    /// ENG-591: two Projects on two brands, each with its own lawyer DRI —
    /// the rollup groups and sums cents on both axes independently.
    #[tokio::test]
    async fn firm_thirty_day_rollup_groups_by_brand_and_lawyer_dri() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let (firm, _) = seed_firm(&application_store).await;
            let neon_matter =
                seed_project_on_firm(&application_store, firm.id, "neon-matter", "neon").await;
            let other_matter = seed_project_on_firm(
                &application_store,
                firm.id,
                "other-brand-matter",
                "delete-your-data",
            )
            .await;
            let lawyer_one = crate::test_support::ensure_person(
                &application_store,
                &crate::persons::NewPerson::with_role(
                    "Lawyer One",
                    "lawyer-one@example.com",
                    crate::persons::Role::Lawyer,
                ),
            )
            .await;
            let lawyer_two = crate::test_support::ensure_person(
                &application_store,
                &crate::persons::NewPerson::with_role(
                    "Lawyer Two",
                    "lawyer-two@example.com",
                    crate::persons::Role::Lawyer,
                ),
            )
            .await;
            crate::projects::designate_dri(
                &application_store,
                neon_matter.id,
                lawyer_one.id,
                crate::projects::DriSide::Lawyer,
            )
            .await
            .unwrap();
            crate::projects::designate_dri(
                &application_store,
                other_matter.id,
                lawyer_two.id,
                crate::projects::DriSide::Lawyer,
            )
            .await
            .unwrap();

            let now = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
            let mut neon_invoice = input(neon_matter.id, "xero-neon", 10_000);
            neon_invoice.issued_at = now - Duration::days(1);
            let mut other_invoice = input(other_matter.id, "xero-other", 5_000);
            other_invoice.issued_at = now - Duration::days(2);
            upsert(&application_store, &neon_invoice).await.unwrap();
            upsert(&application_store, &other_invoice).await.unwrap();
            record_reconcile(&application_store, "xero-neon", "PAID", 10_000)
                .await
                .unwrap();

            let rollup = firm_thirty_day_rollup(&application_store, firm.id, now)
                .await
                .unwrap();
            assert_eq!(rollup.currencies.len(), 1, "one currency, USD");
            let usd = &rollup.currencies[0];
            assert_eq!(usd.currency, "USD");
            assert_eq!(usd.total_invoiced_cents, 15_000);
            assert_eq!(usd.total_paid_cents, 10_000);

            assert_eq!(usd.by_brand.len(), 2);
            let neon_group = usd.by_brand.iter().find(|g| g.label == "neon").unwrap();
            assert_eq!(neon_group.invoiced_cents, 10_000);
            assert_eq!(neon_group.paid_cents, 10_000);
            let other_group = usd
                .by_brand
                .iter()
                .find(|g| g.label == "delete-your-data")
                .unwrap();
            assert_eq!(other_group.invoiced_cents, 5_000);
            assert_eq!(other_group.paid_cents, 0);

            assert_eq!(usd.by_lawyer_dri.len(), 2);
            let one_group = usd
                .by_lawyer_dri
                .iter()
                .find(|g| g.label == "Lawyer One")
                .unwrap();
            assert_eq!(one_group.invoiced_cents, 10_000);
            let two_group = usd
                .by_lawyer_dri
                .iter()
                .find(|g| g.label == "Lawyer Two")
                .unwrap();
            assert_eq!(two_group.invoiced_cents, 5_000);
        })
        .await;
    }

    /// ENG-591: a Project with no lawyer DRI groups under "Unassigned"
    /// rather than being dropped from the by-DRI series.
    #[tokio::test]
    async fn firm_thirty_day_rollup_groups_unassigned_dri_when_no_lawyer_dri() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let (firm, _) = seed_firm(&application_store).await;
            let matter =
                seed_project_on_firm(&application_store, firm.id, "unassigned-matter", "neon")
                    .await;
            let now = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
            let mut invoice = input(matter.id, "xero-unassigned", 7_500);
            invoice.issued_at = now - Duration::days(1);
            upsert(&application_store, &invoice).await.unwrap();

            let rollup = firm_thirty_day_rollup(&application_store, firm.id, now)
                .await
                .unwrap();
            let usd = &rollup.currencies[0];
            assert_eq!(usd.by_lawyer_dri.len(), 1);
            assert_eq!(usd.by_lawyer_dri[0].label, "Unassigned");
            assert_eq!(usd.by_lawyer_dri[0].invoiced_cents, 7_500);
        })
        .await;
    }

    /// ENG-591: the 30-day window is computed from `now`, not from a caller-
    /// supplied `since` — an invoice 31 days old falls outside it and one 29
    /// days old falls inside, proving the subtraction itself, not just
    /// `for_firm_since`'s own inclusive-edge behaviour.
    #[tokio::test]
    async fn firm_thirty_day_rollup_computes_the_window_from_now() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let (firm, _) = seed_firm(&application_store).await;
            let matter =
                seed_project_on_firm(&application_store, firm.id, "window-matter", "neon").await;
            let now = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
            let mut too_old = input(matter.id, "xero-too-old", 100);
            too_old.issued_at = now - Duration::days(31);
            let mut in_window = input(matter.id, "xero-in-window", 200);
            in_window.issued_at = now - Duration::days(29);
            upsert(&application_store, &too_old).await.unwrap();
            upsert(&application_store, &in_window).await.unwrap();

            let rollup = firm_thirty_day_rollup(&application_store, firm.id, now)
                .await
                .unwrap();
            let usd = &rollup.currencies[0];
            assert_eq!(
                usd.total_invoiced_cents, 200,
                "the 31-day-old invoice is excluded"
            );
        })
        .await;
    }

    /// ENG-591: invoices on another Firm's Projects never appear in this
    /// Firm's rollup.
    #[tokio::test]
    async fn firm_thirty_day_rollup_isolates_two_firms() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let (firm_a, _) = seed_firm(&application_store).await;
            let (firm_b, _) = seed_firm(&application_store).await;
            let matter_a =
                seed_project_on_firm(&application_store, firm_a.id, "firm-a-matter", "neon").await;
            let matter_b = seed_project_on_firm(
                &application_store,
                firm_b.id,
                "firm-b-matter",
                "delete-your-data",
            )
            .await;
            let now = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
            let mut invoice_a = input(matter_a.id, "xero-firm-a", 1_000);
            invoice_a.issued_at = now - Duration::days(1);
            let mut invoice_b = input(matter_b.id, "xero-firm-b", 2_000);
            invoice_b.issued_at = now - Duration::days(1);
            upsert(&application_store, &invoice_a).await.unwrap();
            upsert(&application_store, &invoice_b).await.unwrap();

            let rollup_a = firm_thirty_day_rollup(&application_store, firm_a.id, now)
                .await
                .unwrap();
            assert_eq!(rollup_a.currencies[0].total_invoiced_cents, 1_000);
            assert_eq!(rollup_a.currencies[0].by_brand[0].label, "neon");
        })
        .await;
    }

    /// ENG-591: two currencies on the same Firm never sum together — each
    /// gets its own [`CurrencyInvoiceRollup`].
    #[tokio::test]
    async fn firm_thirty_day_rollup_keeps_currencies_separate() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let (firm, _) = seed_firm(&application_store).await;
            let matter =
                seed_project_on_firm(&application_store, firm.id, "mixed-currency-matter", "neon")
                    .await;
            let now = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
            let mut usd_invoice = input(matter.id, "xero-usd", 1_000);
            usd_invoice.issued_at = now - Duration::days(1);
            let mut eur_invoice = input(matter.id, "xero-eur", 2_000);
            eur_invoice.issued_at = now - Duration::days(1);
            eur_invoice.currency = "EUR".to_string();
            upsert(&application_store, &usd_invoice).await.unwrap();
            upsert(&application_store, &eur_invoice).await.unwrap();

            let rollup = firm_thirty_day_rollup(&application_store, firm.id, now)
                .await
                .unwrap();
            assert_eq!(rollup.currencies.len(), 2, "USD and EUR stay separate");
            let usd = rollup
                .currencies
                .iter()
                .find(|c| c.currency == "USD")
                .unwrap();
            let eur = rollup
                .currencies
                .iter()
                .find(|c| c.currency == "EUR")
                .unwrap();
            assert_eq!(usd.total_invoiced_cents, 1_000);
            assert_eq!(eur.total_invoiced_cents, 2_000);
        })
        .await;
    }

    /// ENG-591: a Firm with no invoice in the window renders no currency
    /// groups at all, so the caller can distinguish "nothing to draw" from
    /// "drew a zero-height bar".
    #[tokio::test]
    async fn firm_thirty_day_rollup_is_empty_without_in_window_invoices() {
        crate::test_support::with_required_postgres(async {
            let application_store = ApplicationStore::uninitialized();
            let (firm, _) = seed_firm(&application_store).await;
            seed_project_on_firm(&application_store, firm.id, "no-invoices-matter", "neon").await;

            let now = Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap();
            let rollup = firm_thirty_day_rollup(&application_store, firm.id, now)
                .await
                .unwrap();
            assert!(rollup.currencies.is_empty());
        })
        .await;
    }
}
