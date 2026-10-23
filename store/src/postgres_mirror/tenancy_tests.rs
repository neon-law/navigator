use super::*;
use crate::brands::NewBrand;
use crate::firms::{FirmEdit, FirmMembership, NewFirm, NewPersonFirmRole};
use crate::persons::{NewPerson, Role};
use crate::postgres_mirror::tests::{connect, fresh_schema, mirror_identity, source};
use crate::projects::NewProject;
use crate::test_support::{mem_surreal, seed_entity};

const BRAND: &str = "synthetic-tenancy";

/// An engine with no Brand rows, so every Brand in it is one a test made.
async fn tenancy_surreal() -> SurrealDb {
    let surreal = mem_surreal().await;
    surreal
        .query("DELETE brand")
        .await
        .unwrap()
        .check()
        .unwrap();
    surreal
}

async fn person(surreal: &SurrealDb, role: Role) -> Uuid {
    crate::persons::create(
        surreal,
        &NewPerson::with_role(
            "Synthetic Person",
            format!("tenancy-{}@example.com", Uuid::now_v7()),
            role,
        ),
    )
    .await
    .unwrap()
    .id
}

async fn practice(surreal: &SurrealDb, entity: Uuid, admin: Uuid) -> crate::firms::Firm {
    crate::firms::create(
        surreal,
        &NewFirm {
            name: "Synthetic Practice".into(),
            status: "active".into(),
            entity_id: entity,
            admin_dri_person_id: admin,
        },
    )
    .await
    .unwrap()
}

async fn mirrored(surreal: &SurrealDb, pg: &PostgresDb, key: &MirrorKey) -> Mirrored {
    super::super::mirror(surreal, pg, key).await.unwrap()
}

#[test]
fn upserts_key_on_id_and_write_only_a_changed_row() {
    let sql = upsert_sql("firm_brand", FIRM_BRAND_COLUMNS);
    assert!(sql.starts_with(
        "INSERT INTO firm_brand (id, firm_id, brand_key, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT (id) DO UPDATE SET firm_id = EXCLUDED.firm_id"
    ));
    assert!(sql.contains(
        "WHERE (firm_brand.firm_id, firm_brand.brand_key, firm_brand.inserted_at, \
         firm_brand.updated_at) IS DISTINCT FROM"
    ));
}

#[test]
fn tenancy_keys_name_rows_without_values() {
    let id = Uuid::nil();
    assert_eq!(MirrorKey::Firm(id).table(), "firm");
    assert_eq!(
        MirrorKey::FirmMembership {
            person_id: id,
            firm_id: id
        }
        .table(),
        "person_firm_role"
    );
    assert_eq!(
        MirrorKey::FirmBrand {
            brand_key: "synthetic".into()
        }
        .to_string(),
        "firm_brand:synthetic"
    );
    assert_eq!(
        MirrorKey::ProjectParticipation(id).to_string(),
        "person_project_role:00000000-0000-0000-0000-000000000000"
    );
}

#[test]
fn a_timestamp_is_read_never_invented() {
    assert_eq!(
        timestamp("2026-10-12T21:00:00.123456789+00:00")
            .unwrap()
            .to_rfc3339(),
        "2026-10-12T21:00:00.123456+00:00"
    );
    assert!(matches!(
        timestamp("not a timestamp"),
        Err(MirrorError::SourceTimestamp)
    ));
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one Firm → Brand → Project lifecycle reads in order"
)]
async fn firm_brand_project_and_membership_rows_converge_through_their_lifecycle() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = tenancy_surreal().await;
    let entity = seed_entity(&surreal).await;
    let admin = person(&surreal, Role::Admin).await;
    let lawyer = person(&surreal, Role::Lawyer).await;
    let client = person(&surreal, Role::Client).await;
    mirror_identity(&pg, &[entity], &[admin, lawyer, client]).await;

    // A Firm and its Admin DRI.
    let firm = practice(&surreal, entity, admin).await;
    let firm_key = MirrorKey::Firm(firm.id);
    let admin_key = MirrorKey::FirmMembership {
        person_id: admin,
        firm_id: firm.id,
    };
    assert_eq!(mirrored(&surreal, &pg, &firm_key).await, Mirrored::Written);
    assert_eq!(
        mirrored(&surreal, &pg, &firm_key).await,
        Mirrored::Unchanged
    );
    assert_eq!(mirrored(&surreal, &pg, &admin_key).await, Mirrored::Written);

    // A Brand the Firm owns, and wears.
    let brand = crate::brands::create(
        &surreal,
        Role::Admin,
        Some(admin),
        &NewBrand {
            name: "Synthetic Tenancy Brand".into(),
            key: BRAND.into(),
            firm_id: Some(firm.id),
            ..NewBrand::default()
        },
    )
    .await
    .unwrap();
    let brand_key = MirrorKey::Brand(brand.id);
    assert_eq!(mirrored(&surreal, &pg, &brand_key).await, Mirrored::Written);
    assert_eq!(
        mirrored(&surreal, &pg, &brand_key).await,
        Mirrored::Unchanged
    );
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT firm_id FROM brand WHERE brand_key = $1")
        .bind(BRAND)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(owner, Some(firm.id));
    crate::brands::set_website_url(
        &surreal,
        Role::Owner,
        brand.id,
        Some("https://example.test"),
    )
    .await
    .unwrap();
    assert_eq!(mirrored(&surreal, &pg, &brand_key).await, Mirrored::Written);
    let website: Option<String> =
        sqlx::query_scalar("SELECT website_url FROM brand WHERE brand_key = $1")
            .bind(BRAND)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(website.as_deref(), Some("https://example.test"));
    crate::firms::attach_brand(&surreal, firm.id, BRAND)
        .await
        .unwrap();
    let worn = MirrorKey::FirmBrand {
        brand_key: BRAND.into(),
    };
    assert_eq!(mirrored(&surreal, &pg, &worn).await, Mirrored::Written);
    assert_eq!(mirrored(&surreal, &pg, &worn).await, Mirrored::Unchanged);

    // A Project the Firm owns under that Brand, and a participant.
    let project = crate::projects::create(
        &surreal,
        &NewProject {
            code: crate::projects::code_from_name("Synthetic Matter", Uuid::now_v7()),
            name: "Synthetic Matter".into(),
            status: "open".into(),
            brand: BRAND.into(),
            entity_id: entity,
            firm_id: Some(firm.id),
            ..NewProject::default()
        },
    )
    .await
    .unwrap();
    let project_key = MirrorKey::Project(project.id);
    assert_eq!(
        mirrored(&surreal, &pg, &project_key).await,
        Mirrored::Written
    );
    let linkage: (Option<Uuid>, Option<String>) =
        sqlx::query_as("SELECT firm_id, brand FROM project WHERE id = $1")
            .bind(project.id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(linkage, (Some(firm.id), Some(BRAND.to_string())));
    let participation = crate::projects::add_participation(&surreal, project.id, client, "client")
        .await
        .unwrap();
    let participation_key = MirrorKey::ProjectParticipation(participation.id);
    assert_eq!(
        mirrored(&surreal, &pg, &participation_key).await,
        Mirrored::Written
    );

    // Membership changes and a Firm edit.
    crate::firms::add_membership(
        &surreal,
        &NewPersonFirmRole {
            person_id: lawyer,
            firm_id: firm.id,
            membership: FirmMembership::Lawyer,
            is_dri: false,
        },
    )
    .await
    .unwrap();
    let lawyer_key = MirrorKey::FirmMembership {
        person_id: lawyer,
        firm_id: firm.id,
    };
    assert_eq!(
        mirrored(&surreal, &pg, &lawyer_key).await,
        Mirrored::Written
    );
    crate::firms::update_membership(
        &surreal,
        Role::Owner,
        None,
        lawyer,
        firm.id,
        FirmMembership::Clerk,
    )
    .await
    .unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &lawyer_key).await,
        Mirrored::Written
    );
    let membership: String = sqlx::query_scalar(
        "SELECT membership FROM person_firm_role WHERE person_id = $1 AND firm_id = $2",
    )
    .bind(lawyer)
    .bind(firm.id)
    .fetch_one(&pg.0)
    .await
    .unwrap();
    assert_eq!(membership, "clerk");
    crate::firms::update(
        &surreal,
        Role::Owner,
        None,
        firm.id,
        &FirmEdit {
            name: Some("Synthetic Practice Renamed".into()),
            ..FirmEdit::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(mirrored(&surreal, &pg, &firm_key).await, Mirrored::Written);

    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(
        report
            .tables
            .iter()
            .map(|table| table.target_rows)
            .collect::<Vec<_>>(),
        [1, 1, 1, 1, 2, 1]
    );

    // A Post mirrors once its Brand is in PostgreSQL.
    let mut post = source("tenancy", "Tenancy");
    post.brand_key = BRAND;
    let post = crate::posts::import_source(&surreal, &post, Utc::now())
        .await
        .unwrap()
        .post_id;
    assert_eq!(
        mirrored(&surreal, &pg, &MirrorKey::Post(post)).await,
        Mirrored::Written
    );

    // Removal, children first.
    crate::firms::remove_membership(&surreal, Role::Owner, None, lawyer, firm.id)
        .await
        .unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &lawyer_key).await,
        Mirrored::Deleted
    );
    crate::projects::remove_participation(&surreal, participation.id)
        .await
        .unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &participation_key).await,
        Mirrored::Deleted
    );
    crate::projects::delete_project_with_surreal(&surreal, project.id)
        .await
        .unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &project_key).await,
        Mirrored::Deleted
    );
    surreal
        .query("DELETE $post")
        .bind(("post", crate::surreal::record_id("post", post)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        mirrored(&surreal, &pg, &MirrorKey::Post(post)).await,
        Mirrored::Deleted
    );
    crate::firms::detach_brand(&surreal, Role::Owner, None, firm.id, BRAND)
        .await
        .unwrap();
    assert_eq!(mirrored(&surreal, &pg, &worn).await, Mirrored::Deleted);
    crate::brands::delete(&surreal, Role::Owner, None, brand.id)
        .await
        .unwrap();
    assert_eq!(mirrored(&surreal, &pg, &brand_key).await, Mirrored::Deleted);
    crate::firms::delete(&surreal, Role::Owner, None, firm.id)
        .await
        .unwrap();
    assert_eq!(mirrored(&surreal, &pg, &admin_key).await, Mirrored::Deleted);
    assert_eq!(mirrored(&surreal, &pg, &firm_key).await, Mirrored::Deleted);
    assert_eq!(
        mirrored(&surreal, &pg, &firm_key).await,
        Mirrored::Unchanged
    );

    let report = parity(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert!(report.tables.iter().all(|table| table.target_rows == 0));
}

#[tokio::test]
async fn a_missing_identity_is_refused_reported_and_reconciled() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = tenancy_surreal().await;
    let entity = seed_entity(&surreal).await;
    let admin = person(&surreal, Role::Admin).await;
    let firm = practice(&surreal, entity, admin).await;

    // PostgreSQL holds neither the Firm's Entity nor its Admin's Person yet,
    // and the Entity names references no SurrealDB holds, so the identity
    // mirror cannot supply it.
    let error = super::super::mirror(&surreal, &pg, &MirrorKey::Firm(firm.id))
        .await
        .unwrap_err();
    assert!(matches!(error, MirrorError::Target(ref code) if code == "23503"));

    // A Firm PostgreSQL holds that SurrealDB does not is divergent too.
    let stale = Uuid::new_v4();
    sqlx::query("INSERT INTO firm (id, name, slug, status) VALUES ($1, 'Stale Practice', 'stale-practice', 'active')")
        .bind(stale)
        .execute(&pg.0)
        .await
        .unwrap();

    let report = parity(&surreal, &pg).await.unwrap();
    let divergent: Vec<_> = report.divergent().cloned().collect();
    assert_eq!(divergent.len(), 3, "{divergent:?}");
    assert!(divergent.contains(&MirrorKey::Firm(firm.id)));
    assert!(divergent.contains(&MirrorKey::Firm(stale)));
    assert!(divergent.contains(&MirrorKey::FirmMembership {
        person_id: admin,
        firm_id: firm.id
    }));

    mirror_identity(&pg, &[entity], &[admin]).await;
    let report = reconcile(&surreal, &pg).await.unwrap();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.tables[0].target_rows, 1);
    assert_eq!(report.tables[4].target_rows, 1);
}

#[tokio::test]
async fn an_unreadable_source_timestamp_is_refused_and_reported() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = tenancy_surreal().await;
    let entity = seed_entity(&surreal).await;
    let admin = person(&surreal, Role::Admin).await;
    mirror_identity(&pg, &[entity], &[admin]).await;
    let firm = practice(&surreal, entity, admin).await;
    surreal
        .query("UPDATE $firm SET inserted_at = 'not a timestamp'")
        .bind(("firm", crate::surreal::record_id("firm", firm.id)))
        .await
        .unwrap()
        .check()
        .unwrap();

    let error = super::super::mirror(&surreal, &pg, &MirrorKey::Firm(firm.id))
        .await
        .unwrap_err();
    assert!(matches!(error, MirrorError::SourceTimestamp));
    let report = parity(&surreal, &pg).await.unwrap();
    assert_eq!(report.tables[0].divergent, [MirrorKey::Firm(firm.id)]);
    assert_eq!(report.tables[0].target_rows, 0);
}

#[tokio::test]
async fn firm_slug_mirrors_and_postgres_holds_it_fixed() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = tenancy_surreal().await;
    let entity = seed_entity(&surreal).await;
    let admin = person(&surreal, Role::Admin).await;
    mirror_identity(&pg, &[entity], &[admin]).await;
    let firm = practice(&surreal, entity, admin).await;
    assert_eq!(firm.slug, "synthetic-practice");
    let key = MirrorKey::Firm(firm.id);

    // A row PostgreSQL holds under another id with the same slug is one
    // SurrealDB no longer has, so SurrealDB's Firm replaces it.
    let stale = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO firm (id, name, slug, status) VALUES ($1, 'Stale Practice', $2, 'active')",
    )
    .bind(stale)
    .bind(&firm.slug)
    .execute(&pg.0)
    .await
    .unwrap();
    assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Written);
    let slug: String = sqlx::query_scalar("SELECT slug FROM firm WHERE id = $1")
        .bind(firm.id)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(slug, "synthetic-practice");
    let stale_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM firm WHERE id = $1")
        .bind(stale)
        .fetch_one(&pg.0)
        .await
        .unwrap();
    assert_eq!(stale_rows, 0);

    // A rename converges the name and leaves the slug where it was.
    crate::firms::update(
        &surreal,
        Role::Owner,
        None,
        firm.id,
        &FirmEdit {
            name: Some("Renamed Synthetic Practice".into()),
            ..FirmEdit::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(mirrored(&surreal, &pg, &key).await, Mirrored::Written);
    let (name, slug): (String, String) =
        sqlx::query_as("SELECT name, slug FROM firm WHERE id = $1")
            .bind(firm.id)
            .fetch_one(&pg.0)
            .await
            .unwrap();
    assert_eq!(
        (name.as_str(), slug.as_str()),
        ("Renamed Synthetic Practice", "synthetic-practice")
    );
    let report = parity(&surreal, &pg).await.unwrap();
    assert!(!report.divergent().any(|divergent| divergent == &key));

    // PostgreSQL refuses to move a slug, as SurrealDB's READONLY does.
    let moved = sqlx::query("UPDATE firm SET slug = 'moved-practice' WHERE id = $1")
        .bind(firm.id)
        .execute(&pg.0)
        .await
        .unwrap_err();
    assert_eq!(
        moved
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23514")
    );
}

/// Names whose derivation exercises case, punctuation, apostrophes,
/// separator runs, Unicode folding, and the length cut.
const SLUG_CORPUS: &[&str] = &[
    "Example Law PC",
    "Smith & Jones, LLP",
    "Example Law, P.C.",
    "O'Neil Legal",
    "O\u{2019}Neil Legal",
    "  Example --- Law  ",
    "--Example__Law--",
    "Éxample Låw",
    "Cafe\u{0301} Counsel",
    "\u{0130}stanbul Practice",
    "\u{FB01}rm",
    "\u{FF25}xample",
    "Example \u{00DF} Law",
    "事務所",
    "Example Synthetic Practice Of Several Long Words",
    "Example Synthetic Practice Of Severalxy Z",
];

#[tokio::test]
async fn postgres_derives_the_same_firm_slug_as_rust() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    for name in SLUG_CORPUS {
        let derived: String = sqlx::query_scalar("SELECT firm_slug_from_name($1)")
            .bind(name)
            .fetch_one(&pg.0)
            .await
            .unwrap();
        assert_eq!(derived, crate::firms::slug::derive(name), "{name:?}");
    }
}

const FIRM_SLUG_MIGRATION: &str =
    include_str!("../../postgres/migrations/20261013130000_add_firm_slug.sql");

/// A fresh database holding every migration before the Firm slug.
async fn database_before_firm_slug() -> Option<sqlx::PgPool> {
    let (options, admin_database) = crate::postgres_mirror::tests::test_postgres()?;
    let admin = sqlx::PgPool::connect_with(options.clone().database(&admin_database))
        .await
        .unwrap();
    let database = format!("mirror_test_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pool = sqlx::PgPool::connect_with(options.database(&database))
        .await
        .unwrap();
    let (before, _) = crate::postgres_schema::canonical_schema()
        .split_once("-- Migration: 20261013130000_add_firm_slug.sql")
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(before.to_string()))
        .execute(&pool)
        .await
        .unwrap();
    Some(pool)
}

async fn insert_firm(pool: &sqlx::PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("INSERT INTO firm (name, status) VALUES ($1, 'active') RETURNING id")
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_migration_backfills_existing_firms_and_refuses_a_collision() {
    let Some(pool) = database_before_firm_slug().await else {
        return;
    };
    let first = insert_firm(&pool, "Example Låw PC").await;
    let second = insert_firm(&pool, "Second Example Practice").await;
    sqlx::raw_sql(FIRM_SLUG_MIGRATION)
        .execute(&pool)
        .await
        .unwrap();
    for (id, expected) in [
        (first, "example-law-pc"),
        (second, "second-example-practice"),
    ] {
        let slug: String = sqlx::query_scalar("SELECT slug FROM firm WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(slug, expected);
    }

    let Some(colliding) = database_before_firm_slug().await else {
        return;
    };
    insert_firm(&colliding, "Example Law PC").await;
    insert_firm(&colliding, "Example Law, PC").await;
    let error = sqlx::raw_sql(FIRM_SLUG_MIGRATION)
        .execute(&colliding)
        .await
        .unwrap_err();
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some("23505"),
        "{error}"
    );
    let added: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns \
         WHERE table_name = 'firm' AND column_name = 'slug'",
    )
    .fetch_one(&colliding)
    .await
    .unwrap();
    assert_eq!(added, 0, "a refused migration leaves no partial column");
}

#[tokio::test]
async fn shadow_reads_compare_firm_project_and_entity_reads_through_the_mirror_images() {
    use crate::postgres_shadow::ShadowKey;

    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = tenancy_surreal().await;
    let entity = seed_entity(&surreal).await;
    let admin = person(&surreal, Role::Admin).await;
    mirror_identity(&pg, &[entity], &[admin]).await;
    let firm = practice(&surreal, entity, admin).await;
    let brand = crate::brands::create(
        &surreal,
        Role::Admin,
        Some(admin),
        &NewBrand {
            name: "Synthetic Shadow Brand".into(),
            key: BRAND.into(),
            firm_id: Some(firm.id),
            ..NewBrand::default()
        },
    )
    .await
    .unwrap();
    let project = crate::projects::create(
        &surreal,
        &NewProject {
            code: crate::projects::code_from_name("Shadow Matter", Uuid::now_v7()),
            name: "Shadow Matter".into(),
            status: "open".into(),
            brand: BRAND.into(),
            entity_id: entity,
            firm_id: Some(firm.id),
            ..NewProject::default()
        },
    )
    .await
    .unwrap();

    // Not yet in PostgreSQL: each read is a mismatch naming the row's id.
    let source_firm = crate::firms::find_by_id(&surreal, firm.id).await.unwrap();
    let missing = shadow_firm(&pg, firm.id, source_firm.as_ref())
        .await
        .unwrap();
    assert_eq!((missing.id, missing.equal), (Some(firm.id), false));

    // Mirrored, the same reads match, by id and by code.
    for key in [
        MirrorKey::Firm(firm.id),
        MirrorKey::Brand(brand.id),
        MirrorKey::Project(project.id),
    ] {
        super::super::mirror(&surreal, &pg, &key).await.unwrap();
    }
    let source_project = crate::projects::find_by_id(&surreal, project.id)
        .await
        .unwrap();
    assert!(
        shadow_firm(&pg, firm.id, source_firm.as_ref())
            .await
            .unwrap()
            .equal
    );
    assert!(
        shadow_project(&pg, ShadowKey::Id(project.id), source_project.as_ref())
            .await
            .unwrap()
            .equal
    );
    let by_code = shadow_project(
        &pg,
        ShadowKey::Text(project.code.clone()),
        source_project.as_ref(),
    )
    .await
    .unwrap();
    assert_eq!((by_code.id, by_code.equal), (Some(project.id), true));

    // Both stores agree that an unknown code names nothing.
    let nothing = shadow_project(&pg, ShadowKey::Text("no-such-code".into()), None)
        .await
        .unwrap();
    assert_eq!((nothing.id, nothing.equal), (None, true));

    // A column PostgreSQL holds differently is a semantic mismatch.
    sqlx::query("UPDATE project SET status = 'closed' WHERE id = $1")
        .bind(project.id)
        .execute(&pg.0)
        .await
        .unwrap();
    assert!(
        !shadow_project(&pg, ShadowKey::Id(project.id), source_project.as_ref())
            .await
            .unwrap()
            .equal
    );

    // A row only PostgreSQL holds is a mismatch too.
    let gone = shadow_project(&pg, ShadowKey::Id(project.id), None)
        .await
        .unwrap();
    assert_eq!((gone.id, gone.equal), (Some(project.id), false));

    // Entities compare through the identity image.
    let source_entity = crate::entities::find_by_id(&surreal, entity).await.unwrap();
    super::super::mirror(&surreal, &pg, &MirrorKey::Entity(entity))
        .await
        .unwrap();
    let entity_read = super::super::identity::shadow_entity(&pg, entity, source_entity.as_ref())
        .await
        .unwrap();
    assert_eq!((entity_read.id, entity_read.equal), (Some(entity), true));
}
