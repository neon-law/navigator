//! Native comment scope, immutable lineage and atomic review boundaries.
use crate::notation_documents::{
    self as documents, DecideCommentError, ImportRoot, NotationDocumentError,
};
use crate::postgres::fixtures::{connect, fresh_schema};
use crate::postgres::{
    fixtures::{activate_application, workflow_fixture},
    PostgresDb,
};
use uuid::Uuid;

const AT: &str = "2026-11-18T12:00:00Z";
struct Fixture {
    pg: PostgresDb,
    person: Uuid,
    note: Uuid,
    project: Uuid,
    version: Uuid,
    storage: std::sync::Arc<dyn cloud::StorageService>,
    _dir: tempfile::TempDir,
}
async fn fixture() -> Option<Fixture> {
    let (options, name) = fresh_schema().await?;
    let pg = connect(&options, &name).await;
    activate_application(&pg).await;
    let (person, note, _) = workflow_fixture(&pg).await;
    let project = sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
        .bind(note)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let storage: std::sync::Arc<dyn cloud::StorageService> =
        std::sync::Arc::new(cloud::FsStorage::new(dir.path()).await.unwrap());
    let db = crate::application_store::ApplicationStore::uninitialized();
    let version = PostgresDb::with_test_db(pg.clone(), async {
        let original = crate::assets::ingest_content(
            &db,
            &storage,
            b"synthetic document",
            "application/octet-stream",
        )
        .await
        .unwrap();
        documents::import_root(
            &db,
            &storage,
            ImportRoot {
                project_id: project,
                notation_id: note,
                original_asset_id: original,
                markdown: b"# Synthetic",
                anchor_manifest: b"{\"blocks\":[]}",
                parser_version: "test",
                schema_version: 1,
                authored_by_person_id: person,
                recorded_at: AT,
            },
        )
        .await
        .unwrap()
        .into_model()
        .id
    })
    .await;
    Some(Fixture {
        pg,
        person,
        note,
        project,
        version,
        storage,
        _dir: dir,
    })
}
async fn child(f: &Fixture, parent: Uuid) -> Uuid {
    let markdown = format!("# Revised {parent}");
    documents::append_edit(
        &crate::application_store::ApplicationStore::uninitialized(),
        &f.storage,
        documents::AppendEdit {
            project_id: f.project,
            notation_id: f.note,
            expected_parent_version_id: parent,
            markdown: markdown.as_bytes(),
            anchor_manifest: b"{\"blocks\":[]}",
            parser_version: "test",
            schema_version: 1,
            authored_by_person_id: f.person,
            recorded_at: AT,
        },
    )
    .await
    .unwrap()
    .id
}
#[tokio::test]
async fn comments_keep_version_scope_and_historical_review_admission() {
    let Some(f) = fixture().await else {
        return;
    };
    let db = crate::application_store::ApplicationStore::uninitialized();
    PostgresDb::with_test_db(f.pg.clone(),async {
        assert!(documents::all_comments_decided(&db,f.project,f.note,f.version).await.unwrap());
        let id=documents::add_comment(&db,f.project,f.note,f.version,"anchor",f.person,"synthetic comment").await.unwrap();
        assert!(!documents::all_comments_decided(&db,f.project,f.note,f.version).await.unwrap());
        let rows=documents::comments_for_version(&db,f.project,f.note,f.version).await.unwrap();
        assert_eq!((rows[0].id,rows[0].person_id,rows[0].body.as_str()),(id,f.person,"synthetic comment"));
        assert!(!rows[0].resolved);
        let next=child(&f,f.version).await;
        assert!(documents::comments_for_version(&db,f.project,f.note,next).await.unwrap().is_empty());
        documents::decide_comment(&db,f.project,f.note,id,documents::DECISION_ACCEPTED,f.person,AT).await.unwrap();
        assert!(documents::all_comments_decided(&db,f.project,f.note,f.version).await.unwrap());
        let (_,other,_)=workflow_fixture(&f.pg).await;
        let other_project=sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1").bind(other).fetch_one(f.pg.pool()).await.unwrap();
        assert!(matches!(documents::decide_comment(&db,other_project,other,id,documents::DECISION_ACCEPTED,f.person,AT).await,Err(DecideCommentError::NotFound(found)) if found==id));
        assert!(matches!(documents::add_comment(&db,other_project,f.note,f.version,"bad",f.person,"bad").await,Err(NotationDocumentError::ProjectMismatch {..})));
        assert!(documents::comments_for_version(&db,f.project,f.note,Uuid::now_v7()).await.unwrap().is_empty());
        assert!(matches!(documents::add_comment(&db,f.project,f.note,Uuid::now_v7(),"bad",f.person,"bad").await,Err(NotationDocumentError::VersionNotFound(_))));
        assert!(matches!(documents::decide_comment(&db,f.project,f.note,id,"invalid",f.person,AT).await,Err(DecideCommentError::InvalidDecision(_))));
    }).await;
    f.pg.pool().close().await;
}
#[tokio::test]
async fn carry_comments_checks_lineage_and_preserves_anchor_decisions() {
    let Some(f) = fixture().await else {
        return;
    };
    let db = crate::application_store::ApplicationStore::uninitialized();
    PostgresDb::with_test_db(f.pg.clone(), async {
        let kept =
            documents::add_comment(&db, f.project, f.note, f.version, "kept", f.person, "keep")
                .await
                .unwrap();
        let undecided = documents::add_comment(
            &db,
            f.project,
            f.note,
            f.version,
            "undecided",
            f.person,
            "pending",
        )
        .await
        .unwrap();
        let gone =
            documents::add_comment(&db, f.project, f.note, f.version, "gone", f.person, "gone")
                .await
                .unwrap();
        for id in [kept, gone] {
            documents::decide_comment(
                &db,
                f.project,
                f.note,
                id,
                documents::DECISION_EDITED,
                f.person,
                AT,
            )
            .await
            .unwrap();
        }
        let next = child(&f, f.version).await;
        let rows = documents::carry_comments_to_version(
            &db,
            f.project,
            f.note,
            f.version,
            next,
            &["kept".into(), "undecided".into()],
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().any(|r| r.anchor == "kept"
            && r.resolved
            && r.decision.as_deref() == Some(documents::DECISION_EDITED)));
        assert!(rows
            .iter()
            .any(|r| r.anchor == "gone" && !r.resolved && r.decision.is_none()));
        assert!(rows.iter().all(|r| r.version_id == next
            && r.person_id == f.person
            && r.id != kept
            && r.id != gone));
        assert!(rows.iter().any(|row| row.anchor == "undecided"
            && row.resolved
            && row.decision.is_none()
            && row.id != undecided));
        let later = child(&f, next).await;
        assert!(matches!(
            documents::carry_comments_to_version(&db, f.project, f.note, f.version, later, &[])
                .await,
            Err(NotationDocumentError::Conflict { .. })
        ));
        assert_eq!(
            documents::comments_for_version(&db, f.project, f.note, f.version)
                .await
                .unwrap()
                .len(),
            3
        );
    })
    .await;
    f.pg.pool().close().await;
}
#[tokio::test]
async fn concurrent_comment_decisions_record_exactly_one_pinned_event() {
    let Some(f) = fixture().await else {
        return;
    };
    let db = crate::application_store::ApplicationStore::uninitialized();
    PostgresDb::with_test_db(f.pg.clone(),async {
        let id=documents::add_comment(&db,f.project,f.note,f.version,"anchor",f.person,"private synthetic text").await.unwrap();
        let (a,b)=tokio::join!(documents::decide_comment(&db,f.project,f.note,id,documents::DECISION_ACCEPTED,f.person,AT),documents::decide_comment(&db,f.project,f.note,id,documents::DECISION_REJECTED,f.person,AT));
        assert_eq!(usize::from(a.is_ok())+usize::from(b.is_ok()),1);
        assert!(matches!(a.err().or_else(||b.err()),Some(DecideCommentError::AlreadyDecided(found)) if found==id));
        let event:serde_json::Value=sqlx::query_scalar("SELECT to_jsonb(e) FROM notation_event e WHERE notation_id=$1 AND machine_kind='document_review'").bind(f.note).fetch_one(f.pg.pool()).await.unwrap();
        let template:Uuid=sqlx::query_scalar("SELECT template_id FROM notation WHERE id=$1").bind(f.note).fetch_one(f.pg.pool()).await.unwrap();
        assert_eq!(event["template_version_id"],template.to_string());
        assert_eq!(event["acting_person_id"],f.person.to_string());
        let payload:serde_json::Value=serde_json::from_str(event["payload"].as_str().unwrap()).unwrap();
        assert_eq!(payload["comment_id"],id.to_string());
        assert_eq!(payload["version_id"],f.version.to_string());
        assert!(!event.to_string().contains("private synthetic text"));
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM notation_event WHERE notation_id=$1 AND machine_kind='document_review'").bind(f.note).fetch_one(f.pg.pool()).await.unwrap();assert_eq!(count,1);
    }).await;
    f.pg.pool().close().await;
}
#[tokio::test]
async fn failed_review_event_rolls_back_the_comment_decision() {
    let Some(f) = fixture().await else {
        return;
    };
    let db = crate::application_store::ApplicationStore::uninitialized();
    PostgresDb::with_test_db(f.pg.clone(),async {
        let id=documents::add_comment(&db,f.project,f.note,f.version,"anchor",f.person,"rollback").await.unwrap();
        sqlx::raw_sql("CREATE FUNCTION reject_review() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN IF NEW.machine_kind='document_review' THEN RAISE EXCEPTION 'synthetic rejection'; END IF; RETURN NEW; END$$; CREATE TRIGGER reject_review BEFORE INSERT ON notation_event FOR EACH ROW EXECUTE FUNCTION reject_review();").execute(f.pg.pool()).await.unwrap();
        assert!(documents::decide_comment(&db,f.project,f.note,id,documents::DECISION_ACCEPTED,f.person,AT).await.is_err());
        let rows=documents::comments_for_version(&db,f.project,f.note,f.version).await.unwrap();assert_eq!(rows[0].decision,None);assert!(!rows[0].resolved);
    }).await;
    f.pg.pool().close().await;
}

#[tokio::test]
async fn missing_history_refuses_comment_writes_without_source_fallback() {
    let Some(f) = fixture().await else {
        return;
    };
    let db = crate::application_store::ApplicationStore::uninitialized();
    PostgresDb::with_test_db(f.pg.clone(), async {
        sqlx::query(
            "UPDATE mirror_backfill_run SET status='running',completed_at=NULL WHERE scope='application:1'",
        )
        .execute(f.pg.pool())
        .await
        .unwrap();
        assert!(documents::add_comment(
            &db, f.project, f.note, f.version, "anchor", f.person, "refused"
        )
        .await
        .is_err());
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM notation_document_comment")
            .fetch_one(f.pg.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
    })
    .await;
    f.pg.pool().close().await;
}
