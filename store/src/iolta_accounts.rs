//! The read-only mirror of one pooled IOLTA bank account per US state.
//!
//! IOLTA is pooled **by jurisdiction**: every client whose matter is governed
//! by Nevada law shares one Nevada trust account at the bank, and the firm's
//! obligation is to know, at any moment, how much of that pooled balance
//! belongs to each matter. This module projects the Xero account; the Firm's
//! authoritative journal lives in `firm_journal`. The firm's
//! bookkeeper records deposits and the withdrawal there, and Navigator reads.
//! Navigator opens no account, moves no money, and writes nothing back.
//!
//! A row's identity is its [`IoltaAccount::jurisdiction_id`], UNIQUE in the
//! schema: one pooled account per state, so a second Xero account claiming
//! Nevada is a refused write ([`IoltaAccountError::JurisdictionTaken`])
//! rather than a silent second master balance. The row's own record id is the
//! Xero `AccountID`, the same choice [`crate::xero_invoices`] makes for the
//! same reason — two nightly reads of one Xero account address one key rather
//! than minting two rows the index would only notice afterwards.
//!
//! # Which account a matter belongs to
//!
//! [`for_project`] reads `project.jurisdiction_id` — the matter's **general**
//! governing jurisdiction, not an IOLTA-specific column — and returns that
//! state's account. It answers `Ok(None)`, never a guess, when the matter has
//! no jurisdiction, when the jurisdiction is not a state (a country-scoped
//! matter has no pooled state account), or when that state has no mirrored
//! account yet. Callers count those skips; nothing infers a trust account
//! from the client entity or the owning firm.
//!
//! # What this module does not hold
//!
//! The per-matter side of the three-way reconciliation. That stays in
//! [`crate::trust`], folded from the journal on read, so the master balance
//! and the per-matter balances can never drift apart through a stale column
//! here. [`IoltaAccount::balance_cents`] is only what Xero last reported for
//! the bank account itself.

use chrono::{DateTime, Utc};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::jurisdictions::JURISDICTION_TYPE_STATE;
use crate::postgres::accounting as authority;

/// The prefix a Xero bank account's name carries to declare which state's
/// IOLTA pool it is — the account-side twin of the `Matter <code>` reference
/// invoices use ([`crate::xero_invoices::project_scope_from_reference`]).
///
/// The firm names the account `IOLTA NV — Trust` in Xero and Navigator reads
/// the state code out of it. Xero has no field for "which US state's client
/// funds pool is this", so the mapping has to be declared somewhere; a
/// convention in the account name keeps it in the bookkeeper's own tool
/// rather than in a Navigator-only table an operator would have to remember
/// to edit.
pub const IOLTA_ACCOUNT_NAME_PREFIX: &str = "IOLTA";

/// Read the jurisdiction code out of a Xero bank-account name.
///
/// `"IOLTA NV — Trust"` → `Some("NV")`. `None` for any account that does not
/// open with the prefix and a plausible code — the firm's operating and
/// payroll accounts are listed by the same Xero read, and an account that
/// does not declare a state is skipped and counted, never attached to one.
#[must_use]
pub fn jurisdiction_scope_from_account_name(name: &str) -> Option<String> {
    let rest = name
        .trim()
        .strip_prefix(IOLTA_ACCOUNT_NAME_PREFIX)
        .or_else(|| {
            name.trim()
                .strip_prefix(&IOLTA_ACCOUNT_NAME_PREFIX.to_lowercase())
        })?;
    let code = rest
        .trim_start()
        .split(|c: char| c.is_whitespace() || c == '-')
        .next()?
        .trim();
    let plausible =
        !code.is_empty() && code.len() <= 3 && code.chars().all(|c| c.is_ascii_alphabetic());
    plausible.then(|| code.to_ascii_uppercase())
}

/// Resolve a Xero bank-account name to the state jurisdiction it declares.
///
/// `Ok(None)` when the name declares no code, the code names no
/// jurisdiction, or that jurisdiction is not a state. The caller counts
/// those as unscoped and writes nothing.
///
/// # Errors
///
/// [`IoltaAccountError::Jurisdictions`] when the lookup itself fails.
pub async fn resolve_jurisdiction_scope(
    db: &ApplicationStore,
    account_name: &str,
) -> Result<Option<Uuid>, IoltaAccountError> {
    let Some(code) = jurisdiction_scope_from_account_name(account_name) else {
        return Ok(None);
    };
    let Some(jurisdiction) = crate::jurisdictions::find_by_code(db, &code).await? else {
        return Ok(None);
    };
    if jurisdiction.jurisdiction_type != JURISDICTION_TYPE_STATE {
        return Ok(None);
    }
    Ok(Some(jurisdiction.id))
}

/// One state's pooled IOLTA account, as Xero last reported it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IoltaAccount {
    /// The US state this pooled account serves. One account per state.
    pub jurisdiction_id: Uuid,
    /// Xero `AccountID` (a GUID), and this row's own record id.
    pub xero_account_id: String,
    /// Xero's short chart-of-accounts code (`"090"`), when the account has
    /// one. Display only — [`Self::xero_account_id`] is the identity.
    pub xero_account_code: Option<String>,
    /// The account name as Xero holds it (`"IOLTA Trust — Nevada"`).
    pub name: String,
    /// ISO currency of the account, as Xero reports it.
    pub currency: String,
    /// The bank balance Xero last reported, in minor units. This is the
    /// bank-statement leg; the per-matter leg is folded from
    /// [`crate::trust`].
    pub balance_cents: i64,
    /// When the nightly mirror last read this account. `None` on a row
    /// created before its first successful read.
    pub mirrored_at: Option<DateTime<Utc>>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The pooled-account row as PostgreSQL's `to_jsonb` serves it. The row's
/// `id` is the Xero account id itself, so it adds nothing to decode.
#[derive(serde::Deserialize)]
struct PgIoltaAccountRow {
    jurisdiction_id: Uuid,
    xero_account_id: String,
    xero_account_code: Option<String>,
    name: String,
    currency: String,
    balance_cents: i64,
    mirrored_at: Option<DateTime<Utc>>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgIoltaAccountRow> for IoltaAccount {
    fn from(row: PgIoltaAccountRow) -> Self {
        IoltaAccount {
            jurisdiction_id: row.jurisdiction_id,
            xero_account_id: row.xero_account_id,
            xero_account_code: row.xero_account_code,
            name: row.name,
            currency: row.currency,
            balance_cents: row.balance_cents,
            mirrored_at: row.mirrored_at,
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

/// Errors reading or writing the pooled-account mirror.
#[derive(Debug, Error)]
pub enum IoltaAccountError {
    #[error(transparent)]
    Jurisdictions(#[from] crate::jurisdictions::JurisdictionError),
    #[error(transparent)]
    Projects(#[from] crate::projects::ProjectStoreError),
    /// The named jurisdiction does not exist.
    #[error("no jurisdiction {0}")]
    NoSuchJurisdiction(Uuid),
    /// IOLTA pools by **state**. A country (or any other jurisdiction type)
    /// has no pooled trust account, so attaching one is refused rather than
    /// mapped onto some nearby state.
    #[error("jurisdiction {code} is a {jurisdiction_type}, and IOLTA pools by state")]
    NotAState {
        code: String,
        jurisdiction_type: String,
    },
    /// Another Xero account is already mirrored for this state. One pooled
    /// account per state is the whole point of the model, so this is
    /// reported with both ids rather than silently replacing the incumbent.
    #[error("jurisdiction {jurisdiction_id} already mirrors Xero account {existing}")]
    JurisdictionTaken {
        jurisdiction_id: Uuid,
        existing: String,
    },
    #[error("writing an IOLTA account mirror returned no usable row")]
    WriteReturnedNothing,
    /// PostgreSQL, under the accounting authority, failed.
    #[error(transparent)]
    Postgres(#[from] crate::postgres::accounting::AccountingAuthorityError),
}

/// What the nightly mirror captures for one pooled account.
#[derive(Clone, Debug)]
pub struct UpsertIoltaAccount {
    /// The state this pooled account serves.
    pub jurisdiction_id: Uuid,
    /// Xero `AccountID`.
    pub xero_account_id: String,
    pub xero_account_code: Option<String>,
    pub name: String,
    pub currency: String,
    pub balance_cents: i64,
    /// When this read happened.
    pub mirrored_at: DateTime<Utc>,
}

/// Mirror one pooled account, creating the row or refreshing the one this
/// Xero account already has.
///
/// Refuses a jurisdiction that does not exist or is not a state, and refuses
/// to attach a **second** Xero account to a state that already mirrors a
/// different one — both as typed errors a caller reports, never as a silent
/// second master balance.
///
/// # Errors
///
/// [`IoltaAccountError::NoSuchJurisdiction`], [`IoltaAccountError::NotAState`],
/// [`IoltaAccountError::JurisdictionTaken`], or [`IoltaAccountError::Db`].
pub async fn upsert(
    db: &ApplicationStore,
    input: &UpsertIoltaAccount,
) -> Result<IoltaAccount, IoltaAccountError> {
    let jurisdiction = crate::jurisdictions::find_by_id(db, input.jurisdiction_id)
        .await?
        .ok_or(IoltaAccountError::NoSuchJurisdiction(input.jurisdiction_id))?;
    if jurisdiction.jurisdiction_type != JURISDICTION_TYPE_STATE {
        return Err(IoltaAccountError::NotAState {
            code: jurisdiction.code,
            jurisdiction_type: jurisdiction.jurisdiction_type,
        });
    }
    if let Some(existing) = for_jurisdiction(db, input.jurisdiction_id).await? {
        if existing.xero_account_id != input.xero_account_id {
            return Err(IoltaAccountError::JurisdictionTaken {
                jurisdiction_id: input.jurisdiction_id,
                existing: existing.xero_account_id,
            });
        }
    }

    let pg = authority::authority().await?;
    let row = serde_json::json!({
        "id": input.xero_account_id, "jurisdiction_id": input.jurisdiction_id,
        "xero_account_id": input.xero_account_id,
        "xero_account_code": input.xero_account_code, "name": input.name,
        "currency": input.currency, "balance_cents": input.balance_cents,
        "mirrored_at": input.mirrored_at,
    });
    let query = sqlx::query(
        "INSERT INTO iolta_account SELECT * FROM \
         jsonb_populate_record(NULL::iolta_account, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         ON CONFLICT (id) DO UPDATE SET jurisdiction_id = EXCLUDED.jurisdiction_id, \
         xero_account_code = EXCLUDED.xero_account_code, name = EXCLUDED.name, \
         currency = EXCLUDED.currency, balance_cents = EXCLUDED.balance_cents, \
         mirrored_at = EXCLUDED.mirrored_at, updated_at = now() \
         RETURNING to_jsonb(iolta_account) AS row",
    )
    .bind(row);
    // The pre-check races with concurrent upserts; the unique index is
    // the final word, and it reads as JurisdictionTaken like the
    // SurrealDB path's.
    let written = match authority::maybe::<PgIoltaAccountRow>(&pg, query).await {
        Ok(row) => row
            .map(Into::into)
            .ok_or(IoltaAccountError::WriteReturnedNothing)?,
        Err(error) if error.violates("iolta_account_jurisdiction_key") => {
            let existing = for_jurisdiction(db, input.jurisdiction_id)
                .await?
                .map_or_else(|| input.xero_account_id.clone(), |row| row.xero_account_id);
            return Err(IoltaAccountError::JurisdictionTaken {
                jurisdiction_id: input.jurisdiction_id,
                existing,
            });
        }
        Err(error) => return Err(error.into()),
    };
    Ok(written)
}

/// The pooled account mirrored for one state, if any.
///
/// # Errors
///
/// [`IoltaAccountError::Db`] when the lookup fails.
pub async fn for_jurisdiction(
    _db: &ApplicationStore,
    jurisdiction_id: Uuid,
) -> Result<Option<IoltaAccount>, IoltaAccountError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(iolta_account) AS row FROM iolta_account \
         WHERE jurisdiction_id = $1 LIMIT 1",
    )
    .bind(jurisdiction_id);
    Ok(authority::maybe::<PgIoltaAccountRow>(&pg, query)
        .await?
        .map(Into::into))
}

/// The pooled account a matter's client funds belong in, resolved from the
/// matter's **general** governing jurisdiction.
///
/// `Ok(None)` — never a guess — when the Project does not exist, carries no
/// jurisdiction, is governed by something other than a state, or names a
/// state with no mirrored account. Callers count those skips.
///
/// # Errors
///
/// [`IoltaAccountError::Db`] or a failure reading the Project.
pub async fn for_project(
    db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Option<IoltaAccount>, IoltaAccountError> {
    let Some(project) = crate::projects::find_by_id(db, project_id).await? else {
        return Ok(None);
    };
    let Some(jurisdiction_id) = project.jurisdiction_id else {
        return Ok(None);
    };
    let Some(jurisdiction) = crate::jurisdictions::find_by_id(db, jurisdiction_id).await? else {
        return Ok(None);
    };
    if jurisdiction.jurisdiction_type != JURISDICTION_TYPE_STATE {
        return Ok(None);
    }
    for_jurisdiction(db, jurisdiction_id).await
}

/// Every mirrored pooled account, oldest first — the firm-side list.
///
/// # Errors
///
/// [`IoltaAccountError::Db`] when the query fails.
pub async fn all(_db: &ApplicationStore) -> Result<Vec<IoltaAccount>, IoltaAccountError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(iolta_account) AS row FROM iolta_account \
         ORDER BY inserted_at ASC",
    );
    Ok(authority::rows::<PgIoltaAccountRow>(&pg, query)
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{
        all, for_jurisdiction, for_project, jurisdiction_scope_from_account_name,
        resolve_jurisdiction_scope, upsert, IoltaAccountError, UpsertIoltaAccount,
    };
    use crate::application_store::ApplicationStore;
    use crate::jurisdictions::NewJurisdiction;
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    async fn state(db: &ApplicationStore, name: &str, code: &str) -> Uuid {
        crate::jurisdictions::create(db, &NewJurisdiction::new(name, code, "state"))
            .await
            .unwrap()
            .id
    }

    async fn project_in(db: &ApplicationStore, code: &str, jurisdiction_id: Option<Uuid>) -> Uuid {
        crate::test_support::seed_compiled_brands_postgres(db).await;
        let entity_id = crate::test_support::seed_entity(db).await;
        crate::projects::create(
            db,
            &crate::projects::NewProject {
                code: code.to_string(),
                name: code.to_string(),
                status: "open".to_string(),
                entity_id,
                jurisdiction_id,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    fn mirror(jurisdiction_id: Uuid, xero_account_id: &str, name: &str) -> UpsertIoltaAccount {
        UpsertIoltaAccount {
            jurisdiction_id,
            xero_account_id: xero_account_id.to_string(),
            xero_account_code: Some("090".to_string()),
            name: name.to_string(),
            currency: "USD".to_string(),
            balance_cents: 500_000,
            mirrored_at: Utc.with_ymd_and_hms(2026, 9, 19, 3, 0, 0).unwrap(),
        }
    }

    /// The issue's first acceptance criterion: two states are two rows, each
    /// with its own Xero account.
    #[tokio::test]
    async fn two_states_carry_two_pooled_accounts() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let nevada = state(&db, "Nevada", "NV").await;
            let california = state(&db, "California", "CA").await;

            upsert(&db, &mirror(nevada, "xero-nv", "IOLTA NV — Trust"))
                .await
                .unwrap();
            upsert(&db, &mirror(california, "xero-ca", "IOLTA CA — Trust"))
                .await
                .unwrap();

            let nv = for_jurisdiction(&db, nevada).await.unwrap().unwrap();
            let ca = for_jurisdiction(&db, california).await.unwrap().unwrap();
            assert_eq!(nv.xero_account_id, "xero-nv");
            assert_eq!(ca.xero_account_id, "xero-ca");
            assert_eq!(all(&db).await.unwrap().len(), 2);
        })
        .await;
    }

    /// A re-run of the nightly mirror refreshes the one row rather than
    /// adding a second master balance for the same state.
    #[tokio::test]
    async fn re_mirroring_the_same_account_refreshes_one_row() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let nevada = state(&db, "Nevada", "NV").await;
            upsert(&db, &mirror(nevada, "xero-nv", "IOLTA NV — Trust"))
                .await
                .unwrap();

            let mut refreshed = mirror(nevada, "xero-nv", "IOLTA NV — Trust");
            refreshed.balance_cents = 750_000;
            upsert(&db, &refreshed).await.unwrap();

            let accounts = all(&db).await.unwrap();
            assert_eq!(accounts.len(), 1, "one state, one pooled account");
            assert_eq!(accounts[0].balance_cents, 750_000);
        })
        .await;
    }

    /// One pooled account per state: a *different* Xero account claiming a
    /// state that already has one is refused, naming the incumbent, rather
    /// than quietly replacing it.
    #[tokio::test]
    async fn a_second_xero_account_for_one_state_is_refused() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let nevada = state(&db, "Nevada", "NV").await;
            upsert(&db, &mirror(nevada, "xero-nv", "IOLTA NV — Trust"))
                .await
                .unwrap();

            let second = upsert(&db, &mirror(nevada, "xero-nv-2", "IOLTA NV — Second")).await;
            assert!(
                matches!(
                    second,
                    Err(IoltaAccountError::JurisdictionTaken { ref existing, .. })
                        if existing == "xero-nv"
                ),
                "unexpected upsert result shape"
            );
            assert_eq!(all(&db).await.unwrap().len(), 1);
        })
        .await;
    }

    /// IOLTA pools by state. A country jurisdiction has no pooled trust
    /// account, so attaching one is refused rather than mapped onto a state.
    #[tokio::test]
    async fn a_country_jurisdiction_cannot_hold_a_pooled_account() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let us = crate::jurisdictions::create(
                &db,
                &NewJurisdiction::new("United States", "US", "country"),
            )
            .await
            .unwrap()
            .id;

            let attached = upsert(&db, &mirror(us, "IOLTA US — Trust", "IOLTA US — Trust")).await;
            assert!(
                matches!(attached, Err(IoltaAccountError::NotAState { .. })),
                "expected NotAState error"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn an_unknown_jurisdiction_is_refused() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let attached = upsert(&db, &mirror(Uuid::now_v7(), "xero-x", "IOLTA XX")).await;
            assert!(
                matches!(attached, Err(IoltaAccountError::NoSuchJurisdiction(_))),
                "expected NoSuchJurisdiction error"
            );
        })
        .await;
    }

    /// A matter reaches its pooled account through its *general* governing
    /// jurisdiction — and a matter that names none is answered `None`, never
    /// attached to a nearby state.
    #[tokio::test]
    async fn a_matter_resolves_its_account_through_its_jurisdiction() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let nevada = state(&db, "Nevada", "NV").await;
            let california = state(&db, "California", "CA").await;
            upsert(&db, &mirror(nevada, "xero-nv", "IOLTA NV — Trust"))
                .await
                .unwrap();

            let nv_matter = project_in(&db, "nv-matter", Some(nevada)).await;
            let ca_matter = project_in(&db, "ca-matter", Some(california)).await;
            let unscoped_matter = project_in(&db, "no-jurisdiction", None).await;

            assert_eq!(
                for_project(&db, nv_matter)
                    .await
                    .unwrap()
                    .map(|a| a.xero_account_id),
                Some("xero-nv".to_string())
            );
            assert_eq!(
                for_project(&db, ca_matter).await.unwrap(),
                None,
                "California has no mirrored account yet — nothing is invented"
            );
            assert_eq!(
                for_project(&db, unscoped_matter).await.unwrap(),
                None,
                "a matter with no jurisdiction has no pooled account"
            );
            assert_eq!(for_project(&db, Uuid::now_v7()).await.unwrap(), None);
        })
        .await;
    }

    /// A matter governed by a country — a federal matter — has no pooled
    /// state account, and is skipped rather than guessed at.
    #[tokio::test]
    async fn a_country_scoped_matter_has_no_pooled_account() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            let us = crate::jurisdictions::create(
                &db,
                &NewJurisdiction::new("United States", "US", "country"),
            )
            .await
            .unwrap()
            .id;
            let federal_matter = project_in(&db, "federal", Some(us)).await;

            assert_eq!(for_project(&db, federal_matter).await.unwrap(), None);
        })
        .await;
    }

    #[test]
    fn the_account_name_convention_reads_the_state_code() {
        assert_eq!(
            jurisdiction_scope_from_account_name("IOLTA NV — Trust"),
            Some("NV".to_string())
        );
        assert_eq!(
            jurisdiction_scope_from_account_name("  IOLTA ca  "),
            Some("CA".to_string())
        );
        assert_eq!(
            jurisdiction_scope_from_account_name("IOLTA NV-Pooled"),
            Some("NV".to_string())
        );
        // The firm's other accounts are listed by the same Xero read and
        // must not be mistaken for a trust pool.
        assert_eq!(
            jurisdiction_scope_from_account_name("Business Checking"),
            None
        );
        assert_eq!(jurisdiction_scope_from_account_name("Payroll"), None);
        assert_eq!(jurisdiction_scope_from_account_name("IOLTA"), None);
        assert_eq!(
            jurisdiction_scope_from_account_name("IOLTA Nevada Pooled Trust"),
            None,
            "a spelled-out state is not a code; the convention is the code"
        );
    }

    #[tokio::test]
    async fn an_account_naming_an_unknown_or_non_state_code_is_unscoped() {
        crate::test_support::with_required_postgres(async {
            let db = crate::application_store::ApplicationStore::uninitialized();
            state(&db, "Nevada", "NV").await;
            crate::jurisdictions::create(
                &db,
                &NewJurisdiction::new("United States", "US", "country"),
            )
            .await
            .unwrap();

            assert!(resolve_jurisdiction_scope(&db, "IOLTA NV — Trust")
                .await
                .unwrap()
                .is_some());
            assert_eq!(
                resolve_jurisdiction_scope(&db, "IOLTA ZZ — Trust")
                    .await
                    .unwrap(),
                None,
                "a code no jurisdiction carries is unscoped"
            );
            assert_eq!(
                resolve_jurisdiction_scope(&db, "IOLTA US — Trust")
                    .await
                    .unwrap(),
                None,
                "a country code names no pooled state account"
            );
            assert_eq!(
                resolve_jurisdiction_scope(&db, "Business Checking")
                    .await
                    .unwrap(),
                None
            );
        })
        .await;
    }
}
