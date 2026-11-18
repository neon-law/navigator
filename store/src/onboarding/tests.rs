use super::*;
use crate::postgres_mirror::tests::notation_fixture;

#[tokio::test]
async fn only_completed_docusign_uuids_make_postgres_projects_official() {
    let Some((pg, project, notation)) = notation_fixture().await else {
        return;
    };
    let disconnected = crate::surreal::SurrealDb::uninitialized();
    PostgresDb::with_test_db(pg.clone(), async {
        let envelope = Uuid::now_v7().to_string();
        let provider = crate::signatures::SignatureProvider::DocuSign;
        let (first, second) = tokio::join!(
            crate::signatures::record_request(&disconnected, notation, provider, &envelope),
            crate::signatures::record_request(&disconnected, notation, provider, &envelope),
        );
        let signatures = first.unwrap();
        assert_eq!(signatures.id, second.unwrap().id);
        let (client, firm) = tokio::join!(
            crate::signatures::record_signer_completion(
                &disconnected,
                provider,
                &envelope,
                "client"
            ),
            crate::signatures::record_signer_completion(&disconnected, provider, &envelope, "firm"),
        );
        assert!(client.unwrap() && firm.unwrap());
        let rows = crate::signatures::for_notation(&disconnected, notation)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].signer_completed("client") && rows[0].signer_completed("firm"));
        assert_eq!(
            signatures.state,
            crate::signatures::SignatureState::Requested
        );
        assert!(official_projects(&pg, &[project]).await.unwrap().is_empty());
        crate::signatures::stamp_signed(&disconnected, provider, &envelope, "2026-11-17T00:00:00Z")
            .await
            .unwrap();
        assert!(!crate::signatures::record_signer_completion(
            &disconnected,
            provider,
            &envelope,
            "late"
        )
        .await
        .unwrap());
        assert_eq!(
            official_projects(&pg, &[project]).await.unwrap(),
            HashSet::from([project])
        );

        sqlx::query("UPDATE signature SET provider_id='invalid-envelope' WHERE id=$1")
            .bind(signatures.id)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(official_projects(&pg, &[project]).await.unwrap().is_empty());

        sqlx::query("UPDATE signature SET provider_id=$2,state='declined' WHERE id=$1")
            .bind(signatures.id)
            .bind(&envelope)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(official_projects(&pg, &[project]).await.unwrap().is_empty());
        sqlx::query("UPDATE signature SET state='completed' WHERE id=$1")
            .bind(signatures.id)
            .execute(pg.pool())
            .await
            .unwrap();
        sqlx::query(
        "UPDATE template SET kind='letter' WHERE id=(SELECT template_id FROM notation WHERE id=$1)",
    )
    .bind(notation)
    .execute(pg.pool())
    .await
    .unwrap();
        assert!(official_projects(&pg, &[project]).await.unwrap().is_empty());
        assert!(official_projects(&pg, &[Uuid::now_v7()])
            .await
            .unwrap()
            .is_empty());
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn terminal_signatures_cannot_be_completed_or_reclassified() {
    let Some((pg, _, notation)) = notation_fixture().await else {
        return;
    };
    let disconnected = crate::surreal::SurrealDb::uninitialized();
    let provider = crate::signatures::SignatureProvider::DocuSign;
    PostgresDb::with_test_db(pg.clone(), async {
        let declined = Uuid::now_v7().to_string();
        crate::signatures::record_request(&disconnected, notation, provider, &declined)
            .await
            .unwrap();
        assert!(
            crate::signatures::stamp_declined(&disconnected, provider, &declined)
                .await
                .unwrap()
        );
        assert!(!crate::signatures::stamp_signed(
            &disconnected,
            provider,
            &declined,
            "2026-11-17T00:00:00Z",
        )
        .await
        .unwrap());
        assert!(
            !crate::signatures::stamp_voided(&disconnected, provider, &declined)
                .await
                .unwrap()
        );
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one revision chain covers evidence upgrade, retry, and stale-head rejection"
)]
async fn postgres_uploads_require_verified_evidence_and_retry_without_duplicate_revisions() {
    let Some((pg, project, _)) = notation_fixture().await else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageService> = Arc::new(
        cloud::FsStorage::new(root.path().to_path_buf())
            .await
            .unwrap(),
    );
    let args = IngestArgs {
        project_id: project,
        source: "upload",
        filename: "onboarding.pdf",
        kind: "onboarding",
        content_type: "application/pdf",
        description: None,
        secondary_storage_key: None,
        visibility: "client",
    };
    let mut identity = DocumentIdentity {
        slug: Some("onboarding.pdf"),
        ..Default::default()
    };
    file_document(&pg, &storage, &args, &identity, None, b"signed")
        .await
        .unwrap();
    assert!(official_projects(&pg, &[project]).await.unwrap().is_empty());
    let envelope = Uuid::now_v7();
    identity.metadata = Some(serde_json::json!({"docusign_envelope_id": envelope}));
    file_document(&pg, &storage, &args, &identity, None, b"manual-pointer")
        .await
        .unwrap();
    assert!(official_projects(&pg, &[project]).await.unwrap().is_empty());
    identity.metadata = Some(serde_json::json!({
        "docusign_envelope_id": envelope, "verified_onboarding_envelope_id": envelope,
    }));
    let head = revisions(&pg, project, "onboarding.pdf").await.unwrap();
    let expected = ExpectedHead {
        asset_id: head[0].id,
        version: u32::try_from(head.len()).unwrap(),
        sha256: head[0].sha256_hex.clone(),
    };
    file_document(
        &pg,
        &storage,
        &args,
        &identity,
        Some(&expected),
        b"manual-pointer",
    )
    .await
    .unwrap();
    assert_eq!(
        official_projects(&pg, &[project]).await.unwrap(),
        HashSet::from([project])
    );
    assert!(matches!(
        file_document(
            &pg,
            &storage,
            &args,
            &identity,
            Some(&expected),
            b"manual-pointer"
        )
        .await
        .unwrap(),
        Filed::Unchanged { .. }
    ));
    assert_eq!(
        revisions(&pg, project, "onboarding.pdf")
            .await
            .unwrap()
            .len(),
        3
    );
    assert!(matches!(
        file_document(
            &pg,
            &storage,
            &args,
            &identity,
            Some(&expected),
            b"different"
        )
        .await,
        Err(RevisionError::HeadConflict { .. })
    ));
    let rows = documents(&pg, project).await.unwrap();
    assert_eq!(
        rows[0].metadata.as_ref().unwrap()["docusign_envelope_id"],
        envelope.to_string()
    );
    assert_eq!(rows[1].visibility, "internal");
    assert!(filed_documents(&pg, &[project])
        .await
        .unwrap()
        .0
        .contains(&project));
    // Reconciliation cannot erase an authoritative onboarding row when the old engine has no row.
    let disconnected = crate::surreal::SurrealDb::uninitialized();
    crate::postgres_mirror::mirror(
        &disconnected,
        &pg,
        &crate::postgres_mirror::MirrorKey::Asset(rows[0].id),
    )
    .await
    .unwrap();
    assert_eq!(documents(&pg, project).await.unwrap().len(), 3);
    pg.pool().close().await;
}

#[tokio::test]
async fn configured_signature_writes_never_fall_back_to_another_engine() {
    let Some((pg, _, notation)) = notation_fixture().await else {
        return;
    };
    let disconnected = crate::surreal::SurrealDb::uninitialized();
    let provider = crate::signatures::SignatureProvider::DocuSign;
    let envelope = Uuid::now_v7().to_string();
    PostgresDb::with_test_db(pg.clone(), async {
        let request =
            crate::signatures::record_request(&disconnected, notation, provider, &envelope)
                .await
                .unwrap();
        assert!(crate::signatures::stamp_signed(
            &disconnected,
            provider,
            &envelope,
            "2026-11-17T00:00:00Z",
        )
        .await
        .unwrap());
        let completed = crate::signatures::completed_for_notation(&disconnected, notation)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request.id, completed.id);
    })
    .await;
    pg.pool().close().await;
    let result = PostgresDb::with_test_db(
        pg,
        crate::signatures::record_request(&disconnected, notation, provider, &envelope),
    )
    .await;
    assert!(matches!(
        result,
        Err(crate::signatures::SignatureError::Postgres(
            crate::postgres::signatures::SignatureFamilyError::Query(sqlx::Error::PoolClosed)
        ))
    ));
}
