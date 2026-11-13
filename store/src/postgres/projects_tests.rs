use super::PostgresDb;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::projects::{NewProject, Project};
use uuid::Uuid;

struct Fixture {
    pg: PostgresDb,
    input: NewProject,
}

async fn fixture() -> Option<Fixture> {
    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    let jurisdiction = Uuid::new_v4();
    let entity_type = Uuid::new_v4();
    let entity = Uuid::new_v4();
    let firm = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO jurisdiction (id,name,code) VALUES ($1,'Synthetic Jurisdiction','TST')",
    )
    .bind(jurisdiction)
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic Entity')")
        .bind(entity_type)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Entity',$2,$3)")
        .bind(entity).bind(entity_type).bind(jurisdiction).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO firm (id,name,slug,status,entity_id) VALUES ($1,'Synthetic Firm','synthetic-firm','active',$2)")
        .bind(firm).bind(entity).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO brand (name,brand_key) VALUES ('Synthetic Brand','synthetic-brand')")
        .execute(pg.pool())
        .await
        .unwrap();
    Some(Fixture {
        pg,
        input: NewProject {
            code: "synthetic-project".into(),
            name: "Synthetic Project".into(),
            status: "open".into(),
            brand: "synthetic-brand".into(),
            entity_id: entity,
            firm_id: Some(firm),
            jurisdiction_id: Some(jurisdiction),
            description: Some("Synthetic description".into()),
        },
    })
}

#[tokio::test]
async fn project_code_is_immutable_in_postgres_even_for_direct_sql() {
    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO project (id,code,name,status,brand,entity_id) VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(id)
    .bind(&input.code)
    .bind(&input.name)
    .bind(&input.status)
    .bind(&input.brand)
    .bind(input.entity_id)
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE project SET code=code WHERE id=$1")
        .bind(id)
        .execute(pg.pool())
        .await
        .unwrap();
    let result = sqlx::query("UPDATE project SET code='renamed-project' WHERE id=$1")
        .bind(id)
        .execute(pg.pool())
        .await;
    assert!(
        result.is_err(),
        "PostgreSQL must reject Project code changes structurally"
    );
    let code: String = sqlx::query_scalar("SELECT code FROM project WHERE id=$1")
        .bind(id)
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(code, input.code);
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one native Project lifecycle proves uniqueness, reseed preservation and database constraints"
)]
async fn native_project_records_preserve_identity_and_optional_resources() {
    use super::projects as records;
    use records::ProjectError;
    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let created = records::create_on(&pg, &input).await.unwrap();
    assert_eq!(created.id.get_version_num(), 4);
    assert_eq!(
        records::find_by_id_on(&pg, created.id).await.unwrap(),
        Some(created.clone())
    );
    assert_eq!(
        records::find_by_code_on(&pg, &input.code).await.unwrap(),
        Some(created.clone())
    );
    assert_eq!(
        records::find_by_name_on(&pg, &input.name).await.unwrap(),
        Some(created.clone())
    );
    assert_eq!(records::all_on(&pg).await.unwrap(), vec![created.clone()]);
    assert!(records::find_by_id_on(&pg, Uuid::new_v4())
        .await
        .unwrap()
        .is_none());
    assert!(records::find_by_code_on(&pg, "absent-code")
        .await
        .unwrap()
        .is_none());
    assert!(records::find_by_name_on(&pg, "Absent Project")
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        records::create_on(&pg, &input).await,
        Err(ProjectError::CodeTaken)
    ));

    sqlx::query("UPDATE project SET drive_folder_id='synthetic-folder',repository_url='https://forge.example/synthetic/project',git_initialized_at='synthetic-initialization',forge_provisioned_at='synthetic-provisioning',closed_at='synthetic-closure',closure_reason='completed',internal_slack_channel_url='https://slack.example/synthetic/internal',external_slack_channel_url='https://slack.example/synthetic/external',internal_slack_channel_id='synthetic-channel',private_notion_page_url='https://notion.example/synthetic/private',shared_notion_page_url='https://notion.example/synthetic/shared' WHERE id=$1")
        .bind(created.id).execute(pg.pool()).await.unwrap();
    let before = records::find_by_id_on(&pg, created.id)
        .await
        .unwrap()
        .unwrap();
    let mut changed = input.clone();
    changed.name = "Updated Synthetic Project".into();
    changed.description = None;
    changed.firm_id = None;
    changed.jurisdiction_id = None;
    let updated = records::upsert_with_id_on(&pg, created.id, &changed)
        .await
        .unwrap();
    let mut expected = before.clone();
    expected.name.clone_from(&changed.name);
    expected.description = None;
    expected.updated_at.clone_from(&updated.updated_at);
    assert_eq!(updated, expected);
    changed.code = "renamed-project".into();
    assert!(matches!(
        records::upsert_with_id_on(&pg, created.id, &changed).await,
        Err(ProjectError::CodeImmutable)
    ));
    assert_eq!(
        records::find_by_id_on(&pg, created.id).await.unwrap(),
        Some(updated)
    );

    let mut invalid = input.clone();
    invalid.code = "invalid-reference".into();
    invalid.brand = "absent-brand".into();
    assert!(matches!(
        records::create_on(&pg, &invalid).await,
        Err(ProjectError::UnknownBrand)
    ));
    invalid.brand.clone_from(&input.brand);
    invalid.entity_id = Uuid::new_v4();
    assert!(matches!(
        records::create_on(&pg, &invalid).await,
        Err(ProjectError::NoSuchEntity)
    ));
    invalid.entity_id = input.entity_id;
    invalid.firm_id = Some(Uuid::new_v4());
    assert!(matches!(
        records::create_on(&pg, &invalid).await,
        Err(ProjectError::NoSuchFirm)
    ));
    invalid.firm_id = input.firm_id;
    invalid.jurisdiction_id = Some(Uuid::new_v4());
    assert!(matches!(
        records::create_on(&pg, &invalid).await,
        Err(ProjectError::NoSuchJurisdiction)
    ));
    invalid.jurisdiction_id = input.jurisdiction_id;
    invalid.code = "new".into();
    assert!(matches!(
        records::create_on(&pg, &invalid).await,
        Err(ProjectError::ReservedCode)
    ));
    assert_eq!(
        records::all_on(&pg).await.unwrap().len(),
        1,
        "failed writes leave no rows"
    );

    let mut other = input.clone();
    other.code = "another-project".into();
    other.name = "Alphabetical Synthetic Project".into();
    let earlier = records::create_on(&pg, &other).await.unwrap();
    assert_eq!(records::all_on(&pg).await.unwrap()[0], earlier);
}

#[tokio::test]
async fn concurrent_code_creation_has_one_winner_and_fixture_callers_adopt_it() {
    use super::projects as records;
    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let (first, second) = tokio::join!(
        records::create_on(&pg, &input),
        records::create_on(&pg, &input)
    );
    assert!(matches!(
        (&first, &second),
        (Ok(_), Err(records::ProjectError::CodeTaken))
            | (Err(records::ProjectError::CodeTaken), Ok(_))
    ));
    let winner = first.or(second).unwrap();
    assert_eq!(
        records::find_or_create_by_code_on(&pg, Uuid::new_v4(), &input)
            .await
            .unwrap(),
        winner
    );
    let mut fixture_input = input;
    fixture_input.code = "concurrent-fixture".into();
    let (first, second) = tokio::join!(
        records::find_or_create_by_code_on(&pg, Uuid::new_v4(), &fixture_input),
        records::find_or_create_by_code_on(&pg, Uuid::new_v4(), &fixture_input)
    );
    assert_eq!(first.unwrap(), second.unwrap());
    assert_eq!(records::all_on(&pg).await.unwrap().len(), 2);
}

#[tokio::test]
async fn native_project_operations_propagate_database_failure() {
    use super::projects as records;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://postgres:synthetic@127.0.0.1:1/smoke")
        .unwrap();
    pool.close().await;
    let pg = PostgresDb(pool);
    let id = Uuid::new_v4();
    let input = NewProject::default();
    assert!(records::find_by_id_on(&pg, id).await.is_err());
    assert!(records::find_by_code_on(&pg, "synthetic-project")
        .await
        .is_err());
    assert!(records::find_by_name_on(&pg, "Synthetic Project")
        .await
        .is_err());
    assert!(records::all_on(&pg).await.is_err());
    assert!(records::create_on(&pg, &input).await.is_err());
    assert!(records::upsert_with_id_on(&pg, id, &input).await.is_err());
    assert!(records::find_or_create_by_code_on(&pg, id, &input)
        .await
        .is_err());
}

#[tokio::test]
async fn repository_writes_preserve_the_winning_pointer_and_first_stamps() {
    use super::projects as records;
    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let project = records::create_on(&pg, &input).await.unwrap();
    let first = "https://forge.example/synthetic/first";
    let second = "https://forge.example/synthetic/second";
    let at = "2026-10-01T00:00:00Z";
    let later = "2026-10-02T00:00:00Z";
    let (left, right) = tokio::join!(
        records::record_provisioned_repository_on(&pg, project.id, first, at),
        records::record_provisioned_repository_on(&pg, project.id, second, at)
    );
    let left = left.unwrap().unwrap();
    let right = right.unwrap().unwrap();
    assert_eq!(left.repository_url, right.repository_url);
    let winner = left.repository_url.as_deref().unwrap();
    let loser = if winner == first { second } else { first };
    let unchanged = records::record_source_initialized_on(&pg, project.id, loser, later)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.repository_url.as_deref(), Some(winner));
    assert_eq!(unchanged.git_initialized_at, None);
    let initialized = records::record_source_initialized_on(&pg, project.id, winner, at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initialized.git_initialized_at.as_deref(), Some(at));
    let repeated = records::record_source_initialized_on(&pg, project.id, winner, later)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(repeated.git_initialized_at, initialized.git_initialized_at);
    assert_eq!(
        repeated.forge_provisioned_at,
        initialized.forge_provisioned_at
    );
    assert_eq!(repeated.description, project.description);
    assert!(
        records::record_provisioned_repository_on(&pg, Uuid::new_v4(), first, at)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        records::record_source_initialized_on(&pg, Uuid::new_v4(), first, at)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        records::record_source_initialized_on(&pg, project.id, "javascript:bad", at).await,
        Err(records::ProjectError::InvalidRepositoryUrl)
    ));
    assert!(matches!(
        records::record_provisioned_repository_on(&pg, project.id, "javascript:bad", at).await,
        Err(records::ProjectError::InvalidRepositoryUrl)
    ));
}

#[tokio::test]
async fn source_initialization_records_an_absent_repository_without_erasing_metadata() {
    use super::projects as records;
    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let project = records::create_on(&pg, &input).await.unwrap();
    let at = "2026-10-01T00:00:00Z";
    let url = "https://forge.example/synthetic/source";
    let initialized = records::record_source_initialized_on(&pg, project.id, url, at)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(initialized.repository_url.as_deref(), Some(url));
    assert_eq!(initialized.git_initialized_at.as_deref(), Some(at));
    assert_eq!(initialized.forge_provisioned_at.as_deref(), Some(at));
    assert_eq!(initialized.code, project.code);
    assert_eq!(initialized.description, project.description);
    let later =
        records::record_provisioned_repository_on(&pg, project.id, url, "2026-10-02T00:00:00Z")
            .await
            .unwrap()
            .unwrap();
    assert_eq!(later, initialized);
}

#[tokio::test]
async fn lifecycle_history_and_current_project_state_commit_together() {
    use super::project_lifecycle::{append, baseline, history, lock, StatusTransition};

    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let project = super::projects::create_on(&pg, &input).await.unwrap();
    let at = chrono::Utc::now();
    let actor = Uuid::new_v4();
    let event = |from: &str, to: &str, reason: &str| StatusTransition {
        from_status: Some(from.to_owned()),
        to_status: to.to_owned(),
        effective_at: Some(at),
        recorded_at: Some(at),
        reason: Some(reason.to_owned()),
        attesting_person_id: Some(actor),
    };
    let mut tx = pg.pool().begin().await.unwrap();
    let row = lock(&mut tx, project.id).await.unwrap().unwrap();
    append(&mut tx, &row, &baseline(&row), None, None)
        .await
        .unwrap();
    let closed_at = at.to_rfc3339();
    let close = event("open", "closed", "engagement_completed");
    append(
        &mut tx,
        &row,
        &close,
        Some(&closed_at),
        close.reason.as_deref(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = pg.pool().begin().await.unwrap();
    let row = lock(&mut tx, project.id).await.unwrap().unwrap();
    assert_eq!(row.closed_at.as_deref(), Some(closed_at.as_str()));
    let reopen = event("closed", "open", "new_matter_event");
    append(&mut tx, &row, &reopen, None, None).await.unwrap();
    assert_eq!(
        lock(&mut tx, project.id).await.unwrap().unwrap().status,
        "open"
    );
    tx.rollback().await.unwrap();

    let mut tx = pg.pool().begin().await.unwrap();
    let row = lock(&mut tx, project.id).await.unwrap().unwrap();
    assert_eq!(row.status, "closed");
    assert_eq!(history(&mut tx, project.id).await.unwrap().len(), 2);
    append(&mut tx, &row, &reopen, None, None).await.unwrap();
    tx.commit().await.unwrap();

    sqlx::query("UPDATE project SET status='closed', closed_at=$2 WHERE id=$1")
        .bind(project.id)
        .bind(&closed_at)
        .execute(pg.pool())
        .await
        .unwrap();
    let mut tx = pg.pool().begin().await.unwrap();
    let row = lock(&mut tx, project.id).await.unwrap().unwrap();
    assert_eq!(row.status, "open");
    assert!(row.closed_at.is_none());
    let reclose = event("open", "closed", "client_terminated");
    append(
        &mut tx,
        &row,
        &reclose,
        Some(&closed_at),
        reclose.reason.as_deref(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = pg.pool().begin().await.unwrap();
    let row = lock(&mut tx, project.id).await.unwrap().unwrap();
    assert_eq!(row.status, "closed");
    assert_eq!(row.closure_reason.as_deref(), Some("client_terminated"));
    let events = history(&mut tx, project.id).await.unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[1].reason.as_deref(), Some("engagement_completed"));
    assert_eq!(
        events[1].effective_at.unwrap().timestamp_micros(),
        at.timestamp_micros()
    );
    assert_eq!(events[2].attesting_person_id, Some(actor));
}

async fn insert_person(pg: &PostgresDb, role: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,$2,$3,$4)")
        .bind(id)
        .bind(format!("Synthetic {role}"))
        .bind(format!("{}@authority.example", id.simple()))
        .bind(role)
        .execute(pg.pool())
        .await
        .unwrap();
    id
}

async fn surreal_project(surreal: &crate::surreal::SurrealDb, id: Uuid) -> Option<Project> {
    crate::projects::find_by_id_in_surreal(surreal, id)
        .await
        .unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one Project lifecycle under tenancy authority walks every routed read, write and access seam"
)]
async fn tenancy_authority_routes_projects_participation_and_access_through_postgres() {
    use crate::persons::Role;
    use crate::projects::{self, DriSide, ProjectStoreError};
    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let surreal = crate::test_support::mem_surreal().await;
    let lawyer = insert_person(&pg, "lawyer").await;
    let client = insert_person(&pg, "client").await;
    let stranger = insert_person(&pg, "lawyer").await;

    super::tenancy::with_test_authority(
        pg.clone(),
        true,
        Box::pin(async {
            // Creation commits in PostgreSQL and projects into SurrealDB.
            let created = projects::create(&surreal, &input).await.unwrap();
            assert_eq!(created.id.get_version_num(), 4);
            assert_eq!(
                super::projects::find_by_id_on(&pg, created.id)
                    .await
                    .unwrap(),
                Some(created.clone())
            );
            assert_eq!(
                surreal_project(&surreal, created.id).await,
                Some(created.clone())
            );
            assert!(matches!(
                projects::create(&surreal, &input).await,
                Err(ProjectStoreError::CodeTaken)
            ));
            assert!(matches!(
                projects::create(
                    &surreal,
                    &NewProject {
                        code: "unknown-brand".into(),
                        brand: "absent-brand".into(),
                        ..input.clone()
                    }
                )
                .await,
                Err(ProjectStoreError::UnknownBrand(_))
            ));

            // Reads answer from PostgreSQL even when SurrealDB has drifted.
            surreal
                .query("UPDATE $id SET name = 'Drifted'")
                .bind(("id", crate::surreal::record_id("project", created.id)))
                .await
                .unwrap();
            assert_eq!(
                projects::find_by_id(&surreal, created.id).await.unwrap(),
                Some(created.clone())
            );
            assert_eq!(
                projects::find_by_code(&surreal, &input.code).await.unwrap(),
                Some(created.clone())
            );
            assert_eq!(
                projects::find_by_name(&surreal, &input.name).await.unwrap(),
                Some(created.clone())
            );
            assert_eq!(
                projects::all(&surreal).await.unwrap(),
                vec![created.clone()]
            );
            let adopted = projects::find_or_create_by_code(&surreal, Uuid::new_v4(), &input)
                .await
                .unwrap();
            assert_eq!(adopted.id, created.id);

            // Participation is written in PostgreSQL, labelled from the Person's
            // tier, and projected into SurrealDB.
            let lawyer_row = projects::add_participation(&surreal, created.id, lawyer, "ignored")
                .await
                .unwrap();
            assert_eq!(lawyer_row.participation, "lawyer");
            assert_eq!(
                projects::participation_by_id_in_surreal(&surreal, lawyer_row.id)
                    .await
                    .unwrap(),
                Some(lawyer_row.clone())
            );
            assert!(matches!(
                projects::add_participation(&surreal, created.id, lawyer, "lawyer").await,
                Err(ProjectStoreError::DuplicateParticipation)
            ));
            assert!(matches!(
                projects::add_participation(&surreal, Uuid::new_v4(), lawyer, "lawyer").await,
                Err(ProjectStoreError::NoSuchProject(_))
            ));
            projects::designate_dri_in_surreal(&surreal, created.id, client, DriSide::Client)
                .await
                .unwrap();
            let client_row = projects::participation_for_person(&surreal, client, created.id)
                .await
                .unwrap()
                .unwrap();
            assert!(client_row.is_client_dri);
            assert_eq!(
                projects::participation_by_id_in_surreal(&surreal, client_row.id)
                    .await
                    .unwrap(),
                Some(client_row.clone())
            );
            assert_eq!(
                projects::participations_for_project(&surreal, created.id)
                    .await
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(
                projects::all_participations(&surreal).await.unwrap().len(),
                2
            );
            assert_eq!(
                projects::participations_for_person(&surreal, lawyer)
                    .await
                    .unwrap(),
                vec![lawyer_row.clone()]
            );

            // Access decisions read the PostgreSQL ledger.
            assert!(crate::access::can_see_project_as_lawyer(
                &surreal,
                Some(lawyer),
                Role::Lawyer,
                created.id
            )
            .await
            .unwrap());
            assert!(!crate::access::can_see_project_as_lawyer(
                &surreal,
                Some(stranger),
                Role::Lawyer,
                created.id
            )
            .await
            .unwrap());
            assert!(
                crate::access::can_see_project_as_client(&surreal, Some(client), created.id)
                    .await
                    .unwrap()
            );
            assert!(
                !crate::access::can_see_project_as_client(&surreal, Some(lawyer), created.id)
                    .await
                    .unwrap()
            );

            projects::clear_dri_in_surreal(&surreal, client_row.id, DriSide::Client)
                .await
                .unwrap();
            assert!(
                !projects::participation_by_id_in_surreal(&surreal, client_row.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_client_dri
            );
            let moved = projects::update_participation(&surreal, client_row.id, stranger, "client")
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (moved.person_id, moved.participation.as_str()),
                (stranger, "lawyer")
            );
            projects::remove_participation(&surreal, moved.id)
                .await
                .unwrap();
            projects::remove_participation(&surreal, lawyer_row.id)
                .await
                .unwrap();
            assert!(projects::all_participations(&surreal)
                .await
                .unwrap()
                .is_empty());
            assert!(
                projects::participation_by_id_in_surreal(&surreal, lawyer_row.id)
                    .await
                    .unwrap()
                    .is_none()
            );

            // A write still committed through SurrealDB (repairing the drifted
            // copy first) is not complete until PostgreSQL holds it.
            surreal
                .query("UPDATE $id SET name = $name")
                .bind(("id", crate::surreal::record_id("project", created.id)))
                .bind(("name", created.name.clone()))
                .await
                .unwrap();
            let foldered =
                projects::set_drive_folder_id(&surreal, created.id, Some("synthetic-folder"))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(
                foldered.drive_folder_id.as_deref(),
                Some("synthetic-folder")
            );
            assert_eq!(
                super::projects::find_by_id_on(&pg, created.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .drive_folder_id
                    .as_deref(),
                Some("synthetic-folder")
            );

            let provisioned = projects::record_provisioned_repository(
                &surreal,
                created.id,
                "https://forge.example/synthetic/project.git",
                "2026-11-13T00:00:00Z",
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(
                surreal_project(&surreal, created.id)
                    .await
                    .unwrap()
                    .repository_url,
                provisioned.repository_url
            );

            projects::delete_project_with_surreal(&surreal, created.id)
                .await
                .unwrap();
            assert!(super::projects::find_by_id_on(&pg, created.id)
                .await
                .unwrap()
                .is_none());
        }),
    )
    .await;

    // With the switch off, the legacy path neither reads nor writes PostgreSQL.
    super::tenancy::with_test_authority(
        pg.clone(),
        false,
        Box::pin(async {
            let legacy = projects::create(
                &surreal,
                &NewProject {
                    code: "legacy-project".into(),
                    name: "Legacy Project".into(),
                    status: "open".into(),
                    entity_id: input.entity_id,
                    ..NewProject::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(legacy.id.get_version_num(), 7);
            assert!(super::projects::find_by_id_on(&pg, legacy.id)
                .await
                .unwrap()
                .is_none());
            assert_eq!(
                projects::find_by_id(&surreal, legacy.id).await.unwrap(),
                Some(legacy)
            );
        }),
    )
    .await;
}
