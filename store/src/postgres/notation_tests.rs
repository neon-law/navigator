//! PostgreSQL-only acceptance for the Notation writer family.
use crate::postgres::fixtures::workflow_fixture;
use crate::postgres::fixtures::{connect, fresh_schema};
use crate::postgres::history::Family;
use crate::postgres::matter::with_test_db;
use uuid::Uuid;

#[tokio::test]
async fn answer_json_null_nested_values_and_chronology_are_preserved() {
    use crate::answers::{self, NewAnswer};
    use serde_json::json;
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (person, notation, question) = workflow_fixture(&pg).await;
    crate::postgres::fixtures::activate_family(&pg, Family::Notation).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let first = answers::record(
            &unavailable,
            &NewAnswer::new(question, person, json!(null)).in_notation(notation, "person__client"),
        )
        .await
        .unwrap();
        assert_eq!(first.value, json!(null));
        assert!(
            answers::exists_with_value(&unavailable, question, person, &json!(null))
                .await
                .unwrap()
        );
        let nested = json!({"people": [{"active": true, "value": null}, [1, "two"]]});
        let latest = answers::record(
            &unavailable,
            &NewAnswer::new(question, person, nested.clone())
                .in_notation(notation, "person__client"),
        )
        .await
        .unwrap();
        assert_eq!(
            answers::latest_for_state(&unavailable, question, "person__client", person, notation)
                .await
                .unwrap(),
            Some(latest.clone())
        );
        assert_eq!(
            answers::for_notation(&unavailable, notation).await.unwrap(),
            vec![first.clone(), latest.clone()]
        );
        assert_eq!(
            answers::for_person_in_notation(&unavailable, person, notation)
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            answers::for_question_and_person(
                &unavailable,
                question,
                person,
                Some("person__client")
            )
            .await
            .unwrap()
            .len(),
            2
        );
        assert_eq!(
            answers::for_question_and_person(&unavailable, question, person, Some("absent"))
                .await
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            answers::latest_for_person(&unavailable, person)
                .await
                .unwrap(),
            Some(latest)
        );
        assert!(
            answers::exists_for_state(&unavailable, notation, "person__client")
                .await
                .unwrap()
        );
        assert!(
            !answers::exists_for_state(&unavailable, Uuid::new_v4(), "person__client")
                .await
                .unwrap()
        );
        assert_eq!(answers::list_all(&unavailable).await.unwrap().len(), 2);
        let stored: (serde_json::Value, bool) =
            sqlx::query_as("SELECT value, value IS NULL FROM answer WHERE id = $1")
                .bind(first.id)
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(stored, (json!(null), false));
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn workflow_satellites_preserve_replay_ordering_and_attribution() {
    use crate::{notation_clauses, notation_events, notation_sources};
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (person, notation, _) = workflow_fixture(&pg).await;
    crate::postgres::fixtures::activate_family(&pg, Family::Notation).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let a = notation_clauses::append(&unavailable, notation, "Synthetic first", Some(person))
            .await
            .unwrap();
        let b = notation_clauses::append(&unavailable, notation, "Synthetic second", Some(person))
            .await
            .unwrap();
        assert!(!notation_clauses::move_clause(&unavailable, a, true)
            .await
            .unwrap());
        assert!(notation_clauses::move_clause(&unavailable, b, true)
            .await
            .unwrap());
        assert_eq!(
            notation_clauses::for_notation(&unavailable, notation)
                .await
                .unwrap()
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![b, a]
        );
        assert_eq!(
            notation_clauses::update_body(&unavailable, a, "Edited")
                .await
                .unwrap()
                .unwrap()
                .body_markdown,
            "Edited"
        );
        assert!(notation_clauses::exists_for(&unavailable, notation)
            .await
            .unwrap());
        assert!(notation_clauses::delete(&unavailable, a).await.unwrap());
        assert!(!notation_clauses::delete(&unavailable, a).await.unwrap());
        let source = notation_sources::NewNotationSource {
            notation_id: notation,
            kind: notation_sources::kind::QUESTIONNAIRE.to_owned(),
            communication_id: None,
            source_ref: Some("synthetic".into()),
            position: 0,
        };
        notation_sources::attach(&unavailable, &source)
            .await
            .unwrap();
        assert!(notation_sources::attach(&unavailable, &source)
            .await
            .is_err());
        let event = notation_events::append_event(
            &unavailable,
            notation_events::TransitionRecord {
                notation_id: notation,
                acting_person_id: None,
                machine_kind: notation_events::MACHINE_QUESTIONNAIRE,
                from_state: "START",
                to_state: "END",
                condition: "synthetic",
                payload_json: Some("{\"value\":null}".into()),
                recorded_at: "2026-11-17T00:00:00Z",
            },
        )
        .await
        .unwrap();
        assert_eq!(event.acting_person_id, person);
        assert_eq!(
            notation_events::latest_for_kind(
                &unavailable,
                notation,
                notation_events::MACHINE_QUESTIONNAIRE
            )
            .await
            .unwrap(),
            Some(event.clone())
        );
        assert_eq!(
            notation_events::for_notation(&unavailable, notation)
                .await
                .unwrap(),
            vec![event]
        );
        assert!(notation_events::is_complete(
            &unavailable,
            notation,
            notation_events::MACHINE_QUESTIONNAIRE
        )
        .await
        .unwrap());
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn notation_pins_state_compare_and_swap_and_archive_replay_are_durable() {
    use crate::notations::{self, NewNotation};
    use serde_json::json;
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (person, seed, _) = workflow_fixture(&pg).await;
    let (template, project): (Uuid, Uuid) =
        sqlx::query_as("SELECT template_id, project_id FROM notation WHERE id=$1")
            .bind(seed)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    sqlx::query("UPDATE template SET source_commit_sha='1111111111111111111111111111111111111111',body_sha256='aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' WHERE id=$1")
        .bind(template).execute(pg.pool()).await.unwrap();
    let refused = sqlx::query("DELETE FROM template WHERE id=$1")
        .bind(template)
        .execute(pg.pool())
        .await
        .unwrap_err();
    assert_eq!(
        refused
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23503")
    );
    crate::postgres::fixtures::activate_family(&pg, Family::Notation).await;
    let db = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let opened = notations::create(
            &db,
            &NewNotation::new(template, person, project, "BEGIN")
                .with_questionnaire_snapshot(json!({"nested":[null,true]})),
        )
        .await
        .unwrap();
        assert_eq!(
            opened.template_source_commit_sha.as_deref(),
            Some("1111111111111111111111111111111111111111")
        );
        assert_eq!(
            opened.template_body_sha256.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        sqlx::query(
            "UPDATE template SET source_commit_sha='2222222222222222222222222222222222222222',body_sha256='bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb' WHERE id=$1",
        )
        .bind(template)
        .execute(pg.pool())
        .await
        .unwrap();
        notations::reconcile_state(&db, opened.id, "BEGIN", "intake_persisted")
            .await
            .unwrap();
        notations::reconcile_state(&db, opened.id, "BEGIN", "lawyer_review")
            .await
            .unwrap();
        let stored = notations::find_by_id(&db, opened.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.state, "intake_persisted");
        assert_eq!(stored.template_body_sha256, opened.template_body_sha256);
        assert_eq!(
            notations::restore_archived(&db, &stored).await.unwrap(),
            crate::assets::RowRestore::Identical
        );
        let mut changed = stored.clone();
        changed.state = "lawyer_review".into();
        assert_eq!(
            notations::restore_archived(&db, &changed).await.unwrap(),
            crate::assets::RowRestore::Conflict
        );
        assert_eq!(
            notations::find_by_project_template_person(&db, project, template, person)
                .await
                .unwrap()
                .unwrap()
                .id,
            opened.id
        );
        assert_eq!(
            notations::list_by_projects(&db, &[project])
                .await
                .unwrap()
                .len(),
            2
        );
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn notation_accessors_refuse_provisional_rows_before_scope_completion() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (_, notation, _) = workflow_fixture(&pg).await;
    let project: Uuid = sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
        .bind(notation)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert!(crate::onboarding::official_projects(&pg, &[project])
        .await
        .is_err());
    assert!(crate::onboarding::filed_documents(&pg, &[project])
        .await
        .is_err());
    assert!(
        crate::plan_metrics::template_effect(&pg, Uuid::new_v4(), &[])
            .await
            .is_err()
    );
    let db = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        assert!(crate::notations::find_by_id(&db, notation).await.is_err());
        assert!(
            crate::notations::update_state(&db, notation, "lawyer_review")
                .await
                .is_err()
        );
        assert!(crate::notation_reset::delete_draft(&db, notation)
            .await
            .is_err());
    })
    .await;
    let state: String = sqlx::query_scalar("SELECT state FROM notation WHERE id=$1")
        .bind(notation)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(state, "BEGIN");
    pg.pool().close().await;
}

#[tokio::test]
async fn reset_rolls_back_children_on_failure_then_retries_without_cross_notation_deletion() {
    use crate::notation_reset::{delete_draft, ResetOutcome};
    use crate::{
        answers::{self, NewAnswer},
        notation_clauses, notation_events, notation_sources,
    };
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (person, notation, question) = workflow_fixture(&pg).await;
    let (_, other, _) = workflow_fixture(&pg).await;
    crate::postgres::fixtures::activate_family(&pg, Family::Notation).await;
    let db = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        answers::record(&db, &NewAnswer::new(question, person, serde_json::json!({"safe":true})).in_notation(notation,"person__client")).await.unwrap();
        notation_clauses::append(&db, notation,"Synthetic",Some(person)).await.unwrap();
        notation_sources::attach(&db, &notation_sources::NewNotationSource { notation_id:notation,kind:notation_sources::kind::QUESTIONNAIRE.into(),communication_id:None,source_ref:None,position:0 }).await.unwrap();
        notation_events::append_event(&db, notation_events::TransitionRecord { notation_id:notation,acting_person_id:Some(person),machine_kind:notation_events::MACHINE_QUESTIONNAIRE,from_state:"BEGIN",to_state:"intake_persisted",condition:"synthetic",payload_json:None,recorded_at:"2026-11-18T00:00:00Z" }).await.unwrap();
        sqlx::raw_sql("CREATE FUNCTION reject_reset_fixture() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic reset failure'; END $$; CREATE TRIGGER reject_reset_fixture BEFORE DELETE ON notation FOR EACH ROW EXECUTE FUNCTION reject_reset_fixture()")
            .execute(pg.pool()).await.unwrap();
        assert!(delete_draft(&db, notation).await.is_err());
        let retained: (i64,i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM answer WHERE notation_id=$1),(SELECT count(*) FROM notation_event WHERE notation_id=$1),(SELECT count(*) FROM notation_clause WHERE notation_id=$1),(SELECT count(*) FROM notation_source WHERE notation_id=$1)")
            .bind(notation).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(retained,(1,1,1,1));
        sqlx::query("DROP TRIGGER reject_reset_fixture ON notation").execute(pg.pool()).await.unwrap();
        assert_eq!(delete_draft(&db, notation).await.unwrap(),ResetOutcome::Deleted);
        assert_eq!(delete_draft(&db, notation).await.unwrap(),ResetOutcome::AlreadyGone);
        assert!(crate::notations::find_by_id(&db,other).await.unwrap().is_some());
        assert!(answers::for_notation(&db,notation).await.unwrap().is_empty());
        assert!(notation_events::for_notation(&db,notation).await.unwrap().is_empty());
        assert!(notation_clauses::for_notation(&db,notation).await.unwrap().is_empty());
        let sources: i64 = sqlx::query_scalar("SELECT count(*) FROM notation_source WHERE notation_id=$1").bind(notation).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(sources,0);
    }).await;
    pg.pool().close().await;
}

#[tokio::test]
async fn archived_questionnaire_snapshots_distinguish_missing_json_null_and_nested_values() {
    use crate::notations::{self, NewNotation};
    use serde_json::json;
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (person, seed, _) = workflow_fixture(&pg).await;
    let (template, project): (Uuid, Uuid) =
        sqlx::query_as("SELECT template_id, project_id FROM notation WHERE id=$1")
            .bind(seed)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    crate::postgres::fixtures::activate_family(&pg, Family::Notation).await;
    let db = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        for snapshot in [None, Some(json!(null)), Some(json!({"nested":[null,{"value":null}]}))] {
            let mut new = NewNotation::new(template, person, project, "BEGIN");
            new.questionnaire_snapshot = snapshot.clone();
            let opened = notations::create(&db, &new).await.unwrap();
            assert_eq!(opened.questionnaire_snapshot, snapshot);
            assert_eq!(notations::find_by_id(&db,opened.id).await.unwrap().unwrap().questionnaire_snapshot,snapshot);
            let encoded = serde_json::to_value(&opened).unwrap();
            assert_eq!(encoded.get("questionnaire_snapshot").is_some(), snapshot.is_some());
            let mut archived: notations::Notation = serde_json::from_value(encoded).unwrap();
            assert_eq!(archived.questionnaire_snapshot,snapshot);
            archived.id=Uuid::now_v7();
            assert_eq!(notations::restore_archived(&db,&archived).await.unwrap(),crate::assets::RowRestore::Created);
            assert_eq!(notations::find_by_id(&db,archived.id).await.unwrap().unwrap().questionnaire_snapshot,snapshot);
            assert_eq!(notations::restore_archived(&db,&archived).await.unwrap(),crate::assets::RowRestore::Identical);
            let updated = notations::update_questionnaire_snapshot(&db,archived.id,json!(null)).await.unwrap();
            assert_eq!(updated.questionnaire_snapshot,Some(json!(null)));
            let (is_sql_null,is_json_null):(bool,bool)=sqlx::query_as("SELECT questionnaire_snapshot IS NULL, questionnaire_snapshot='null'::jsonb FROM notation WHERE id=$1")
                .bind(archived.id).fetch_one(pg.pool()).await.unwrap();
            assert_eq!((is_sql_null,is_json_null),(false,true));
        }
    }).await;
    pg.pool().close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one document import, CAS, and immutable archive journey"
)]
async fn document_root_edits_and_archive_replay_use_postgres_without_source_parents() {
    use crate::{
        assets, notation_documents as documents,
        notation_reset::{self, ResetOutcome},
    };
    use std::sync::Arc;
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (person, notation, _) = workflow_fixture(&pg).await;
    let (_, other, _) = workflow_fixture(&pg).await;
    let project: Uuid = sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
        .bind(notation)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let other_project: Uuid = sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
        .bind(other)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    crate::postgres::fixtures::activate_family(&pg, Family::Notation).await;
    let directory = tempfile::tempdir().unwrap();
    let storage: Arc<dyn cloud::StorageService> =
        Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
    let db = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        let original = assets::ingest_content(
            &db,
            &storage,
            b"Synthetic original",
            "application/octet-stream",
        )
        .await
        .unwrap();
        let root_args = || documents::ImportRoot {
            project_id: project,
            notation_id: notation,
            original_asset_id: original,
            markdown: b"# Synthetic root",
            anchor_manifest: b"{}",
            parser_version: "synthetic-parser",
            schema_version: 1,
            authored_by_person_id: person,
            recorded_at: "2026-11-18T00:00:00Z",
        };
        let saved = documents::import_root(&db, &storage, root_args())
            .await
            .unwrap();
        assert!(saved.was_written());
        let root = saved.into_model();
        let replay = documents::import_root(&db, &storage, root_args())
            .await
            .unwrap();
        assert!(!replay.was_written());
        assert_eq!(replay.into_model().id, root.id);
        let edit_args = || documents::AppendEdit {
            project_id: project,
            notation_id: notation,
            expected_parent_version_id: root.id,
            markdown: b"# Synthetic edited",
            anchor_manifest: b"{}",
            parser_version: "synthetic-parser",
            schema_version: 1,
            authored_by_person_id: person,
            recorded_at: "2026-11-18T00:01:00Z",
        };
        let edited = documents::append_edit(&db, &storage, edit_args())
            .await
            .unwrap();
        assert!(matches!(
            documents::append_edit(&db, &storage, edit_args())
                .await
                .unwrap_err(),
            documents::NotationDocumentError::Conflict { .. }
        ));
        assert_eq!(
            documents::current_version(&db, project, notation)
                .await
                .unwrap()
                .unwrap()
                .id,
            edited.id
        );
        assert_eq!(
            documents::history(&db, project, notation)
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            documents::markdown_of(&db, &storage, &edited)
                .await
                .unwrap(),
            "# Synthetic edited"
        );
        assert!(documents::find_version(&db, other_project, other, root.id)
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            documents::find_document(&db, other_project, notation)
                .await
                .unwrap_err(),
            documents::NotationDocumentError::ProjectMismatch { .. }
        ));
        let document = documents::find_document(&db, project, notation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            documents::restore_archived_document(&db, &document)
                .await
                .unwrap(),
            assets::RowRestore::Identical
        );
        assert_eq!(
            documents::restore_archived_version(&db, &root)
                .await
                .unwrap(),
            assets::RowRestore::Identical
        );
        let mut conflict = root.clone();
        conflict.status = "conflicting-history".into();
        assert_eq!(
            documents::restore_archived_version(&db, &conflict)
                .await
                .unwrap(),
            assets::RowRestore::Conflict
        );
        assert_eq!(
            notation_reset::delete_draft(&db, notation).await.unwrap(),
            ResetOutcome::Kept
        );
        assert!(matches!(
            crate::notations::delete_uncommitted(&db, notation)
                .await
                .unwrap_err(),
            crate::notations::NotationError::ProtectedFromDeletion(_)
        ));
        assert_eq!(
            notation_reset::delete_draft(&db, other).await.unwrap(),
            ResetOutcome::Deleted
        );
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one Asset lifecycle and protected parent reset journey"
)]
async fn asset_metadata_lifecycle_and_note_key_protection_are_postgres_authoritative() {
    use crate::{
        assets, documents,
        notation_reset::{self, ResetOutcome},
    };
    use serde_json::json;
    use std::sync::Arc;
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let (person, notation, _) = workflow_fixture(&pg).await;
    let project: Uuid = sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
        .bind(notation)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    crate::postgres::fixtures::activate_family(&pg, Family::Notation).await;
    let directory = tempfile::tempdir().unwrap();
    let storage: Arc<dyn cloud::StorageService> =
        Arc::new(cloud::FsStorage::new(directory.path()).await.unwrap());
    let db = crate::application_store::ApplicationStore::uninitialized();
    Box::pin(with_test_db(pg.clone(), async {
        let key = crate::notations::document_pdf_storage_key(notation);
        let args = documents::IngestArgs {
            project_id: project,
            source: "generated",
            filename: "synthetic.pdf",
            kind: "unclassified",
            content_type: "application/pdf",
            description: None,
            visibility: documents::visibility::CLIENT,
            secondary_storage_key: Some(&key),
        };
        let identity = documents::DocumentIdentity {
            slug: Some("synthetic.pdf"),
            metadata: Some(json!(null)),
            ..Default::default()
        };
        let document =
            documents::ingest_bytes_as(&db, &storage, &args, &identity, b"Synthetic PDF")
                .await
                .unwrap();
        let row = assets::find_by_id(&db, document.asset_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.metadata, Some(json!(null)));
        assert_eq!(row.secondary_storage_key.as_deref(), Some(key.as_str()));
        assert_eq!(
            notation_reset::delete_draft(&db, notation).await.unwrap(),
            ResetOutcome::Kept
        );
        assert_eq!(
            assets::set_visibility(&db, project, row.id, documents::visibility::INTERNAL)
                .await
                .unwrap(),
            Some(true)
        );
        assert!(
            assets::current(&db, project, "synthetic.pdf", assets::Lens::Client)
                .await
                .unwrap()
                .is_none()
        );
        let now = chrono::Utc::now();
        assert!(
            assets::soft_delete(&db, project, row.id, person, None, now)
                .await
                .unwrap()
                .changed
        );
        assert!(assets::for_project(&db, project).await.unwrap().is_empty());
        assert_eq!(
            assets::set_legal_hold(&db, project, row.id, true, now)
                .await
                .unwrap(),
            Some(1)
        );
        assert!(assets::restore(&db, project, row.id).await.unwrap().changed);
        let restored = assets::find_by_id(&db, row.id).await.unwrap().unwrap();
        assert_eq!(restored.metadata, Some(json!(null)));
        assert!(restored.legal_hold_at.is_some());
        assert_eq!(
            assets::restore_archived(&db, &restored).await.unwrap(),
            assets::RowRestore::Identical
        );
        let mut conflicting = restored.clone();
        conflicting.visibility = documents::visibility::CLIENT.into();
        assert_eq!(
            assets::restore_archived(&db, &conflicting).await.unwrap(),
            assets::RowRestore::Conflict
        );
        assert!(assets::delete(&db, row.id).await.unwrap().is_some());
        assert_eq!(
            notation_reset::delete_draft(&db, notation).await.unwrap(),
            ResetOutcome::Deleted
        );
        assert!(
            documents::ingest_bytes_as(&db, &storage, &args, &identity, b"late fact")
                .await
                .is_err()
        );
        assert_eq!(
            assets::count_for_projects(&db, &[project]).await.unwrap(),
            0
        );
    }))
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn native_notation_query_shapes_preserve_project_person_and_template_isolation() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    crate::postgres::fixtures::activate_application(&pg).await;
    let (person_a, a, _) = workflow_fixture(&pg).await;
    let (person_b, b, _) = workflow_fixture(&pg).await;
    let (project_a, template_a): (Uuid, Uuid) =
        sqlx::query_as("SELECT project_id,template_id FROM notation WHERE id=$1")
            .bind(a)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    let project_b: Uuid = sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
        .bind(b)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    with_test_db(pg.clone(), async {
        use crate::notations;
        let ids = |rows: Vec<notations::Notation>| {
            rows.into_iter()
                .map(|row| row.id)
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(
            ids(notations::list_by_project(&unavailable, project_a)
                .await
                .unwrap()),
            [a].into()
        );
        assert_eq!(
            ids(
                notations::list_by_projects(&unavailable, &[project_a, project_b])
                    .await
                    .unwrap()
            ),
            [a, b].into()
        );
        for (person, expected) in [(person_a, a), (person_b, b)] {
            assert_eq!(
                ids(notations::list_by_person(&unavailable, person)
                    .await
                    .unwrap()),
                [expected].into()
            );
        }
        assert_eq!(
            ids(notations::list_all(&unavailable).await.unwrap()),
            [a, b].into()
        );
        assert_eq!(
            notations::find_by_project_template_person(
                &unavailable,
                project_a,
                template_a,
                person_a
            )
            .await
            .unwrap()
            .unwrap()
            .id,
            a
        );
        for (project, person) in [(project_a, person_b), (project_b, person_a)] {
            assert!(notations::find_by_project_template_person(
                &unavailable,
                project,
                template_a,
                person
            )
            .await
            .unwrap()
            .is_none());
        }
        assert!(notations::exists_for_project(&unavailable, project_a)
            .await
            .unwrap());
        assert!(!notations::exists_for_project(&unavailable, Uuid::new_v4())
            .await
            .unwrap());
    })
    .await;
    pg.pool().close().await;
}
