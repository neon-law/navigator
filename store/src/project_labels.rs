use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{postgres::PgRow, Row};
use uuid::Uuid;

use crate::postgres::PostgresDb;

#[derive(Debug, Serialize)]
pub struct Label {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub category: String,
    pub retired_at: Option<DateTime<Utc>>,
}

impl Label {
    fn read(row: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            key: row.try_get("key")?,
            name: row.try_get("name")?,
            category: row.try_get("category")?,
            retired_at: row.try_get("retired_at")?,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct CatalogEntry {
    #[serde(flatten)]
    pub label: Label,
    pub assigned: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum LabelError {
    #[error("use a lowercase label key with digits or single hyphens, up to 64 characters")]
    InvalidKey,
    #[error("enter a label name and category, each up to 80 characters")]
    InvalidDisplay,
    #[error("that label key already has a different name or category")]
    KeyConflict,
    #[error("provide between 1 and 100 label keys")]
    InvalidCount,
    #[error("at least one label key is unavailable for this operation in this Firm")]
    Unavailable,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

fn validate_key(key: &str) -> Result<(), LabelError> {
    if key.len() > 64
        || !key.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
    {
        return Err(LabelError::InvalidKey);
    }
    Ok(())
}

pub async fn catalog(
    pg: &PostgresDb,
    firm_id: Uuid,
    project_id: Uuid,
) -> Result<Vec<CatalogEntry>, LabelError> {
    Ok(sqlx::query(
        "SELECT l.id, l.key, l.name, l.category, l.retired_at, EXISTS (
            SELECT 1 FROM project_label_assignment a
            JOIN project p ON p.id = a.project_id AND p.firm_id = l.firm_id
            WHERE a.project_id = $2 AND a.label_id = l.id
         ) AS assigned
         FROM project_label l
         JOIN project p ON p.id = $2 AND p.firm_id = l.firm_id
         WHERE l.firm_id = $1
         ORDER BY l.category, l.name, l.key",
    )
    .bind(firm_id)
    .bind(project_id)
    .try_map(|row: PgRow| {
        Ok(CatalogEntry {
            label: Label::read(&row)?,
            assigned: row.try_get("assigned")?,
        })
    })
    .fetch_all(pg.pool())
    .await?)
}

pub async fn create(
    pg: &PostgresDb,
    firm_id: Uuid,
    key: &str,
    name: &str,
    category: &str,
) -> Result<(), LabelError> {
    validate_key(key)?;
    let name = name.trim();
    let category = category.trim();
    if [name, category].into_iter().any(|value| {
        !(1..=80).contains(&value.chars().count()) || value.chars().any(char::is_control)
    }) {
        return Err(LabelError::InvalidDisplay);
    }

    let mut tx = pg.pool().begin().await?;
    sqlx::query(
        "INSERT INTO project_label (firm_id, key, name, category)
         VALUES ($1, $2, $3, $4) ON CONFLICT (firm_id, key) DO NOTHING",
    )
    .bind(firm_id)
    .bind(key)
    .bind(name)
    .bind(category)
    .execute(&mut *tx)
    .await?;
    let saved: (String, String) =
        sqlx::query_as("SELECT name, category FROM project_label WHERE firm_id = $1 AND key = $2")
            .bind(firm_id)
            .bind(key)
            .fetch_one(&mut *tx)
            .await?;
    if saved.0 != name || saved.1 != category {
        return Err(LabelError::KeyConflict);
    }
    tx.commit().await?;
    Ok(())
}

pub async fn set_retired(
    pg: &PostgresDb,
    firm_id: Uuid,
    key: &str,
    retired: bool,
) -> Result<(), LabelError> {
    validate_key(key)?;
    let result = sqlx::query(
        "UPDATE project_label SET retired_at = CASE WHEN $3 THEN COALESCE(retired_at, now()) END
         WHERE firm_id = $1 AND key = $2",
    )
    .bind(firm_id)
    .bind(key)
    .bind(retired)
    .execute(pg.pool())
    .await?;
    if result.rows_affected() == 0 {
        return Err(LabelError::Unavailable);
    }
    Ok(())
}

pub async fn set_assigned(
    pg: &PostgresDb,
    firm_id: Uuid,
    project_id: Uuid,
    keys: &[String],
    assigned: bool,
) -> Result<(), LabelError> {
    if !(1..=100).contains(&keys.len()) {
        return Err(LabelError::InvalidCount);
    }
    let mut unique = keys.to_vec();
    unique.sort_unstable();
    unique.dedup();
    for key in &unique {
        validate_key(key)?;
    }

    let mut tx = pg.pool().begin().await?;
    // Hold the Project's Firm and the labels' retirement state through the write.
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT l.id FROM project_label l
         JOIN project p ON p.firm_id = l.firm_id
         WHERE p.id = $1 AND l.firm_id = $2 AND l.key = ANY($3)
           AND (NOT $4 OR l.retired_at IS NULL)
         ORDER BY l.key FOR SHARE OF p, l",
    )
    .bind(project_id)
    .bind(firm_id)
    .bind(&unique)
    .bind(assigned)
    .fetch_all(&mut *tx)
    .await?;
    if ids.len() != unique.len() {
        return Err(LabelError::Unavailable);
    }

    let sql = if assigned {
        "INSERT INTO project_label_assignment (project_id, label_id)
         SELECT $1, id FROM unnest($2::uuid[]) AS id
         ON CONFLICT (project_id, label_id) DO NOTHING"
    } else {
        "DELETE FROM project_label_assignment WHERE project_id = $1 AND label_id = ANY($2)"
    };
    sqlx::query(sql)
        .bind(project_id)
        .bind(&ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Read active labels for an already-authorized set of Projects.
pub async fn active_for_projects(
    pg: &PostgresDb,
    projects: &[Uuid],
) -> Result<HashMap<Uuid, Vec<Label>>, LabelError> {
    let rows = sqlx::query(
        "SELECT a.project_id, l.id, l.key, l.name, l.category, l.retired_at
         FROM project_label_assignment a
         JOIN project p ON p.id = a.project_id
         JOIN project_label l ON l.id = a.label_id AND l.firm_id = p.firm_id
         WHERE p.id = ANY($1) AND l.retired_at IS NULL
         ORDER BY l.category, l.name, l.key",
    )
    .bind(projects)
    .try_map(|row: PgRow| Ok((row.try_get("project_id")?, Label::read(&row)?)))
    .fetch_all(pg.pool())
    .await?;
    let mut labels: HashMap<Uuid, Vec<Label>> = HashMap::new();
    for (project_id, label) in rows {
        labels.entry(project_id).or_default().push(label);
    }
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_keys_are_stable_url_values() {
        for key in ["matter-divorce", "kids-yes", "chapter-13"] {
            assert!(validate_key(key).is_ok());
        }
        for key in ["", "-first", "Upper", "a--b", "a b", "a/other", "é"] {
            assert!(matches!(validate_key(key), Err(LabelError::InvalidKey)));
        }
        assert!(validate_key(&"a".repeat(65)).is_err());
    }

    #[tokio::test]
    async fn project_label_batches_and_retirement_are_firm_scoped() {
        use crate::postgres::fixtures::{connect, fresh_schema};

        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let firms: Vec<Uuid> = sqlx::query_scalar(
            "INSERT INTO firm (name, slug, status)
             VALUES ('Label Firm A', 'label-a', 'active'), ('Label Firm B', 'label-b', 'active')
             RETURNING id",
        )
        .fetch_all(pg.pool())
        .await
        .unwrap();
        let firm = firms[0];
        let other = firms[1];
        let project: Uuid = sqlx::query_scalar(
            "WITH entity_type_row AS (
                INSERT INTO entity_type (name) VALUES ('Label Entity Type') RETURNING id
             ), jurisdiction_row AS (
                INSERT INTO jurisdiction (name, code) VALUES ('Label Jurisdiction', 'LBL') RETURNING id
             ), entity_row AS (
                INSERT INTO entity (name, entity_type_id, jurisdiction_id)
                SELECT 'Label Entity', entity_type_row.id, jurisdiction_row.id
                FROM entity_type_row CROSS JOIN jurisdiction_row RETURNING id
             )
             INSERT INTO project (code, name, status, entity_id, firm_id)
             SELECT 'label-test', 'Label Project', 'open', id, $1 FROM entity_row RETURNING id",
        )
        .bind(firm)
        .fetch_one(pg.pool())
        .await
        .unwrap();

        create(&pg, firm, "filed", "Filed", "Court filing")
            .await
            .unwrap();
        create(&pg, firm, "filed", "Filed", "Court filing")
            .await
            .unwrap();
        assert!(matches!(
            create(&pg, firm, "filed", "Other", "Court filing").await,
            Err(LabelError::KeyConflict)
        ));
        create(&pg, other, "foreign", "Foreign", "Court filing")
            .await
            .unwrap();
        let filed = ["filed".to_string()];
        let mixed = ["filed".to_string(), "foreign".to_string()];
        assert!(matches!(
            set_assigned(&pg, firm, project, &mixed, true).await,
            Err(LabelError::Unavailable)
        ));
        assert!(!catalog(&pg, firm, project).await.unwrap()[0].assigned);
        assert!(catalog(&pg, other, project).await.unwrap().is_empty());

        set_assigned(&pg, firm, project, &filed, true)
            .await
            .unwrap();
        set_assigned(&pg, firm, project, &filed, true)
            .await
            .unwrap();
        assert_eq!(
            active_for_projects(&pg, &[project]).await.unwrap()[&project].len(),
            1
        );

        set_retired(&pg, firm, "filed", true).await.unwrap();
        let retired = catalog(&pg, firm, project).await.unwrap();
        assert!(retired[0].assigned && retired[0].label.retired_at.is_some());
        assert!(!active_for_projects(&pg, &[project])
            .await
            .unwrap()
            .contains_key(&project));
        assert!(matches!(
            set_assigned(&pg, firm, project, &filed, true).await,
            Err(LabelError::Unavailable)
        ));
        set_retired(&pg, firm, "filed", false).await.unwrap();
        assert_eq!(
            active_for_projects(&pg, &[project]).await.unwrap()[&project].len(),
            1
        );

        sqlx::query("UPDATE project SET firm_id = $1 WHERE id = $2")
            .bind(other)
            .bind(project)
            .execute(pg.pool())
            .await
            .unwrap();
        assert!(!active_for_projects(&pg, &[project])
            .await
            .unwrap()
            .contains_key(&project));
        assert!(!catalog(&pg, other, project).await.unwrap()[0].assigned);
    }
}
