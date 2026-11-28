//! LAW-586 against PostgreSQL and object storage: a rendered onboarding
//! letter files as a dated PDF, a completed envelope files one canonical
//! signed copy and certificate however often it is delivered, and a missing
//! signed copy is flagged.
use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use crate::signature::CompletedDocuments;

async fn onboarding_matter() -> (
    store::application_store::ApplicationStore,
    Arc<dyn cloud::StorageService>,
    Uuid,
    Uuid,
    tempfile::TempDir,
) {
    let db = store::application_store::ApplicationStore::uninitialized();
    let notation = store::test_support::seed_notation(&db).await;
    let project = store::notations::find_by_id(&db, notation)
        .await
        .unwrap()
        .unwrap()
        .project_id;
    let root = tempfile::tempdir().unwrap();
    let storage: Arc<dyn cloud::StorageService> =
        Arc::new(cloud::FsStorage::new(root.path()).await.unwrap());
    (db, storage, notation, project, root)
}

fn names(assets: &[store::assets::Asset]) -> Vec<String> {
    let mut names = assets
        .iter()
        .filter_map(|asset| asset.slug.clone().or_else(|| asset.filename.clone()))
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[tokio::test]
async fn a_rendered_onboarding_letter_files_as_one_dated_pdf() {
    store::test_support::with_postgres(async {
        let (db, storage, notation, project, _root) = onboarding_matter().await;
        let payload = workflows::DocumentPayload::Typst {
            storage_key: store::notations::document_pdf_storage_key(notation),
            typst_source: "Synthetic engagement letter for review".into(),
        };
        for _ in 0..2 {
            workflows::dispatch_generate_pdf(&storage, Some(&db), notation, &payload)
                .await
                .unwrap();
        }
        let today = Utc::now().date_naive().format("%Y.%m.%d");
        let assets = store::assets::for_project(&db, project).await.unwrap();
        assert_eq!(names(&assets), [format!("{today}_engagement-letter.pdf")]);
        assert!(assets
            .iter()
            .all(|asset| asset.content_type == "application/pdf"));
    })
    .await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One completion, its re-deliveries, and the status read.
async fn a_completed_envelope_files_one_canonical_copy_and_a_missing_copy_is_flagged() {
    store::test_support::with_postgres(async {
        let (db, storage, notation, project, _root) = onboarding_matter().await;
        let envelope = Uuid::now_v7().to_string();
        let provider = store::signatures::SignatureProvider::DocuSign;
        store::signatures::record_request(&db, notation, provider, &envelope)
            .await
            .unwrap();
        let signed_on = chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let docs = || CompletedDocuments {
            signed_pdf: pdf::render("Synthetic signed engagement letter").unwrap(),
            certificate_pdf: pdf::render("Synthetic certificate of completion").unwrap(),
        };
        // The webhook, a re-delivery, and an adoption of the same envelope.
        for suffix in [None, None, Some(envelope.as_str())] {
            super::file_completed_documents(
                &db, &storage, &envelope, notation, signed_on, suffix, docs(),
            )
            .await
            .unwrap();
        }
        let filed = store::assets::for_project(&db, project).await.unwrap();
        assert_eq!(
            names(&filed),
            [
                "2026.11.02_engagement-letter_certificate.pdf",
                "2026.11.02_engagement-letter_signed.pdf"
            ]
        );
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        let before = store::onboarding::evidence(&pg, project).await.unwrap();
        assert!(!before.official, "not official until the envelope completes");
        store::signatures::stamp_signed(&db, provider, &envelope, "2026-11-02T18:00:00Z")
            .await
            .unwrap();
        let evidence = store::onboarding::evidence(&pg, project).await.unwrap();
        assert!(evidence.official);
        assert_eq!(
            evidence.signed_pdf.as_deref(),
            Some("2026.11.02_engagement-letter_signed.pdf")
        );
        assert!(evidence.missing.is_empty(), "{:?}", evidence.missing);

        // A legacy-named copy of the same bytes is the duplicate the check names.
        let signed = filed
            .iter()
            .find(|asset| asset.slug.as_deref() == Some("2026.11.02_engagement-letter_signed.pdf"))
            .unwrap();
        sqlx::query(
            "INSERT INTO asset (storage_key,content_type,byte_size,sha256_hex,project_id,filename,kind,source,slug)
             SELECT storage_key,content_type,byte_size,sha256_hex,project_id,'document.pdf',kind,'upload','document.pdf'
             FROM asset WHERE id=$1",
        )
        .bind(signed.id)
        .execute(pg.pool())
        .await
        .unwrap();
        let duplicated = store::onboarding::evidence(&pg, project).await.unwrap();
        assert!(
            duplicated
                .missing
                .iter()
                .any(|message| message.contains("more than once") && message.contains("document.pdf")),
            "{:?}",
            duplicated.missing
        );

        sqlx::query("UPDATE asset SET deleted_at=now() WHERE project_id=$1 AND kind='onboarding' AND (slug LIKE '%signed.pdf' OR slug='document.pdf')")
            .bind(project)
            .execute(pg.pool())
            .await
            .unwrap();
        let flagged = store::onboarding::evidence(&pg, project).await.unwrap();
        assert!(flagged.official);
        assert_eq!(flagged.signed_pdf, None);
        assert_eq!(flagged.missing.len(), 1, "{:?}", flagged.missing);
        assert!(flagged.missing[0].contains("no signed engagement letter PDF"));
    })
    .await;
}
