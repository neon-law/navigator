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
    let stored = batches::insert_with_reservations(&pg, &batch, Some(0))
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
            Some(0)
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
    batches::attach_invoice(&pg, batch.id, "synthetic-invoice")
        .await
        .unwrap();
    batches::attach_invoice(&pg, batch.id, "synthetic-invoice")
        .await
        .unwrap();
    assert!(matches!(
        batches::attach_invoice(&pg, batch.id, "different-invoice").await,
        Err(BillingBatchError::NotFound)
    ));
    batches::complete(&pg, batch.id).await.unwrap();
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
            Some(0)
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
        xero_contact_id: Some("synthetic-contact".into()),
        stripe_account_id: None,
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
        xero_invoice_id: None,
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
        xero_contact_id: None,
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
