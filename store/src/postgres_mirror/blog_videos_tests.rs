use super::*;
use crate::{
    postgres_mirror::tests::{connect, fresh_schema},
    surreal::record_id,
    test_support::mem_surreal,
};

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
    let brand_key = format!("synthetic-video-{}", Uuid::now_v7());
    sqlx::query("INSERT INTO brand (name, brand_key) VALUES ('Synthetic Brand', $1)")
        .bind(&brand_key)
        .execute(pg.pool())
        .await
        .unwrap();
    let object_key = format!("synthetic/{brand_key}/upload.mp4");
    let id = Uuid::now_v7();
    surreal.query("CREATE $id SET brand_key=$brand, object_key=$key, width=1080, height=1920, orientation='vertical', inserted_at=time::now()")
        .bind(("id", record_id("blog_video", id)))
        .bind(("brand", brand_key.clone()))
        .bind(("key", object_key.clone()))
        .await.unwrap().check().unwrap();
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
    .bind(format!("missing-{}", Uuid::now_v7()))
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
