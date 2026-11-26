use super::PostgresDb;
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::projects::NewProject;
use uuid::Uuid;

pub(super) struct Fixture {
    pub(super) pg: PostgresDb,
    pub(super) input: NewProject,
}

pub(super) async fn fixture() -> Option<Fixture> {
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
    crate::postgres::fixtures::activate_family(
        &pg,
        crate::postgres_backfill::family::Family::Notation,
    )
    .await;
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
async fn firm_source_inventory_pages_through_closed_and_unprovisioned_projects() {
    use super::projects::{create_on, firm_project_sources_on};

    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let firm = input.firm_id.unwrap();
    let open = create_on(&pg, &input).await.unwrap();
    let closed = create_on(
        &pg,
        &NewProject {
            code: "synthetic-closed".into(),
            status: "closed".into(),
            ..input
        },
    )
    .await
    .unwrap();
    let first = firm_project_sources_on(&pg, firm, None, 1).await.unwrap();
    assert_eq!(first.len(), 1);
    let second = firm_project_sources_on(&pg, firm, Some(first[0].id), 1)
        .await
        .unwrap();
    assert_eq!(second.len(), 1);
    let mut expected = [open.id, closed.id];
    expected.sort();
    assert_eq!([first[0].id, second[0].id], expected);
    assert!(first[0].repository_url.is_none());
    assert!(second[0].repository_url.is_none());
    assert!(firm_project_sources_on(&pg, Uuid::new_v4(), None, 1)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn visible_project_lists_and_code_lookups_respect_participation_side() {
    use crate::access::{visible_firm_project_by_code, visible_projects};
    use crate::persons::Role;
    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let viewer = insert_person(&pg, "admin").await;
    let other = insert_person(&pg, "lawyer").await;
    for (code, name, membership) in [
        ("visible-b", "Zebra", Some(("lawyer", false))),
        ("visible-a", "Alpha", Some(("custom", false))),
        ("client-only", "Client", Some(("counterparty", false))),
        ("client-dri", "Dri", Some(("lawyer", true))),
        ("unrelated", "Other", None),
    ] {
        let project = super::projects::create_on(
            &pg,
            &NewProject {
                code: code.into(),
                name: name.into(),
                ..input.clone()
            },
        )
        .await
        .unwrap();
        super::participation::add_on(&pg, project.id, other)
            .await
            .unwrap();
        if let Some((side, dri)) = membership {
            sqlx::query("INSERT INTO person_project_role (person_id,project_id,participation,is_client_dri) VALUES ($1,$2,$3,$4)")
                .bind(viewer).bind(project.id).bind(side).bind(dri).execute(pg.pool()).await.unwrap();
        }
    }

    let db = crate::surreal::SurrealDb::uninitialized();
    super::tenancy::with_test_authority(pg.clone(), true, async {
        for (role, expected) in [
            (Role::Admin, ["visible-a", "visible-b"]),
            (Role::Client, ["client-only", "client-dri"]),
        ] {
            let list = visible_projects(&db, Some(viewer), role).await.unwrap();
            assert_eq!(
                list.iter().map(|p| p.code.as_str()).collect::<Vec<_>>(),
                expected
            );
        }
        for code in [
            "visible-a",
            "client-only",
            "client-dri",
            "unrelated",
            "absent",
        ] {
            let project = visible_firm_project_by_code(&db, viewer, code)
                .await
                .unwrap();
            assert_eq!(
                project.as_ref().map(|p| p.code.as_str()),
                (code == "visible-a").then_some(code)
            );
        }
        sqlx::query("DELETE FROM person_project_role WHERE person_id=$1")
            .bind(viewer)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(visible_projects(&db, Some(viewer), Role::Admin)
            .await
            .unwrap()
            .is_empty());
        assert!(visible_firm_project_by_code(&db, viewer, "visible-a")
            .await
            .unwrap()
            .is_none());
    })
    .await;
}

#[tokio::test]
async fn preview_candidates_resolve_primary_and_alias_within_the_review_scope() {
    use super::projects as records;
    let Some(Fixture { pg, input }) = fixture().await else {
        return;
    };
    let firm_id = input.firm_id.unwrap();
    let other_firm = Uuid::now_v7();
    let other_entity = Uuid::now_v7();
    let contact = Uuid::now_v7();
    sqlx::query("INSERT INTO entity (id, name, entity_type_id, jurisdiction_id) SELECT $1,'Other Entity',entity_type_id,jurisdiction_id FROM entity WHERE id=$2")
        .bind(other_entity).bind(input.entity_id).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO firm (id, name, slug, status, entity_id) VALUES ($1,'Other Firm','other-firm','active',$2)")
        .bind(other_firm).bind(other_entity).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO brand (name, brand_key) VALUES ('Other Brand','other-brand')")
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO person (id, name, email, role) VALUES ($1,'Inbox Contact','contact@example.test','client')")
        .bind(contact).execute(pg.pool()).await.unwrap();

    sqlx::query("INSERT INTO person_mailbox (id, person_id) VALUES ('alias@example.test', $1)")
        .bind(contact)
        .execute(pg.pool())
        .await
        .unwrap();
    let mut ids = std::collections::HashMap::new();
    for (code, firm, brand, status) in [
        ("candidate-match", firm_id, input.brand.as_str(), "open"),
        ("other-brand", firm_id, "other-brand", "open"),
        ("other-firm", other_firm, input.brand.as_str(), "open"),
        ("closed-project", firm_id, input.brand.as_str(), "closed"),
    ] {
        let project = records::create_on(
            &pg,
            &NewProject {
                code: code.to_owned(),
                firm_id: Some(firm),
                brand: brand.to_owned(),
                status: status.to_owned(),
                ..input.clone()
            },
        )
        .await
        .unwrap();
        super::participation::add_on(&pg, project.id, contact)
            .await
            .unwrap();
        ids.insert(code, project.id);
    }

    let addresses = [
        (firm_id, "CONTACT@example.test".to_owned()),
        (firm_id, "ALIAS@example.test".to_owned()),
        (firm_id, "unknown@example.test".to_owned()),
        (other_firm, "alias@example.test".to_owned()),
    ];
    for (brand, interactive) in [(Some(input.brand.as_str()), false), (None, true)] {
        let candidates = super::inbox::assignment_candidates_on(&pg, &addresses, brand)
            .await
            .unwrap();
        assert_eq!(candidates.len(), addresses.len());
        for (firm, candidate) in candidates {
            let expected = match (firm == firm_id, candidate.address.as_str()) {
                (_, "unknown@example.test") => {
                    assert_eq!(candidate.person_id, None);
                    assert!(candidate.open_projects.is_empty());
                    continue;
                }
                (false, _) => vec![ids["other-firm"]],
                (true, _) if interactive => vec![ids["candidate-match"], ids["other-brand"]],
                (true, _) => vec![ids["candidate-match"]],
            };
            assert_eq!(candidate.person_id, Some(contact));
            let mut expected = expected;
            expected.sort_unstable();
            assert_eq!(candidate.open_projects, expected);
        }
    }
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

pub(super) async fn insert_person(pg: &PostgresDb, role: &str) -> Uuid {
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

#[path = "project_compatibility_tests.rs"]
mod compatibility;
