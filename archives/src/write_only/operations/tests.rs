use super::*;

fn config(root: &Path) -> Config {
    Config {
        format_version: 1,
        object_store: None,
        local_root: root.to_owned(),
        source_project_number: None,
        source_identity_sha256: "ab".repeat(32),
        capture: CaptureScope::RegisteredNavigator,
        destinations: vec![Destination {
            project_number: 11,
            bucket: "example-backup-one".into(),
            location: "us-west4".into(),
            minimum_retention_seconds: cloud::archives_project::OFFSITE_ARCHIVES_RETENTION_SECONDS,
        }],
    }
}

#[test]
fn config_cannot_retarget_a_journaled_request_or_lower_existing_locked_retention() {
    let mut request = Request {
        capture_id: None,
        delivery_attempt: 0,
        config: config(Path::new("/synthetic-private-archive")),
        config_sha256: String::new(),
        reuse_local: None,
    };
    request.config_sha256 = request.config.digest().unwrap();
    request.validate().unwrap();
    request.config.destinations[0].bucket = "different-example-bucket".into();
    assert!(request.validate().is_err());
    request.config.destinations[0].minimum_retention_seconds = 1;
    assert!(request.config.validate().is_err());
    let config = config(Path::new("/synthetic-private-archive"));
    let plan = provisioning_plan(
        &config,
        "serviceAccount:example-writer@example.iam.gserviceaccount.com",
        "serviceAccount:example-verifier@example.iam.gserviceaccount.com",
    )
    .unwrap();
    assert_eq!(
        plan["destinations"][0]["writer_binding"]["role"],
        "roles/storage.objectCreator"
    );
    assert_eq!(
        plan["destinations"][0]["bucket"]["iamConfiguration"]["uniformBucketLevelAccess"]
            ["enabled"],
        true
    );
    assert_eq!(plan["destinations"][0]["automatic_apply"], false);
}

#[test]
fn verifier_refuses_wrong_project_unlocked_short_retention_and_nonuniform_access() {
    let destination = &config(Path::new("/synthetic-private-archive")).destinations[0];
    let policy = serde_json::json!({"projectNumber":"11","location":"US-WEST4","iamConfiguration":{
        "uniformBucketLevelAccess":{"enabled":true},"publicAccessPrevention":"enforced"},
        "retentionPolicy":{"isLocked":true,"retentionPeriod":destination.minimum_retention_seconds.to_string()}});
    verify::policy(&policy, destination).unwrap();
    for (pointer, value) in [
        ("/projectNumber", serde_json::json!("12")),
        (
            "/iamConfiguration/uniformBucketLevelAccess/enabled",
            serde_json::json!(false),
        ),
        ("/retentionPolicy/isLocked", serde_json::json!(false)),
        ("/retentionPolicy/retentionPeriod", serde_json::json!("1")),
    ] {
        let mut changed = policy.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(verify::policy(&changed, destination).is_err());
    }
}

#[tokio::test]
async fn closed_source_reuses_only_sealed_local_rows_and_never_recaptures_missing_or_changed_copy()
{
    store::test_support::with_postgres(async {
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        let storage = tempfile::tempdir().unwrap();
        let root = storage.path().canonicalize().unwrap();
        let mut config = config(&root);
        config.source_identity_sha256 = source_identity(pg.pool()).await.unwrap();
        let receipt = prepare(Some(&pg), &config, None).await.unwrap();
        pg.pool().close().await;
        assert_eq!(
            prepare(None, &config, Some(&receipt))
                .await
                .unwrap()
                .inventory_sha256,
            receipt.inventory_sha256
        );
        let prepared = prepare_destination(&receipt, &config, 0).await.unwrap();
        // A used marker refuses before GCS construction, with no ADC/network requirement.
        let marker = prepared
            .root
            .join(format!("{}.started", prepared.inventory_key));
        tokio::fs::write(marker, []).await.unwrap();
        assert!(matches!(
            upload(&prepared, &config, 0, &receipt.inventory_sha256)
                .await
                .unwrap(),
            DestinationOutcome::Unconfirmed {
                acknowledged: 0,
                ..
            }
        ));
        let count = std::fs::read_dir(root.join("attempts")).unwrap().count();
        tokio::fs::write(root.join(&receipt.inventory_key), b"synthetic tampering")
            .await
            .unwrap();
        assert!(prepare(Some(&pg), &config, Some(&receipt)).await.is_err());
        assert_eq!(
            std::fs::read_dir(root.join("attempts")).unwrap().count(),
            count
        );
        tokio::fs::remove_file(root.join(&receipt.inventory_key))
            .await
            .unwrap();
        assert!(prepare(Some(&pg), &config, Some(&receipt)).await.is_err());
        assert_eq!(
            std::fs::read_dir(root.join("attempts")).unwrap().count(),
            count
        );
    })
    .await;
}
