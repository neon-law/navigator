//! Real captured Tenancy enrollment and preservation of canonical lifecycle history.

use super::*;
use crate::postgres_backfill::{self as backfill, family::Family};
use crate::postgres_mirror::tests::{
    connect, fresh_schema, import_predecessor_histories, sealed_prior_family_histories,
    synthetic_entity, verified_family_capture,
};

struct Capture {
    source: SurrealDb,
    actor: Uuid,
    project: Uuid,
    prior: serde_json::Value,
}

async fn capture(pg: &PostgresDb) -> Capture {
    capture_with_domain_parents(pg, false).await
}

async fn capture_with_domain_parents(pg: &PostgresDb, domain_parents: bool) -> Capture {
    let source = crate::surreal::test_support::unmigrated().await;
    super::super::from_source(crate::schema::apply(&source))
        .await
        .unwrap();
    let entity = synthetic_entity(&source).await;
    if !domain_parents {
        import_predecessor_histories(&source, pg).await;
    }
    let (actor, project) = super::super::from_source(async {
        let actor = crate::persons::create(
            &source,
            &crate::persons::NewPerson::with_role(
                "Synthetic tenancy administrator",
                "tenancy-capture@example.invalid",
                crate::persons::Role::Owner,
            ),
        )
        .await
        .unwrap()
        .id;
        let firm = crate::firms::create(
            &source,
            &crate::firms::NewFirm {
                name: "Synthetic captured practice".into(),
                status: "active".into(),
                entity_id: entity,
                admin_dri_person_id: actor,
            },
        )
        .await
        .unwrap();
        let brand = crate::brands::create(
            &source,
            crate::persons::Role::Owner,
            Some(actor),
            &crate::brands::NewBrand {
                name: "Synthetic captured brand".into(),
                key: "synthetic-tenancy-capture".into(),
                firm_id: Some(firm.id),
                primary_color: None,
                accent_color: None,
                typeface: None,
            },
        )
        .await
        .unwrap();
        crate::firms::ensure_brand(&source, firm.id, &brand.key)
            .await
            .unwrap();
        let project = crate::projects::create(
            &source,
            &crate::projects::NewProject {
                code: "synthetic-tenancy-capture".into(),
                name: "Synthetic captured matter".into(),
                status: "open".into(),
                brand: brand.key,
                entity_id: entity,
                firm_id: Some(firm.id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        crate::projects::add_participation(&source, project.id, actor, "lawyer")
            .await
            .unwrap();
        (actor, project.id)
    })
    .await;
    // Later Application mailrooms require an already-enrolled canonical Address.
    super::super::from_source(crate::addresses::create(
        &source,
        &crate::addresses::NewAddress {
            line1: "100 Synthetic Way".into(),
            city: "Reno".into(),
            region: "NV".into(),
            postal_code: "89501".into(),
            country: "US".into(),
            ..Default::default()
        },
    ))
    .await
    .unwrap();
    if domain_parents {
        captured_domain_parents(&source, actor, project).await;
        import_predecessor_histories(&source, pg).await;
    }
    let prior = sealed_prior_family_histories(pg).await;
    let receipt = verified_family_capture(&source).await;
    let identity = backfill::start_family(&source, pg, Family::Identity, receipt, 10, None, true)
        .await
        .unwrap();
    assert!(identity.is_clean(), "{identity:?}");
    Capture {
        source,
        actor,
        project,
        prior,
    }
}

async fn captured_domain_parents(source: &SurrealDb, actor: Uuid, project: Uuid) {
    let asset = Uuid::now_v7();
    source.query("CREATE $id SET storage_key='synthetic/domain-parent',content_type='application/pdf',byte_size=25,sha256_hex=$sha,project_id=$project,filename='synthetic-parent.pdf',visibility='internal'")
        .bind(("id", crate::surreal::record_id("asset", asset)))
        .bind(("project", crate::surreal::record_id("project", project)))
        .bind(("sha", crate::assets::sha256_hex(b"Synthetic parent document")))
        .await.unwrap().check().unwrap();
    let firm: Option<surrealdb::types::RecordId> = source
        .query("SELECT VALUE firm_id FROM ONLY $id")
        .bind(("id", crate::surreal::record_id("project", project)))
        .await
        .unwrap()
        .check()
        .unwrap()
        .take(0)
        .unwrap();
    let conversation = Uuid::now_v7();
    source.query("CREATE $id SET firm_id=$firm,project_id=$project,channel='email',external_address='synthetic@example.invalid',provider_account='synthetic',unread=true")
        .bind(("id", crate::surreal::record_id("inbox_conversation", conversation)))
        .bind(("firm", firm.unwrap()))
        .bind(("project", crate::surreal::record_id("project", project)))
        .await.unwrap().check().unwrap();
    source.query("CREATE $id SET conversation_id=$conversation,direction='inbound',body='Synthetic captured message',author_person_id=$actor,delivery_state='received',occurred_at='2026-11-18T12:00:00Z'")
        .bind(("id", crate::surreal::record_id("message", Uuid::now_v7())))
        .bind(("conversation", crate::surreal::record_id("inbox_conversation", conversation)))
        .bind(("actor", crate::surreal::record_id("person", actor)))
        .await.unwrap().check().unwrap();
}

pub(crate) async fn enrolled_source_with_domain_parents(
    pg: &PostgresDb,
) -> (SurrealDb, Uuid, Uuid) {
    let captured = capture_with_domain_parents(pg, true).await;
    enroll_tenancy(pg, captured).await
}

pub(crate) async fn enrolled_source(pg: &PostgresDb) -> (SurrealDb, Uuid, Uuid) {
    let captured = capture(pg).await;
    enroll_tenancy(pg, captured).await
}

async fn enroll_tenancy(pg: &PostgresDb, captured: Capture) -> (SurrealDb, Uuid, Uuid) {
    let receipt = verified_family_capture(&captured.source).await;
    let completed = backfill::start_family(
        &captured.source,
        pg,
        Family::Tenancy,
        receipt,
        10,
        None,
        true,
    )
    .await
    .unwrap();
    assert!(completed.is_clean(), "{completed:?}");
    (captured.source, captured.actor, captured.project)
}

async fn start(pg: &PostgresDb, capture: &Capture) -> backfill::RunReport {
    let receipt = verified_family_capture(&capture.source).await;
    let started = backfill::start_family(
        &capture.source,
        pg,
        Family::Tenancy,
        receipt,
        1,
        Some(1),
        false,
    )
    .await
    .unwrap();
    assert_ne!(started.status, backfill::RunStatus::Completed);
    started
}

async fn copy(pg: &PostgresDb, capture: &Capture, run: Uuid) -> backfill::RunReport {
    backfill::resume_family(&capture.source, pg, Family::Tenancy, run, None, false)
        .await
        .unwrap()
}

async fn baseline(pg: &PostgresDb, capture: &Capture, close: bool) {
    use crate::postgres::project_lifecycle::{self, StatusTransition};
    let mut tx = pg.pool().begin().await.unwrap();
    let project = project_lifecycle::lock(&mut tx, capture.project)
        .await
        .unwrap()
        .unwrap();
    project_lifecycle::append(
        &mut tx,
        &project,
        &StatusTransition {
            from_status: None,
            to_status: project.status.clone(),
            effective_at: None,
            recorded_at: Some(Utc::now()),
            reason: None,
            attesting_person_id: None,
        },
        project.closed_at.as_deref(),
        project.closure_reason.as_deref(),
    )
    .await
    .unwrap();
    if close {
        project_lifecycle::append(
            &mut tx,
            &project,
            &StatusTransition {
                from_status: Some(project.status.clone()),
                to_status: "closed".into(),
                effective_at: Some(Utc::now()),
                recorded_at: Some(Utc::now()),
                reason: Some("Synthetic completion".into()),
                attesting_person_id: Some(capture.actor),
            },
            Some("2026-11-18T00:00:00Z"),
            Some("Synthetic completion"),
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn captured_tenancy_enrolls_all_six_tables_then_replays_without_source() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let captured = capture(&pg).await;
    let started = start(&pg, &captured).await;
    let copied = copy(&pg, &captured, started.run_id).await;
    assert_eq!(copied.status, backfill::RunStatus::Paused);
    assert!(copied.failures.is_empty(), "{copied:?}");
    assert!(copied.unresolved_references.is_empty(), "{copied:?}");
    assert_eq!(copied.parity.len(), Family::Tenancy.tables().len());
    assert!(
        copied
            .parity
            .iter()
            .all(|table| table.missing == 0 && table.differing == 0 && table.extra == 0),
        "{copied:?}"
    );
    let completed = backfill::resume_family(
        &captured.source,
        &pg,
        Family::Tenancy,
        started.run_id,
        None,
        true,
    )
    .await
    .unwrap();
    assert_eq!(completed.status, backfill::RunStatus::Completed);
    for table in Family::Tenancy.tables() {
        let table = completed
            .parity
            .iter()
            .find(|row| row.table == *table)
            .unwrap();
        assert!(
            table.surreal > 0,
            "every Tenancy table has actual captured history"
        );
    }
    let before: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(r) FROM project r WHERE id=$1")
            .bind(captured.project)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    let replay = backfill::resume_family(
        &SurrealDb::uninitialized(),
        &pg,
        Family::Tenancy,
        started.run_id,
        None,
        true,
    )
    .await
    .unwrap();
    assert!(replay.is_clean());
    let after: serde_json::Value =
        sqlx::query_scalar("SELECT to_jsonb(r) FROM project r WHERE id=$1")
            .bind(captured.project)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert_eq!(after, before);
    assert_eq!(sealed_prior_family_histories(&pg).await, captured.prior);
    pg.pool().close().await;
}

#[tokio::test]
async fn captured_old_project_state_preserves_a_verified_native_transition_chain() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let captured = capture(&pg).await;
    let started = start(&pg, &captured).await;
    let copied = copy(&pg, &captured, started.run_id).await;
    assert_eq!(copied.status, backfill::RunStatus::Paused);
    assert!(copied.failures.is_empty(), "{copied:?}");
    assert!(copied.unresolved_references.is_empty(), "{copied:?}");
    assert_eq!(copied.parity.len(), Family::Tenancy.tables().len());
    assert!(
        copied
            .parity
            .iter()
            .all(|table| table.missing == 0 && table.differing == 0 && table.extra == 0),
        "{copied:?}"
    );
    baseline(&pg, &captured, true).await;
    let completed = backfill::resume_family(
        &captured.source,
        &pg,
        Family::Tenancy,
        started.run_id,
        None,
        true,
    )
    .await
    .unwrap();
    assert_eq!(completed.status, backfill::RunStatus::Completed);
    let proof = completed
        .parity
        .iter()
        .find(|row| row.table == "project")
        .unwrap()
        .project_lifecycle
        .as_ref()
        .unwrap();
    assert_eq!(proof.retained_native_projections, 1);
    let status: String = sqlx::query_scalar("SELECT status FROM project WHERE id=$1")
        .bind(captured.project)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(status, "closed");
    assert_eq!(sealed_prior_family_histories(&pg).await, captured.prior);
    pg.pool().close().await;
}

#[derive(Clone, Copy)]
enum Drift {
    Metadata,
    UnrelatedBaseline,
    TargetOnly,
}

async fn refuses_drift(kind: Drift) {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let captured = capture(&pg).await;
    let source = super::super::from_source(crate::projects::find_by_id(
        &captured.source,
        captured.project,
    ))
    .await
    .unwrap()
    .unwrap();
    let mut image = ProjectImage::from_source(&source).unwrap();
    match kind {
        Drift::Metadata => image.name = "Synthetic native name".into(),
        Drift::UnrelatedBaseline => {
            image.status = "closed".into();
            image.closed_at = Some("2026-11-18".into());
            image.closure_reason = Some("Synthetic historical baseline".into());
        }
        Drift::TargetOnly => {
            image.id = Uuid::now_v7();
            image.code = "synthetic-native-only".into();
        }
    }
    // Seed the same historical parent identity without inventing an import receipt or copy context.
    let firm = super::super::from_source(crate::firms::find_by_id(
        &captured.source,
        image.firm_id.unwrap(),
    ))
    .await
    .unwrap()
    .unwrap();
    upsert_firm(&pg, &FirmImage::from_source(&firm).unwrap())
        .await
        .unwrap();
    upsert_project(&pg, &image).await.unwrap();
    if matches!(kind, Drift::UnrelatedBaseline) {
        baseline(&pg, &captured, false).await;
    }
    let started = start(&pg, &captured).await;
    let report = backfill::resume_family(
        &captured.source,
        &pg,
        Family::Tenancy,
        started.run_id,
        None,
        true,
    )
    .await
    .unwrap();
    assert_ne!(report.status, backfill::RunStatus::Completed);
    assert!(!report.is_clean());
    let table = report
        .parity
        .iter()
        .find(|table| table.table == "project")
        .unwrap();
    let proof = table.project_lifecycle.as_ref().unwrap();
    match kind {
        Drift::Metadata | Drift::UnrelatedBaseline => {
            assert_eq!(proof.unresolved_overlaps, 1);
            assert_eq!(table.differing, 1);
        }
        Drift::TargetOnly => {
            assert_eq!(proof.target_only, 1);
            assert_eq!(table.extra, 1);
        }
    }
    assert!(backfill::family::require_scope(&pg, Family::Tenancy)
        .await
        .is_err());
    let statement = format!("{} WHERE id=$1", PROJECT_SELECT.as_str());
    // Only the canonical static projection and a literal bound predicate form this query.
    let actual = sqlx::query(sqlx::AssertSqlSafe(statement))
        .bind(image.id)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(ProjectImage::from_row(&actual).unwrap(), image);
    assert_eq!(sealed_prior_family_histories(&pg).await, captured.prior);
    pg.pool().close().await;
}

#[tokio::test]
async fn captured_tenancy_cannot_overwrite_native_non_lifecycle_fields() {
    refuses_drift(Drift::Metadata).await;
}

#[tokio::test]
async fn an_unrelated_terminal_baseline_is_not_evidence_for_source_history() {
    refuses_drift(Drift::UnrelatedBaseline).await;
}

#[tokio::test]
async fn native_target_only_project_is_blocking_drift_and_never_deleted() {
    refuses_drift(Drift::TargetOnly).await;
}

#[tokio::test]
async fn changed_capture_refuses_resume_before_additional_writes_or_activation() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let captured = capture(&pg).await;
    let started = start(&pg, &captured).await;
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM project")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    captured
        .source
        .query("UPDATE $id SET name='Synthetic changed capture'")
        .bind(("id", crate::surreal::record_id("project", captured.project)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert!(backfill::resume_family(
        &captured.source,
        &pg,
        Family::Tenancy,
        started.run_id,
        None,
        true
    )
    .await
    .is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM project")
            .fetch_one(pg.pool())
            .await
            .unwrap(),
        before
    );
    assert!(backfill::family::require_scope(&pg, Family::Tenancy)
        .await
        .is_err());
    assert_eq!(sealed_prior_family_histories(&pg).await, captured.prior);
    pg.pool().close().await;
}
