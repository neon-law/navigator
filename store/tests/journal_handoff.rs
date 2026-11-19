use chrono::{DateTime, Utc};
use futures::FutureExt;
use sha2::{Digest, Sha256};
use store::{
    firm_journal::*,
    journal_startup_migration::{
        CapturedJournalRow, JournalDifferenceReason as Reason, handoff_captured,
    },
    postgres::PostgresDb,
};
use uuid::Uuid;

async fn database() -> (PostgresDb, sqlx::postgres::PgConnectOptions, String) {
    let port = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT")
        .expect("required PostgreSQL fixture")
        .parse::<u16>()
        .unwrap();
    let options = sqlx::postgres::PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(sqlx::postgres::PgSslMode::Disable);
    let admin = sqlx::PgPool::connect_with(options.clone()).await.unwrap();
    let name = format!("journal_handoff_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = PostgresDb::connect(options.clone().database(&name))
        .await
        .unwrap();
    (pg, options, name)
}

async fn with_postgres<F: std::future::Future<Output = ()>>(test: impl FnOnce(PostgresDb) -> F) {
    let (pg, options, name) = database().await;
    let result = std::panic::AssertUnwindSafe(async {
        pg.migrate_embedded(None).await.unwrap();
        test(pg.clone()).await;
    })
    .catch_unwind()
    .await;
    pg.pool().close().await;
    let admin = sqlx::PgPool::connect_with(options).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {name} WITH (FORCE)"
    )))
    .execute(&admin)
    .await
    .unwrap();
    admin.close().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
async fn firm(pg: &PostgresDb) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO firm(id,name,slug,status) VALUES($1,'Synthetic',$2,'active')")
        .bind(id)
        .bind(id.to_string())
        .execute(pg.pool())
        .await
        .unwrap();
    id
}
fn event(firm: Uuid, key: &str) -> JournalEntry {
    JournalEntry {
        id: Uuid::new_v4(),
        firm_id: firm,
        key: key.into(),
        actor_id: Uuid::new_v4(),
        occurred_at: "2026-10-11T07:12:13.123456789+05:30".into(),
        memo: "Synthetic private event".into(),
        postings: vec![
            Posting {
                account: "cash".into(),
                ownership: BeneficialOwnership::Firm,
                restrictions: vec![],
                unit: Unit::fiat("USD", 2),
                quantity: "100".into(),
                project_id: Some(Uuid::new_v4()),
                category: Category::Cash,
            },
            Posting {
                account: "equity".into(),
                ownership: BeneficialOwnership::Firm,
                restrictions: vec![],
                unit: Unit::fiat("USD", 2),
                quantity: "-100".into(),
                project_id: None,
                category: Category::Cash,
            },
        ],
        sources: vec![],
        receipts: vec![],
        links: vec![],
        reversal_of: None,
    }
}
fn captured(entry: &JournalEntry, timestamp: Option<&str>) -> CapturedJournalRow {
    CapturedJournalRow {
        id: entry.id,
        firm_id: entry.firm_id.to_string(),
        command_key: entry.key.clone(),
        payload: serde_json::to_string(entry).unwrap(),
        inserted_at: timestamp.map(str::to_owned),
    }
}
const AT: &str = "2026-10-12T01:02:03.987654321-07:00";
async fn insert(pg: &PostgresDb, entry: &JournalEntry, legacy: Option<&str>) {
    let mut tx = pg.pool().begin().await.unwrap();
    sqlx::query("INSERT INTO journal_entry(id,firm_id,command_key,actor_id,occurred_at,memo,payload,digest,reversal_of,inserted_at,legacy_imported_at,legacy_inserted_at_known,legacy_inserted_at_source) VALUES($1,$2,$3,$4,$5::timestamptz,$6,$7,$8,$9,$10::timestamptz,CASE WHEN $11 THEN now() ELSE NULL END,CASE WHEN $11 THEN true ELSE NULL END,$12)")
        .bind(entry.id).bind(entry.firm_id).bind(&entry.key).bind(entry.actor_id).bind(&entry.occurred_at).bind(&entry.memo)
        .bind(sqlx::types::Json(entry)).bind(entry_digest(entry).unwrap()).bind(entry.reversal_of)
        .bind(legacy.unwrap_or("2026-11-19T00:00:00Z")).bind(legacy.is_some()).bind(legacy)
        .execute(&mut *tx).await.unwrap();
    for (position, p) in entry.postings.iter().enumerate() {
        sqlx::query("INSERT INTO journal_posting(entry_id,firm_id,position,account,unit,quantity,project_id,category,ownership,restrictions) VALUES($1,$2,$3,$4,$5,$6::numeric,$7,$8,$9,$10)")
            .bind(entry.id).bind(entry.firm_id).bind(i32::try_from(position).unwrap()).bind(&p.account)
            .bind(sqlx::types::Json(&p.unit)).bind(&p.quantity).bind(p.project_id)
            .bind(serde_json::to_value(p.category).unwrap().as_str().unwrap())
            .bind(serde_json::to_value(p.ownership).unwrap().as_str().unwrap()).bind(sqlx::types::Json(&p.restrictions))
            .execute(&mut *tx).await.unwrap();
    }
    for source in &entry.sources {
        let id = hex::encode(Sha256::digest(
            format!(
                "{}:{}",
                entry.firm_id,
                serde_json::to_string(source).unwrap()
            )
            .as_bytes(),
        ));
        sqlx::query("INSERT INTO journal_source_reference(id,firm_id,provider,provider_scope,account,object_type,external_id,locator,payload) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(id) DO NOTHING")
            .bind(&id).bind(entry.firm_id).bind(&source.provider).bind(&source.scope).bind(&source.account).bind(&source.object_type).bind(&source.external_id).bind(&source.locator).bind(sqlx::types::Json(source)).execute(&mut *tx).await.unwrap();
        sqlx::query("INSERT INTO journal_entry_source(entry_id,source_id) VALUES($1,$2)")
            .bind(entry.id)
            .bind(id)
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
}
async fn standing(pg: &PostgresDb) -> serde_json::Value {
    sqlx::query_scalar("SELECT jsonb_build_object('headers',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM journal_entry r),'postings',(SELECT jsonb_agg(to_jsonb(r) ORDER BY entry_id,position) FROM journal_posting r),'sources',(SELECT jsonb_agg(to_jsonb(r) ORDER BY id) FROM journal_source_reference r),'links',(SELECT jsonb_agg(to_jsonb(r) ORDER BY entry_id,source_id) FROM journal_entry_source r))")
        .fetch_one(pg.pool()).await.unwrap()
}
fn movement() -> SourceReference {
    SourceReference {
        provider: "synthetic".into(),
        scope: "firm".into(),
        account: "cash".into(),
        object_type: "payment".into(),
        external_id: "synthetic-payment".into(),
        locator: None,
    }
}

#[tokio::test]
async fn native_overlap_and_retained_target_only_are_read_only_and_clean() {
    with_postgres(|pg| async move {
        let firm = firm(&pg).await;
        let first = event(firm, "captured-private-key");
        let other = event(firm, "native-private-key");
        insert(&pg, &first, None).await;
        insert(&pg, &other, None).await;
        let before = standing(&pg).await;
        let report = handoff_captured(&[captured(&first, Some(AT))], &pg)
            .await
            .unwrap();
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(
            (
                report.source_rows,
                report.represented_source_rows,
                report.compared_overlaps,
                report.source_only_remaining,
                report.target_rows,
                report.native_target_only,
                report.unresolved_source_rows,
                report.target_integrity_failures
            ),
            (1, 1, 1, 0, 2, 1, 0, 0)
        );
        assert_eq!(before, standing(&pg).await);
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("private"));
    })
    .await;
}

#[tokio::test]
async fn source_only_and_unknown_timestamps_block_even_existing_native_overlaps() {
    with_postgres(|pg| async move {
        let firm = firm(&pg).await;
        let first = event(firm, "captured");
        insert(&pg, &first, None).await;
        for timestamp in [None, Some("unknown")] {
            let report = handoff_captured(&[captured(&first, timestamp)], &pg)
                .await
                .unwrap();
            assert!(!report.is_clean());
            assert_eq!(
                (
                    report.compared_overlaps,
                    report.represented_source_rows,
                    report.unresolved_source_rows
                ),
                (1, 0, 1)
            );
            assert!(
                report
                    .differences
                    .iter()
                    .any(|d| d.reason == Reason::SourceTimestamp)
            );
        }
        let missing = event(firm, "missing");
        let report = handoff_captured(&[captured(&missing, Some(AT))], &pg)
            .await
            .unwrap();
        assert_eq!(
            (
                report.source_only_remaining,
                report.native_target_only,
                report.represented_source_rows
            ),
            (1, 1, 0)
        );
        assert!(!report.is_clean());
    })
    .await;
}

#[tokio::test]
async fn physical_identity_outer_fields_and_duplicate_capture_rows_are_rejected() {
    with_postgres(|pg| async move {
        let original = captured(&event(Uuid::new_v4(), "private-command-key"), Some(AT));
        let mut bad = original.clone();
        bad.id = Uuid::new_v4();
        assert!(handoff_captured(&[bad], &pg).await.is_err());
        let mut bad = original.clone();
        bad.firm_id = Uuid::new_v4().to_string();
        assert!(handoff_captured(&[bad], &pg).await.is_err());
        let mut bad = original.clone();
        bad.command_key = "different-private-key".into();
        assert!(handoff_captured(&[bad], &pg).await.is_err());
        assert!(
            handoff_captured(&[original.clone(), original], &pg)
                .await
                .is_err()
        );
    })
    .await;
}

#[tokio::test]
async fn known_legacy_provenance_preserves_offset_nanoseconds_and_exact_capture_text() {
    with_postgres(|pg| async move {
        let entry = event(firm(&pg).await, "legacy");
        insert(&pg, &entry, Some(AT)).await;
        let before = standing(&pg).await;
        assert!(
            handoff_captured(&[captured(&entry, Some(AT))], &pg)
                .await
                .unwrap()
                .is_clean()
        );
        let equivalent = DateTime::parse_from_rfc3339(AT)
            .unwrap()
            .with_timezone(&Utc)
            .to_rfc3339();
        let report = handoff_captured(&[captured(&entry, Some(&equivalent))], &pg)
            .await
            .unwrap();
        assert!(
            !report.is_clean(),
            "legacy evidence retains exact original Source text"
        );
        assert!(
            report
                .differences
                .iter()
                .any(|d| d.reason == Reason::LegacyProvenance)
        );
        assert_eq!(before, standing(&pg).await);
    })
    .await;
}

#[tokio::test]
async fn balanced_extra_postings_after_immutable_header_are_not_hidden_by_payload_digest() {
    with_postgres(|pg| async move {

    let entry = event(firm(&pg).await, "projection");
    insert(&pg, &entry, None).await;
    sqlx::query("INSERT INTO journal_posting(entry_id,firm_id,position,account,unit,quantity,category,ownership,restrictions) SELECT entry_id,firm_id,position+2,account,unit,quantity,category,ownership,restrictions FROM journal_posting WHERE entry_id=$1")
        .bind(entry.id).execute(pg.pool()).await.unwrap();
    let before = standing(&pg).await;
    let report = handoff_captured(&[captured(&entry, Some(AT))], &pg)
        .await
        .unwrap();
    assert!(!report.is_clean());
    assert_eq!(
        (
            report.target_integrity_failures,
            report.represented_source_rows
        ),
        (1, 0)
    );
    assert!(
        report
            .differences
            .iter()
            .any(|d| d.reason == Reason::PostingProjection)
    );
    assert_eq!(before, standing(&pg).await);

    }).await;
}

#[tokio::test]
async fn reversal_then_replacement_is_clean_but_balanced_wrong_reversal_is_not() {
    with_postgres(|pg| async move {
        let firm = firm(&pg).await;
        let mut original = event(firm, "original");
        original.sources = vec![movement()];
        let reversal = original
            .reversal("reversal", Uuid::new_v4(), "2025-01-01T00:00:00Z")
            .unwrap();
        let mut replacement = event(firm, "replacement");
        replacement.sources = original.sources.clone();
        for entry in [&original, &reversal, &replacement] {
            insert(&pg, entry, None).await;
        }
        let rows = [&original, &reversal, &replacement].map(|entry| captured(entry, Some(AT)));
        assert!(handoff_captured(&rows, &pg).await.unwrap().is_clean());
        let other = event(firm, "other-original");
        insert(&pg, &other, None).await;
        let mut wrong = other.reversal("wrong", Uuid::new_v4(), AT).unwrap();
        wrong.postings[0].quantity = "-99".into();
        wrong.postings[1].quantity = "99".into();
        insert(&pg, &wrong, None).await;
        let report = handoff_captured(&[], &pg).await.unwrap();
        assert!(!report.is_clean());
        assert_eq!(report.target_integrity_failures, 1);
        assert!(
            report
                .differences
                .iter()
                .any(|d| d.entry_id == Some(wrong.id) && d.reason == Reason::ReversalNegation)
        );
    })
    .await;
}

#[tokio::test]
async fn missing_cross_firm_and_cyclic_causal_parents_block_without_chronology_guessing() {
    with_postgres(|pg| async move {
        let first = event(firm(&pg).await, "first");
        let mut second = first.reversal("cross-firm", Uuid::new_v4(), AT).unwrap();
        second.firm_id = firm(&pg).await;
        insert(&pg, &first, None).await;
        insert(&pg, &second, None).await;
        let report = handoff_captured(&[], &pg).await.unwrap();
        assert!(!report.is_clean());
        assert_eq!(report.target_integrity_failures, 1);
        let mut a = event(first.firm_id, "cycle-a");
        let mut b = a.reversal("cycle-b", Uuid::new_v4(), AT).unwrap();
        a.reversal_of = Some(b.id);
        b.postings = a
            .postings
            .iter()
            .cloned()
            .map(|mut p| {
                p.quantity = (-p.quantity.parse::<i128>().unwrap()).to_string();
                p
            })
            .collect();
        let report = handoff_captured(&[captured(&a, Some(AT)), captured(&b, Some(AT))], &pg)
            .await
            .unwrap();
        assert!(!report.is_clean());
        assert_eq!(report.unresolved_source_rows, 2);
        assert!(
            report
                .differences
                .iter()
                .any(|d| d.reason == Reason::ReversalCycle)
        );
    })
    .await;
}

#[tokio::test]
async fn source_reference_projection_and_active_movement_uniqueness_cover_native_extras() {
    with_postgres(|pg| async move {

    let firm = firm(&pg).await;
    let mut first = event(firm, "first");
    first.sources = vec![movement()];
    let mut duplicate = event(firm, "duplicate");
    duplicate.sources = first.sources.clone();
    insert(&pg, &first, None).await;
    insert(&pg, &duplicate, None).await;
    let report = handoff_captured(&[], &pg).await.unwrap();
    assert_eq!(report.target_integrity_failures, 2);
    assert!(
        report
            .differences
            .iter()
            .any(|d| d.reason == Reason::ActiveMovement)
    );
    let extra = event(firm, "extra-source-link");
    insert(&pg, &extra, None).await;
    sqlx::query("INSERT INTO journal_entry_source(entry_id,source_id) SELECT $1,source_id FROM journal_entry_source WHERE entry_id=$2")
        .bind(extra.id).bind(first.id).execute(pg.pool()).await.unwrap();
    let report = handoff_captured(&[], &pg).await.unwrap();
    assert_eq!(report.target_integrity_failures, 3);
    assert!(
        report
            .differences
            .iter()
            .any(|d| d.entry_id == Some(extra.id) && d.reason == Reason::SourceProjection)
    );

    }).await;
}

#[tokio::test]
async fn native_history_beyond_one_page_is_fully_checked_without_source_row_caps() {
    with_postgres(|pg| async move {
        let firm = firm(&pg).await;
        for n in 0..34 {
            insert(&pg, &event(firm, &format!("native-{n}")), None).await;
        }
        let mut last = event(firm, "last-page-projection");
        last.id = Uuid::from_u128(u128::MAX);
        insert(&pg, &last, None).await;
        sqlx::query("INSERT INTO journal_posting(entry_id,firm_id,position,account,unit,quantity,category,ownership,restrictions) SELECT entry_id,firm_id,position+2,account,unit,quantity,category,ownership,restrictions FROM journal_posting WHERE entry_id=$1")
            .bind(last.id).execute(pg.pool()).await.unwrap();
        let report = handoff_captured(&[], &pg).await.unwrap();
        assert_eq!((report.target_rows, report.native_target_only, report.target_integrity_failures), (35,35,1));
        assert!(report.differences.iter().any(|difference| difference.entry_id == Some(last.id) && difference.reason == Reason::PostingProjection));
        assert!(!report.is_clean());
    }).await;
}

fn approval(kind: LinkKind, target: Uuid) -> JournalLink {
    JournalLink {
        kind,
        target_id: target,
        verified_by: Uuid::new_v4(),
        verified_at: "2020-01-01T00:00:00Z".into(),
        evidence_document_id: Uuid::new_v4(),
    }
}
fn cost(firm: Uuid, key: &str, project: Uuid) -> JournalEntry {
    let mut entry = event(firm, key);
    entry.postings[0].category = Category::AdvancedCost;
    entry.postings[0].project_id = Some(project);
    entry.links = vec![approval(LinkKind::Engagement, project)];
    entry
}
fn repayment(cost: &JournalEntry, key: &str, amount: i128) -> JournalEntry {
    let mut entry = event(cost.firm_id, key);
    entry.postings[0].quantity = amount.checked_neg().unwrap().to_string();
    entry.postings[0].project_id = None;
    entry.postings[1].category = Category::Reimbursement;
    entry.postings[1].project_id = cost.postings[0].project_id;
    entry.postings[1].quantity = amount.to_string();
    entry.links = cost.links.clone();
    entry.links.push(approval(LinkKind::Repayment, cost.id));
    entry
}

#[tokio::test]
async fn historical_partial_full_nested_reversals_and_later_cost_refund_remain_accepted() {
    with_postgres(|pg| async move {
        let original = cost(firm(&pg).await, "historical-cost", Uuid::new_v4());
        let first = repayment(&original, "partial", -40);
        let second = repayment(&original, "complete", -60);
        let reverse = first
            .reversal("reverse-partial", Uuid::new_v4(), AT)
            .unwrap();
        let replace = repayment(&original, "replace-partial", -40);
        let nested = reverse
            .reversal("nested-reverse", Uuid::new_v4(), AT)
            .unwrap();
        let refunded = original
            .reversal("refund-cost", Uuid::new_v4(), AT)
            .unwrap();
        for entry in [
            &original, &first, &second, &reverse, &replace, &nested, &refunded,
        ] {
            insert(&pg, entry, None).await;
        }
        let before = standing(&pg).await;
        let rows = [
            &original, &first, &second, &reverse, &replace, &nested, &refunded,
        ]
        .map(|entry| captured(entry, Some(AT)));
        let report = handoff_captured(&rows, &pg).await.unwrap();
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(before, standing(&pg).await);
    })
    .await;
}

#[tokio::test]
async fn retargeted_reversal_links_do_not_invent_a_per_entry_or_final_repayment_cap() {
    with_postgres(|pg| async move {
        let firm = firm(&pg).await;
        let project = Uuid::new_v4();
        let first_cost = cost(firm, "cost-a", project);
        let second_cost = cost(firm, "cost-b", project);
        let first = repayment(&first_cost, "repayment-a", -100);
        let mut reverse = first
            .reversal("retargeted-reverse", Uuid::new_v4(), AT)
            .unwrap();
        reverse
            .links
            .iter_mut()
            .find(|link| link.kind == LinkKind::Repayment)
            .unwrap()
            .target_id = second_cost.id;
        let second = repayment(&second_cost, "repayment-b", -200);
        for entry in [&first_cost, &second_cost, &first, &reverse, &second] {
            insert(&pg, entry, None).await;
        }
        let report = handoff_captured(&[], &pg).await.unwrap();
        assert!(report.is_clean(), "{report:?}");
        assert_eq!(report.native_target_only, 5);
    })
    .await;
}

#[tokio::test]
async fn repayment_shape_missing_cross_firm_and_overflowing_cost_evidence_are_rejected() {
    with_postgres(|pg| async move {
        let firm_id = firm(&pg).await;
        let original = cost(firm_id, "valid-cost", Uuid::new_v4());
        insert(&pg, &original, None).await;
        let mut missing = repayment(&original, "missing-cost", -40);
        missing
            .links
            .iter_mut()
            .find(|link| link.kind == LinkKind::Repayment)
            .unwrap()
            .target_id = Uuid::new_v4();
        let mut wrong_firm = repayment(&original, "cross-firm-cost", -40);
        wrong_firm.firm_id = firm(&pg).await;
        let positive = repayment(&original, "positive-reimbursement", 40);
        let mut duplicate = repayment(&original, "duplicate-allocation", -40);
        duplicate.postings[1].quantity = "-20".into();
        duplicate.postings.push(duplicate.postings[1].clone());
        let mut two_links = repayment(&original, "two-cost-links", -40);
        two_links
            .links
            .push(approval(LinkKind::Repayment, original.id));
        for entry in [&missing, &wrong_firm, &positive, &duplicate, &two_links] {
            insert(&pg, entry, None).await;
        }
        let report = handoff_captured(&[], &pg).await.unwrap();
        assert_eq!(report.target_integrity_failures, 5);
        assert!(
            report
                .differences
                .iter()
                .any(|d| d.entry_id == Some(missing.id) && d.reason == Reason::RepaymentCost)
        );
        assert!(
            report
                .differences
                .iter()
                .any(|d| d.entry_id == Some(duplicate.id) && d.reason == Reason::RepaymentShape)
        );
        let mut overflow = cost(firm_id, "overflow-cost", Uuid::new_v4());
        overflow.postings[0].quantity = i128::MAX.to_string();
        overflow.postings[1].quantity = (-i128::MAX).to_string();
        let mut plus = overflow.postings[0].clone();
        plus.quantity = "1".into();
        let mut minus = overflow.postings[1].clone();
        minus.quantity = "-1".into();
        overflow.postings.extend([plus, minus]);
        insert(&pg, &overflow, None).await;
        let attempt = repayment(&overflow, "overflow-repayment", -1);
        insert(&pg, &attempt, None).await;
        let report = handoff_captured(&[], &pg).await.unwrap();
        assert_eq!(report.target_integrity_failures, 6);
        assert!(
            report
                .differences
                .iter()
                .any(|d| d.entry_id == Some(attempt.id) && d.reason == Reason::RepaymentCost)
        );
    })
    .await;
}
