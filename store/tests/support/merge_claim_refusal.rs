//! Legal Plan Claim actors retain their original immutable collection evidence.
use serde_json::Value;
use store::{application_store::ApplicationStore, plan_claims, postgres::PostgresDb};
use uuid::Uuid;

async fn claim_rows(pg: &PostgresDb) -> Vec<Value> {
    let mut rows = Vec::new();
    for table in ["legal_plan_claim", "legal_plan_claim_event"] {
        rows.push(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM {table} t"
        ))).fetch_one(pg.pool()).await.unwrap());
    }
    rows
}

async fn history(
    pg: &PostgresDb,
    db: &ApplicationStore,
    project: &store::projects::Project,
    actor: Uuid,
) {
    let firm = project.firm_id.unwrap();
    let plan = store::legal_plans::create(
        pg,
        firm,
        &store::legal_plans::NewLegalPlan {
            name: "ARAG".into(),
            contact: None,
            portal: None,
            acceptance_sla_hours: Some(24),
            turnaround_sla_days: Some(3),
            reporting_requirements: None,
        },
    )
    .await
    .unwrap();
    store::projects::set_plan_columns(
        db,
        pg,
        project.id,
        &store::projects::PlanColumns {
            legal_plan_id: Some(plan.id),
            plan_case_number: Some("synthetic-plan-case".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let today = chrono::Utc::now().date_naive();
    let work = plan_claims::Work {
        service: plan_claims::Service::AragRepresentation,
        benefit: "SYNTHETIC".into(),
        performed_on: today,
        attorney_minutes: Some(90),
        time_entry_ids: Vec::new(),
        authorization: Some(plan_claims::Authorization::FixedFee {
            reference: "synthetic-fee-authorization".into(),
            cents: 12_000,
        }),
    };
    let claim = Uuid::new_v4();
    for event in [
        plan_claims::Event::WorkRecorded { work },
        plan_claims::Event::Prepared,
        plan_claims::Event::Submission {
            provider_reference: "synthetic-claim-filing".into(),
            claimed_cents: Some(12_000),
        },
        plan_claims::Event::Approval {
            reference: "synthetic-claim-approval".into(),
            cents: 12_000,
        },
        plan_claims::Event::Payment {
            reference: "synthetic-claim-payment".into(),
            cents: 5_000,
        },
        plan_claims::Event::Reversal {
            reference: "synthetic-claim-reversal".into(),
            cents: 1_000,
        },
    ] {
        plan_claims::record(
            pg,
            project.id,
            firm,
            actor,
            plan_claims::Change {
                claim_id: claim,
                event_id: Uuid::new_v4(),
                occurred_on: today,
                event,
            },
        )
        .await
        .unwrap();
    }
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM legal_plan_claim_event WHERE claim_id=$1 AND actor_id=$2",
    )
    .bind(claim)
    .bind(actor)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(
        events, 6,
        "real collection and reversal evidence must exist"
    );
}

#[tokio::test]
async fn retained_claim_actor_refuses_merge_without_rewriting_collection_or_identity_history() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let pg = PostgresDb::application().await.unwrap();
        let parent = super::native_project::create("synthetic-claim-merge-boundary").await;
        let scenario = super::merge_fixture::Scenario::seed(&db, &pg, &parent).await;
        store::persons::set_role(&db, scenario.dropped, store::persons::Role::Lawyer)
            .await
            .unwrap()
            .unwrap();
        history(&pg, &db, &scenario.target, scenario.dropped).await;
        store::persons::set_role(&db, scenario.dropped, store::persons::Role::Client)
            .await
            .unwrap()
            .unwrap();
        let canonical_before = super::merge_fixture::snapshot(&pg).await;
        let claims_before = claim_rows(&pg).await;
        assert!(matches!(
            store::person_merge::merge(&db, scenario.kept, scenario.dropped).await,
            Err(store::person_merge::PersonMergeError::RetainedClaimHistory)
        ));
        assert_eq!(super::merge_fixture::snapshot(&pg).await, canonical_before);
        assert_eq!(claim_rows(&pg).await, claims_before);
        assert!(store::persons::find_by_id(&db, scenario.dropped)
            .await
            .unwrap()
            .is_some());
        assert!(store::persons::find_by_id(&db, scenario.kept)
            .await
            .unwrap()
            .is_some());
    })
    .await;
}
