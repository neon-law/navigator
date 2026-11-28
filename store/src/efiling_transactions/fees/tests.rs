use super::*;
use crate::{
    efiling_transactions::{record_postgres_with_fee, EfilingReport},
    postgres::fixtures::{connect, fresh_schema},
};
use serde_json::json;

const ENVELOPE: &str = "SYNTHETIC-ENVELOPE-1";

async fn fixture() -> Option<(PostgresDb, crate::firm_side::tests::Matter, Uuid)> {
    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    let matter = crate::firm_side::tests::matter(&pg).await;
    let other_project: Uuid = sqlx::query_scalar(
        "INSERT INTO project (code,name,status,entity_id,firm_id)
         SELECT 'synthetic-fee-scope','Synthetic fee scope','open',entity_id,firm_id
         FROM project WHERE id=$1 RETURNING id",
    )
    .bind(matter.project_id)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    Some((pg, matter, other_project))
}

async fn receipt_document(
    pg: &PostgresDb,
    project: Uuid,
    envelope: &str,
    visibility: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query_scalar(
        "INSERT INTO asset (id,storage_key,content_type,byte_size,sha256_hex,project_id,kind,visibility,metadata)
         VALUES ($1,$2,'application/pdf',100,repeat('a',64),$3,'receipt',$4,$5) RETURNING id",
    )
    .bind(id)
    .bind(format!("synthetic/{id}"))
    .bind(project)
    .bind(visibility)
    .bind(json!({"receipt":{
        "amount_cents":1225,"currency":"USD","payee":"Synthetic Filing Service",
        "category":"court_filing_fee","transaction_id":envelope
    }}))
    .fetch_one(pg.pool())
    .await
    .unwrap()
}

fn report(project_id: Uuid, status: &str) -> EfilingReport {
    EfilingReport {
        project_id,
        status: status.into(),
        transaction_id: ENVELOPE.into(),
        ..Default::default()
    }
}

fn settled_charge() -> SettledCharge {
    SettledCharge {
        bank_account: Uuid::new_v4(),
        transaction: Uuid::new_v4(),
        amount_cents: 1225,
        posted_on: NaiveDate::from_ymd_opt(2026, 11, 16).unwrap(),
    }
}

#[tokio::test]
async fn accepted_fee_remains_pending_until_exact_settlement_and_deduplicates_replays() {
    let Some((pg, matter, other)) = fixture().await else {
        return;
    };
    let project = matter.project_id;
    let receipt = receipt_document(&pg, project, ENVELOPE, "internal").await;
    let accepted = report(project, "accepted");
    let recorded = record_postgres_with_fee(&pg, &accepted, Some(receipt))
        .await
        .unwrap();
    let observation = recorded.fee.unwrap();
    assert_eq!((observation.status, observation.cost_id), ("pending", None));
    assert!(costs::list_for_project(&pg, project)
        .await
        .unwrap()
        .is_empty());

    let mut charge = settled_charge();
    charge.amount_cents += 1;
    assert!(settle(&pg, project, matter.lawyer, ENVELOPE, &charge)
        .await
        .is_err());
    charge.amount_cents -= 1;
    let first = settle(&pg, project, matter.lawyer, ENVELOPE, &charge)
        .await
        .unwrap();
    let second = settle(&pg, project, matter.lawyer, ENVELOPE, &charge)
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(
        existing_settlement(
            &pg,
            project,
            ENVELOPE,
            charge.bank_account,
            charge.transaction
        )
        .await
        .unwrap(),
        Some(first)
    );

    record_postgres_with_fee(&pg, &report(project, "unknown"), None)
        .await
        .unwrap();
    let envelope: (Option<Uuid>, Option<Uuid>, String) = sqlx::query_as(
        "SELECT fee_cost_id,fee_matched_by,status FROM efiling_transactions WHERE id=$1",
    )
    .bind(recorded.filing.id)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(
        envelope,
        (Some(first), Some(matter.lawyer), "accepted".into())
    );
    assert_eq!(costs::list_events(&pg, first).await.unwrap().len(), 1);
    assert_eq!(
        costs::list_for_project(&pg, project).await.unwrap().len(),
        1
    );

    let other_receipt = receipt_document(&pg, other, ENVELOPE, "internal").await;
    record_postgres_with_fee(&pg, &report(other, "accepted"), Some(other_receipt))
        .await
        .unwrap();
    assert!(settle(&pg, other, matter.lawyer, ENVELOPE, &charge)
        .await
        .is_err());
    let competing = bank_import(&pg, other, matter.lawyer, other_receipt, &charge).await;
    assert!(matches!(
        competing,
        Err(costs::ProjectCostError::SourceTaken)
    ));
    assert!(costs::list_for_project(&pg, other)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn pending_observation_refuses_cross_project_public_and_wrong_envelope_receipts() {
    let Some((pg, matter, other)) = fixture().await else {
        return;
    };
    let project = matter.project_id;
    for (belongs_to, envelope, visibility) in [
        (other, ENVELOPE, "internal"),
        (project, "SYNTHETIC-DIFFERENT", "internal"),
        (project, ENVELOPE, "client"),
    ] {
        let receipt = receipt_document(&pg, belongs_to, envelope, visibility).await;
        assert!(
            record_postgres_with_fee(&pg, &report(project, "accepted"), Some(receipt))
                .await
                .is_err()
        );
    }
    let receipt = receipt_document(&pg, project, ENVELOPE, "internal").await;
    assert!(
        record_postgres_with_fee(&pg, &report(project, "submitted"), Some(receipt))
            .await
            .is_err()
    );
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM efiling_transactions WHERE project_id=$1")
            .bind(project)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
}

async fn bank_import(
    pg: &PostgresDb,
    project: Uuid,
    actor: Uuid,
    evidence: Uuid,
    charge: &SettledCharge,
) -> Result<Uuid, costs::ProjectCostError> {
    let input = costs::draft_mercury_cost(
        &costs::MercuryCostDraft {
            transaction_id: charge.transaction,
            occurred_on: charge.posted_on,
            payee: "Synthetic provider".into(),
            description: "Synthetic bank evidence".into(),
            amount_cents: charge.amount_cents,
        },
        evidence,
    )?;
    let mut tx = pg.pool().begin().await?;
    costs::lock_project(&mut tx, project).await?;
    costs::lock_source(&mut tx, &input.source_reference).await?;
    let (cost, _) = costs::create_on(&mut tx, project, actor, &input, false).await?;
    tx.commit().await?;
    Ok(cost.id)
}

#[tokio::test]
async fn settlement_reuses_an_imported_cost_and_finances_uses_the_filing_receipt() {
    let Some((pg, matter, other)) = fixture().await else {
        return;
    };
    let project = matter.project_id;
    let filing_receipt = receipt_document(&pg, project, ENVELOPE, "internal").await;
    let bank_evidence = receipt_document(&pg, project, "SYNTHETIC-BANK", "internal").await;
    let other_evidence = receipt_document(&pg, other, "SYNTHETIC-OTHER-BANK", "internal").await;
    let charge = settled_charge();
    bank_import(&pg, other, matter.lawyer, other_evidence, &charge)
        .await
        .unwrap();
    record_postgres_with_fee(&pg, &report(project, "accepted"), Some(filing_receipt))
        .await
        .unwrap();
    assert!(settle(&pg, project, matter.lawyer, ENVELOPE, &charge)
        .await
        .is_err());

    let same_project_charge = settled_charge();
    let existing = bank_import(
        &pg,
        project,
        matter.lawyer,
        bank_evidence,
        &same_project_charge,
    )
    .await
    .unwrap();
    let settled = settle(&pg, project, matter.lawyer, ENVELOPE, &same_project_charge)
        .await
        .unwrap();
    assert_eq!(settled, existing);
    let rows = costs::for_expense_review(&pg, &[project], true)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].receipt_id, Some(filing_receipt));
    assert_eq!(rows[0].category, ExpenseCategory::CourtFilingFee);
    assert_eq!(costs::list_events(&pg, settled).await.unwrap().len(), 1);
}

#[tokio::test]
async fn bank_import_and_settlement_competing_across_projects_post_at_most_one_cost() {
    let Some((pg, matter, other)) = fixture().await else {
        return;
    };
    let filing_receipt = receipt_document(&pg, matter.project_id, ENVELOPE, "internal").await;
    let other_receipt = receipt_document(&pg, other, "SYNTHETIC-BANK", "internal").await;
    record_postgres_with_fee(
        &pg,
        &report(matter.project_id, "accepted"),
        Some(filing_receipt),
    )
    .await
    .unwrap();
    let charge = settled_charge();
    let (settlement, import) = tokio::join!(
        settle(&pg, matter.project_id, matter.lawyer, ENVELOPE, &charge),
        bank_import(&pg, other, matter.lawyer, other_receipt, &charge),
    );
    assert_ne!(settlement.is_ok(), import.is_ok());
    let source = costs::mercury_source_reference(charge.transaction);
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM project_costs WHERE source_reference=$1")
            .bind(source)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);
}
