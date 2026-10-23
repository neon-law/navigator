//! Closing out a Project: acknowledgments bound to manifests, and every
//! blocker in front of final archival and repository deletion.
use std::sync::Arc;

use chrono::{Duration, Utc};
use cloud::{FsStorage, StorageService};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::assets::{self, Filed};
use store::documents::{visibility, DocumentIdentity, IngestArgs};
use store::persons::{self, NewPerson, Role};
use store::postgres::PostgresDb;
use store::project_closeout::{self as closeout, AcknowledgmentError, Closeout};
use store::projects::{self, ClosureReason, NewProject, Transition};
use store::test_support::{mem_surreal, seed_entity, seed_firm_with_brand, MemoryStorage};
use uuid::Uuid;

async fn database() -> Option<PostgresDb> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
            Ok("1"),
            "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
        );
        return None;
    };
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = PgPool::connect_with(options.clone()).await.unwrap();
    let name = format!("closeout_test_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = PostgresDb::connect(options.database(&name)).await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
}

async fn participant(
    surreal: &store::surreal::SurrealDb,
    project_id: Uuid,
    name: &str,
    role: Role,
) -> Uuid {
    let person = persons::create(
        surreal,
        &NewPerson::with_role(
            name,
            format!("{}@example.com", name.replace(' ', ".")),
            role,
        ),
    )
    .await
    .unwrap();
    store::participation::add_participant(
        surreal,
        &store::participation::AddParticipantCommand {
            project_id,
            person_id: person.id,
            dri: store::participation::DriRequest::Unchanged,
            actor: store::participation::DriActor::System,
        },
    )
    .await
    .unwrap();
    person.id
}

fn blocked_by(readiness: &closeout::Readiness, text: &str) -> bool {
    readiness
        .blockers
        .iter()
        .any(|blocker| blocker.contains(text))
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn closeout_waits_on_recovery_acknowledgment_holds_and_the_live_head() {
    let Some(pg) = database().await else {
        return;
    };
    let surreal = mem_surreal().await;
    let firm_id = seed_firm_with_brand(&surreal, "neon").await;
    let project = projects::create(
        &surreal,
        &NewProject {
            code: format!("closeout-{}", &Uuid::now_v7().simple().to_string()[..8]),
            name: "Synthetic Closeout Matter".into(),
            status: "open".into(),
            entity_id: seed_entity(&surreal).await,
            firm_id: Some(firm_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let lawyer = participant(&surreal, project.id, "Closeout Lawyer", Role::Lawyer).await;
    let client = participant(&surreal, project.id, "Closeout Client", Role::Client).await;
    let dir = tempfile::tempdir().unwrap();
    let documents: Arc<dyn StorageService> = Arc::new(FsStorage::new(dir.path()).await.unwrap());
    let args = IngestArgs {
        project_id: project.id,
        source: "upload",
        filename: "engagement.pdf",
        kind: "onboarding",
        content_type: "application/pdf",
        description: None,
        visibility: visibility::CLIENT,
        secondary_storage_key: None,
    };
    let identity = DocumentIdentity {
        slug: Some("judgment"),
        ..DocumentIdentity::default()
    };
    let Filed::Revision(judgment) =
        assets::file_revision(&surreal, &documents, &args, &identity, b"judgment")
            .await
            .unwrap()
    else {
        panic!("first revision");
    };

    // Open, unarchived, no handoff: every blocker at once.
    let open = closeout::readiness(&surreal, Some(&pg), project.id, Closeout::Archive)
        .await
        .unwrap();
    assert!(blocked_by(&open, "not closed"));
    assert!(blocked_by(&open, "no committed recovery generation"));
    // Without the archive store, nothing is assumed safe.
    let unavailable = closeout::readiness(&surreal, None, project.id, Closeout::Archive)
        .await
        .unwrap();
    assert!(blocked_by(
        &unavailable,
        "recovery archive store is unavailable"
    ));

    // A required handoff, then the close.
    let handoff = store::project_handoffs::prepare(
        &surreal,
        &pg,
        &documents,
        None,
        project.id,
        (lawyer, Role::Lawyer),
        client,
        true,
        Utc::now(),
    )
    .await
    .unwrap();
    projects::transition_project_with_reason(
        &surreal,
        project.id,
        Transition::Close,
        Some(ClosureReason::EngagementCompleted),
        None,
    )
    .await
    .unwrap();
    let owner = persons::create(
        &surreal,
        &NewPerson::with_role("Closeout Owner", "closeout.owner@example.com", Role::Owner),
    )
    .await
    .unwrap();
    let authority = store::project_archives::authorize(&surreal, Role::Owner, owner.id, firm_id)
        .await
        .unwrap();
    let archive: Arc<dyn StorageService> = Arc::new(MemoryStorage::default());
    let stores = store::project_archives::ArchiveStores {
        surreal: &surreal,
        pg: &pg,
        documents: &documents,
        archive: &archive,
        source_backups: None,
    };
    let first = store::project_archives::request(
        &stores,
        &authority,
        project.id,
        "close-1",
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    store::project_archives::run(&stores, &authority, first.id, Utc::now())
        .await
        .unwrap();
    let pending = closeout::readiness(&surreal, Some(&pg), project.id, Closeout::Archive)
        .await
        .unwrap();
    assert_eq!(
        pending.blockers,
        vec![format!(
            "required handoff {} is not acknowledged",
            handoff.id
        )]
    );

    // Acknowledgment: only the recipient, only the exact manifest, idempotent.
    assert!(matches!(
        closeout::acknowledge(
            &pg,
            project.id,
            handoff.id,
            lawyer,
            &handoff.manifest_sha256,
            Utc::now()
        )
        .await,
        Err(AcknowledgmentError::NotRecipient)
    ));
    assert!(matches!(
        closeout::acknowledge(
            &pg,
            project.id,
            handoff.id,
            client,
            &"0".repeat(64),
            Utc::now()
        )
        .await,
        Err(AcknowledgmentError::ManifestMismatch)
    ));
    let acknowledged = closeout::acknowledge(
        &pg,
        project.id,
        handoff.id,
        client,
        &handoff.manifest_sha256,
        Utc::now(),
    )
    .await
    .unwrap();
    let again = closeout::acknowledge(
        &pg,
        project.id,
        handoff.id,
        client,
        &handoff.manifest_sha256,
        Utc::now() + Duration::minutes(5),
    )
    .await
    .unwrap();
    assert_eq!(again, acknowledged, "a retried acknowledgment converges");
    assert!(
        closeout::readiness(&surreal, Some(&pg), project.id, Closeout::Archive)
            .await
            .unwrap()
            .is_ready()
    );

    // A document changed after the generation reopens the gap.
    assets::set_legal_hold(&surreal, project.id, judgment.asset_id, true, Utc::now())
        .await
        .unwrap();
    let changed = closeout::readiness(&surreal, Some(&pg), project.id, Closeout::Archive)
        .await
        .unwrap();
    assert!(blocked_by(&changed, "documents changed after"));
    let second = store::project_archives::request(
        &stores,
        &authority,
        project.id,
        "close-2",
        None,
        Utc::now(),
    )
    .await
    .unwrap();
    store::project_archives::run(&stores, &authority, second.id, Utc::now())
        .await
        .unwrap();
    assert!(
        closeout::readiness(&surreal, Some(&pg), project.id, Closeout::Archive)
            .await
            .unwrap()
            .is_ready()
    );

    // Deleting a repository also needs the live HEAD in a verified bundle and
    // no Legal Hold.
    let delete = closeout::readiness(
        &surreal,
        Some(&pg),
        project.id,
        Closeout::DeleteRepository {
            live_head: &"a".repeat(40),
        },
    )
    .await
    .unwrap();
    assert!(blocked_by(&delete, "Legal Hold"));
    assert!(blocked_by(&delete, "does not hold the live HEAD"));
}
