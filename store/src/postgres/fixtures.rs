//! Canonical PostgreSQL-only parent rows for synthetic storage acceptance.

use super::PostgresDb;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use uuid::Uuid;

pub(crate) async fn workflow_fixture(pg: &PostgresDb) -> (Uuid, Uuid, Uuid) {
    let (person, entity, kind, jurisdiction, project, template, notation, question) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    sqlx::query("INSERT INTO person(id, name, email) VALUES ($1, 'Synthetic', $2)")
        .bind(person)
        .bind(format!("{person}@example.invalid"))
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity_type(id, name) VALUES ($1, $2)")
        .bind(kind)
        .bind(kind.to_string())
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO jurisdiction(id, name) VALUES ($1, $2)")
        .bind(jurisdiction)
        .bind(jurisdiction.to_string())
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity(id, name, entity_type_id, jurisdiction_id) VALUES ($1, 'Synthetic', $2, $3)")
        .bind(entity).bind(kind).bind(jurisdiction).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO project(id, code, name, status, entity_id) VALUES ($1, $2, 'Synthetic', 'open', $3)")
        .bind(project).bind(project.to_string()).bind(entity).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO template(id, code, title, respondent_type, is_current, inserted_at, updated_at) VALUES ($1, $2, 'Synthetic', 'person', true, now(), now())")
        .bind(template).bind(template.to_string()).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO notation(id, template_id, person_id, project_id, state) VALUES ($1, $2, $3, $4, 'BEGIN')")
        .bind(notation).bind(template).bind(person).bind(project).execute(pg.pool()).await.unwrap();
    sqlx::query(
        "INSERT INTO question(id, code, prompt, answer_type) VALUES ($1, $2, 'Synthetic', 'text')",
    )
    .bind(question)
    .bind(question.to_string())
    .execute(pg.pool())
    .await
    .unwrap();
    (person, notation, question)
}

pub(crate) async fn firm_for_notation(pg: &PostgresDb, notation: Uuid) -> (Uuid, Uuid) {
    let firm = Uuid::new_v4();
    sqlx::query("INSERT INTO firm(id, name, status, slug) VALUES ($1, 'Synthetic', 'active', $2)")
        .bind(firm)
        .bind(firm.to_string())
        .execute(pg.pool())
        .await
        .unwrap();
    let project: Uuid = sqlx::query_scalar("UPDATE project SET firm_id = $1 WHERE id = (SELECT project_id FROM notation WHERE id = $2) RETURNING id")
        .bind(firm).bind(notation).fetch_one(pg.pool()).await.unwrap();
    (firm, project)
}

pub(crate) async fn activate_family(pg: &PostgresDb, family: crate::postgres::history::Family) {
    crate::postgres::history::activate_fixture(pg, family).await;
}

/// Enroll the actual synthetic database before application-facade tests.
pub(crate) async fn activate_application(pg: &PostgresDb) {
    sqlx::query("INSERT INTO mirror_backfill_run (id,batch_size,status,scope,completed_at) VALUES ($1,1,'completed','global',now())")
        .bind(Uuid::new_v4()).execute(pg.pool()).await.unwrap();
    for family in crate::postgres::history::Family::all() {
        activate_family(pg, *family).await;
    }
}

pub(crate) async fn notation_fixture() -> Option<(PostgresDb, Uuid, Uuid)> {
    let (options, database) = fresh_schema().await?;
    let pg = connect(&options, &database).await;
    let jurisdiction = Uuid::now_v7();
    let entity_type = Uuid::now_v7();
    let entity = Uuid::now_v7();
    let project = Uuid::now_v7();
    let person = Uuid::now_v7();
    let template = Uuid::now_v7();
    let notation = Uuid::now_v7();
    sqlx::query("INSERT INTO jurisdiction(id,name) VALUES ($1,'Synthetic Jurisdiction')")
        .bind(jurisdiction)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity_type(id,name) VALUES ($1,'Synthetic Entity')")
        .bind(entity_type)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO entity(id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Entity',$2,$3)")
        .bind(entity).bind(entity_type).bind(jurisdiction).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO project(id,code,name,status,entity_id) VALUES ($1,'synthetic-onboarding','Synthetic Project','open',$2)")
        .bind(project).bind(entity).execute(pg.pool()).await.unwrap();
    sqlx::query(
        "INSERT INTO person(id,name,email) VALUES ($1,'Synthetic Client','client@example.com')",
    )
    .bind(person)
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO template(id,code,title,respondent_type,is_current,kind,inserted_at,updated_at)
                 VALUES ($1,'onboarding','Synthetic Onboarding','person',true,'onboarding',now(),now())")
        .bind(template).execute(pg.pool()).await.unwrap();
    sqlx::query("INSERT INTO notation(id,template_id,person_id,project_id,state) VALUES ($1,$2,$3,$4,'END')")
        .bind(notation).bind(template).bind(person).bind(project).execute(pg.pool()).await.unwrap();
    Some((pg, project, notation))
}

pub(crate) fn test_postgres() -> (PgConnectOptions, String) {
    (
        PgConnectOptions::new()
            .host("127.0.0.1")
            .port(crate::test_support::postgres_test_port())
            .username("postgres")
            .password("synthetic")
            .ssl_mode(PgSslMode::Disable),
        std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
    )
}

/// A fresh database holding only the canonical schema.
pub(crate) async fn fresh_schema() -> Option<(PgConnectOptions, String)> {
    let (options, admin_database) = test_postgres();
    let admin = sqlx::PgPool::connect_with(options.clone().database(&admin_database))
        .await
        .unwrap();
    let database = format!("postgres_test_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let setup = sqlx::PgPool::connect_with(options.clone().database(&database))
        .await
        .unwrap();
    // The graph migration needs the AGE extension, which only a superuser
    // creates: in production the superuser init path (`roles.sql`) runs
    // before any migration, and these fixtures stand in for it. `LOAD` puts
    // the library in this session, the way `session_preload_libraries` does
    // for the application's roles.
    sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS age; LOAD 'age';")
        .execute(&setup)
        .await
        .unwrap();
    sqlx::raw_sql(crate::postgres_schema::canonical_schema())
        .execute(&setup)
        .await
        .unwrap();
    setup.close().await;
    // Production role settings (`roles.sql`) preload AGE into every
    // application session; test sessions connect as the superuser, so the
    // equivalent rides on the connect options.
    let options = options.options([("session_preload_libraries", "age")]);
    Some((options, database))
}

pub(crate) async fn connect(options: &PgConnectOptions, database: &str) -> PostgresDb {
    PostgresDb::connect(options.clone().database(database))
        .await
        .unwrap()
}
