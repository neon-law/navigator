use archives::coverage::{CoverageManifest, Relation, RelationKind, Rule};
use cloud::StorageService;

fn table(schema: &str, name: &str) -> Relation {
    Relation {
        schema: schema.into(),
        name: name.into(),
        kind: RelationKind::Table,
        accessible: true,
    }
}

#[test]
fn coverage_detects_added_dropped_and_inaccessible_relations() {
    let expected = table("dbos", "workflow_status");
    let manifest = CoverageManifest {
        relations: vec![Rule::Export(expected.clone())],
    };
    assert!(manifest.verify(std::slice::from_ref(&expected)).is_ok());
    assert!(manifest.verify(&[]).is_err());
    assert!(manifest
        .verify(&[expected.clone(), table("graph", "edge")])
        .is_err());
    let mut inaccessible = expected;
    inaccessible.accessible = false;
    assert!(manifest.verify(&[inaccessible]).is_err());
}

#[test]
fn only_explicit_derived_views_can_be_excluded() {
    let mut view = table("public", "projection");
    view.kind = RelationKind::View;
    let manifest = CoverageManifest {
        relations: vec![Rule::Derived {
            relation: view.clone(),
            reason: "rebuild from stored events".into(),
        }],
    };
    assert!(manifest.verify(std::slice::from_ref(&view)).is_ok());
    view.kind = RelationKind::Table;
    assert!(manifest.verify(&[view]).is_err());
    let manifest = CoverageManifest {
        relations: vec![Rule::Derived {
            relation: table("public", "audit"),
            reason: "derived".into(),
        }],
    };
    assert!(manifest.verify(&[table("public", "audit")]).is_err());
}

#[tokio::test]
async fn configured_postgres_refuses_missing_paired_inventory() {
    let dir = tempfile::tempdir().unwrap();
    let storage = cloud::FsStorage::new(dir.path()).await.unwrap();
    let result = archives::postgres::export_configured(
        |key| (key == "NAVIGATOR_POSTGRES_HOST").then(|| "127.0.0.1".into()),
        &storage,
        None,
    )
    .await;
    assert!(matches!(
        result,
        Some(archives::postgres::PostgresExport::Failed { .. })
    ));
    assert!(storage.list("").await.unwrap().is_empty());
}

#[tokio::test]
async fn paired_configuration_requires_the_navigator_connection() {
    let dir = tempfile::tempdir().unwrap();
    let storage = cloud::FsStorage::new(dir.path()).await.unwrap();
    let result = archives::postgres::export_configured(
        |key| (key == "NAVIGATOR_ARCHIVE_FORGEJO_USER").then(|| "synthetic".into()),
        &storage,
        None,
    )
    .await;
    assert!(matches!(
        result,
        Some(archives::postgres::PostgresExport::Failed { .. })
    ));
}
