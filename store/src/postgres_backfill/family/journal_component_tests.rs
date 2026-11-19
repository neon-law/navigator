//! Row-only component copies use real persisted synthetic enrollment receipts.
//! These tests exercise importer admission, not evidence of a live Source export.
use super::{during_import, Family};
use crate::firm_journal::{
    BeneficialOwnership, Category, JournalEntry, JournalError, JournalLink, LinkKind, Posting,
    SourceReference, Unit,
};
use crate::journal_startup_migration::{
    import_captured_event, CapturedImportReport, CapturedJournalRow,
};
use crate::postgres::PostgresDb;
use uuid::Uuid;

const AT: &str = "2026-10-12T07:08:09.123456789+05:30";
async fn fixture() -> PostgresDb {
    let (options, database) = crate::postgres_mirror::tests::fresh_schema()
        .await
        .expect("required PostgreSQL fixture");
    crate::postgres_mirror::tests::connect(&options, &database).await
}
async fn firm(pg: &PostgresDb) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO firm(id,name,slug,status) VALUES($1,'Synthetic Journal Firm',$2,'active')",
    )
    .bind(id)
    .bind(id.to_string())
    .execute(pg.pool())
    .await
    .unwrap();
    id
}
fn entry(firm: Uuid, id: u128, key: &str) -> JournalEntry {
    JournalEntry {
        id: Uuid::from_u128(id),
        firm_id: firm,
        key: key.into(),
        actor_id: Uuid::new_v4(),
        occurred_at: AT.into(),
        memo: "Synthetic component".into(),
        postings: vec![posting("cash", "100"), posting("equity", "-100")],
        sources: vec![],
        receipts: vec![],
        links: vec![],
        reversal_of: None,
    }
}
fn posting(account: &str, quantity: &str) -> Posting {
    Posting {
        account: account.into(),
        ownership: BeneficialOwnership::Firm,
        restrictions: vec![],
        unit: Unit::fiat("USD", 2),
        quantity: quantity.into(),
        project_id: None,
        category: Category::Cash,
    }
}
fn captured(entry: &JournalEntry) -> CapturedJournalRow {
    CapturedJournalRow {
        id: entry.id,
        firm_id: entry.firm_id.to_string(),
        command_key: entry.key.clone(),
        payload: serde_json::to_string(entry).unwrap(),
        inserted_at: Some(AT.into()),
    }
}
async fn enroll(pg: &PostgresDb, rows: &[CapturedJournalRow]) -> Uuid {
    let bytes = serde_json::to_vec(rows).unwrap();
    let mut receipt = super::tests::receipt(Family::Application, &bytes);
    receipt
        .table_counts
        .insert("journal_entry".into(), i64::try_from(rows.len()).unwrap());
    let run = Uuid::new_v4();
    super::enroll(
        pg,
        run,
        Family::Application,
        1,
        receipt.verify_export(&bytes).unwrap(),
    )
    .await
    .unwrap();
    run
}
async fn copy(
    pg: &PostgresDb,
    run: Uuid,
    rows: &[CapturedJournalRow],
    requested: Uuid,
) -> Result<CapturedImportReport, JournalError> {
    crate::postgres_backfill::with_import_lock(pg, async {
        Ok(during_import(
            run,
            Family::Application,
            import_captured_event(rows, pg, requested),
        )
        .await)
    })
    .await
    .expect("isolated pending import lease")
}
fn movement() -> SourceReference {
    SourceReference {
        provider: "synthetic".into(),
        scope: "captured".into(),
        account: "cash".into(),
        object_type: "transaction".into(),
        external_id: "one-movement".into(),
        locator: None,
    }
}

#[tokio::test]
async fn replacement_uuid_before_old_and_reversal_copies_one_atomic_component_then_replays() {
    let pg = fixture().await;
    let firm = firm(&pg).await;
    let mut old = entry(firm, 2, "old");
    old.sources.push(movement());
    let mut replacement = entry(firm, 1, "replacement");
    replacement.sources = old.sources.clone();
    let mut reversal = old.reversal("cancel", Uuid::new_v4(), AT).unwrap();
    reversal.id = Uuid::from_u128(3);
    let rows = vec![captured(&replacement), captured(&old), captured(&reversal)];
    let run = enroll(&pg, &rows).await;
    let report = copy(&pg, run, &rows, replacement.id).await.unwrap();
    assert_eq!(
        (
            report.imported_requested,
            report.imported_dependencies,
            report.retained_dependencies
        ),
        (1, 2, 0)
    );
    let replay = copy(&pg, run, &rows, replacement.id).await.unwrap();
    assert_eq!(
        (
            replay.imported_requested,
            replay.imported_dependencies,
            replay.retained_dependencies
        ),
        (0, 0, 2)
    );
    let stored = crate::firm_journal::entries(&pg, firm).await.unwrap();
    assert_eq!(stored.len(), 3);
    assert!(stored.contains(&old) && stored.contains(&replacement) && stored.contains(&reversal));
    let mut changed = rows.clone();
    let mut drift = replacement.clone();
    drift.memo = "Conflicting captured value".into();
    changed[0] = captured(&drift);
    assert!(copy(&pg, run, &changed, replacement.id).await.is_err());
    assert_eq!(
        crate::firm_journal::entries(&pg, firm).await.unwrap(),
        stored
    );
}

#[tokio::test]
async fn repayment_uuid_before_cost_copies_causal_cost_without_time_order_or_cap_policy() {
    let pg = fixture().await;
    let firm = firm(&pg).await;
    let project = Uuid::new_v4();
    let engagement = JournalLink {
        kind: LinkKind::Engagement,
        verified_by: Uuid::new_v4(),
        evidence_document_id: Uuid::new_v4(),
        target_id: Uuid::new_v4(),
        verified_at: AT.into(),
    };
    let mut cost = entry(firm, 12, "cost");
    cost.postings[0].category = Category::AdvancedCost;
    cost.postings[0].project_id = Some(project);
    cost.links.push(engagement.clone());
    let mut repayment = entry(firm, 11, "repayment");
    repayment.postings[0].category = Category::Reimbursement;
    repayment.postings[0].project_id = Some(project);
    repayment.postings[0].quantity = "-100".into();
    repayment.postings[1].quantity = "100".into();
    repayment.links = vec![
        engagement,
        JournalLink {
            kind: LinkKind::Repayment,
            verified_by: Uuid::new_v4(),
            evidence_document_id: Uuid::new_v4(),
            target_id: cost.id,
            verified_at: AT.into(),
        },
    ];
    let rows = vec![captured(&repayment), captured(&cost)];
    let run = enroll(&pg, &rows).await;
    let report = copy(&pg, run, &rows, repayment.id).await.unwrap();
    assert_eq!(
        (report.imported_requested, report.imported_dependencies),
        (1, 1)
    );
    let stored = crate::firm_journal::entries(&pg, firm).await.unwrap();
    assert_eq!(stored.len(), 2);
    assert!(stored.contains(&cost) && stored.contains(&repayment));
}

#[tokio::test]
async fn component_copy_refuses_missing_pending_scope_even_in_initial_copy_or_source_context() {
    let pg = fixture().await;
    let firm = firm(&pg).await;
    let event = entry(firm, 21, "not-pending");
    let rows = vec![captured(&event)];
    assert!(
        matches!(import_captured_event(&rows,&pg,event.id).await,Err(JournalError::Invalid(reason)) if reason=="Journal import is not pending")
    );
    let forced = crate::postgres_mirror::from_source(crate::postgres_mirror::during_initial_copy(
        import_captured_event(&rows, &pg, event.id),
    ))
    .await;
    assert!(
        matches!(forced,Err(JournalError::Invalid(reason)) if reason=="Journal import is not pending")
    );
    assert!(crate::firm_journal::entries(&pg, firm)
        .await
        .unwrap()
        .is_empty());
    let run = enroll(&pg, &rows).await;
    assert_eq!(
        copy(&pg, run, &rows, event.id)
            .await
            .unwrap()
            .imported_requested,
        1
    );
}

#[tokio::test]
async fn completed_ownership_refuses_component_and_operator_replay_despite_source_overrides() {
    let pg = fixture().await;
    // This is a known-empty synthetic fixture, activated before any domain data.
    super::activate_fixture(&pg, Family::Application).await;
    let firm = firm(&pg).await;
    let event = entry(firm, 31, "already-owned");
    let rows = vec![captured(&event)];
    let completed: Uuid = sqlx::query_scalar("SELECT id FROM mirror_backfill_run WHERE scope=$1")
        .bind(Family::Application.scope())
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let result = during_import(
        completed,
        Family::Application,
        crate::postgres_mirror::from_source(crate::postgres_mirror::during_initial_copy(
            import_captured_event(&rows, &pg, event.id),
        )),
    )
    .await;
    assert!(
        matches!(result,Err(JournalError::Invalid(reason)) if reason=="Journal import is not pending")
    );
    let inert = crate::surreal::SurrealDb::uninitialized();
    assert!(
        crate::postgres_mirror::from_source(crate::journal_startup_migration::import_firm(
            &inert, &pg, firm
        ))
        .await
        .is_err()
    );
    assert!(crate::firm_journal::entries(&pg, firm)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn pending_component_waits_on_native_firm_lock_and_retains_committed_metadata() {
    let pg = fixture().await;
    let firm = firm(&pg).await;
    let event = entry(firm, 41, "serialized");
    let rows = vec![captured(&event)];
    let run = enroll(&pg, &rows).await;
    let mut native = pg.pool().begin().await.unwrap();
    sqlx::query("SELECT id FROM firm WHERE id=$1 FOR UPDATE")
        .bind(firm)
        .execute(&mut *native)
        .await
        .unwrap();
    let copying_pg = pg.clone();
    let mut importing = tokio::spawn(async move { copy(&copying_pg, run, &rows, event.id).await });
    let waiting = async {
        loop {
            let blocked:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%FROM firm WHERE id%FOR UPDATE%')")
                .fetch_one(pg.pool()).await.unwrap();
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::select! {
        result=&mut importing=>panic!("component completed before native lock release: {result:?}"),
        result=tokio::time::timeout(std::time::Duration::from_secs(10),waiting)=>result.expect("component must wait on native Firm lock"),
    }
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM journal_entry")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(before, 0);
    sqlx::query("UPDATE firm SET name='Retained native metadata' WHERE id=$1")
        .bind(firm)
        .execute(&mut *native)
        .await
        .unwrap();
    native.commit().await.unwrap();
    assert_eq!(importing.await.unwrap().unwrap().imported_requested, 1);
    let name: String = sqlx::query_scalar("SELECT name FROM firm WHERE id=$1")
        .bind(firm)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(name, "Retained native metadata");
}

#[tokio::test]
async fn invalid_balanced_reversal_refuses_the_entire_pending_component_before_effects() {
    let pg = fixture().await;
    let firm = firm(&pg).await;
    let mut old = entry(firm, 52, "old-balanced");
    old.sources.push(movement());
    let mut replacement = entry(firm, 51, "new-balanced");
    replacement.sources = old.sources.clone();
    let mut reversal = old.reversal("wrong-negation", Uuid::new_v4(), AT).unwrap();
    reversal.id = Uuid::from_u128(53);
    reversal.postings[0].quantity = "-99".into();
    reversal.postings[1].quantity = "99".into();
    reversal.validate().unwrap();
    let rows = vec![captured(&replacement), captured(&old), captured(&reversal)];
    let run = enroll(&pg, &rows).await;
    assert!(matches!(
        copy(&pg, run, &rows, replacement.id).await,
        Err(JournalError::Conflict)
    ));
    assert!(crate::firm_journal::entries(&pg, firm)
        .await
        .unwrap()
        .is_empty());
}
