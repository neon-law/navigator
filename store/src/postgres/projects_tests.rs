use super::PostgresDb;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::projects::NewProject;
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
