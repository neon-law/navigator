use super::upload_sessions;
use super::*;
use crate::postgres_mirror::tests::{connect, fresh_schema};

async fn database() -> Option<PostgresDb> {
    let (options, database) = fresh_schema().await?;
    Some(connect(&options, &database).await)
}

/// A synthetic PostgreSQL Project for a session to belong to.
pub(crate) async fn project(pg: &PostgresDb) -> Uuid {
    let (jurisdiction, entity_type, entity, project) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    sqlx::query("INSERT INTO jurisdiction (id, name) VALUES ($1, 'Synthetic jurisdiction')")
        .bind(jurisdiction)
        .execute(&pg.0)
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity_type (id, name) VALUES ($1, 'Synthetic entity type')")
        .bind(entity_type)
        .execute(&pg.0)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO entity (id, name, entity_type_id, jurisdiction_id) \
         VALUES ($1, 'Synthetic entity', $2, $3)",
    )
    .bind(entity)
    .bind(entity_type)
    .bind(jurisdiction)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO project (id, code, name, status, entity_id) \
         VALUES ($1, $2, 'Synthetic project', 'open', $3)",
    )
    .bind(project)
    .bind(format!("upload-{project}"))
    .bind(entity)
    .execute(&pg.0)
    .await
    .unwrap();
    project
}

pub(crate) fn args(project_id: Uuid) -> UploadSessionArgs<'static> {
    UploadSessionArgs {
        project_id,
        filename: "exhibit.mp4",
        kind: "agreement",
        content_type: "video/mp4",
        visibility: visibility::INTERNAL,
        description: None,
        slug: None,
        transcript_quality: None,
        metadata: Some(serde_json::json!({"source": "synthetic"})),
        derived_from: None,
        signing: None,
        notarization: None,
        folder: Some("Discovery"),
        declared_size: 10,
        declared_sha256: "0000000000000000000000000000000000000000000000000000000000000000",
    }
}

fn session(project_id: Uuid) -> UploadSession {
    let now = Utc::now();
    UploadSession {
        id: Uuid::now_v7(),
        project_id,
        filename: "exhibit.mp4".into(),
        kind: "agreement".into(),
        content_type: "video/mp4".into(),
        visibility: visibility::INTERNAL.into(),
        description: None,
        slug: None,
        transcript_quality: None,
        metadata: Some(serde_json::json!({"source": "synthetic"})),
        derived_from: None,
        signing: None,
        notarization: None,
        folder: Some("Discovery".into()),
        declared_size: 10,
        declared_sha256: args(project_id).declared_sha256.into(),
        chunk_size: 8,
        received_bytes: 0,
        created_at: now,
        expires_at: now + chrono::Duration::hours(UPLOAD_SESSION_TTL_HOURS),
    }
}

#[tokio::test]
async fn a_session_resumes_advances_once_per_offset_and_is_removed() {
    let Some(pg) = database().await else { return };
    let project_id = project(&pg).await;
    let created = session(project_id);
    upload_sessions::create(&pg, &created).await.unwrap();

    let found = upload_sessions::find(&pg, project_id, created.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.id, created.id);
    assert_eq!(found.folder.as_deref(), Some("Discovery"));
    assert_eq!(found.metadata, created.metadata);
    assert_eq!(
        upload_sessions::find(&pg, Uuid::now_v7(), created.id)
            .await
            .unwrap(),
        None,
        "another matter cannot read the session"
    );
    let resumable = upload_sessions::find_resumable(&pg, &args(project_id), Utc::now())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resumable.id, created.id);
    assert_eq!(
        upload_sessions::find_resumable(
            &pg,
            &args(project_id),
            created.expires_at + chrono::Duration::seconds(1)
        )
        .await
        .unwrap(),
        None,
        "an expired session is not resumed"
    );

    assert!(upload_sessions::advance(&pg, created.id, 0, 8)
        .await
        .unwrap());
    assert!(
        !upload_sessions::advance(&pg, created.id, 0, 8)
            .await
            .unwrap(),
        "a stale offset cannot move the resume point again"
    );
    let advanced = upload_sessions::find(&pg, project_id, created.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(advanced.received_bytes, 8);

    upload_sessions::remove(&pg, created.id).await.unwrap();
    assert_eq!(
        upload_sessions::find(&pg, project_id, created.id)
            .await
            .unwrap(),
        None
    );
    pg.0.close().await;
}

#[tokio::test]
async fn surreal_sessions_copy_once_with_their_resume_point() {
    let Some(pg) = database().await else { return };
    let surreal = crate::test_support::mem_surreal().await;
    let project_id = project(&pg).await;
    let opened = create_upload_session(&surreal, &args(project_id))
        .await
        .unwrap();

    let copied = upload_sessions::copy_from_surreal(&surreal, &pg)
        .await
        .unwrap();
    assert_eq!((copied.applied, copied.failures.len()), (1, 0));
    assert_eq!(
        upload_sessions::copy_from_surreal(&surreal, &pg)
            .await
            .unwrap()
            .applied,
        0
    );
    let carried = upload_sessions::find(&pg, project_id, opened.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(carried.folder.as_deref(), Some("Discovery"));
    assert_eq!(carried.declared_sha256, opened.declared_sha256);
    assert_eq!(carried.received_bytes, 0);
    pg.0.close().await;
}
