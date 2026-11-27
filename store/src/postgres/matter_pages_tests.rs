//! PostgreSQL Matter Page replacement and Project/Firm boundaries.
use super::super::{
    fixtures::{activate_application, firm_for_notation, workflow_fixture},
    PostgresDb,
};
use crate::matter_pages::{self as pages, MatterPageError};
use crate::postgres::fixtures::{connect, fresh_schema};
use rules::{dashboard::Lens, pages::PageEntry};
use uuid::Uuid;
const AT: &str = "2026-11-18T12:00:00Z";
fn page(slug: &str) -> PageEntry {
    PageEntry {
        slug: slug.into(),
        kind: "docket_deadline_board".into(),
        title: format!("Page {slug}"),
        draft: None,
        document_revision: None,
        media: vec![],
        lenses: std::collections::BTreeMap::from([(
            "lawyer".into(),
            vec![
                "deadline_board".into(),
                "boundary_note".into(),
                "provenance_statement".into(),
            ],
        )]),
    }
}
async fn fixture() -> Option<(PostgresDb, Uuid)> {
    let (options, name) = fresh_schema().await?;
    let pg = connect(&options, &name).await;
    activate_application(&pg).await;
    let (_, note, _) = workflow_fixture(&pg).await;
    firm_for_notation(&pg, note).await;
    let project = sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
        .bind(note)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    Some((pg, project))
}
#[tokio::test]
async fn replacement_keeps_order_lenses_and_other_projects_isolated() {
    let Some((pg, project)) = fixture().await else {
        return;
    };
    let db = crate::application_store::ApplicationStore::uninitialized();
    PostgresDb::with_test_db(pg.clone(), async {
        let (_, other_note, _) = workflow_fixture(&pg).await;
        let (other_firm, _) = firm_for_notation(&pg, other_note).await;
        let other = sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
            .bind(other_note)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        pages::replace_for_project(&db, other, &[page("other")])
            .await
            .unwrap();
        let mut first = page("timeline");
        first.lenses.insert(
            "client".into(),
            vec![
                "key_dates_strip".into(),
                "boundary_note".into(),
                "provenance_statement".into(),
            ],
        );
        let revision = Uuid::now_v7().to_string();
        let mut rich = page("messages");
        rich.kind = "whatsapp_timeline".into();
        rich.document_revision = Some(revision.clone());
        rich.media.push(rules::pages::TimelineMedia {
            document_revision: revision,
            created_at: Some(AT.parse().unwrap()),
        });
        rich.lenses.insert(
            "lawyer".into(),
            vec![
                "whatsapp_timeline".into(),
                "boundary_note".into(),
                "provenance_statement".into(),
            ],
        );
        let declared = vec![first.clone(), page("custom-deadlines"), rich];
        let rows = pages::replace_for_project(&db, project, &declared)
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows.iter().map(|row| row.entry.clone()).collect::<Vec<_>>(),
            declared
        );
        assert_eq!(rows[0].position, 0);
        assert_eq!(rows[1].position, 1);
        assert_eq!(rows[2].position, 2);
        assert!(rows[1].entry.sections_for(Lens::Client).is_none());
        assert_eq!(pages::inbox_entries(&db, project).await.unwrap(), declared);
        pages::replace_for_project(&db, project, &[page("custom-deadlines")])
            .await
            .unwrap();
        let rows = pages::list_for_project(&db, project).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entry.slug, "custom-deadlines");
        assert_eq!(rows[0].position, 0);
        pages::delete_for_project(&db, project).await.unwrap();
        assert!(pages::list_for_project(&db, project)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(pages::list_for_project(&db, other).await.unwrap().len(), 1);
        let firm: Uuid = sqlx::query_scalar("SELECT firm_id FROM matter_page WHERE project_id=$1")
            .bind(other)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(firm, other_firm);
    })
    .await;
    pg.pool().close().await;
}
#[tokio::test]
async fn invalid_manifest_or_failed_insert_preserves_the_previous_set() {
    let Some((pg, project)) = fixture().await else {
        return;
    };
    let db = crate::application_store::ApplicationStore::uninitialized();
    PostgresDb::with_test_db(pg.clone(),async {
        let original=pages::replace_for_project(&db,project,&[page("original")]).await.unwrap();
        let mut invalid=page("duplicate");invalid.lenses.insert("client".into(),vec!["deadline_board".into(),"provenance_statement".into()]);
        let error=pages::replace_for_project(&db,project,&[invalid.clone(),invalid]).await.unwrap_err();assert!(matches!(error,MatterPageError::Invalid(_)));let message=error.to_string();assert!(message.contains("Y021")&&message.contains("D003"));
        sqlx::raw_sql("CREATE FUNCTION reject_page() RETURNS trigger LANGUAGE plpgsql AS $$BEGIN IF NEW.slug='rejected' THEN RAISE EXCEPTION 'synthetic rejection'; END IF; RETURN NEW; END$$; CREATE TRIGGER reject_page BEFORE INSERT ON matter_page FOR EACH ROW EXECUTE FUNCTION reject_page();").execute(pg.pool()).await.unwrap();
        assert!(pages::replace_for_project(&db,project,&[page("accepted"),page("rejected")]).await.is_err());assert_eq!(pages::list_for_project(&db,project).await.unwrap(),original);
    }).await;
    pg.pool().close().await;
}

#[tokio::test]
async fn concurrent_replacements_commit_one_complete_manifest() {
    let Some((pg, project)) = fixture().await else {
        return;
    };
    let db = crate::application_store::ApplicationStore::uninitialized();
    PostgresDb::with_test_db(pg.clone(), async {
        let first = [page("first"), page("first-second")];
        let second = [page("second")];
        let (a, b) = tokio::join!(
            pages::replace_for_project(&db, project, &first),
            pages::replace_for_project(&db, project, &second)
        );
        assert_eq!(a.unwrap().len(), 2);
        assert_eq!(b.unwrap().len(), 1);
        let entries = pages::inbox_entries(&db, project).await.unwrap();
        assert!(entries == first || entries == second);
    })
    .await;
    pg.pool().close().await;
}
