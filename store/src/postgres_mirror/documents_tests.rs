use std::sync::Arc;

use cloud::{FsStorage, StorageService};

use super::*;
use crate::documents::{
    DocumentIdentity, IngestArgs, Notarization, NotarizationMethod, Signing, SigningMethod,
};
use crate::postgres_mirror::tests::{connect, fresh_schema, mirror_identity};
use crate::test_support::mem_surreal;

const SLUG: &str = "synthetic-engagement";

async fn mirrored(surreal: &SurrealDb, pg: &PostgresDb, key: &MirrorKey) -> Mirrored {
    super::super::during_initial_copy(super::super::mirror(surreal, pg, key))
        .await
        .unwrap()
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

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one frozen import including normalization and retained native drift"
)]
async fn frozen_asset_import_preserves_json_presence_and_refuses_native_drift_repair() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let (storage, _tmp) = storage().await;
    let project = matter(&surreal, &pg).await;
    let mirrored_asset = file(&surreal, &storage, project, b"synthetic mirrored").await;
    surreal
        .query("UPDATE $id SET metadata=NULL, derived_from=NONE, evidence={nested:NULL}")
        .bind(("id", record_id("asset", mirrored_asset)))
        .await
        .unwrap()
        .check()
        .unwrap();
    let raw = AssetImage::from_source(
        source_asset(&surreal, mirrored_asset)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(raw.metadata, Some(serde_json::Value::Null));
    assert_eq!(raw.derived_from, None);
    assert_eq!(raw.evidence, Some(serde_json::json!({"nested":null})));
    mirrored(&surreal, &pg, &MirrorKey::Asset(mirrored_asset)).await;
    let held = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{} WHERE id=$1",
        ASSET_SELECT.as_str()
    )))
    .bind(mirrored_asset)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    assert_eq!(AssetImage::from_row(&held).unwrap(), raw);

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

    let repaired = super::super::during_initial_copy(reconcile(&surreal, &pg))
        .await
        .unwrap();
    assert_eq!(
        repaired.tables[0].divergent,
        vec![MirrorKey::Asset(mirrored_asset), MirrorKey::Asset(stale)]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    );
    assert_eq!(repaired.tables[0].target_rows, 3);
    let replay = super::super::during_initial_copy(reconcile(&surreal, &pg))
        .await
        .unwrap();
    assert_eq!(replay, repaired);
    // Importing Source-only metadata cannot establish trusted provider ownership.
    let forged = crate::assets::ingest_content(&surreal, &storage, b"forged marker", "text/plain")
        .await
        .unwrap();
    let envelope = Uuid::new_v4();
    surreal.query("UPDATE $id SET kind='onboarding', project_id=$project, metadata=$metadata")
        .bind(("id",record_id("asset",forged))).bind(("project",record_id("project",project)))
        .bind(("metadata",serde_json::json!({"docusign_envelope_id":envelope,"verified_onboarding_envelope_id":envelope})))
        .await.unwrap().check().unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &MirrorKey::Asset(forged)).await,
        Mirrored::Written
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "{} WHERE id=$1",
        ASSET_SELECT.as_str()
    )))
    .bind(forged)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    let held = AssetImage::from_row(&row).unwrap();
    assert!(held
        .metadata
        .as_ref()
        .unwrap()
        .get(crate::assets::VERIFIED_ONBOARDING_ENVELOPE_KEY)
        .is_none());
    assert!(!native_onboarding(&pg, &held).await.unwrap());
    let source =
        AssetImage::from_source(source_asset(&surreal, forged).await.unwrap().unwrap()).unwrap();
    let (parity, proof) = compare_union(
        &pg,
        BTreeMap::from([(forged, source)]),
        BTreeMap::from([(forged, held)]),
    )
    .await
    .unwrap();
    assert!(parity.divergent.is_empty());
    assert_eq!(proof.resolved_native_overlaps, 0);
    assert_eq!(proof.proven_native_target_only, 0);
}
