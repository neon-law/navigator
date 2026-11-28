//! The accounting authority's tests, against the PostgreSQL fixture. Each
//! test fails when `NAVIGATOR_TEST_POSTGRES_PORT` is unset.
use super::*;
use crate::application_store::ApplicationStore;
use crate::finance_aggregate::{self, FinanceReport, Money, Parameters};
use crate::iolta_accounts::{self, UpsertIoltaAccount};
use crate::iolta_withdrawals::{self, WithdrawalInput};
use crate::matter_invoices::{self, NewPayment, NewRecord, Source, Status};
use crate::postgres::fixtures::{connect, fresh_schema};
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

fn invoice(external_id: &str, reference: &str) -> NewRecord {
    NewRecord {
        source: Source::Stripe,
        external_id: external_id.to_string(),
        amount_cents: 100_000,
        currency: "USD".to_string(),
        status: Status::Sent,
        reference: Some(reference.to_string()),
        issued_on: Some(when().date_naive()),
        due_on: None,
        payer: None,
        memo: None,
        client_visible: true,
        receipt_document_id: None,
    }
}

fn payment(cents: i64, external_id: &str) -> NewPayment {
    NewPayment {
        amount_cents: cents,
        received_on: when().date_naive(),
        source: Source::Check,
        external_id: Some(external_id.to_string()),
        payer: Some("Synthetic Client".to_string()),
        memo: None,
        client_visible: true,
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One ledger walk: record, replay, pay, look up, void.
async fn matter_invoices_record_payments_and_allocation_reads_are_postgres_only() {
    let Some(pg) = database().await else { return };
    let application_store = ApplicationStore::uninitialized();
    let (_, project) = seed_project(&pg).await;

    with_test_db(pg.clone(), async {
        let created = matter_invoices::record(project, None, &invoice("in_one", "Matter ACCT-1"))
            .await
            .unwrap();
        assert_eq!(
            (created.status, created.amount_paid_cents),
            (Status::Sent, 0)
        );
        assert_eq!(
            matter_invoices::record(project, None, &invoice("in_one", "Matter ACCT-1"))
                .await
                .unwrap()
                .id,
            created.id,
            "a replay returns the first row"
        );
        let mut changed = invoice("in_one", "Matter ACCT-1");
        changed.amount_cents = 1;
        assert!(matches!(
            matter_invoices::record(project, None, &changed).await,
            Err(matter_invoices::MatterInvoiceError::Conflict(_))
        ));
        let mut xero = invoice("x-1", "Matter ACCT-1");
        xero.source = Source::Xero;
        assert!(matches!(
            matter_invoices::record(project, None, &xero).await,
            Err(matter_invoices::MatterInvoiceError::Invalid(_))
        ));

        // A partial payment leaves a balance; the rest settles it.
        let (partial, _) =
            matter_invoices::record_payment(project, created.id, None, &payment(40_000, "1001"))
                .await
                .unwrap();
        assert_eq!(
            (partial.status, partial.balance_due_cents()),
            (Status::Sent, 60_000)
        );
        let (replayed, _) =
            matter_invoices::record_payment(project, created.id, None, &payment(40_000, "1001"))
                .await
                .unwrap();
        assert_eq!(
            replayed.amount_paid_cents, 40_000,
            "a replayed payment counts once"
        );
        let (paid, _) =
            matter_invoices::record_payment(project, created.id, None, &payment(60_000, "1002"))
                .await
                .unwrap();
        assert_eq!((paid.status, paid.balance_due_cents()), (Status::Paid, 0));
        assert_eq!(
            matter_invoices::payments(created.id).await.unwrap().len(),
            2
        );
        assert!(
            sqlx::query("DELETE FROM matter_invoice_payment WHERE invoice_id=$1")
                .bind(created.id)
                .execute(pg.pool())
                .await
                .is_err()
        );

        // A second Project's invoice with the same reference makes the
        // reference ambiguous; the issuer id and ledger id stay exact.
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
        let other = matter_invoices::record(competitor, None, &invoice("in_two", "Matter ACCT-1"))
            .await
            .unwrap();
        for (reference, expected) in [
            ("Matter ACCT-1", None),
            ("in_one", Some(created.id)),
            ("in_two", Some(other.id)),
        ] {
            assert_eq!(
                matter_invoices::find_for_allocation(&application_store, reference)
                    .await
                    .unwrap()
                    .map(|found| found.id),
                expected,
                "{reference}"
            );
        }
        assert_eq!(
            matter_invoices::find_for_allocation(&application_store, &other.id.to_string())
                .await
                .unwrap()
                .map(|found| found.project_id),
            Some(competitor)
        );

        // Voiding needs a reason and an open invoice.
        assert!(
            matter_invoices::set_status(competitor, other.id, Status::Void, None)
                .await
                .is_err()
        );
        let void =
            matter_invoices::set_status(competitor, other.id, Status::Void, Some("Sent twice"))
                .await
                .unwrap();
        assert_eq!(void.status_reason.as_deref(), Some("Sent twice"));
        assert!(
            matter_invoices::set_status(competitor, other.id, Status::Sent, None)
                .await
                .is_err()
        );
        assert!(
            matter_invoices::set_status(project, created.id, Status::Void, Some("x"))
                .await
                .is_err(),
            "a paid invoice is not voided"
        );
        assert_eq!(
            matter_invoices::for_projects(&application_store, &[project, competitor])
                .await
                .unwrap()
                .len(),
            2
        );
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
        let settled =
            matter_invoices::record(project, None, &invoice("in_settled", "Matter ACCT-1"))
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
        assert_eq!(lines[0].matter_invoice_id, settled.id);
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
