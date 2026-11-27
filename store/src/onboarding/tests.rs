use super::*;
use crate::postgres::fixtures::{connect, fresh_schema};

#[tokio::test]
async fn adoption_links_an_existing_upload_and_reuses_the_filing_on_retry() {
    let Some((pg, project, _)) = native_notation_fixture().await else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let storage: Arc<dyn StorageService> =
        Arc::new(cloud::FsStorage::new(root.path()).await.unwrap());
    let upload = IngestArgs {
        project_id: project,
        source: "upload",
        filename: "signed.pdf",
        kind: "onboarding",
        content_type: "application/pdf",
        description: None,
        secondary_storage_key: None,
        visibility: "internal",
    };
    let identity = DocumentIdentity::default();
    file_document(&pg, &storage, &upload, &identity, None, b"signed")
        .await
        .unwrap();
    let signed_copy = DocumentIdentity {
        signing: Some(Signing {
            method: SigningMethod::DropboxSign,
            signed_on: chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
        }),
        ..Default::default()
    };
    assert!(matches!(
        file_document(&pg, &storage, &upload, &signed_copy, None, b"signed")
            .await
            .unwrap(),
        Filed::Revision(_)
    ));
    assert!(matches!(
        file_document(&pg, &storage, &upload, &signed_copy, None, b"signed")
            .await
            .unwrap(),
        Filed::Unchanged { .. }
    ));
    assert_eq!(
        filed_signing_methods(&pg, &[project]).await.unwrap()[&project],
        BTreeSet::from([SigningMethod::DropboxSign])
    );
    assert!(official_projects(&pg, &[project]).await.unwrap().is_empty());
    let provider = IngestArgs {
        source: "docusign",
        ..upload
    };
    let envelope = Uuid::new_v4();
    let tagged = DocumentIdentity {
        metadata: Some(serde_json::json!({"docusign_envelope_id": envelope})),
        ..identity
    };
    let recorded = file_document(&pg, &storage, &provider, &tagged, None, b"signed")
        .await
        .unwrap();
    assert!(matches!(recorded, Filed::Revision(_)));
    let replay = file_document(&pg, &storage, &provider, &tagged, None, b"signed")
        .await
        .unwrap();
    assert!(matches!(replay, Filed::Unchanged { .. }));
    let revisions = revisions(&pg, project, upload.filename).await.unwrap();
    assert_eq!(revisions.len(), 3);
    assert_eq!(revisions[0].storage_key, revisions[1].storage_key);
    assert_eq!(
        crate::document_pointers::envelope_id_from_asset_metadata(revisions[0].metadata.as_ref()),
        Some(envelope)
    );
    assert!(official_projects(&pg, &[project]).await.unwrap().is_empty());
    pg.pool().close().await;
}

async fn native_notation_fixture() -> Option<(PostgresDb, Uuid, Uuid)> {
    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    crate::postgres::fixtures::activate_application(&pg).await;
    let (_, notation, _) = crate::postgres::fixtures::workflow_fixture(&pg).await;
    let project =
        sqlx::query_scalar("UPDATE notation SET state='END' WHERE id=$1 RETURNING project_id")
            .bind(notation)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    sqlx::query("UPDATE template SET kind='onboarding' WHERE id=(SELECT template_id FROM notation WHERE id=$1)")
        .bind(notation).execute(pg.pool()).await.unwrap();
    Some((pg, project, notation))
}

#[tokio::test]
async fn only_completed_docusign_uuids_make_postgres_projects_official() {
    let Some((pg, project, notation)) = native_notation_fixture().await else {
        return;
    };
    let disconnected = crate::application_store::ApplicationStore::uninitialized();
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
    let Some((pg, _, notation)) = native_notation_fixture().await else {
        return;
    };
    let disconnected = crate::application_store::ApplicationStore::uninitialized();
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
async fn configured_signature_writes_never_fall_back_to_another_engine() {
    let Some((pg, _, notation)) = native_notation_fixture().await else {
        return;
    };
    let disconnected = crate::application_store::ApplicationStore::uninitialized();
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
            crate::postgres::signatures::SignatureFamilyError::Unavailable
        ))
    ));
}

#[tokio::test]
async fn recorded_retainer_methods_use_scoped_onboarding_evidence() {
    let Some((pg, project, notation)) = native_notation_fixture().await else {
        return;
    };
    let signatures = crate::signatures::SignatureProvider::ALL;
    let ids = [Uuid::new_v4().to_string(), "synthetic-request".to_string()];
    for (provider, id) in signatures.iter().zip(&ids) {
        sqlx::query(
            "INSERT INTO signature(id,notation_id,provider,provider_id,state,inserted_at,updated_at)
                     VALUES ($1,$2,$3,$4,'requested',now(),now())",
        )
        .bind(Uuid::now_v7())
        .bind(notation)
        .bind(provider.as_str())
        .bind(id)
        .execute(pg.pool())
        .await
        .unwrap();
    }
    assert!(verified_providers(&pg, &[project])
        .await
        .unwrap()
        .is_empty());
    sqlx::query("UPDATE signature SET state='completed',signed_at=now() WHERE notation_id=$1")
        .bind(notation)
        .execute(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        verified_providers(&pg, &[project]).await.unwrap()[&project].len(),
        2
    );
    let mut asset_ids = Vec::new();
    for (kind, method) in [
        ("onboarding", "wet_ink"),
        ("onboarding", "dropbox_sign"),
        ("letter", "docusign"),
    ] {
        let id = Uuid::now_v7();
        asset_ids.push(id);
        sqlx::query("INSERT INTO asset (id,project_id,kind,storage_key,content_type,byte_size,sha256_hex,signing)
                     VALUES ($1,$2,$3,$4,'application/pdf',1,'synthetic',$5)")
            .bind(id).bind(project).bind(kind).bind(id.to_string())
            .bind(serde_json::json!({"method":method,"signed_on":"2026-10-01"}))
            .execute(pg.pool()).await.unwrap();
    }
    let methods = filed_signing_methods(&pg, &[project]).await.unwrap();
    assert_eq!(methods[&project].len(), 2);
    assert!(methods[&project].contains(&crate::documents::SigningMethod::WetInk));
    assert!(methods[&project].contains(&crate::documents::SigningMethod::DropboxSign));
    sqlx::query("UPDATE asset SET deleted_at=now() WHERE id=$1")
        .bind(asset_ids[0])
        .execute(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        filed_signing_methods(&pg, &[project]).await.unwrap()[&project].len(),
        1
    );
    // An unsigned replacement is now the head of the same filing.
    sqlx::query("UPDATE asset SET slug='agreement.pdf' WHERE id=$1")
        .bind(asset_ids[1])
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO asset (id,project_id,kind,slug,storage_key,content_type,byte_size,sha256_hex)
                 VALUES ($1,$2,'onboarding','agreement.pdf','replacement','application/pdf',1,'synthetic')")
        .bind(Uuid::now_v7()).bind(project).execute(pg.pool()).await.unwrap();
    assert!(filed_signing_methods(&pg, &[project])
        .await
        .unwrap()
        .is_empty());
    // Older generated documents may carry only the Onboarding Notation's storage key.
    sqlx::query("UPDATE asset SET secondary_storage_key=$1 WHERE id=$2")
        .bind(crate::notations::signed_document_storage_key(notation))
        .bind(asset_ids[2])
        .execute(pg.pool())
        .await
        .unwrap();
    assert_eq!(
        filed_signing_methods(&pg, &[project]).await.unwrap()[&project],
        BTreeSet::from([SigningMethod::Docusign])
    );
    assert!(filed_signing_methods(&pg, &[Uuid::now_v7()])
        .await
        .unwrap()
        .is_empty());
    pg.pool().close().await;
}
