use std::sync::Arc;

use cloud::{FsStorage, StorageService};

use super::*;
use crate::documents::{
    DocumentIdentity, IngestArgs, Notarization, NotarizationMethod, Signing, SigningMethod,
};
use crate::persons::{NewPerson, Role};
use crate::postgres_mirror::tests::{connect, fresh_schema, mirror_identity};
use crate::test_support::mem_surreal;

const SLUG: &str = "synthetic-engagement";

async fn mirrored(surreal: &SurrealDb, pg: &PostgresDb, key: &MirrorKey) -> Mirrored {
    super::super::mirror(surreal, pg, key).await.unwrap()
}

/// A Project whose Entity PostgreSQL already holds, so the asset mirror can
/// bring the Project itself.
async fn matter(surreal: &SurrealDb, pg: &PostgresDb) -> Uuid {
    let project = crate::test_support::seed_project_surreal(surreal, "Synthetic Matter").await;
    let entity = crate::projects::find_by_id(surreal, project)
        .await
        .unwrap()
        .unwrap()
        .entity_id;
    mirror_identity(pg, &[entity], &[]).await;
    project
}

async fn lawyer(surreal: &SurrealDb) -> Uuid {
    crate::persons::create(
        surreal,
        &NewPerson::with_role(
            "Synthetic Lawyer",
            format!("documents-{}@example.com", Uuid::now_v7()),
            Role::Lawyer,
        ),
    )
    .await
    .unwrap()
    .id
}

async fn storage() -> (Arc<dyn StorageService>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let storage = FsStorage::new(tmp.path().to_path_buf()).await.unwrap();
    (Arc::new(storage), tmp)
}

async fn file(
    surreal: &SurrealDb,
    storage: &Arc<dyn StorageService>,
    project_id: Uuid,
    bytes: &[u8],
) -> Uuid {
    let args = IngestArgs {
        project_id,
        source: "upload",
        filename: "engagement.pdf",
        kind: "unclassified",
        content_type: "application/pdf",
        description: Some("Synthetic engagement letter"),
        visibility: crate::documents::visibility::INTERNAL,
        secondary_storage_key: None,
    };
    let identity = DocumentIdentity {
        slug: Some(SLUG),
        published_at: Some("2026-01-02T00:00:00Z"),
        metadata: Some(serde_json::json!({"entry": {"sequence": 3}})),
        signing: Some(Signing {
            method: SigningMethod::WetInk,
            signed_on: "2026-01-02".parse().unwrap(),
        }),
        notarization: vec![Notarization {
            method: NotarizationMethod::InPerson,
            acknowledged_on: "2026-01-02".parse().unwrap(),
            state: "NV".into(),
            county: "Synthetic".into(),
            commission_expires: "2030-01-01".parse().unwrap(),
        }],
        ..DocumentIdentity::default()
    };
    match crate::assets::file_revision(surreal, storage, &args, &identity, bytes)
        .await
        .unwrap()
    {
        crate::assets::Filed::Revision(doc) => doc.asset_id,
        crate::assets::Filed::Unchanged { asset_id } => asset_id,
    }
}

#[test]
fn asset_keys_name_rows_without_values() {
    let id = Uuid::nil();
    assert_eq!(MirrorKey::Asset(id).table(), "asset");
    assert_eq!(
        MirrorKey::Asset(id).to_string(),
        "asset:00000000-0000-0000-0000-000000000000"
    );
}

#[test]
fn the_asset_upsert_copies_every_retention_stamp() {
    let sql = upsert_sql("asset", ASSET_COLUMNS);
    for column in [
        "notarization",
        "deleted_at",
        "purge_after",
        "deleted_by_person_id",
        "deleted_commit_sha",
        "legal_hold_at",
        "purged_at",
        "sha256_hex",
        "visibility",
    ] {
        assert!(
            sql.contains(&format!("{column} = EXCLUDED.{column}")),
            "{column}"
        );
    }
    assert!(sql.contains("IS DISTINCT FROM"));
}

/// The soft-deletion stamps one asset row carries in PostgreSQL.
type Stamps = (Option<Uuid>, Option<DateTime<Utc>>, Option<DateTime<Utc>>);

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one document identity's lifecycle reads in order"
)]
async fn a_revision_chain_converges_through_retention_and_removal() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let (storage, _tmp) = storage().await;
    let project = matter(&surreal, &pg).await;
    let retiring_lawyer = lawyer(&surreal).await;

    // Two revisions of one document identity, newest id first in both.
    let first = file(&surreal, &storage, project, b"synthetic v1").await;
    let first_key = MirrorKey::Asset(first);
    assert_eq!(mirrored(&surreal, &pg, &first_key).await, Mirrored::Written);
    assert_eq!(
        mirrored(&surreal, &pg, &first_key).await,
        Mirrored::Unchanged
    );
    let second = file(&surreal, &storage, project, b"synthetic v2").await;
    assert_eq!(
        mirrored(&surreal, &pg, &MirrorKey::Asset(second)).await,
        Mirrored::Written
    );
    let chain: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM asset WHERE project_id = $1 AND slug = $2 ORDER BY id DESC",
    )
    .bind(project)
    .bind(SLUG)
    .fetch_all(&pg.0)
    .await
    .unwrap();
    let source: Vec<Uuid> = crate::assets::revisions(&surreal, project, SLUG)
        .await
        .unwrap()
        .iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(chain, source);
    assert_eq!(chain, [second, first]);

    // Checksum, signing, and notarization are copied as stored.
    let (sha, notarization): (String, Jsonb<Json>) =
        sqlx::query_as("SELECT sha256_hex, notarization FROM asset WHERE id = $1")
            .bind(first)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(sha, crate::assets::sha256_hex(b"synthetic v1"));
    assert_eq!(notarization.0[0]["state"], "NV");

    // Visibility.
    crate::assets::set_visibility(&surreal, project, second, "client")
        .await
        .unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &MirrorKey::Asset(second)).await,
        Mirrored::Written
    );

    // Soft deletion stamps the whole chain and names the deleting Person,
    // whom the mirror brings with it.
    let now = Utc::now();
    crate::assets::soft_delete(
        &surreal,
        project,
        second,
        retiring_lawyer,
        Some("abc123"),
        now,
    )
    .await
    .unwrap();
    crate::assets::set_legal_hold(&surreal, project, second, true, now)
        .await
        .unwrap();
    for id in [first, second] {
        assert_eq!(
            mirrored(&surreal, &pg, &MirrorKey::Asset(id)).await,
            Mirrored::Written
        );
    }
    let stamped: Vec<Stamps> = sqlx::query_as(
        "SELECT deleted_by_person_id, purge_after, legal_hold_at FROM asset \
             WHERE project_id = $1 ORDER BY id",
    )
    .bind(project)
    .fetch_all(&pg.0)
    .await
    .unwrap();
    let retention = now + chrono::Duration::days(crate::assets::RETENTION_DAYS);
    for (person, purge_after, hold) in stamped {
        assert_eq!(person, Some(retiring_lawyer));
        assert_eq!(purge_after, Some(micros(retention)));
        assert_eq!(hold, Some(micros(now)));
    }
    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");

    // Restore clears the stamps; the purge tombstone is recorded.
    crate::assets::restore(&surreal, project, second)
        .await
        .unwrap();
    crate::assets::mark_purged(&surreal, &[first], now)
        .await
        .unwrap();
    for id in [first, second] {
        assert_eq!(
            mirrored(&surreal, &pg, &MirrorKey::Asset(id)).await,
            Mirrored::Written
        );
    }
    let (deleted, purged): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT deleted_at, purged_at FROM asset WHERE id = $1")
            .bind(first)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!((deleted, purged), (None, Some(micros(now))));

    // A bare content asset has no Project.
    let bare = crate::assets::ingest_content(&surreal, &storage, b"# Synthetic", "text/markdown")
        .await
        .unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &MirrorKey::Asset(bare)).await,
        Mirrored::Written
    );
    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.tables[0].target_rows, 3);

    // Removal.
    crate::assets::delete(&surreal, bare).await.unwrap();
    let bare_key = MirrorKey::Asset(bare);
    assert_eq!(mirrored(&surreal, &pg, &bare_key).await, Mirrored::Deleted);
    assert_eq!(
        mirrored(&surreal, &pg, &bare_key).await,
        Mirrored::Unchanged
    );
    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.tables[0].target_rows, 2);
}

#[tokio::test]
async fn parity_names_divergent_assets_and_reconcile_repairs_them() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let (storage, _tmp) = storage().await;
    let project = matter(&surreal, &pg).await;
    let mirrored_asset = file(&surreal, &storage, project, b"synthetic mirrored").await;
    mirrored(&surreal, &pg, &MirrorKey::Asset(mirrored_asset)).await;
    let missing = crate::assets::ingest_content(&surreal, &storage, b"synthetic", "text/plain")
        .await
        .unwrap();

    // PostgreSQL holds a row SurrealDB does not, and one whose visibility
    // drifted.
    let stale = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO asset (id, storage_key, content_type, byte_size, sha256_hex) \
         VALUES ($1, 'blobs/stale', 'text/plain', 1, 'ab')",
    )
    .bind(stale)
    .execute(&pg.0)
    .await
    .unwrap();
    sqlx::query("UPDATE asset SET visibility = 'client' WHERE id = $1")
        .bind(mirrored_asset)
        .execute(&pg.0)
        .await
        .unwrap();

    let report = parity(&surreal, &pg).await.unwrap();
    let mut expected = vec![
        MirrorKey::Asset(mirrored_asset),
        MirrorKey::Asset(missing),
        MirrorKey::Asset(stale),
    ];
    expected.sort();
    assert_eq!(report.tables[0].divergent, expected);
    // The report carries keys and counts, never a filename or storage key.
    assert!(!format!("{report:?}").contains("engagement.pdf"));

    let repaired = reconcile(&surreal, &pg).await.unwrap();
    assert!(repaired.is_clean(), "{repaired:?}");
    assert_eq!(repaired.tables[0].target_rows, 2);
    let replay = reconcile(&surreal, &pg).await.unwrap();
    assert_eq!(replay, repaired);
}
