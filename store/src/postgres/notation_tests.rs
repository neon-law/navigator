//! The notation authority's tests, against the PostgreSQL fixture.
//! Each test skips when `NAVIGATOR_TEST_POSTGRES_PORT` is unset.
use super::*;
use crate::assets::RowRestore;
use crate::notations::{self, NewNotation, NotationError};
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::surreal::SurrealDb;
use uuid::Uuid;

async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

/// The engine the SurrealDB fallback would need. Every dispatched path in
/// this family reads and writes PostgreSQL only, so the handle stays
/// uninitialized: touching it would panic, which is the point.
fn surreal() -> SurrealDb {
    SurrealDb::uninitialized()
}

/// The parent rows the notation table's foreign keys name, plus the ids a
/// notation row carries: `(pool, project, person, template)`.
async fn fixture() -> Option<(PostgresDb, Uuid, Uuid, Uuid)> {
    let pg = database().await?;
    let jurisdiction = Uuid::now_v7();
    let entity_type = Uuid::now_v7();
    let entity = Uuid::now_v7();
    let project = Uuid::now_v7();
    let person = Uuid::now_v7();
    let template = Uuid::now_v7();
    sqlx::query("INSERT INTO jurisdiction(id,name) VALUES ($1,'Synthetic Jurisdiction')")
        .bind(jurisdiction)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity_type(id,name) VALUES ($1,'Synthetic Entity')")
        .bind(entity_type)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Entity',$2,$3)")
        .bind(entity).bind(entity_type).bind(jurisdiction).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO project(id,code,name,status,entity_id) VALUES ($1,'synthetic-onboarding','Synthetic Project','open',$2)")
        .bind(project).bind(entity).execute(pg.pool()).await.unwrap();
    sqlx::query(
        "INSERT INTO person(id,name,email) VALUES ($1,'Synthetic Client','client@example.com')",
    )
    .bind(person)
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO template(id,code,title,respondent_type,is_current,kind,inserted_at,updated_at)
                 VALUES ($1,'onboarding','Synthetic Onboarding','person',true,'onboarding',now(),now())")
        .bind(template).execute(pg.pool()).await.unwrap();
    Some((pg, project, person, template))
}

/// A template a notation can pin, with the version fields set.
fn a_template(id: Uuid) -> crate::templates::Template {
    crate::templates::Template {
        id,
        code: "onboarding".into(),
        title: "Synthetic Onboarding".into(),
        respondent_type: "person".into(),
        project_id: None,
        firm_id: None,
        is_current: true,
        asset_id: None,
        form_code: None,
        kind: Some("onboarding".into()),
        source_commit_sha: Some("abc123".into()),
        body_sha256: Some("a".repeat(64)),
        inserted_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

/// Pins copied from the resolved template.
fn pins() -> TemplatePins {
    template_pins(Some(&a_template(Uuid::now_v7())))
}

#[test]
fn template_pins_fall_back_to_none_when_the_template_does_not_read_back() {
    let pins = template_pins(None);
    assert_eq!(pins.source_commit_sha, None);
    assert_eq!(pins.body_sha256, None);
}

#[test]
fn template_pins_copy_the_resolved_template_version_fields() {
    let template = a_template(Uuid::now_v7());
    let pins = template_pins(Some(&template));
    assert_eq!(pins.source_commit_sha.as_deref(), Some("abc123"));
    assert_eq!(pins.body_sha256.as_deref(), Some(&*"a".repeat(64)));
}

#[tokio::test]
async fn notation_create_and_reads_round_trip_in_postgres() {
    let Some((pg, project, person, template)) = fixture().await else {
        return;
    };
    let db = surreal();
    with_test_db(pg.clone(), async {
        let new = NewNotation::new(template, person, project, "BEGIN");
        let id = Uuid::now_v7();
        let notation = create(&pg, id, &new, pins()).await.unwrap();
        assert_eq!(notation.id, id);
        assert_eq!(notation.state, "BEGIN");

        let found = notations::find_by_id(&db, id).await.unwrap();
        assert_eq!(found.as_ref(), Some(&notation));
        assert!(notations::find_by_id(&db, Uuid::now_v7())
            .await
            .unwrap()
            .is_none());

        let by_project = notations::list_by_project(&db, project).await.unwrap();
        assert_eq!(by_project, vec![notation.clone()]);
        assert!(notations::list_by_project(&db, Uuid::now_v7())
            .await
            .unwrap()
            .is_empty());

        let by_projects = notations::list_by_projects(&db, &[project, Uuid::now_v7()])
            .await
            .unwrap();
        assert_eq!(by_projects, vec![notation.clone()]);

        let by_person = notations::list_by_person(&db, person).await.unwrap();
        assert_eq!(by_person, vec![notation.clone()]);

        let all = notations::list_all(&db).await.unwrap();
        assert_eq!(all, vec![notation.clone()]);

        let probe = notations::find_by_project_template_person(&db, project, template, person)
            .await
            .unwrap();
        assert_eq!(probe.as_ref(), Some(&notation));
        assert!(
            notations::find_by_project_template_person(&db, project, template, Uuid::now_v7())
                .await
                .unwrap()
                .is_none()
        );

        assert!(notations::exists_for_project(&db, project).await.unwrap());
        assert!(!notations::exists_for_project(&db, Uuid::now_v7())
            .await
            .unwrap());
    })
    .await;
}

#[tokio::test]
async fn notation_state_writes_update_reconcile_and_snapshot_in_postgres() {
    let Some((pg, project, person, template)) = fixture().await else {
        return;
    };
    let db = surreal();
    with_test_db(pg.clone(), async {
        let new = NewNotation::new(template, person, project, "BEGIN");
        let notation = create(&pg, Uuid::now_v7(), &new, pins()).await.unwrap();

        let updated = notations::update_state(&db, notation.id, "lawyer_review")
            .await
            .unwrap();
        assert_eq!(updated.state, "lawyer_review");
        assert!(matches!(
            notations::update_state(&db, Uuid::now_v7(), "lawyer_review").await,
            Err(NotationError::NotFound(_))
        ));

        // A reconcile whose observed state no longer holds changes nothing.
        notations::reconcile_state(&db, notation.id, "sent_for_signature", "client_review")
            .await
            .unwrap();
        notations::reconcile_state(&db, notation.id, "lawyer_review", "client_review")
            .await
            .unwrap();
        let reconciled = notations::find_by_id(&db, notation.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(reconciled.state, "client_review");

        let snapshot = serde_json::json!({"question": "answer"});
        let snapshotted =
            notations::update_questionnaire_snapshot(&db, notation.id, snapshot.clone())
                .await
                .unwrap();
        assert_eq!(snapshotted.questionnaire_snapshot, Some(snapshot.clone()));
        assert!(matches!(
            notations::update_questionnaire_snapshot(&db, Uuid::now_v7(), snapshot).await,
            Err(NotationError::NotFound(_))
        ));
    })
    .await;
}

#[tokio::test]
async fn notation_restore_archived_honors_the_row_already_present() {
    let Some((pg, project, person, template)) = fixture().await else {
        return;
    };
    let db = surreal();
    with_test_db(pg.clone(), async {
        let new = NewNotation::new(template, person, project, "BEGIN");
        let notation = create(&pg, Uuid::now_v7(), &new, pins()).await.unwrap();

        // The row already reads back identical, so the restore is a no-op.
        assert_eq!(
            notations::restore_archived(&db, &notation).await.unwrap(),
            RowRestore::Identical
        );

        // A different row under the same id is a conflict, unchanged.
        let mut conflicting = notation.clone();
        conflicting.state = "lawyer_review".into();
        assert_eq!(
            notations::restore_archived(&db, &conflicting)
                .await
                .unwrap(),
            RowRestore::Conflict
        );

        // A free id restores with its frozen fields and timestamps intact.
        let mut archived = notation.clone();
        archived.id = Uuid::now_v7();
        archived.template_source_commit_sha = Some("abc123".into());
        assert_eq!(
            notations::restore_archived(&db, &archived).await.unwrap(),
            RowRestore::Created
        );
        let restored = notations::find_by_id(&db, archived.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restored, archived);
    })
    .await;
}

#[tokio::test]
async fn notation_delete_uncommitted_removes_only_its_own_row() {
    let Some((pg, project, person, template)) = fixture().await else {
        return;
    };
    let db = surreal();
    with_test_db(pg.clone(), async {
        let new = NewNotation::new(template, person, project, "BEGIN");
        let notation = create(&pg, Uuid::now_v7(), &new, pins()).await.unwrap();

        notations::delete_uncommitted(&db, notation.id)
            .await
            .unwrap();
        assert!(notations::find_by_id(&db, notation.id)
            .await
            .unwrap()
            .is_none());
        // A delete of an unknown id is fine — the compensation is idempotent.
        notations::delete_uncommitted(&db, notation.id)
            .await
            .unwrap();
        assert_eq!(notations::list_all(&db).await.unwrap().len(), 0);
    })
    .await;
}

/// The daily draft reset's authority write: the notation row and its
/// `notation_event` and `answer` dependents go in one transaction, the
/// `notation_clause` dependents cascade at the foreign key, and a second
/// delete is a no-op that reports it — the rerun-safe outcome.
#[tokio::test]
async fn notation_delete_removes_dependents_once_and_reports_a_repeat() {
    let Some((pg, project, person, template)) = fixture().await else {
        return;
    };
    with_test_db(pg.clone(), async {
        let new = NewNotation::new(template, person, project, "BEGIN");
        let notation = create(&pg, Uuid::now_v7(), &new, pins()).await.unwrap();
        // A clause and an event name the draft: the clause cascades at the
        // foreign key, the event goes with the transaction's own delete.
        sqlx::query(
            "INSERT INTO notation_clause (id, notation_id, position, body_markdown, \
             inserted_at, updated_at) \
             VALUES ($1, $2, 0, 'Draft clause.', now(), now())",
        )
        .bind(Uuid::now_v7())
        .bind(notation.id)
        .execute(pg.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO notation_event (id, notation_id, acting_person_id, template_version_id, \
             machine_kind, from_state, to_state, condition, recorded_at) \
             VALUES ($1, $2, $3, $4, 'questionnaire', 'BEGIN', 'client_name', '', now())",
        )
        .bind(Uuid::now_v7())
        .bind(notation.id)
        .bind(person)
        .bind(template)
        .execute(pg.pool())
        .await
        .unwrap();

        assert!(delete(&pg, notation.id).await.unwrap());
        assert!(!delete(&pg, notation.id).await.unwrap());

        assert!(find_by_id(&pg, notation.id).await.unwrap().is_none());
        let dependents: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM notation_clause WHERE notation_id = $1) + \
             (SELECT count(*) FROM notation_event WHERE notation_id = $1)",
        )
        .bind(notation.id)
        .fetch_one(pg.pool())
        .await
        .unwrap();
        assert_eq!(dependents, 0, "the dependents went with the row");
    })
    .await;
}
