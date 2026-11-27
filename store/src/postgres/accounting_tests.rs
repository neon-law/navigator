//! The accounting authority's tests, against the PostgreSQL fixture. Each
//! test fails when `NAVIGATOR_TEST_POSTGRES_PORT` is unset.
use super::*;
use crate::application_store::ApplicationStore;
use crate::finance_aggregate::{self, FinanceReport, Money, Parameters};
use crate::iolta_accounts::{self, UpsertIoltaAccount};
use crate::iolta_withdrawals::{self, WithdrawalInput};
use crate::postgres::fixtures::{connect, fresh_schema};
use crate::xero_invoices::{self, UpsertXeroInvoice};
use chrono::{TimeZone, Utc};
use uuid::Uuid;

async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

/// Seed jurisdiction → entity_type → entity → project, the parent chain the
/// accounting tables' foreign keys name, and return both ids.
async fn seed_project(pg: &PostgresDb) -> (Uuid, Uuid) {
    let jurisdiction: Uuid = sqlx::query_scalar(
        "INSERT INTO jurisdiction (name, code, jurisdiction_type) \
         VALUES ('Accounting State', 'AS', 'state') RETURNING id",
    )
    .fetch_one(pg.pool())
    .await
    .expect("jurisdiction seeds");
    let entity_type: Uuid = sqlx::query_scalar(
        "INSERT INTO entity_type (name) VALUES ('Accounting Entity Type') RETURNING id",
    )
    .fetch_one(pg.pool())
    .await
    .expect("entity type seeds");
    let entity: Uuid = sqlx::query_scalar(
        "INSERT INTO entity (name, entity_type_id, jurisdiction_id) \
         VALUES ('Accounting Entity', $1, $2) RETURNING id",
    )
    .bind(entity_type)
    .bind(jurisdiction)
    .fetch_one(pg.pool())
    .await
    .expect("entity seeds");
    let project: Uuid = sqlx::query_scalar(
        "INSERT INTO project (code, name, status, entity_id) \
         VALUES ('ACCT', 'Accounting Matter', 'open', $1) RETURNING id",
    )
    .bind(entity)
    .fetch_one(pg.pool())
    .await
    .expect("project seeds");
    (jurisdiction, project)
}

fn when() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 17, 0, 0, 0).unwrap()
}

fn invoice(project_id: Uuid, xero_invoice_id: &str, reference: &str) -> UpsertXeroInvoice {
    UpsertXeroInvoice {
        project_id,
        xero_invoice_id: xero_invoice_id.to_string(),
        reference: reference.to_string(),
        status: "AUTHORISED".to_string(),
        amount_cents: 100_000,
        currency: "USD".to_string(),
        issued_at: when(),
        due_at: None,
    }
}

#[tokio::test]
async fn xero_invoice_upsert_reconcile_and_reads_are_postgres_only() {
    let Some(pg) = database().await else { return };
    let application_store = ApplicationStore::uninitialized();
    let (_, project) = seed_project(&pg).await;

    with_test_db(pg.clone(), async {
        let created = xero_invoices::upsert(
            &application_store,
            &invoice(project, "xero-1", "Matter ACCT-1"),
        )
        .await
        .unwrap();
        assert_eq!(created.xero_invoice_id, "xero-1");
        assert_eq!(created.amount_paid_cents, 0);
        assert_eq!(created.status, "AUTHORISED");

        // A second upsert of the same invoice refreshes the fields the
        // upsert owns and preserves the reconciled amount_paid_cents.
        let mut refreshed = invoice(project, "xero-1", "Matter ACCT-1");
        refreshed.amount_cents = 120_000;
        refreshed.status = "VOIDED".to_string();
        let updated = xero_invoices::upsert(&application_store, &refreshed)
            .await
            .unwrap();
        assert_eq!(updated.amount_cents, 120_000);
        assert_eq!(updated.status, "VOIDED");
        assert_eq!(updated.amount_paid_cents, 0);
        assert_eq!(updated.inserted_at, created.inserted_at);

        // A second Project's invoice with the same reference makes the
        // allocation lookup ambiguous, and ambiguity reads as absent.
        let competitor = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO project (id, code, name, status, entity_id) \
             SELECT $1, code || '-2', name, status, entity_id FROM project WHERE id = $2",
        )
        .bind(competitor)
        .bind(project)
        .execute(pg.pool())
        .await
        .unwrap();
        xero_invoices::upsert(
            &application_store,
            &invoice(competitor, "xero-2", "Matter ACCT-1"),
        )
        .await
        .unwrap();
        assert!(
            xero_invoices::find_for_allocation(&application_store, "Matter ACCT-1")
                .await
                .unwrap()
                .is_none()
        );

        let reconciled =
            xero_invoices::record_reconcile(&application_store, "xero-1", "PAID", 120_000)
                .await
                .unwrap()
                .expect("reconciled");
        assert_eq!(reconciled.status, "PAID");
        assert_eq!(reconciled.amount_paid_cents, 120_000);

        assert_eq!(
            xero_invoices::find_for_allocation(&application_store, "xero-1")
                .await
                .unwrap(),
            Some(reconciled)
        );
        assert_eq!(
            xero_invoices::find_for_allocation(&application_store, "xero-2")
                .await
                .unwrap()
                .map(|found| found.xero_invoice_id),
            Some("xero-2".to_string())
        );

        // issued_at orders the for_projects listing (newest first): keep
        // this invoice strictly newer than xero-1 so the order is stable.
        let mut late = invoice(project, "xero-3", "Matter ACCT-3");
        late.issued_at = when() + chrono::Duration::seconds(1);
        let unique = xero_invoices::upsert(&application_store, &late)
            .await
            .unwrap();
        let projects = xero_invoices::for_projects(&application_store, &[project])
            .await
            .unwrap();
        assert_eq!(
            projects
                .iter()
                .map(|row| row.xero_invoice_id.as_str())
                .collect::<Vec<_>>(),
            ["xero-3", "xero-1"]
        );
        // needing_reconcile does not order its results; compare as a set.
        let mut needing = xero_invoices::needing_reconcile(&application_store)
            .await
            .unwrap();
        needing.sort_by(|a, b| a.xero_invoice_id.cmp(&b.xero_invoice_id));
        assert_eq!(
            needing
                .iter()
                .map(|row| row.xero_invoice_id.as_str())
                .collect::<Vec<_>>(),
            ["xero-2", "xero-3"]
        );
        assert_eq!(unique.project_id, project);
    })
    .await;
}

#[tokio::test]
async fn iolta_accounts_upsert_and_reads_are_postgres_only() {
    let Some(pg) = database().await else { return };
    let application_store = ApplicationStore::uninitialized();
    let (jurisdiction, _) = seed_project(&pg).await;
    // The jurisdiction lookup inside upsert and for_jurisdiction goes
    // through the shared catalogs' PostgreSQL authority; without that scope
    // an uninitialized Surreal handle would answer the read.
    crate::postgres::catalog::with_test_db(
        pg.clone(),
        with_test_db(pg.clone(), async {
            let account = iolta_accounts::upsert(
                &application_store,
                &UpsertIoltaAccount {
                    jurisdiction_id: jurisdiction,
                    xero_account_id: "xero-nv".to_string(),
                    xero_account_code: Some("090".to_string()),
                    name: "IOLTA AS — Trust".to_string(),
                    currency: "USD".to_string(),
                    balance_cents: 1_000_000,
                    mirrored_at: when(),
                },
            )
            .await
            .unwrap();
            assert_eq!(account.jurisdiction_id, jurisdiction);
            assert_eq!(account.balance_cents, 1_000_000);

            // A second account claiming the same pool is refused before any
            // write: one pooled account per state.
            assert!(matches!(
                iolta_accounts::upsert(
                    &application_store,
                    &UpsertIoltaAccount {
                        jurisdiction_id: jurisdiction,
                        xero_account_id: "xero-other".to_string(),
                        xero_account_code: None,
                        name: "IOLTA AS — Other".to_string(),
                        currency: "USD".to_string(),
                        balance_cents: 0,
                        mirrored_at: when(),
                    },
                )
                .await,
                Err(iolta_accounts::IoltaAccountError::JurisdictionTaken { .. })
            ));

            let refreshed = UpsertIoltaAccount {
                jurisdiction_id: account.jurisdiction_id,
                xero_account_id: account.xero_account_id.clone(),
                xero_account_code: account.xero_account_code.clone(),
                name: account.name.clone(),
                currency: account.currency.clone(),
                balance_cents: 1_500_000,
                mirrored_at: when(),
            };
            let updated = iolta_accounts::upsert(&application_store, &refreshed)
                .await
                .unwrap();
            assert_eq!(updated.balance_cents, 1_500_000);

            assert_eq!(
                iolta_accounts::for_jurisdiction(&application_store, jurisdiction)
                    .await
                    .unwrap(),
                Some(updated)
            );
            assert_eq!(
                iolta_accounts::for_jurisdiction(&application_store, Uuid::new_v4())
                    .await
                    .unwrap(),
                None
            );
            assert_eq!(
                iolta_accounts::all(&application_store).await.unwrap().len(),
                1
            );
        }),
    )
    .await;
}

#[tokio::test]
async fn iolta_withdrawal_writes_and_reads_are_postgres_only() {
    let Some(pg) = database().await else { return };
    let application_store = ApplicationStore::uninitialized();
    let (jurisdiction, project) = seed_project(&pg).await;

    with_test_db(pg.clone(), async {
        let settled = xero_invoices::upsert(
            &application_store,
            &invoice(project, "xero-1", "Matter ACCT-1"),
        )
        .await
        .unwrap();
        let input = WithdrawalInput {
            xero_transaction_id: "txn-1".to_string(),
            xero_account_id: "xero-nv".to_string(),
            total_cents: 40_000,
            currency: "USD".to_string(),
            occurred_at: when(),
            lines: Vec::new(),
        };
        iolta_withdrawals::write_withdrawal(&application_store, &input, jurisdiction)
            .await
            .unwrap();
        iolta_withdrawals::write_allocation(&application_store, &input, &settled, 40_000)
            .await
            .unwrap();

        let withdrawal = iolta_withdrawals::find(&application_store, "txn-1")
            .await
            .unwrap()
            .expect("withdrawal");
        assert_eq!(withdrawal.jurisdiction_id, jurisdiction);
        assert_eq!(withdrawal.total_cents, 40_000);

        // Re-reading the same transfer refreshes its line instead of
        // doubling the matter's drawn total.
        iolta_withdrawals::write_allocation(&application_store, &input, &settled, 40_000)
            .await
            .unwrap();

        let lines = iolta_withdrawals::lines_for(&application_store, "txn-1")
            .await
            .unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].xero_invoice_id, "xero-1");
        assert_eq!(lines[0].project_id, project);
        assert_eq!(lines[0].amount_cents, 40_000);

        assert_eq!(
            iolta_withdrawals::for_project(&application_store, project)
                .await
                .unwrap(),
            lines
        );
        assert!(
            iolta_withdrawals::for_project(&application_store, Uuid::new_v4())
                .await
                .unwrap()
                .is_empty()
        );
    })
    .await;
}

#[tokio::test]
async fn finance_aggregate_upsert_and_latest_are_postgres_only() {
    let Some(pg) = database().await else { return };
    let application_store = ApplicationStore::uninitialized();
    let (_, project) = seed_project(&pg).await;

    with_test_db(pg.clone(), async {
        let totals = Money {
            matters: 1,
            billed_cents: 100_000,
            collected_cents: 100_000,
            overhead_cents: 28_000,
            net_cents: 72_000,
            attorney_share_cents: 50_400,
            firm_share_cents: 21_600,
            lead_share_cents: 17_280,
            rain_maker_share_cents: 4_320,
        };
        let report = FinanceReport {
            source: finance_aggregate::REPORT_SOURCE.to_string(),
            run_id: "run-1".to_string(),
            computed_at: when(),
            parameters: Parameters::default(),
            totals,
            by_month: Vec::new(),
            by_state: Vec::new(),
            by_vertical: Vec::new(),
            by_attorney: Vec::new(),
        };
        finance_aggregate::upsert(&application_store, project, &report)
            .await
            .unwrap();
        // Replaying the same run overwrites its own row.
        finance_aggregate::upsert(&application_store, project, &report)
            .await
            .unwrap();

        let latest = finance_aggregate::latest_for_project(&application_store, project)
            .await
            .unwrap()
            .expect("report");
        assert_eq!(latest.run_id, "run-1");
        assert_eq!(latest.parameters, Parameters::default());
        assert_eq!(latest.totals.billed_cents, 100_000);
        assert_eq!(latest.computed_at, report.computed_at);

        assert!(
            finance_aggregate::latest_for_project(&application_store, Uuid::new_v4())
                .await
                .unwrap()
                .is_none()
        );
    })
    .await;
}
