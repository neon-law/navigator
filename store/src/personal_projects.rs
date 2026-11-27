//! A signed-in founder's Project and its individual Nevada LLC questionnaires.

use sqlx::Row as _;
use uuid::Uuid;

use crate::notations::{NewNotation, NotationError};
use crate::postgres::PostgresDb;

pub const FORMATION: &str = "nv__llc_formation";

#[must_use]
pub fn personal_code(owner: Uuid) -> String {
    format!("navigator-{owner}")
}

pub struct Formation {
    pub id: Uuid,
    pub project_code: String,
    pub label: String,
}

impl Formation {
    #[must_use]
    pub fn intake_path(&self) -> String {
        format!("/app/projects/{}/intake/{}", self.project_code, self.id)
    }
}

pub enum Start {
    Intake(String),
    Choose,
}

/// For a personal Project, check the owner and serving Brand. Firm matters use ordinary participation.
pub async fn owner_access(
    project: Uuid,
    person: Uuid,
    brand: &str,
) -> anyhow::Result<Option<bool>> {
    let pg = crate::postgres::tenancy::db().await?;
    Ok(sqlx::query_scalar(
        "SELECT p.client_dri_person_id=$2 AND p.brand=$3 AND EXISTS(
           SELECT 1 FROM firm_brand b WHERE b.firm_id=p.firm_id AND b.brand_key=p.brand)
         FROM project p WHERE p.id=$1 AND p.self_serve",
    )
    .bind(project)
    .bind(person)
    .bind(brand)
    .fetch_optional(pg.pool())
    .await?)
}

/// Resolve an old intake bookmark only while its owner can access the transfer's current destination.
pub async fn moved_intake(
    source: Uuid,
    notation: Uuid,
    owner: Uuid,
    brand: &str,
) -> anyhow::Result<Option<String>> {
    let pg = crate::postgres::tenancy::db().await?;
    let code: Option<String> = sqlx::query_scalar(
        "SELECT p.code FROM notation_project_transfer m JOIN notation n ON n.id=m.notation_id
         JOIN project p ON p.id=n.project_id AND p.id=m.destination_project_id
         JOIN person_project_role r ON r.project_id=p.id AND r.person_id=m.person_id
         JOIN firm_brand b ON b.firm_id=p.firm_id AND b.brand_key=p.brand
         WHERE m.source_project_id=$1 AND m.notation_id=$2 AND m.person_id=$3
           AND n.person_id=$3 AND p.self_serve AND p.client_dri_person_id=$3 AND p.brand=$4
           AND r.participation='client' AND r.is_client_dri AND p.status='open'",
    )
    .bind(source)
    .bind(notation)
    .bind(owner)
    .bind(brand)
    .fetch_optional(pg.pool())
    .await?;
    Ok(code.map(|code| format!("/app/projects/{code}/intake/{notation}")))
}

/// Find the founder's formations on the serving Brand, including the name saved in the questionnaire.
pub async fn formations(
    pg: &PostgresDb,
    person: Uuid,
    brand: &str,
) -> Result<Vec<Formation>, sqlx::Error> {
    list(pg.pool(), person, brand).await
}

async fn list<'a>(
    executor: impl sqlx::Executor<'a, Database = sqlx::Postgres>,
    person: Uuid,
    brand: &str,
) -> Result<Vec<Formation>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT n.id,p.code,coalesce((
             SELECT nullif(btrim(a.value->>'name'),'') FROM answer a
             WHERE a.notation_id=n.id AND a.state_name='entity__company'
             ORDER BY a.updated_at DESC,a.id DESC LIMIT 1
         ), 'Formation started ' || to_char(n.inserted_at AT TIME ZONE 'UTC','YYYY-MM-DD') || ' · ' || right(n.id::text,8)) AS label
         FROM notation n JOIN template t ON t.id=n.template_id
         JOIN project p ON p.id=n.project_id JOIN firm_brand b ON b.firm_id=p.firm_id AND b.brand_key=p.brand
         JOIN person_project_role r ON r.project_id=p.id AND r.person_id=n.person_id
         WHERE n.person_id=$1 AND p.brand=$2 AND p.self_serve AND p.client_dri_person_id=$1
           AND r.participation='client' AND r.is_client_dri AND p.status='open' AND t.code=$3
         ORDER BY n.inserted_at DESC,n.id DESC",
    ).bind(person).bind(brand).bind(FORMATION).fetch_all(executor).await?;
    rows.iter()
        .map(|row| {
            Ok(Formation {
                id: row.try_get("id")?,
                project_code: row.try_get("code")?,
                label: row.try_get("label")?,
            })
        })
        .collect()
}

/// Open once under the owner's row lock. A second business requires a confirmation from the current chooser.
/// The Project, owner participation, template pins and first questionnaire commit together.
pub async fn start_formation(
    pg: &PostgresDb,
    owner: Uuid,
    brand: &str,
    template: Uuid,
    snapshot: serde_json::Value,
    seen: Option<Uuid>,
) -> Result<Start, NotationError> {
    let mut tx = pg.pool().begin().await?;
    let owner_name: String =
        sqlx::query_scalar("SELECT name FROM person WHERE id=$1 AND role='client' FOR UPDATE")
            .bind(owner)
            .fetch_one(&mut *tx)
            .await?;
    let existing = list(&mut *tx, owner, brand).await?;
    if existing.first().map(|row| row.id) != seen {
        return Ok(match existing.as_slice() {
            [only] => Start::Intake(only.intake_path()),
            _ => Start::Choose,
        });
    }
    let firm: Uuid = sqlx::query_scalar(
        "SELECT b.firm_id FROM firm_brand b JOIN template t ON t.id=$2
         WHERE b.brand_key=$1 AND t.code=$3 AND t.project_id IS NULL AND (t.firm_id IS NULL OR t.firm_id=b.firm_id)
         FOR SHARE OF b,t",
    ).bind(brand).bind(template).bind(FORMATION).fetch_one(&mut *tx).await?;
    let code = personal_code(owner);
    let project: Option<Uuid> = sqlx::query_scalar(
        "SELECT p.id FROM project p JOIN person_project_role r ON r.project_id=p.id
         WHERE p.code=$1 AND p.brand=$2 AND p.firm_id=$3 AND p.self_serve AND p.status='open'
           AND p.client_dri_person_id=$4 AND r.person_id=$4 AND r.participation='client' AND r.is_client_dri FOR SHARE OF p,r",
    ).bind(&code).bind(brand).bind(firm).bind(owner).fetch_optional(&mut *tx).await?;
    let project = if let Some(existing) = project {
        existing
    } else {
        crate::durable_object_handover::hold_creation_epoch(&mut tx).await?;
        let entity: Uuid = sqlx::query_scalar(
                "INSERT INTO entity (name,entity_type_id,jurisdiction_id)
                 SELECT $1,t.id,j.id FROM entity_type t,jurisdiction j WHERE t.name='Human' AND j.name='United States' RETURNING id",
            ).bind(owner_name).fetch_one(&mut *tx).await?;
        create_owned(
            &mut tx,
            &code,
            "My Navigator Project",
            owner,
            entity,
            firm,
            brand,
        )
        .await?
    };
    let new =
        NewNotation::new(template, owner, project, "BEGIN").with_questionnaire_snapshot(snapshot);
    let notation = crate::postgres::notations::insert(&mut *tx, &new).await?;
    tx.commit().await?;
    Ok(Start::Intake(format!(
        "/app/projects/{code}/intake/{}",
        notation.id
    )))
}

async fn create_owned(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    code: &str,
    label: &str,
    owner: Uuid,
    entity: Uuid,
    firm: Uuid,
    brand: &str,
) -> Result<Uuid, sqlx::Error> {
    let project: Uuid = sqlx::query_scalar(
        "INSERT INTO project (code,name,status,brand,entity_id,firm_id,client_dri_person_id,self_serve)
         VALUES ($1,$2,'open',$3,$4,$5,$6,true) RETURNING id"
    ).bind(code).bind(label).bind(brand).bind(entity).bind(firm).bind(owner).fetch_one(&mut **tx).await?;
    sqlx::query(
        "INSERT INTO person_project_role (project_id,person_id,participation,is_client_dri)
                 VALUES ($1,$2,'client',true)",
    )
    .bind(project)
    .bind(owner)
    .execute(&mut **tx)
    .await?;
    crate::project_slack_deliveries::initialize_fresh(tx, project).await?;
    Ok(project)
}

/// Put one working formation in its own owner-only Project. Its ID, questionnaire pins and answers follow it.
/// A retry returns the same destination. An ineligible or inaccessible formation returns no destination.
pub async fn separate_formation(
    pg: &PostgresDb,
    owner: Uuid,
    brand: &str,
    id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let mut tx = pg.pool().begin().await?;
    sqlx::query("SELECT id FROM person WHERE id=$1 AND role='client' FOR UPDATE")
        .bind(owner)
        .fetch_one(&mut *tx)
        .await?;
    let Some(formation) = list(&mut *tx, owner, brand)
        .await?
        .into_iter()
        .find(|f| f.id == id)
    else {
        return Ok(None);
    };
    let destination = format!("navigator-llc-{id}");
    if formation.project_code == destination {
        return Ok(Some(formation.intake_path()));
    }
    if formation.project_code != personal_code(owner) {
        return Ok(None);
    }
    let source: Option<(Uuid,Uuid,Uuid)> = sqlx::query_as(
        "SELECT p.id,p.entity_id,p.firm_id FROM project p JOIN notation n ON n.project_id=p.id
         JOIN person_project_role r ON r.project_id=p.id AND r.person_id=n.person_id
         JOIN firm_brand b ON b.brand_key=p.brand AND b.firm_id=p.firm_id
         WHERE n.id=$1 AND n.state='BEGIN' AND n.person_id=$2 AND p.client_dri_person_id=$2
           AND r.is_client_dri AND r.participation='client' AND p.code=$3 AND p.self_serve AND p.status='open'
           AND p.brand=$4 AND n.package_id IS NULL FOR UPDATE OF n,p,r,b",
    ).bind(id).bind(owner).bind(&formation.project_code).bind(brand).fetch_optional(&mut *tx).await?;
    let Some((source, entity, firm)) = source else {
        return Ok(None);
    };
    let (document, signature): (bool, bool) =
        sqlx::query_as(crate::postgres::notations::DRAFT_FACTS_SQL)
            .bind(id)
            .bind(format!("notations/{id}/"))
            .fetch_one(&mut *tx)
            .await?;
    if document || signature {
        return Ok(None);
    }
    crate::durable_object_handover::hold_creation_epoch(&mut tx).await?;
    let project = create_owned(
        &mut tx,
        &destination,
        &formation.label,
        owner,
        entity,
        firm,
        brand,
    )
    .await?;
    sqlx::query("UPDATE notation SET project_id=$2,updated_at=now() WHERE id=$1")
        .bind(id)
        .bind(project)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO notation_project_transfer (notation_id,source_project_id,destination_project_id,person_id)
                 VALUES ($1,$2,$3,$4)").bind(id).bind(source).bind(project).bind(owner).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(format!("/app/projects/{destination}/intake/{id}")))
}
