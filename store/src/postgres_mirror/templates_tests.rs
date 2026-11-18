//! Frozen Source extraction, first-copy retention, and immutable pin admission.
use super::*;
use crate::postgres::fixtures::workflow_fixture;
use crate::postgres_mirror::tests::{connect, fresh_schema, synthetic_entity};
use crate::test_support::mem_surreal;

async fn harness() -> Option<(SurrealDb, PostgresDb)> {
    let (options, name) = fresh_schema().await?;
    Some((mem_surreal().await, connect(&options, &name).await))
}

async fn seed_template(
    source: &SurrealDb,
    project: Option<Uuid>,
    asset: Option<Uuid>,
    title: &str,
) -> Uuid {
    let id = Uuid::now_v7();
    let code = format!("captured-{id}");
    let digest = asset.map(|_| crate::assets::sha256_hex(b"# Engagement"));
    source.query("CREATE $id SET code=$code,title=$title,respondent_type='person',project_id=$project,firm_id=NONE,is_current=true,asset_id=$asset,form_code=NONE,kind='letter',source_commit_sha='captured-commit',body_sha256=$digest,current_key=$key")
        .bind(("id",record_id("template",id))).bind(("code",code.clone())).bind(("title",title.to_owned()))
        .bind(("project",project.map(|id|record_id("project",id)))).bind(("asset",asset.map(|id|record_id("asset",id))))
        .bind(("digest",digest)).bind(("key",crate::templates::current_key_in(crate::templates::Scope::of_project(project),&code)))
        .await.unwrap().check().unwrap();
    id
}

#[tokio::test]
async fn first_copy_extracts_the_shared_version_and_is_idempotent() {
    super::super::from_source(super::super::during_initial_copy(async {
        let Some((source, pg)) = harness().await else {
            return;
        };
        let id = seed_template(&source, None, None, "Captured").await;
        assert_eq!(mirror(&source, &pg, id).await.unwrap(), Mirrored::Written);
        assert_eq!(mirror(&source, &pg, id).await.unwrap(), Mirrored::Unchanged);
        let row = crate::templates::find_by_id(&source, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.title, "Captured");
        assert_eq!(row.source_commit_sha.as_deref(), Some("captured-commit"));
        assert!(parity(&source, &pg).await.unwrap().is_clean());
        source
            .query("UPDATE $id SET current_key='noncanonical'")
            .bind(("id", record_id("template", id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert!(matches!(
            mirror(&source, &pg, id).await,
            Err(MirrorError::Source)
        ));
        let retained: String = sqlx::query_scalar("SELECT title FROM template WHERE id=$1")
            .bind(id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(retained, "Captured");
        pg.pool().close().await;
    }))
    .await;
}

#[tokio::test]
async fn first_copy_keeps_the_postgres_version_when_source_is_gone() {
    super::super::from_source(super::super::during_initial_copy(async {
        let Some((source, pg)) = harness().await else {
            return;
        };
        let id = seed_template(&source, None, None, "Retained").await;
        mirror(&source, &pg, id).await.unwrap();
        source
            .query("DELETE $id")
            .bind(("id", record_id("template", id)))
            .await
            .unwrap()
            .check()
            .unwrap();
        assert!(matches!(
            mirror(&source, &pg, id).await,
            Err(MirrorError::Source)
        ));
        assert!(!parity(&source, &pg).await.unwrap().is_clean());
        let present: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM template WHERE id=$1)")
            .bind(id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert!(present);
        pg.pool().close().await;
    }))
    .await;
}

#[tokio::test]
async fn reconcile_preserves_unknown_immutable_version_drift() {
    super::super::from_source(super::super::during_initial_copy(async {
        let Some((source, pg)) = harness().await else {
            return;
        };
        let id = seed_template(&source, None, None, "Captured").await;
        mirror(&source, &pg, id).await.unwrap();
        sqlx::query("UPDATE template SET title='Native drift' WHERE id=$1")
            .bind(id)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(!reconcile(&source, &pg).await.unwrap().is_clean());
        let title: String = sqlx::query_scalar("SELECT title FROM template WHERE id=$1")
            .bind(id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(title, "Native drift");
        pg.pool().close().await;
    }))
    .await;
}

#[tokio::test]
async fn first_copy_resolves_captured_project_and_body_parents() {
    super::super::from_source(super::super::during_initial_copy(async {
        let Some((source,pg))=harness().await else {return;};
        let entity=synthetic_entity(&source).await;
        let project=Uuid::now_v7();
        let now=chrono::Utc::now().to_rfc3339();
        source.query("CREATE $id SET code=$code,name='Captured Project',status='open',brand='neon',entity_id=$entity,inserted_at=$now,updated_at=$now")
            .bind(("id",record_id("project",project))).bind(("code",project.to_string())).bind(("entity",record_id("entity",entity))).bind(("now",now))
            .await.unwrap().check().unwrap();
        let asset=Uuid::now_v7();
        let sha=crate::assets::sha256_hex(b"# Engagement");
        source.query("CREATE $id SET storage_key=$key,content_type='text/markdown',byte_size=12,sha256_hex=$sha,visibility='internal'")
            .bind(("id",record_id("asset",asset))).bind(("key",format!("blobs/{sha}"))).bind(("sha",sha))
            .await.unwrap().check().unwrap();
        let id=seed_template(&source,Some(project),Some(asset),"Captured Project Version").await;
        assert_eq!(mirror(&source,&pg,id).await.unwrap(),Mirrored::Written);
        let present:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM project WHERE id=$1)").bind(project).fetch_one(pg.pool()).await.unwrap();
        assert!(present);
        assert!(parity(&source,&pg).await.unwrap().is_clean());
        pg.pool().close().await;
    })).await;
}

#[tokio::test]
async fn refresh_only_changes_the_current_pointer() {
    super::super::from_source(super::super::during_initial_copy(async {
        let Some((source, pg)) = harness().await else {
            return;
        };
        let id = seed_template(&source, None, None, "Immutable").await;
        mirror(&source, &pg, id).await.unwrap();
        let original = source_one(&source, id).await.unwrap().unwrap();
        let mut retired = original.clone();
        retired.is_current = false;
        retired.current_key = None;
        assert_eq!(
            upsert(&pg, &retired, ImportMode::Refresh).await.unwrap(),
            Mirrored::Written
        );
        let row = sqlx::query("SELECT * FROM template WHERE id=$1")
            .bind(id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let stored = TemplateImage::from_row(&row).unwrap();
        assert!(stored.same_version(&original));
        assert!(!stored.is_current);
        assert_eq!(stored.current_key, None);
        let mut changed = retired;
        changed.title = "Rewritten".into();
        assert!(matches!(
            upsert(&pg, &changed, ImportMode::Refresh).await,
            Err(MirrorError::Source)
        ));
        pg.pool().close().await;
    }))
    .await;
}

#[tokio::test]
async fn scoped_parity_checks_native_notation_pins_even_for_unchanged_templates() {
    super::super::from_source(super::super::during_initial_copy(async {
        let Some((source,pg))=harness().await else {return;};
        let id=seed_template(&source,None,None,"Pinned").await;
        mirror(&source,&pg,id).await.unwrap();
        let (_,notation,_)=workflow_fixture(&pg).await;
        sqlx::query("UPDATE notation SET template_id=$1,template_source_commit_sha='unrelated-commit' WHERE id=$2")
            .bind(id).bind(notation).execute(pg.pool()).await.unwrap();
        assert!(matches!(scoped_parity(&source,&pg).await,Err(MirrorError::Source)));
        assert!(matches!(mirror(&source,&pg,id).await,Err(MirrorError::Source)));
        let pin:String=sqlx::query_scalar("SELECT template_source_commit_sha FROM notation WHERE id=$1").bind(notation).fetch_one(pg.pool()).await.unwrap();
        assert_eq!(pin,"unrelated-commit");
        pg.pool().close().await;
    })).await;
}
