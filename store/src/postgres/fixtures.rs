//! Canonical PostgreSQL-only parent rows for synthetic storage acceptance.

use super::PostgresDb;
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

pub(crate) async fn activate_family(
    pg: &PostgresDb,
    family: crate::postgres_backfill::family::Family,
) {
    crate::postgres_backfill::family::activate_fixture(pg, family).await;
}
