//! Who may handle a matter's firm work product held in PostgreSQL.
//!
//! The [Contingency Term](crate::contingency_terms), [Outcome
//! Estimate](crate::outcome_estimates) and [Matter
//! Outcome](crate::matter_outcomes) are firm work product: only a
//! firm-side participant reads or writes them. This is the PostgreSQL twin of
//! [`crate::projects::can_access_as_lawyer_in_surreal`]: the actor must hold a
//! lawyer-tier role, and either an admin-tier role or a firm-side
//! participation row on the matter. A client-side participant, a client DRI
//! and a clerk are refused whatever their participation.
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// Whether `person_id` may read and write `project_id`'s firm work product.
///
/// # Errors
///
/// The database error when the lookup fails.
pub async fn is_firm_side(
    pg: &PostgresDb,
    person_id: Uuid,
    project_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (\
             SELECT 1 FROM person p WHERE p.id=$1 AND (\
                 p.role IN ('owner','admin') \
                 OR (p.role='lawyer' AND EXISTS (\
                     SELECT 1 FROM person_project_role r \
                     WHERE r.person_id=p.id AND r.project_id=$2 \
                     AND r.participation NOT IN ('client','counterparty') \
                     AND NOT r.is_client_dri))))",
    )
    .bind(person_id)
    .bind(project_id)
    .fetch_one(&pg.0)
    .await
}

#[cfg(test)]
pub(crate) mod tests {
    //! Synthetic matter fixtures shared by the firm work-product modules.
    use sqlx::Row;
    use uuid::Uuid;

    use crate::postgres::PostgresDb;

    /// A Firm-owned matter with a two-stage vocabulary, and one Person per
    /// side of the firm-side boundary.
    pub(crate) struct Matter {
        pub project_id: Uuid,
        pub intake: Uuid,
        pub trial: Uuid,
        /// A lawyer with a firm-side participation row.
        pub lawyer: Uuid,
        /// A lawyer-tier Person with no participation row.
        pub outside_lawyer: Uuid,
        /// The matter's client, with a client participation row.
        pub client: Uuid,
        /// A clerk with a participation row.
        pub clerk: Uuid,
        /// An admin with no participation row.
        pub admin: Uuid,
    }

    async fn person(pg: &PostgresDb, role: &str) -> Uuid {
        sqlx::query("INSERT INTO person (id,name,email,role) VALUES ($1,$2,$3,$4) RETURNING id")
            .bind(Uuid::new_v4())
            .bind(format!("Synthetic {role}"))
            .bind(format!("{role}-{}@example.com", Uuid::new_v4().simple()))
            .bind(role)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0)
    }

    async fn participate(pg: &PostgresDb, person_id: Uuid, project_id: Uuid, participation: &str) {
        sqlx::query(
            "INSERT INTO person_project_role (person_id,project_id,participation) VALUES ($1,$2,$3)",
        )
        .bind(person_id)
        .bind(project_id)
        .bind(participation)
        .execute(pg.pool())
        .await
        .unwrap();
    }

    pub(crate) async fn stage(pg: &PostgresDb, firm_id: Uuid, label: &str, position: i32) -> Uuid {
        crate::case_stages::create_definition(
            pg,
            firm_id,
            &crate::case_stages::NewStageDefinition {
                label: label.into(),
                position,
                case_type: None,
            },
        )
        .await
        .unwrap()
        .id
    }

    pub(crate) async fn matter(pg: &PostgresDb) -> Matter {
        let firm_id: Uuid = sqlx::query(
            "INSERT INTO firm (id,name,slug,status) VALUES ($1,'Synthetic Contingency Firm',$2,'active') RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(format!("cf-{}", Uuid::new_v4().simple()))
        .fetch_one(pg.pool())
        .await
        .unwrap()
        .get(0);
        let jurisdiction: Uuid = sqlx::query(
            "INSERT INTO jurisdiction (id,name,code) VALUES ($1,$2,'CTG') RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(format!(
            "Synthetic Jurisdiction {}",
            Uuid::new_v4().simple()
        ))
        .fetch_one(pg.pool())
        .await
        .unwrap()
        .get(0);
        let entity_type: Uuid =
            sqlx::query("INSERT INTO entity_type (id,name) VALUES ($1,$2) RETURNING id")
                .bind(Uuid::new_v4())
                .bind(format!("Synthetic Entity Type {}", Uuid::new_v4().simple()))
                .fetch_one(pg.pool())
                .await
                .unwrap()
                .get(0);
        let entity: Uuid = sqlx::query(
            "INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Contingency Entity',$2,$3) RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(entity_type)
        .bind(jurisdiction)
        .fetch_one(pg.pool())
        .await
        .unwrap()
        .get(0);
        let project_id: Uuid = sqlx::query(
            "INSERT INTO project (id,code,name,status,entity_id,firm_id) VALUES ($1,$2,'Synthetic Contingent Matter','open',$3,$4) RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(format!("contingent-{}", Uuid::new_v4().simple()))
        .bind(entity)
        .bind(firm_id)
        .fetch_one(pg.pool())
        .await
        .unwrap()
        .get(0);
        let intake = stage(pg, firm_id, "Intake", 1).await;
        let trial = stage(pg, firm_id, "Trial", 5).await;
        let lawyer = person(pg, "lawyer").await;
        participate(pg, lawyer, project_id, "lawyer").await;
        let outside_lawyer = person(pg, "lawyer").await;
        let client = person(pg, "client").await;
        participate(pg, client, project_id, "client").await;
        let clerk = person(pg, "clerk").await;
        participate(pg, clerk, project_id, "clerk").await;
        let admin = person(pg, "admin").await;
        Matter {
            project_id,
            intake,
            trial,
            lawyer,
            outside_lawyer,
            client,
            clerk,
            admin,
        }
    }

    mod database {
        use super::super::is_firm_side;
        use crate::postgres_mirror::tests::{connect, fresh_schema};

        #[tokio::test]
        async fn only_a_firm_side_lawyer_or_an_admin_handles_work_product() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let m = super::matter(&pg).await;
            for (person, expected, who) in [
                (m.lawyer, true, "the matter's lawyer"),
                (
                    m.admin,
                    true,
                    "an admin, as the Surreal lawyer guard allows",
                ),
                (m.outside_lawyer, false, "a lawyer off the matter"),
                (m.client, false, "the client"),
                (m.clerk, false, "a clerk"),
                (uuid::Uuid::new_v4(), false, "nobody"),
            ] {
                assert_eq!(
                    is_firm_side(&pg, person, m.project_id).await.unwrap(),
                    expected,
                    "{who}"
                );
            }
        }
    }
}
