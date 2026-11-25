//! Attorney fee structures: effective-dated hourly rates and matter flat
//! fees, stored relationally in PostgreSQL.
//!
//! An `AttorneyRate` prices one attorney's hour either firm-wide
//! (`project_id IS NULL`) or on one matter (the override). Rates are
//! **effective-dated and never edited**: a change inserts a row with a new
//! `effective_on` (the database rejects updates), so the table is its own
//! history. Resolution prefers the matter override, then the firm default,
//! taking the latest row effective on or before the requested date.
//!
//! The snapshot discipline lives where the money is: a Time Entry freezes
//! `rate_cents_per_hour` at creation (`crate::time_entries`), so later rate
//! edits never reprice captured work — the `notation_meter_entry` precedent.
//! A `MatterFlatFee` is the matter's current agreed fee, not history; the
//! engagement record carries the agreement. Amounts are integer cents, never
//! floats. See `docs/glossary/fee-structure.md`.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// One attorney's hourly rate, firm-wide or matter-scoped, effective from a
/// date. Rows are insert-only: a rate change is a new row, never an edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttorneyRate {
    pub id: Uuid,
    pub person_id: Uuid,
    pub project_id: Option<Uuid>,
    pub hourly_rate_cents: i32,
    pub effective_on: NaiveDate,
}

#[derive(Debug, thiserror::Error)]
pub enum FeeStructureError {
    #[error("invalid Fee Structure: {0}")]
    Invalid(&'static str),
    #[error("an Attorney Rate already exists for this scope and effective date")]
    Conflict,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("PostgreSQL is unavailable")]
    Postgres(#[from] crate::postgres::PostgresError),
}

/// The inputs to `set_hourly_rate`. Every row is new history; a duplicate
/// attorney, scope, and effective date is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAttorneyRate {
    pub person_id: Uuid,
    pub project_id: Option<Uuid>,
    pub hourly_rate_cents: i32,
    pub effective_on: NaiveDate,
}

/// Counts from reconciling an `AttorneyRate.yaml` document to one matter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedRateReport {
    pub created: usize,
    pub unchanged: usize,
}

impl NewAttorneyRate {
    fn validate(&self) -> Result<(), FeeStructureError> {
        if self.hourly_rate_cents < 0 {
            return Err(FeeStructureError::Invalid("hourly_rate_cents"));
        }
        Ok(())
    }
}

/// Record an attorney's hourly rate. The rows are insert-only: a duplicate
/// scope and effective date is refused, and captured Time Entries are immune
/// to later rows because they snapshot at creation.
pub async fn set_hourly_rate(
    pg: &PostgresDb,
    input: NewAttorneyRate,
) -> Result<AttorneyRate, FeeStructureError> {
    input.validate()?;
    let rate = sqlx::query(
        "INSERT INTO attorney_rate (person_id,project_id,hourly_rate_cents,effective_on) \
         VALUES ($1,$2,$3,$4) RETURNING id,person_id,project_id,hourly_rate_cents,effective_on",
    )
    .bind(input.person_id)
    .bind(input.project_id)
    .bind(input.hourly_rate_cents)
    .bind(input.effective_on)
    .fetch_one(&pg.0)
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .and_then(|database| database.code())
            .is_some_and(|code| code == "23505")
        {
            FeeStructureError::Conflict
        } else {
            FeeStructureError::Database(error)
        }
    })?;
    Ok(AttorneyRate {
        id: rate.get(0),
        person_id: rate.get(1),
        project_id: rate.get(2),
        hourly_rate_cents: rate.get(3),
        effective_on: rate.get(4),
    })
}

/// The hourly rate that governs this attorney on this matter on `on`: the
/// matter override if one is effective, else the firm default, else None.
pub async fn resolve_hourly_rate_cents(
    pg: &PostgresDb,
    person_id: Uuid,
    project_id: Uuid,
    on: NaiveDate,
) -> Result<Option<i32>, FeeStructureError> {
    Ok(resolve_hourly_rates_cents(pg, person_id, &[project_id], on)
        .await?
        .remove(&project_id))
}

/// Resolve the same effective fee schedule for all Projects in a billing session.
pub async fn resolve_hourly_rates_cents(
    pg: &PostgresDb,
    person_id: Uuid,
    projects: &[Uuid],
    on: NaiveDate,
) -> Result<std::collections::HashMap<Uuid, i32>, FeeStructureError> {
    let rows = sqlx::query_as::<_, (Uuid, i32)>(
        "SELECT p.id, r.hourly_rate_cents FROM unnest($2::uuid[]) AS p(id)
         CROSS JOIN LATERAL (
             SELECT hourly_rate_cents FROM attorney_rate
             WHERE person_id=$1 AND (project_id=p.id OR project_id IS NULL) AND effective_on<=$3
             ORDER BY project_id NULLS LAST, effective_on DESC, inserted_at DESC LIMIT 1
         ) AS r",
    )
    .bind(person_id)
    .bind(projects)
    .bind(on)
    .fetch_all(&pg.0)
    .await?;
    Ok(rows.into_iter().collect())
}

/// Set the matter's flat fee, replacing any prior figure — the current
/// agreement, not history; captured invoices are unaffected.
pub async fn set_flat_fee(
    pg: &PostgresDb,
    project_id: Uuid,
    fee_cents: i64,
) -> Result<i64, FeeStructureError> {
    if fee_cents < 0 {
        return Err(FeeStructureError::Invalid("fee_cents"));
    }
    sqlx::query(
        "INSERT INTO matter_flat_fee (project_id,fee_cents) VALUES ($1,$2) \
         ON CONFLICT (project_id) DO UPDATE SET fee_cents=EXCLUDED.fee_cents \
         RETURNING fee_cents",
    )
    .bind(project_id)
    .bind(fee_cents)
    .fetch_one(&pg.0)
    .await?
    .try_get(0)
    .map_err(Into::into)
}

/// The matter's current flat fee, if one is agreed.
pub async fn flat_fee_cents(
    pg: &PostgresDb,
    project_id: Uuid,
) -> Result<Option<i64>, FeeStructureError> {
    sqlx::query("SELECT fee_cents FROM matter_flat_fee WHERE project_id=$1")
        .bind(project_id)
        .fetch_optional(&pg.0)
        .await?
        .map(|row| Ok(row.get(0)))
        .transpose()
}

/// The firm's rate history for one attorney — firm defaults and matter
/// overrides together, newest effective date first. Rows are never edited,
/// so the table reads as its own history.
pub async fn rates_for_person(
    pg: &PostgresDb,
    person_id: Uuid,
) -> Result<Vec<AttorneyRate>, FeeStructureError> {
    sqlx::query(
        "SELECT id,person_id,project_id,hourly_rate_cents,effective_on \
         FROM attorney_rate WHERE person_id=$1 \
         ORDER BY effective_on DESC, inserted_at DESC",
    )
    .bind(person_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok(AttorneyRate {
            id: row.get(0),
            person_id: row.get(1),
            project_id: row.get(2),
            hourly_rate_cents: row.get(3),
            effective_on: row.get(4),
        })
    })
    .collect()
}

/// Import insert-only Attorney Rates for one Project. A matching row makes a
/// repository re-import a no-op; different terms for the same effective date
/// are refused instead of replacing the historical row.
pub async fn import_project_seed(
    pg: &PostgresDb,
    project_id: Uuid,
    yaml: &str,
    dry_run: bool,
) -> Result<SeedRateReport, FeeStructureError> {
    use model::seed::{parse_seed, validate_yaml, OperatorAttorneyRateRec, SeedModel};

    validate_yaml(SeedModel::AttorneyRate, yaml)
        .map_err(|_| FeeStructureError::Invalid("attorney_rate seed"))?;
    let records = parse_seed::<OperatorAttorneyRateRec>(
        yaml,
        SeedModel::AttorneyRate,
        &["person.email", "effective_on"],
    )
    .map_err(|_| FeeStructureError::Invalid("attorney_rate seed"))?;
    let mut tx = pg.0.begin().await?;
    let mut report = SeedRateReport::default();
    for record in records {
        let person_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT p.id FROM person p JOIN person_project_role r ON r.person_id=p.id \
             WHERE lower(p.email)=lower($1) AND p.role='lawyer' AND r.project_id=$2 \
             AND r.participation NOT IN ('client','counterparty') AND NOT r.is_client_dri",
        )
        .bind(record.person.email.trim())
        .bind(project_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(FeeStructureError::Invalid("project attorney"))?;
        let existing = sqlx::query_scalar::<_, i32>(
            "SELECT hourly_rate_cents FROM attorney_rate \
             WHERE person_id=$1 AND project_id=$2 AND effective_on=$3",
        )
        .bind(person_id)
        .bind(project_id)
        .bind(record.effective_on)
        .fetch_optional(&mut *tx)
        .await?;
        match existing {
            Some(cents) if cents == record.hourly_rate_cents => report.unchanged += 1,
            Some(_) => return Err(FeeStructureError::Conflict),
            None => {
                report.created += 1;
                if !dry_run {
                    if let Err(error) = sqlx::query(
                        "INSERT INTO attorney_rate (person_id,project_id,hourly_rate_cents,effective_on) \
                         VALUES ($1,$2,$3,$4)",
                    )
                    .bind(person_id)
                    .bind(project_id)
                    .bind(record.hourly_rate_cents)
                    .bind(record.effective_on)
                    .execute(&mut *tx)
                    .await
                    {
                        if error
                            .as_database_error()
                            .and_then(|database| database.code())
                            .is_some_and(|code| code == "23505")
                        {
                            return Err(FeeStructureError::Conflict);
                        }
                        return Err(FeeStructureError::Database(error));
                    }
                }
            }
        }
    }
    if dry_run {
        tx.rollback().await?;
    } else {
        tx.commit().await?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::{FeeStructureError, NewAttorneyRate};
    use uuid::Uuid;

    #[test]
    fn a_negative_rate_is_refused_without_touching_the_database() {
        let rate = |cents: i32| NewAttorneyRate {
            person_id: Uuid::new_v4(),
            project_id: None,
            hourly_rate_cents: cents,
            effective_on: chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
        };
        assert!(matches!(
            rate(-1).validate(),
            Err(FeeStructureError::Invalid("hourly_rate_cents"))
        ));
        assert!(rate(0).validate().is_ok(), "a pro bono rate is legitimate");
    }

    // The remaining assertions need a live PostgreSQL: resolution order,
    // effective dating, and the update-rejecting trigger are database rules.
    mod database {
        use super::super::{
            flat_fee_cents, resolve_hourly_rate_cents, set_flat_fee, set_hourly_rate,
            FeeStructureError, NewAttorneyRate,
        };
        use crate::postgres::PostgresDb;
        use crate::postgres_mirror::tests::{connect, fresh_schema};
        use crate::time_entries::{create, NewTimeEntry, TimeEntrySource};
        use chrono::{Duration, Utc};
        use sqlx::Row;
        use uuid::Uuid;

        struct Seed {
            project_id: Uuid,
            attorney: Uuid,
        }

        async fn seed(pg: &PostgresDb) -> Seed {
            let jurisdiction: Uuid = sqlx::query(
                "INSERT INTO jurisdiction (id,name,code) VALUES ($1,'Synthetic Rate Jurisdiction','RAT') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let entity_type: Uuid = sqlx::query(
                "INSERT INTO entity_type (id,name) VALUES ($1,'Synthetic Rate Entity Type') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let entity: Uuid = sqlx::query(
                "INSERT INTO entity (id,name,entity_type_id,jurisdiction_id) VALUES ($1,'Synthetic Rate Entity',$2,$3) RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(entity_type)
            .bind(jurisdiction)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let project_id: Uuid = sqlx::query(
                "INSERT INTO project (id,code,name,status,entity_id) VALUES ($1,$2,'Synthetic Rate Matter','open',$3) RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(format!("rate-{}", Uuid::new_v4().simple()))
            .bind(entity)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            let attorney: Uuid = sqlx::query(
                "INSERT INTO person (id,name,email,role) VALUES ($1,'Synthetic Rate Attorney',$2,'lawyer') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(format!("rate-{}@example.com", Uuid::new_v4().simple()))
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            sqlx::query(
                "INSERT INTO person_project_role (person_id,project_id,participation) VALUES ($1,$2,'lawyer')",
            )
            .bind(attorney)
            .bind(project_id)
            .execute(pg.pool())
            .await
            .unwrap();
            Seed {
                project_id,
                attorney,
            }
        }

        fn rate(
            attorney: Uuid,
            project_id: Option<Uuid>,
            cents: i32,
            on: chrono::NaiveDate,
        ) -> NewAttorneyRate {
            NewAttorneyRate {
                person_id: attorney,
                project_id,
                hourly_rate_cents: cents,
                effective_on: on,
            }
        }

        #[tokio::test]
        async fn matter_override_wins_then_firm_default_then_none() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let on = chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
            // No rate at all yet.
            assert_eq!(
                resolve_hourly_rate_cents(&pg, seed.attorney, seed.project_id, on)
                    .await
                    .unwrap(),
                None
            );
            // Firm default appears.
            set_hourly_rate(&pg, rate(seed.attorney, None, 25_000, on))
                .await
                .unwrap();
            assert!(
                set_hourly_rate(&pg, rate(seed.attorney, None, 30_000, on))
                    .await
                    .is_err(),
                "an existing effective date is history and cannot be replaced"
            );
            assert_eq!(
                resolve_hourly_rate_cents(&pg, seed.attorney, seed.project_id, on)
                    .await
                    .unwrap(),
                Some(25_000)
            );
            // The matter override beats the firm default.
            set_hourly_rate(&pg, rate(seed.attorney, Some(seed.project_id), 30_000, on))
                .await
                .unwrap();
            assert_eq!(
                resolve_hourly_rate_cents(&pg, seed.attorney, seed.project_id, on)
                    .await
                    .unwrap(),
                Some(30_000)
            );
            // The default still governs the attorney's other matters.
            assert_eq!(
                resolve_hourly_rate_cents(&pg, seed.attorney, Uuid::new_v4(), on)
                    .await
                    .unwrap(),
                Some(25_000)
            );
        }

        #[tokio::test]
        async fn the_latest_effective_rate_governs_and_future_rates_wait() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let jan = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
            let mar = chrono::NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
            let feb = chrono::NaiveDate::from_ymd_opt(2026, 2, 15).unwrap();
            set_hourly_rate(&pg, rate(seed.attorney, None, 20_000, jan))
                .await
                .unwrap();
            // A future raise is on file but must not govern today.
            set_hourly_rate(&pg, rate(seed.attorney, None, 40_000, mar))
                .await
                .unwrap();
            assert_eq!(
                resolve_hourly_rate_cents(&pg, seed.attorney, seed.project_id, feb)
                    .await
                    .unwrap(),
                Some(20_000),
                "a future rate never prices present work"
            );
            assert_eq!(
                resolve_hourly_rate_cents(&pg, seed.attorney, seed.project_id, mar)
                    .await
                    .unwrap(),
                Some(40_000)
            );
        }

        #[tokio::test]
        async fn rate_rows_reject_updates_but_captured_entries_keep_their_snapshot() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            let on = Utc::now().date_naive();
            set_hourly_rate(&pg, rate(seed.attorney, None, 25_000, on))
                .await
                .unwrap();
            // The attorney logs time at the resolved rate — the snapshot.
            let participation: bool = sqlx::query(
                "SELECT EXISTS(SELECT 1 FROM person_project_role WHERE person_id=$1 AND project_id=$2)",
            )
            .bind(seed.attorney)
            .bind(seed.project_id)
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            assert!(participation);
            let entry = create(
                &pg,
                seed.project_id,
                seed.attorney,
                &NewTimeEntry {
                    attorney_id: seed.attorney,
                    performed_on: on,
                    minutes: 60,
                    description: "Deposition preparation".into(),
                    billable: true,
                    rate_cents_per_hour: resolve_hourly_rate_cents(
                        &pg,
                        seed.attorney,
                        seed.project_id,
                        on,
                    )
                    .await
                    .unwrap()
                    .expect("the firm default resolves"),
                    source: TimeEntrySource::Manual,
                },
            )
            .await
            .unwrap();
            assert_eq!(entry.rate_cents_per_hour, 25_000);
            // The rate rises for future work — captured work is untouched.
            set_hourly_rate(
                &pg,
                rate(seed.attorney, None, 90_000, on + Duration::days(1)),
            )
            .await
            .unwrap();
            let reread = crate::time_entries::find_by_id(&pg, entry.id)
                .await
                .unwrap();
            assert_eq!(
                reread.rate_cents_per_hour, 25_000,
                "later rate edits never reprice captured work"
            );
            // The rate table itself refuses edits — history is append-only.
            let existing: Uuid = sqlx::query("SELECT id FROM attorney_rate WHERE person_id=$1")
                .bind(seed.attorney)
                .fetch_one(pg.pool())
                .await
                .unwrap()
                .get(0);
            let refused = sqlx::query("UPDATE attorney_rate SET hourly_rate_cents=1 WHERE id=$1")
                .bind(existing)
                .execute(pg.pool())
                .await;
            assert!(
                refused.is_err(),
                "rate rows are effective-dated, never edited"
            );
            let refused = sqlx::query("DELETE FROM attorney_rate WHERE id=$1")
                .bind(existing)
                .execute(pg.pool())
                .await;
            assert!(
                refused.is_err(),
                "rate rows are effective-dated history and cannot be deleted"
            );
        }

        #[tokio::test]
        async fn the_flat_fee_is_the_matters_current_agreement() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let seed = seed(&pg).await;
            assert_eq!(flat_fee_cents(&pg, seed.project_id).await.unwrap(), None);
            assert_eq!(
                set_flat_fee(&pg, seed.project_id, 750_000).await.unwrap(),
                750_000
            );
            assert_eq!(
                set_flat_fee(&pg, seed.project_id, 800_000).await.unwrap(),
                800_000
            );
            assert_eq!(
                flat_fee_cents(&pg, seed.project_id).await.unwrap(),
                Some(800_000),
                "the current agreement, one row per matter"
            );
            assert!(matches!(
                set_flat_fee(&pg, seed.project_id, -1).await,
                Err(FeeStructureError::Invalid("fee_cents"))
            ));
        }
    }
}
