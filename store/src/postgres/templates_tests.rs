//! PostgreSQL-only Template version and isolation boundaries.
use crate::postgres::fixtures::{activate_application, firm_for_notation, workflow_fixture};
use crate::postgres::fixtures::{connect, fresh_schema};
use crate::postgres::history::Family;
use crate::postgres::PostgresDb;
use crate::template_source::TemplateSourceError;
use crate::templates::{self, Scope, Version};
use uuid::Uuid;

async fn database() -> Option<PostgresDb> {
    let (options, name) = fresh_schema().await?;
    let pg = connect(&options, &name).await;
    activate_application(&pg).await;
    Some(pg)
}

fn version(title: &str, asset_id: Option<Uuid>) -> Version {
    Version {
        title: title.into(),
        respondent_type: "person".into(),
        asset_id,
        form_code: None,
        kind: Some(" letter ".into()),
        source_commit_sha: Some("first-commit".into()),
    }
}

async fn storage() -> (tempfile::TempDir, std::sync::Arc<dyn cloud::StorageService>) {
    let dir = tempfile::tempdir().unwrap();
    let storage = std::sync::Arc::new(cloud::FsStorage::new(dir.path()).await.unwrap());
    (dir, storage)
}

async fn project_for_notation(pg: &PostgresDb, notation: Uuid) -> Uuid {
    sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
        .bind(notation)
        .fetch_one(pg.pool())
        .await
        .unwrap()
}

async fn assert_scope_title(
    source: &crate::application_store::ApplicationStore,
    project: Option<Uuid>,
    title: &str,
) {
    assert_eq!(
        templates::resolve(source, project, "scoped")
            .await
            .unwrap()
            .unwrap()
            .title,
        title
    );
}

#[tokio::test]
async fn template_versions_keep_the_first_provenance_and_pinned_notation() {
    let Some(pg) = database().await else {
        return;
    };
    let (person, notation, _) = workflow_fixture(&pg).await;
    let project = project_for_notation(&pg, notation).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    let (_dir, storage) = storage().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let asset =
            crate::assets::ingest_content(&unavailable, &storage, b"original", "text/markdown")
                .await
                .unwrap();
        let mut spec = version("Original", Some(asset));
        spec.form_code = Some("synthetic-form".into());
        let first = templates::save_version(&unavailable, Some(project), "native-version", spec)
            .await
            .unwrap()
            .into_model();
        let pinned = crate::notations::create(
            &unavailable,
            &crate::notations::NewNotation::new(first.id, person, project, "BEGIN"),
        )
        .await
        .unwrap();
        let mut identical = version("Original", Some(asset));
        identical.form_code = Some("synthetic-form".into());
        identical.source_commit_sha = Some("later-commit".into());
        let same =
            templates::save_version(&unavailable, Some(project), "native-version", identical)
                .await
                .unwrap();
        assert!(!same.was_written());
        assert_eq!(same.into_model(), first);
        let next_asset =
            crate::assets::ingest_content(&unavailable, &storage, b"revised", "text/markdown")
                .await
                .unwrap();
        let mut changed = version("Revised", Some(next_asset));
        changed.source_commit_sha = Some("second-commit".into());
        let current =
            templates::save_version(&unavailable, Some(project), "native-version", changed)
                .await
                .unwrap()
                .into_model();
        assert_ne!(first.id, current.id);
        assert_eq!(current.form_code, None);
        assert_eq!(current.kind.as_deref(), Some("letter"));
        let retired = templates::find_by_id(&unavailable, first.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!retired.is_current);
        assert_eq!(retired.source_commit_sha, first.source_commit_sha);
        assert_eq!(retired.body_sha256, first.body_sha256);
        assert_eq!(retired.asset_id, first.asset_id);
        let unchanged_pin = crate::notations::find_by_id(&unavailable, pinned.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unchanged_pin.template_id, first.id);
        assert_eq!(unchanged_pin.template_body_sha256, first.body_sha256);
        assert_eq!(
            unchanged_pin.template_source_commit_sha,
            first.source_commit_sha
        );
        assert_eq!(
            templates::versions_of(&unavailable, Some(project), "native-version")
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            templates::find_by_ids(&unavailable, &[first.id, current.id])
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(templates::find_by_ids(&unavailable, &[])
            .await
            .unwrap()
            .is_empty());
        assert!(templates::exists_for_project(&unavailable, project)
            .await
            .unwrap());
        assert_eq!(
            templates::body(&unavailable, &storage, &retired)
                .await
                .unwrap(),
            "original"
        );
    })
    .await;
    pg.pool().close().await;
}

async fn scope_fixture(pg: &PostgresDb) -> (Uuid, Uuid, Uuid, Uuid) {
    let (_, notation, _) = workflow_fixture(pg).await;
    let (firm, project) = firm_for_notation(pg, notation).await;
    let other = Uuid::new_v4();
    sqlx::query("INSERT INTO firm(id,name,status,slug) VALUES($1,'Synthetic','active',$2)")
        .bind(other)
        .bind(other.to_string())
        .execute(pg.pool())
        .await
        .unwrap();
    let (_, unowned_note, _) = workflow_fixture(pg).await;
    let unowned: Uuid = sqlx::query_scalar("SELECT project_id FROM notation WHERE id=$1")
        .bind(unowned_note)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    (firm, project, other, unowned)
}

#[tokio::test]
async fn template_scope_resolution_keeps_other_firms_private() {
    let Some(pg) = database().await else {
        return;
    };
    let (firm, project, other, unowned) = scope_fixture(&pg).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    let (_dir, storage) = storage().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let asset =
            crate::assets::ingest_content(&unavailable, &storage, b"scoped", "text/markdown")
                .await
                .unwrap();
        for (scope, title) in [
            (Scope::Shared, "Shared"),
            (Scope::Firm(firm), "Own firm"),
            (Scope::Firm(other), "Other firm"),
        ] {
            templates::save_version_in(&unavailable, scope, "scoped", version(title, Some(asset)))
                .await
                .unwrap();
        }
        assert_scope_title(&unavailable, None, "Shared").await;
        assert_scope_title(&unavailable, Some(project), "Own firm").await;
        assert_eq!(
            templates::list_for_firm(&unavailable, other).await.unwrap()[0].title,
            "Other firm"
        );
        templates::save_version(
            &unavailable,
            Some(project),
            "scoped",
            version("Own project", Some(asset)),
        )
        .await
        .unwrap();
        assert_scope_title(&unavailable, Some(project), "Own project").await;
        assert_eq!(
            templates::list_for_project(&unavailable, project)
                .await
                .unwrap()[0]
                .title,
            "Own project"
        );
        assert_scope_title(&unavailable, Some(unowned), "Shared").await;
        sqlx::query("UPDATE project SET firm_id=$1 WHERE id=$2")
            .bind(other)
            .bind(unowned)
            .execute(pg.pool())
            .await
            .unwrap();
        assert_scope_title(&unavailable, Some(unowned), "Other firm").await;
        assert!(templates::list_current(&unavailable)
            .await
            .unwrap()
            .iter()
            .all(|row| row.firm_id.is_none()));
        assert!(
            templates::resolve_in(&unavailable, Scope::Firm(Uuid::new_v4()), "scoped")
                .await
                .unwrap()
                .is_none()
        );
        assert!(!templates::exists_for_project(&unavailable, Uuid::new_v4())
            .await
            .unwrap());
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn concurrent_identical_template_versions_share_one_current_row() {
    let Some(pg) = database().await else {
        return;
    };
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    PostgresDb::with_test_db(pg.clone(), async {
        let (left, right) = tokio::join!(
            templates::save_version(&unavailable, None, "concurrent", version("Same", None)),
            templates::save_version(&unavailable, None, "concurrent", version("Same", None)),
        );
        let (left, right) = (left.unwrap(), right.unwrap());
        assert_ne!(left.was_written(), right.was_written());
        assert_eq!(left.into_model().id, right.into_model().id);
        assert_eq!(
            templates::versions_of(&unavailable, None, "concurrent")
                .await
                .unwrap()
                .len(),
            1
        );
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn a_failed_template_insert_preserves_the_current_version() {
    let Some(pg) = database().await else {
        return;
    };
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    PostgresDb::with_test_db(pg.clone(), async {
        let first = templates::save_version(&unavailable, None, "atomic", version("Original", None)).await.unwrap().into_model();
        sqlx::raw_sql("CREATE FUNCTION reject_template_insert() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic rejected insert'; END $$; CREATE TRIGGER reject_template_insert BEFORE INSERT ON template FOR EACH ROW EXECUTE FUNCTION reject_template_insert();")
            .execute(pg.pool()).await.unwrap();
        assert!(templates::save_version(&unavailable, None, "atomic", version("Rejected", None)).await.is_err());
        assert_eq!(templates::resolve_exact(&unavailable, None, "atomic").await.unwrap().unwrap().id, first.id);
        assert!(templates::find_by_id(&unavailable, first.id).await.unwrap().unwrap().is_current);
        assert_eq!(templates::versions_of(&unavailable, None, "atomic").await.unwrap().len(), 1);
    }).await;
    pg.pool().close().await;
}

#[tokio::test]
async fn template_history_admission_precedes_content_and_question_writes() {
    let Some(pg) = database().await else {
        return;
    };
    let (_, notation, _) = workflow_fixture(&pg).await;
    let project = project_for_notation(&pg, notation).await;
    sqlx::query(
        "UPDATE mirror_backfill_run SET status='running', completed_at=NULL WHERE scope=$1",
    )
    .bind(Family::for_table("template").unwrap().scope())
    .execute(pg.pool())
    .await
    .unwrap();
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    let (dir, storage) = storage().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let error = crate::template_source::persist_contents(
            &unavailable,
            &storage,
            project,
            "onboarding__letter",
            include_str!("../../../templates/notations/neon_law/onboarding__letter.md").into(),
            "commit".into(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("PostgreSQL"), "{error}");
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM asset")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(count, 0);
        let questions: i64 = sqlx::query_scalar("SELECT count(*) FROM question")
            .fetch_one(pg.pool())
            .await
            .unwrap();
        assert_eq!(
            questions, 1,
            "only the pre-existing fixture Question remains"
        );
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn template_source_persists_and_reads_a_pinned_version_without_source() {
    let Some(pg) = database().await else {
        return;
    };
    let (_, notation, _) = workflow_fixture(&pg).await;
    let project = project_for_notation(&pg, notation).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    let (_dir, storage) = storage().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let body = include_str!("../../../templates/notations/neon_law/onboarding__letter.md");
        let saved = crate::template_source::persist_contents(
            &unavailable,
            &storage,
            project,
            "onboarding__letter",
            body.into(),
            "commit".into(),
        )
        .await
        .unwrap();
        assert!(saved.written);
        let repeated = crate::template_source::persist_contents(
            &unavailable,
            &storage,
            project,
            "onboarding__letter",
            body.into(),
            "later".into(),
        )
        .await
        .unwrap();
        assert!(!repeated.written);
        assert_eq!(repeated.template, saved.template);
        assert_eq!(
            templates::body(&unavailable, &storage, &saved.template)
                .await
                .unwrap(),
            rules::clauses::compile(body).unwrap()
        );
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn every_changed_spec_field_appends_and_whitespace_kind_is_normalized() {
    let Some(pg) = database().await else {
        return;
    };
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    let (_dir, storage) = storage().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let asset =
            crate::assets::ingest_content(&unavailable, &storage, b"new body", "text/markdown")
                .await
                .unwrap();
        for field in ["title", "respondent", "asset", "form", "kind"] {
            let initial =
                templates::save_version(&unavailable, None, field, version("Original", None))
                    .await
                    .unwrap()
                    .into_model();
            let mut spec = version("Original", None);
            match field {
                "title" => spec.title = "Different".into(),
                "respondent" => spec.respondent_type = "entity".into(),
                "asset" => spec.asset_id = Some(asset),
                "form" => spec.form_code = Some("synthetic-form".into()),
                _ => spec.kind = Some("memo".into()),
            }
            let changed = templates::save_version(&unavailable, None, field, spec)
                .await
                .unwrap();
            assert!(changed.was_written(), "{field}");
            assert_ne!(changed.into_model().id, initial.id);
            assert_eq!(
                templates::versions_of(&unavailable, None, field)
                    .await
                    .unwrap()
                    .len(),
                2
            );
        }
        let mut blank = version("Blank", None);
        blank.kind = Some("   ".into());
        let saved = templates::save_version(&unavailable, None, "blank", blank)
            .await
            .unwrap()
            .into_model();
        assert_eq!(saved.kind, None);
        assert!(templates::find_by_id(&unavailable, Uuid::new_v4())
            .await
            .unwrap()
            .is_none());
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn body_checks_missing_assets_content_and_notation_pins() {
    let Some(pg) = database().await else {
        return;
    };
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    let (_dir, storage) = storage().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let empty = templates::save_version(&unavailable, None, "empty", version("Empty", None))
            .await
            .unwrap()
            .into_model();
        assert!(matches!(
            templates::body(&unavailable, &storage, &empty).await,
            Err(templates::TemplateBodyError::MissingBody(_))
        ));
        let dangling = templates::Template {
            asset_id: Some(Uuid::new_v4()),
            ..empty
        };
        assert!(matches!(
            templates::body(&unavailable, &storage, &dangling).await,
            Err(templates::TemplateBodyError::DanglingAsset(_))
        ));
        let asset_id =
            crate::assets::ingest_content(&unavailable, &storage, b"original", "text/markdown")
                .await
                .unwrap();
        let mut shared = version("Body", Some(asset_id));
        shared.source_commit_sha = None;
        let row = templates::save_version(&unavailable, None, "body", shared)
            .await
            .unwrap()
            .into_model();
        assert_eq!(row.source_commit_sha, None);
        let asset = crate::assets::find_by_id(&unavailable, asset_id)
            .await
            .unwrap()
            .unwrap();
        storage
            .put(&asset.storage_key, b"forged", "text/markdown")
            .await
            .unwrap();
        assert!(matches!(
            templates::body(&unavailable, &storage, &row).await,
            Err(templates::TemplateBodyError::DigestMismatch(_))
        ));
        storage
            .put(&asset.storage_key, b"original", "text/markdown")
            .await
            .unwrap();
        let wrong_pin = crate::assets::sha256_hex(b"other");
        assert!(matches!(
            templates::body_pinned_to(&unavailable, &storage, &row, Some(&wrong_pin)).await,
            Err(templates::TemplateBodyError::DigestMismatch(_))
        ));
        assert_eq!(
            templates::body(&unavailable, &storage, &row).await.unwrap(),
            "original"
        );
        let invalid =
            crate::assets::ingest_content(&unavailable, &storage, &[0xff], "text/markdown")
                .await
                .unwrap();
        let invalid = templates::save_version(
            &unavailable,
            None,
            "invalid-utf8",
            version("Body", Some(invalid)),
        )
        .await
        .unwrap()
        .into_model();
        assert!(matches!(
            templates::body(&unavailable, &storage, &invalid).await,
            Err(templates::TemplateBodyError::NotUtf8)
        ));
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn an_unpinned_version_is_retained_when_a_pinned_version_is_appended() {
    let Some(pg) = database().await else {
        return;
    };
    let (_, notation, _) = workflow_fixture(&pg).await;
    let project = project_for_notation(&pg, notation).await;
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    let (_dir, storage) = storage().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let asset = crate::assets::ingest_content(&unavailable, &storage, b"body", "text/markdown")
            .await
            .unwrap();
        let first = templates::save_version(
            &unavailable,
            Some(project),
            "upgrade",
            version("Same", Some(asset)),
        )
        .await
        .unwrap()
        .into_model();
        sqlx::query("UPDATE template SET source_commit_sha=NULL,body_sha256=NULL WHERE id=$1")
            .bind(first.id)
            .execute(pg.pool())
            .await
            .unwrap();
        let next = templates::save_version(
            &unavailable,
            Some(project),
            "upgrade",
            version("Same", Some(asset)),
        )
        .await
        .unwrap();
        assert!(next.was_written());
        let next = next.into_model();
        assert_ne!(next.id, first.id);
        assert!(next.body_sha256.is_some());
        let retained = templates::find_by_id(&unavailable, first.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!retained.is_current);
        assert_eq!(retained.body_sha256, None);
        assert_eq!(retained.source_commit_sha, None);
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn different_concurrent_versions_keep_one_current_and_all_saved_versions() {
    let Some(pg) = database().await else {
        return;
    };
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    PostgresDb::with_test_db(pg.clone(), async {
        let (a, b, c) = tokio::join!(
            templates::save_version(&unavailable, None, "different", version("A", None)),
            templates::save_version(&unavailable, None, "different", version("B", None)),
            templates::save_version(&unavailable, None, "different", version("C", None)),
        );
        for result in [a, b, c] {
            assert!(result.unwrap().was_written());
        }
        let versions = templates::versions_of(&unavailable, None, "different")
            .await
            .unwrap();
        assert_eq!(versions.len(), 3);
        assert_eq!(versions.iter().filter(|row| row.is_current).count(), 1);
    })
    .await;
    pg.pool().close().await;
}

async fn repository_project(pg: &PostgresDb) -> crate::projects::Project {
    let (_, notation, _) = workflow_fixture(pg).await;
    let entity: Uuid = sqlx::query_scalar(
        "SELECT entity_id FROM project WHERE id=(SELECT project_id FROM notation WHERE id=$1)",
    )
    .bind(notation)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO brand(name,brand_key) VALUES('Synthetic Template Brand','synthetic-template')",
    )
    .execute(pg.pool())
    .await
    .unwrap();
    crate::postgres::projects::create_on(
        pg,
        &crate::projects::NewProject {
            code: format!("template-repository-{}", Uuid::new_v4()),
            name: "Synthetic Template Project".into(),
            status: "open".into(),
            brand: "synthetic-template".into(),
            entity_id: entity,
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

fn repo_with(project: &str, files: &[(&str, &[u8])]) -> (repos::RepoStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let repo = repos::RepoStore::new(dir.path());
    repo.ensure_code(project).unwrap();
    if !files.is_empty() {
        repo.commit_as_code(
            project,
            repos::Author {
                name: "Synthetic",
                email: "synthetic@example.invalid",
            },
            "Synthetic Draft",
            files,
        )
        .unwrap();
    }
    (repo, dir)
}

#[tokio::test]
async fn repository_blueprint_aliases_keep_the_same_version_and_first_commit() {
    let Some(pg) = database().await else {
        return;
    };
    let project = repository_project(&pg).await;
    let (project, code) = (project.id, project.code);
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    let (_dir, storage) = storage().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let body = include_str!("../../../templates/notations/neon_law/onboarding__letter.md");
        let mut first = None;
        for root in ["templates", "drafts"] {
            let path = format!("{root}/onboarding__letter.md");
            let (repo, _dir) = repo_with(&code, &[(&path, body.as_bytes())]);
            let saved = crate::template_source::persist_from_repo(
                &unavailable,
                &storage,
                &repo,
                project,
                "onboarding__letter",
            )
            .await
            .unwrap();
            if let Some(first) = &first {
                assert!(!saved.written);
                assert_eq!(&saved.template, first);
            } else {
                assert!(saved.written);
                assert_eq!(
                    saved.template.source_commit_sha,
                    repo.head_oid_code(&code).unwrap()
                );
                first = Some(saved.template);
            }
        }
    })
    .await;
    pg.pool().close().await;
}

#[tokio::test]
async fn repository_errors_and_invalid_drafts_refuse_persistence() {
    let Some(pg) = database().await else {
        return;
    };
    let project = repository_project(&pg).await;
    let (project, code) = (project.id, project.code);
    let unavailable = crate::application_store::ApplicationStore::uninitialized();
    let (_dir, storage) = storage().await;
    PostgresDb::with_test_db(pg.clone(), async {
        let body =
            include_str!("../../../templates/notations/neon_law/onboarding__letter.md").as_bytes();
        for (expected, files) in [
            ("empty", vec![]),
            ("missing", vec![("templates/other.md", body)]),
            (
                "ambiguous",
                vec![
                    ("templates/onboarding__letter.md", body),
                    ("drafts/onboarding__letter.md", body),
                ],
            ),
            ("layout", vec![("templates/nested/wrong.md", body)]),
        ] {
            let (repo, _dir) = repo_with(&code, &files);
            let error = crate::template_source::persist_from_repo(
                &unavailable,
                &storage,
                &repo,
                project,
                "onboarding__letter",
            )
            .await
            .unwrap_err();
            assert!(
                match (expected, error) {
                    ("empty", TemplateSourceError::RepoEmpty(id)) => id == project,
                    ("missing", TemplateSourceError::TemplateNotFound { code }) =>
                        code == "onboarding__letter",
                    ("ambiguous", TemplateSourceError::AmbiguousBlueprintDirectories)
                    | ("layout", TemplateSourceError::InvalidLayout(_)) => true,
                    _ => false,
                },
                "{expected}"
            );
        }
        for root in ["templates", "drafts"] {
            for bad in [
                "",
                "---\nnot: [valid\n---\nBody",
                "---\ncode: wrong\ntitle: Draft\n---\nBody",
            ] {
                let path = format!("{root}/onboarding__letter.md");
                let (repo, _dir) = repo_with(&code, &[(&path, bad.as_bytes())]);
                assert!(matches!(
                    crate::template_source::persist_from_repo(
                        &unavailable,
                        &storage,
                        &repo,
                        project,
                        "onboarding__letter"
                    )
                    .await,
                    Err(crate::template_source::TemplateSourceError::Invalid { .. })
                ));
            }
        }
        assert!(templates::list_for_project(&unavailable, project)
            .await
            .unwrap()
            .is_empty());
    })
    .await;
    pg.pool().close().await;
}
