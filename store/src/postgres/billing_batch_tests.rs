//! PostgreSQL acceptance for durable billing intent and provider replay identity.

use super::fixtures::{firm_for_notation, workflow_fixture};
use crate::billing_batches::{self as batches, BillingBatchError, MatterBillingBatch};
use crate::postgres::fixtures::{connect, fresh_schema};
use uuid::Uuid;

#[tokio::test]
async fn billing_batch_retries_keep_the_frozen_attempt_and_refuse_stale_fee_compositions() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (actor, notation, _) = workflow_fixture(&pg).await;
    let (_, project) = firm_for_notation(&pg, notation).await;
    let now = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
    let batch = synthetic_batch(actor, project, now);
    let stored = batches::insert_with_reservations(&pg, &batch, Some(0), None)
        .await
        .unwrap();
    assert_eq!(stored.line_items, batch.line_items);
    assert_eq!(
        batches::pending_for_project(&pg, project)
            .await
            .unwrap()
            .unwrap()
            .id,
        batch.id
    );
    assert!(matches!(
        batches::insert_with_reservations(
            &pg,
            &MatterBillingBatch {
                id: Uuid::new_v4(),
                ..batch.clone()
            },
            Some(0),
            None
        )
        .await,
        Err(BillingBatchError::PendingExists)
    ));
    let first = batches::start_attempt(&pg, batch.id, now).await.unwrap();
    assert_eq!(first.first_attempted_at, Some(now));
    let retry = batches::start_attempt(&pg, batch.id, now + chrono::Duration::hours(1))
        .await
        .unwrap();
    assert_eq!(retry.first_attempted_at, first.first_attempted_at);
    let ledger = |external: &'static str| {
        let pg = pg.clone();
        async move {
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO matter_invoice (id,project_id,source,external_id,reference,status,amount_cents,currency)
                 VALUES ($1,$2,'stripe',$3,'Synthetic','draft',100,'USD') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(project)
            .bind(external)
            .fetch_one(pg.pool())
            .await
            .unwrap()
        }
    };
    let (invoice, other) = (ledger("in_settled").await, ledger("in_other").await);
    batches::settle(&pg, batch.id, invoice).await.unwrap();
    batches::settle(&pg, batch.id, invoice).await.unwrap();
    assert!(matches!(
        batches::settle(&pg, batch.id, other).await,
        Err(BillingBatchError::NotFound)
    ));
    assert_eq!(
        batches::flat_fee_billed_cents(&pg, project).await.unwrap(),
        100
    );
    assert!(batches::pending_for_project(&pg, project)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        batches::insert_with_reservations(
            &pg,
            &MatterBillingBatch {
                id: Uuid::new_v4(),
                ..batch.clone()
            },
            Some(0),
            None
        )
        .await,
        Err(BillingBatchError::FlatFeeAlreadyBilled)
    ));
    assert_stripe_draft_identity(&pg, batch).await;
    assert!(matches!(
        batches::start_attempt(&pg, Uuid::new_v4(), now).await,
        Err(BillingBatchError::NotFound)
    ));
    pg.pool().close().await;
}

fn synthetic_batch(
    actor: Uuid,
    project: Uuid,
    now: chrono::DateTime<chrono::Utc>,
) -> MatterBillingBatch {
    MatterBillingBatch {
        id: Uuid::new_v4(),
        project_id: project,
        status: "pending".into(),
        reference: "Synthetic".into(),
        contact_name: "Synthetic".into(),
        contact_email: "synthetic@example.invalid".into(),
        billed_person_id: None,
        stripe_account_id: Some("acct_synthetic".into()),
        trust_state: None,
        stripe_iolta: false,
        stripe_invoice_id: None,
        due_date: now.date_naive(),
        first_attempted_at: None,
        line_items: serde_json::json!([{ "description": "Synthetic", "amount": 100 }]),
        total_cents: 100,
        currency: "USD".into(),
        entry_ids: vec![],
        cost_ids: vec![],
        matter_invoice_id: None,
        destination_approval_id: None,
        flat_fee_cents: Some(100),
        actor_id: actor,
        inserted_at: now,
        updated_at: now,
    }
}

#[test]
fn stripe_retries_keep_the_reviewed_state_or_operating_destination() {
    use crate::firm_secrets::{stripe_batch_matches_route, TrustRoute};
    let firm = Uuid::new_v4();
    let route: TrustRoute = serde_json::from_value(serde_json::json!({
        "firm": firm, "state": "WA", "account": "acct_state"
    }))
    .unwrap();
    let mut batch = synthetic_batch(Uuid::new_v4(), Uuid::new_v4(), chrono::Utc::now());
    batch.stripe_account_id = Some("acct_operating".into());
    assert!(stripe_batch_matches_route(&batch, None));
    assert!(!stripe_batch_matches_route(&batch, Some(&route)));
    batch.stripe_iolta = true;
    batch.trust_state = Some("WA".into());
    assert!(!stripe_batch_matches_route(&batch, None));
    assert!(!stripe_batch_matches_route(&batch, Some(&route)));
    batch.stripe_account_id = Some("acct_state".into());
    assert!(stripe_batch_matches_route(&batch, Some(&route)));
}

async fn assert_stripe_draft_identity(pg: &super::PostgresDb, batch: MatterBillingBatch) {
    let project = batch.project_id;
    let stripe = MatterBillingBatch {
        id: Uuid::new_v4(),
        stripe_account_id: Some("acct_synthetic".into()),
        flat_fee_cents: None,
        ..batch
    };
    batches::insert(pg, &stripe).await.unwrap();
    batches::attach_stripe_draft(pg, stripe.id, "in_synthetic")
        .await
        .unwrap();
    batches::attach_stripe_draft(pg, stripe.id, "in_synthetic")
        .await
        .unwrap();
    assert!(matches!(
        batches::attach_stripe_draft(pg, stripe.id, "in_different").await,
        Err(BillingBatchError::NotFound)
    ));
    assert_eq!(
        batches::pending_for_project(pg, project)
            .await
            .unwrap()
            .unwrap()
            .stripe_invoice_id
            .as_deref(),
        Some("in_synthetic")
    );
}

#[tokio::test]
async fn an_operating_batch_commits_with_its_retained_approval() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (actor, notation, _) = workflow_fixture(&pg).await;
    let (firm, project) = firm_for_notation(&pg, notation).await;
    let mut batch = synthetic_batch(actor, project, chrono::Utc::now());
    batch.flat_fee_cents = None;
    let stored = batches::insert_with_reservations(
        &pg,
        &batch,
        None,
        Some(&batches::OperatingApproval {
            firm_id: firm,
            reason: "Earned fees for completed work",
            preview_digest: "synthetic-digest",
        }),
    )
    .await
    .unwrap();
    let approval = stored.destination_approval_id.expect("approval linked");
    let recorded = crate::invoice_destinations::find(&pg, firm, project, approval)
        .await
        .unwrap()
        .expect("approval retained");
    assert_eq!(
        (recorded.approved_by, recorded.reason.as_str(), recorded.preview_digest.as_str()),
        (actor, "Earned fees for completed work", "synthetic-digest")
    );
    assert!(crate::invoice_destinations::find(&pg, uuid::Uuid::new_v4(), project, approval)
        .await
        .unwrap()
        .is_none(), "another Firm reads nothing");
    for statement in [
        "UPDATE invoice_destination_approval SET reason='changed' WHERE id=$1",
        "DELETE FROM invoice_destination_approval WHERE id=$1",
    ] {
        assert!(sqlx::query(statement).bind(approval).execute(pg.pool()).await.is_err());
    }
    pg.pool().close().await;
}
