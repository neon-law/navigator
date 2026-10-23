use super::*;
use crate::{
    brands::NewBrand,
    firms::NewFirm,
    persons::{NewPerson, Role},
    postgres_mirror::tests::{connect, fresh_schema, synthetic_entity},
    surreal::record_id,
    test_support::mem_surreal,
};
use surrealdb::types::SurrealValue;

async fn practice(surreal: &SurrealDb, suffix: &str) -> (Uuid, String) {
    let person = crate::persons::create(
        surreal,
        &NewPerson::with_role(
            "Synthetic administrator",
            format!("blog-video-{suffix}@example.test"),
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    let firm = crate::firms::create(
        surreal,
        &NewFirm {
            name: format!("Synthetic Firm {suffix}"),
            status: "active".into(),
            entity_id: synthetic_entity(surreal).await,
            admin_dri_person_id: person.id,
        },
    )
    .await
    .unwrap();
    let brand_key = format!("blog-video-{suffix}");
    crate::brands::create(
        surreal,
        Role::Admin,
        Some(person.id),
        &NewBrand {
            name: format!("Synthetic Brand {suffix}"),
            key: brand_key.clone(),
            firm_id: Some(firm.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (firm.id, brand_key)
}

async fn prepare_target(pg: &PostgresDb, surreal: &SurrealDb) {
    crate::postgres_mirror::identity::reconcile(surreal, pg)
        .await
        .unwrap();
    crate::postgres_mirror::tenancy::reconcile(surreal, pg)
        .await
        .unwrap();
}

#[test]
fn blog_video_key_names_only_its_source_uuid() {
    let id = Uuid::new_v4();
    let key = MirrorKey::BlogVideo(id);
    assert_eq!(key.table(), "blog_video");
    assert_eq!(key.to_string(), format!("blog_video:{id}"));
    assert_eq!(key.report_id(), Some(id.to_string()));
}

#[test]
fn blog_video_mirror_runs_after_brand_ownership() {
    let names = crate::postgres_mirror::MIRRORS
        .iter()
        .map(|mirror| mirror.name)
        .collect::<Vec<_>>();
    let tenancy = names.iter().position(|name| *name == "tenancy").unwrap();
    let videos = names
        .iter()
        .position(|name| *name == "blog_videos")
        .unwrap();
    assert!(tenancy < videos);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn video_uploads_mirror_replay_and_reconcile_with_brand_ownership() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let (firm_id, brand_key) = practice(&surreal, "one").await;
    prepare_target(&pg, &surreal).await;

    let object_key = format!("synthetic/{brand_key}/upload.mp4");
    let video = crate::posts::record_video(&surreal, &brand_key, &object_key, 1080, 1920)
        .await
        .unwrap();
    let mut source = surreal
        .query("SELECT id FROM blog_video WHERE object_key = $key LIMIT 1")
        .bind(("key", object_key.clone()))
        .await
        .unwrap()
        .check()
        .unwrap();
    let row: Option<SourceId> = source.take(0).unwrap();
    let id = crate::surreal::record_uuid(&row.unwrap().id).unwrap();
    assert_eq!(
        crate::postgres_mirror::mirror(&surreal, &pg, &MirrorKey::BlogVideo(id))
            .await
            .unwrap(),
        Mirrored::Written
    );
    assert_eq!(
        crate::postgres_mirror::mirror(&surreal, &pg, &MirrorKey::BlogVideo(id))
            .await
            .unwrap(),
        Mirrored::Unchanged
    );
    let image: (Uuid, String, String, i64, i64, String) = sqlx::query_as(
        "SELECT id, brand_key, object_key, width, height, orientation FROM blog_video WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pg.0)
    .await
    .unwrap();
    assert_eq!(image.0, id);
    assert_eq!(image.1, brand_key);
    assert_eq!(image.2, object_key);
    assert_eq!(
        (image.3, image.4, image.5.as_str()),
        (1080, 1920, "vertical")
    );
    assert_eq!(video.width, 1080);
    assert_eq!(
        crate::postgres_mirror::blog_videos::parity(&surreal, &pg)
            .await
            .unwrap()
            .tables[0]
            .divergent,
        []
    );
    sqlx::query("UPDATE blog_video SET orientation = 'landscape' WHERE id = $1")
        .bind(id)
        .execute(&pg.0)
        .await
        .unwrap();
    let drift = crate::postgres_mirror::blog_videos::parity(&surreal, &pg)
        .await
        .unwrap();
    assert_eq!(drift.tables[0].divergent, [MirrorKey::BlogVideo(id)]);
    assert!(
        crate::postgres_mirror::blog_videos::reconcile(&surreal, &pg)
            .await
            .unwrap()
            .is_clean()
    );

    let mismatch = sqlx::query(
        "INSERT INTO blog_video (id, brand_key, object_key, width, height, orientation) \
         VALUES ($1, $2, $3, 1080, 1920, 'vertical')",
    )
    .bind(Uuid::new_v4())
    .bind(format!("missing-{firm_id}"))
    .bind("synthetic/missing-brand/upload.mp4")
    .execute(&pg.0)
    .await
    .unwrap_err();
    assert_eq!(
        mismatch
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23503")
    );

    surreal
        .query("DELETE $id")
        .bind(("id", record_id("blog_video", id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let report = crate::postgres_mirror::blog_videos::reconcile(&surreal, &pg)
        .await
        .unwrap();
    assert_eq!(report.tables[0].source_rows, 0);
    assert_eq!(report.tables[0].target_rows, 0);
}

#[derive(SurrealValue)]
struct SourceId {
    id: surrealdb::types::RecordId,
}
