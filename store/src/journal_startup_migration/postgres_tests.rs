//! Synthetic captured-format journal inputs use the production parser and transaction on an owned unactivated PostgreSQL target.
//! This verifies immutable input bytes, not a remote Source database or a live export receipt.
use super::{CapturedJournalRow, ImportInput, ImportReport, JournalError, TriageItem};
use crate::firm_journal::{entries, Category, JournalEntry, Posting, SourceReference, Unit};
use crate::postgres::PostgresDb;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uuid::Uuid;

#[derive(Default, Clone)]
struct CaptureBuilder {
    rows: Vec<CapturedJournalRow>,
    firms: BTreeMap<Uuid, crate::firms::Firm>,
}
struct CapturedInput {
    bytes: Arc<[u8]>,
    firms: BTreeMap<Uuid, crate::firms::Firm>,
    rows_called: AtomicUsize,
    firms_called: AtomicUsize,
}
impl CaptureBuilder {
    fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.rows).unwrap()
    }
    fn freeze(&self) -> CapturedInput {
        CapturedInput {
            bytes: self.bytes().into(),
            firms: self.firms.clone(),
            rows_called: AtomicUsize::new(0),
            firms_called: AtomicUsize::new(0),
        }
    }
}
#[async_trait::async_trait]
impl ImportInput for CapturedInput {
    async fn rows(&self, firm: Option<Uuid>) -> Result<Vec<CapturedJournalRow>, JournalError> {
        self.rows_called.fetch_add(1, Ordering::SeqCst);
        let rows: Vec<CapturedJournalRow> = serde_json::from_slice(&self.bytes)?;
        Ok(rows
            .into_iter()
            .filter(|row| firm.is_none_or(|firm| row.firm_id == firm.to_string()))
            .collect())
    }
    async fn firm(&self, id: Uuid) -> anyhow::Result<Option<crate::firms::Firm>> {
        self.firms_called.fetch_add(1, Ordering::SeqCst);
        Ok(self.firms.get(&id).cloned())
    }
}
fn firm(input: &mut CaptureBuilder) -> Uuid {
    let id = Uuid::new_v4();
    input.firms.insert(
        id,
        crate::firms::Firm {
            id,
            name: "Synthetic accounting firm".into(),
            slug: format!("fixture-{}", id.simple()),
            status: "active".into(),
            entity_id: None,
            portal_theme_brand_key: None,
            inserted_at: "2026-10-10T00:00:00Z".into(),
            updated_at: "2026-10-10T00:00:00Z".into(),
        },
    );
    id
}
fn legacy(input: &mut CaptureBuilder, entry: &JournalEntry, timestamp: Option<&str>) {
    input.rows.push(CapturedJournalRow {
        id: entry.id,
        firm_id: entry.firm_id.to_string(),
        command_key: entry.key.clone(),
        payload: serde_json::to_string(entry).unwrap(),
        inserted_at: timestamp.map(str::to_owned),
    });
}
async fn import(input: &CaptureBuilder, pg: &PostgresDb) -> Result<ImportReport, JournalError> {
    super::import_selected(&input.freeze(), pg, None).await
}
async fn import_firm(
    input: &CaptureBuilder,
    pg: &PostgresDb,
    firm: Uuid,
) -> Result<ImportReport, JournalError> {
    super::import_selected(&input.freeze(), pg, Some(firm)).await
}
async fn triage(input: &CaptureBuilder, firm: Uuid) -> Result<Vec<TriageItem>, JournalError> {
    super::triage_input(&input.freeze(), firm).await
}
async fn capture(input: &CaptureBuilder) -> Result<Vec<CapturedJournalRow>, JournalError> {
    let rows = input.freeze().rows(None).await?;
    super::handoff::parse_captured(&rows)?;
    Ok(rows)
}

fn entry(firm: Uuid, key: &str) -> JournalEntry {
    JournalEntry {
        id: Uuid::now_v7(),
        firm_id: firm,
        key: key.into(),
        actor_id: Uuid::now_v7(),
        occurred_at: "2026-10-11T00:00:00Z".into(),
        memo: "Synthetic receipt".into(),
        postings: vec![
            Posting {
                account: "bank".into(),
                ownership: crate::firm_journal::BeneficialOwnership::Firm,
                restrictions: vec![],
                unit: Unit::fiat("USD", 2),
                quantity: "10000".into(),
                project_id: None,
                category: Category::Cash,
            },
            Posting {
                account: "client-liability".into(),
                ownership: crate::firm_journal::BeneficialOwnership::Client,
                restrictions: vec![],
                unit: Unit::fiat("USD", 2),
                quantity: "-10000".into(),
                project_id: None,
                category: Category::Trust,
            },
        ],
        sources: vec![],
        receipts: vec![],
        links: vec![],
        reversal_of: None,
    }
}

#[tokio::test]
async fn startup_import_preserves_identity_timestamps_and_reversal_order_then_retries() {
    crate::test_support::with_unactivated_postgres(async {
        let pg = PostgresDb::from_env().await.unwrap();
        let pool = pg.pool().clone();
        let mut db = CaptureBuilder::default();
        let firm = firm(&mut db);
        let mut original = entry(firm, "historical");
        original.sources.push(SourceReference {
            provider: "synthetic-bank".into(),
            scope: "synthetic-firm".into(),
            account: "operating".into(),
            object_type: "transaction".into(),
            external_id: "synthetic-payment".into(),
            locator: None,
        });
        let reversal = original
            .reversal(
                "historical-reversal",
                Uuid::now_v7(),
                "2026-10-11T01:00:00Z",
            )
            .unwrap();
        // Insert reversal first to prove the source's arbitrary order is irrelevant.
        legacy(&mut db, &reversal, Some("2026-10-11T01:05:00Z"));
        legacy(&mut db, &original, Some("2026-10-10T01:02:03.123456789Z"));
        let captured_before = db.bytes();
        let (left, right) = tokio::join!(
            import(&db, &pg),
            import(&db, &pg),
        );
        assert_eq!(left.unwrap().imported + right.unwrap().imported, 2);
        assert_eq!(
            import(&db, &pg)
                .await
                .unwrap()
                .imported,
            0
        );
        let imported = entries(&pg, firm).await.unwrap();
        assert_eq!(imported.len(), 2);
        assert!(imported
            .iter()
            .any(|e| serde_json::to_value(e).unwrap() == serde_json::to_value(&original).unwrap()));
        assert!(imported
            .iter()
            .any(|e| serde_json::to_value(e).unwrap() == serde_json::to_value(&reversal).unwrap()));
        let (inserted_at, known, imported, source): (chrono::DateTime<chrono::Utc>, bool, bool, String) = sqlx::query_as(
            "SELECT inserted_at,legacy_inserted_at_known,legacy_imported_at IS NOT NULL,legacy_inserted_at_source FROM journal_entry WHERE id=$1")
            .bind(original.id).fetch_one(&pool).await.unwrap();
        assert_eq!(inserted_at.to_rfc3339(), "2026-10-10T01:02:03.123457+00:00");
        assert_eq!(source, "2026-10-10T01:02:03.123456789Z");
        assert!(known && imported);
        let known: bool =
            sqlx::query_scalar("SELECT legacy_inserted_at_known FROM journal_entry WHERE id=$1")
                .bind(reversal.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(known);
        let sources: i64 =
            sqlx::query_scalar("SELECT count(*) FROM journal_entry_source WHERE entry_id=$1")
                .bind(original.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(sources, 1);
        let postings: i64 =
            sqlx::query_scalar("SELECT count(*) FROM journal_posting WHERE entry_id=$1")
                .bind(original.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(postings, 2);
        assert_eq!(db.rows.len(), 2);
        assert_eq!(db.bytes(), captured_before);
    })
    .await;
}

#[tokio::test]
async fn operator_import_refuses_changed_source_and_rolls_back_entire_batch() {
    crate::test_support::with_unactivated_postgres(async {
        let pg = PostgresDb::from_env().await.unwrap();
        let mut db = CaptureBuilder::default();
        let firm = firm(&mut db);
        let original = entry(firm, "collision");
        legacy(&mut db, &original, Some("2026-10-10T00:00:00Z"));
        import(&db, &pg).await.unwrap();
        let mut altered = original.clone();
        altered.memo = "Altered historical source".into();
        db.rows
            .iter_mut()
            .find(|row| row.id == altered.id)
            .unwrap()
            .payload = serde_json::to_string(&altered).unwrap();
        legacy(
            &mut db,
            &entry(firm, "must-roll-back"),
            Some("2026-10-10T00:00:00Z"),
        );
        assert!(matches!(
            import(&db, &pg).await,
            Err(JournalError::Conflict)
        ));
        assert_eq!(entries(&pg, firm).await.unwrap().len(), 1);
    })
    .await;
}

#[tokio::test]
async fn absent_insertion_timestamp_and_dependent_reversal_are_triaged_without_import() {
    crate::test_support::with_unactivated_postgres(async {
        use super::TriageReason;
        let pg = PostgresDb::from_env().await.unwrap();
        let pool = pg.pool().clone();
        let mut db = CaptureBuilder::default();
        let firm = firm(&mut db);
        let unknown = entry(firm, "no-insertion-timestamp");
        let dependent = unknown
            .reversal("dependent", Uuid::now_v7(), "2026-10-11T01:00:00Z")
            .unwrap();
        let valid = entry(firm, "known-insertion-timestamp");
        let invalid = entry(firm, "invalid-insertion-timestamp");
        legacy(&mut db, &invalid, Some("not-a-date"));
        legacy(&mut db, &unknown, None);
        legacy(&mut db, &dependent, Some("2026-10-11T01:05:00Z"));
        legacy(&mut db, &valid, Some("2026-10-10T01:00:00Z"));
        let read_only = triage(&db, firm).await.unwrap();
        assert_eq!(read_only.len(), 3);
        assert!(read_only.iter().any(|row| row.entry_id == invalid.id
            && row.reason == TriageReason::InvalidInsertionTimestamp));
        let issue = read_only
            .iter()
            .find(|row| row.entry_id == unknown.id)
            .unwrap();
        assert_eq!(issue.firm_id, firm);
        assert_eq!(issue.reason, TriageReason::MissingInsertionTimestamp);
        assert_eq!(issue.occurred_at, unknown.occurred_at);
        assert_eq!(issue.inserted_at, None);
        let safe_json = serde_json::to_string(&read_only).unwrap();
        assert!(!safe_json.contains("Synthetic receipt"));
        assert!(!safe_json.contains("client-liability"));
        assert!(!safe_json.contains("not-a-date"));
        let report = import(&db, &pg).await.unwrap();
        assert_eq!(report.imported, 1);
        assert_eq!(report.triage, read_only);
        assert_eq!(entries(&pg, firm).await.unwrap()[0].id, valid.id);
        let unknown_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM journal_entry WHERE id=$1 OR id=$2")
                .bind(unknown.id)
                .bind(dependent.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(unknown_count, 0);
        assert_eq!(import(&db, &pg).await.unwrap().imported, 0);
    })
    .await;
}

#[tokio::test]
async fn missing_reversal_parent_is_triaged_while_independent_entry_migrates() {
    crate::test_support::with_unactivated_postgres(async {
        use super::TriageReason;
        let pg = PostgresDb::from_env().await.unwrap();
        let mut db = CaptureBuilder::default();
        let firm = firm(&mut db);
        let missing_parent = entry(firm, "not-in-source");
        let reversal = missing_parent
            .reversal("orphan", Uuid::now_v7(), "2026-10-11T01:00:00Z")
            .unwrap();
        let valid = entry(firm, "independent");
        legacy(&mut db, &reversal, Some("2026-10-11T01:05:00Z"));
        legacy(&mut db, &valid, Some("2026-10-10T01:00:00Z"));
        let read_only = triage(&db, firm).await.unwrap();
        let report = import(&db, &pg).await.unwrap();
        assert_eq!(report.imported, 1);
        assert_eq!(report.triage.len(), 1);
        assert_eq!(report.triage, read_only);
        assert_eq!(report.triage[0].entry_id, reversal.id);
        assert_eq!(
            report.triage[0].reason,
            TriageReason::UnresolvedReversalTarget
        );
        assert_eq!(entries(&pg, firm).await.unwrap().len(), 1);
    })
    .await;
}

#[tokio::test]
async fn source_overflow_fails_visibly_without_importing_a_partial_subset() {
    crate::test_support::with_unactivated_postgres(async {
        let pg = PostgresDb::from_env().await.unwrap();
        let mut db = CaptureBuilder::default();
        let firm = firm(&mut db);
        for index in 0..1001 {
            legacy(
                &mut db,
                &entry(firm, &format!("bounded-source-{index}")),
                Some("2026-10-10T00:00:00Z"),
            );
        }
        assert!(matches!(triage(&db, firm).await,
            Err(JournalError::Invalid(message)) if message.contains("1000")));
        assert!(matches!(import(&db, &pg).await,
            Err(JournalError::Invalid(message)) if message.contains("1000")));
        assert!(entries(&pg, firm).await.unwrap().is_empty());
    })
    .await;
}

#[tokio::test]
async fn raw_capture_preserves_physical_identity_and_refuses_payload_aliases() {
    let mut db = CaptureBuilder::default();
    let firm = firm(&mut db);
    let original = entry(firm, "physical-identity");
    legacy(&mut db, &original, Some("2026-10-12T00:00:00Z"));
    let rows = capture(&db).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, original.id);
    let mut alias = entry(firm, "payload-alias");
    alias.id = original.id;
    db.rows.push(CapturedJournalRow {
        id: Uuid::new_v4(),
        firm_id: firm.to_string(),
        command_key: alias.key.clone(),
        payload: serde_json::to_string(&alias).unwrap(),
        inserted_at: Some("2026-10-12T00:00:00Z".into()),
    });
    assert!(capture(&db).await.is_err());
}

#[tokio::test]
async fn operator_import_waits_for_native_firm_lock_before_reading_history() {
    crate::test_support::with_unactivated_postgres(async {
        let pg = PostgresDb::from_env().await.unwrap();
        let pool = pg.pool().clone();
        let mut db = CaptureBuilder::default();
        let firm = firm(&mut db);
        let original = entry(firm, "already-imported");
        legacy(&mut db, &original, Some("2026-10-12T00:00:00Z"));
        import(&db, &pg)
            .await
            .unwrap();
        let incoming = entry(firm, "pending-after-native-lock");
        legacy(&mut db, &incoming, Some("2026-10-12T00:00:01Z"));
        let mut native = pg.pool().begin().await.unwrap();
        sqlx::query("SELECT id FROM firm WHERE id=$1 FOR UPDATE")
            .bind(firm)
            .execute(&mut *native)
            .await
            .unwrap();
        let source = db.clone();
        let importing_pg = pg.clone();
        let mut importing = tokio::spawn(async move {
            import(&source, &importing_pg).await
        });
        let waiting = async {
            loop {
                let blocked:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%FROM firm WHERE id%FOR UPDATE%')")
                    .fetch_one(&pool).await.unwrap();
                if blocked {
                    break;
                }
                tokio::task::yield_now().await;
            }
        };
        tokio::select! {
            result=&mut importing=>panic!("operator import completed before native Firm lock release: {result:?}"),
            result=tokio::time::timeout(std::time::Duration::from_secs(10),waiting)=>result.expect("operator must wait on the native Firm row lock"),
        }
        sqlx::query("UPDATE firm SET name='Native metadata retained' WHERE id=$1")
            .bind(firm)
            .execute(&mut *native)
            .await
            .unwrap();
        native.commit().await.unwrap();
        assert_eq!(importing.await.unwrap().unwrap().imported, 1);
        let name: String = sqlx::query_scalar("SELECT name FROM firm WHERE id=$1")
            .bind(firm)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name, "Native metadata retained");
        assert_eq!(entries(&pg, firm).await.unwrap().len(), 2);
    })
    .await;
}

#[tokio::test]
async fn operator_import_is_firm_scoped_and_keeps_source_unchanged() {
    crate::test_support::with_unactivated_postgres(async {
        let pg = PostgresDb::from_env().await.unwrap();
        let mut db = CaptureBuilder::default();
        // A selected Firm's explicit import must not parse or import another
        // Firm's malformed history, and must leave the Source unchanged.
        let selected = self::firm(&mut db);
        let selected_entry = entry(selected, "selected-firm");
        legacy(&mut db, &selected_entry, Some("2026-10-11T01:05:00Z"));
        let outside = self::firm(&mut db);
        db.rows.push(CapturedJournalRow {
            id: Uuid::new_v4(),
            firm_id: outside.to_string(),
            command_key: "unreviewed-outside-firm".into(),
            payload: "not-json".into(),
            inserted_at: Some("2026-10-11T01:05:00Z".into()),
        });
        let captured_before = db.bytes();
        let report = import_firm(&db, &pg, selected).await.unwrap();
        assert_eq!(report.imported, 1);
        assert!(report.triage.is_empty());
        assert_eq!(entries(&pg, selected).await.unwrap(), vec![selected_entry]);
        assert!(entries(&pg, outside).await.unwrap().is_empty());
        assert_eq!(import_firm(&db, &pg, selected).await.unwrap().imported, 0);
        assert_eq!(db.rows.len(), 2);
        assert_eq!(db.bytes(), captured_before);
    })
    .await;
}

#[tokio::test]
async fn native_ownership_refuses_before_any_captured_input_callback() {
    crate::test_support::with_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let mut builder = CaptureBuilder::default();
        let owner = firm(&mut builder);
        legacy(
            &mut builder,
            &entry(owner, "native-refusal"),
            Some("2026-10-10T00:00:00Z"),
        );
        let input = builder.freeze();
        assert!(super::import_selected(&input, &pg, None).await.is_err());
        assert_eq!(input.rows_called.load(Ordering::SeqCst), 0);
        assert_eq!(input.firms_called.load(Ordering::SeqCst), 0);
        assert!(entries(&pg, owner).await.unwrap().is_empty());
    })
    .await;
}

async fn parent_import(
    input: &mut CaptureBuilder,
    pg: &PostgresDb,
    entry: &JournalEntry,
) -> Result<ImportReport, JournalError> {
    legacy(input, entry, Some("2026-10-11T00:00:00Z"));
    import(input, pg).await
}
#[tokio::test]
async fn journal_firm_parent_rejects_another_identity_with_the_same_slug() {
    crate::test_support::with_unactivated_postgres(async {
        let pg = PostgresDb::from_env().await.unwrap();
        let mut db = CaptureBuilder::default();
        let firm_id = firm(&mut db);
        let source_slug = db.firms[&firm_id].slug.clone();
        let other_id = Uuid::new_v4();
        sqlx::query(
        "INSERT INTO firm (id,name,slug,status) VALUES ($1,'Synthetic existing Firm',$2,'active')",
    )
    .bind(other_id)
    .bind(&source_slug)
    .execute(pg.pool())
    .await
    .unwrap();
        assert!(matches!(
            parent_import(&mut db, &pg, &entry(firm_id, "synthetic:slug-conflict")).await,
            Err(JournalError::Invalid(_))
        ));
        let held: Uuid = sqlx::query_scalar("SELECT id FROM firm WHERE slug=$1")
            .bind(&source_slug)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(held, other_id);
        assert!(entries(&pg, firm_id).await.unwrap().is_empty());
    })
    .await;
}

#[tokio::test]
async fn journal_firm_parent_rejects_an_immutable_slug_mismatch() {
    crate::test_support::with_unactivated_postgres(async {
    let pg = PostgresDb::from_env().await.unwrap();
    let mut db = CaptureBuilder::default();
    let firm_id = firm(&mut db);
    sqlx::query("INSERT INTO firm (id,name,slug,status) VALUES ($1,'Synthetic existing Firm','synthetic-other-slug','active')")
        .bind(firm_id).execute(pg.pool()).await.unwrap();
    assert!(matches!(
        parent_import(&mut db, &pg, &entry(firm_id, "synthetic:identity-conflict")).await,
        Err(JournalError::Invalid(_))
    ));
    let held: String = sqlx::query_scalar("SELECT slug FROM firm WHERE id=$1")
        .bind(firm_id)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(held, "synthetic-other-slug");
    assert!(entries(&pg, firm_id).await.unwrap().is_empty());

    }).await;
}

#[tokio::test]
async fn journal_firm_parent_preserves_existing_nonidentity_fields() {
    crate::test_support::with_unactivated_postgres(async {
    let pg = PostgresDb::from_env().await.unwrap();
    let mut db = CaptureBuilder::default();
    let firm_id = firm(&mut db);
let source_slug = db.firms[&firm_id].slug.clone();
    let retained_at: chrono::DateTime<chrono::Utc> = "2025-01-02T03:04:05Z".parse().unwrap();
    sqlx::query("INSERT INTO firm (id,name,slug,status,inserted_at,updated_at) VALUES ($1,'Synthetic retained Firm',$2,'inactive',$3,$3)")
        .bind(firm_id).bind(&source_slug).bind(retained_at).execute(pg.pool()).await.unwrap();
    parent_import(&mut db, &pg, &entry(firm_id, "synthetic:retained-parent"))
        .await
        .unwrap();
    let held: (
        String,
        String,
        chrono::DateTime<chrono::Utc>,
        chrono::DateTime<chrono::Utc>,
    ) = sqlx::query_as("SELECT name,status,inserted_at,updated_at FROM firm WHERE id=$1")
        .bind(firm_id)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        held,
        (
            "Synthetic retained Firm".into(),
            "inactive".into(),
            retained_at,
            retained_at
        )
    );

    }).await;
}
