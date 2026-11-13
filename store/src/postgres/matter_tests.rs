use super::*;
use crate::cap_table_holdings::{self, CapTableHoldingError, CapTableHoldingFields};
use crate::estate_assets::{self, EstateAssetError, EstateAssetFields};
use crate::fiduciary_roles::{self, FiduciaryRoleError, FiduciaryRoleFields};
use crate::git_repositories::{self, GitRepositoryError};
use crate::notation_drafts::{self, NewNotationDraft};
use crate::postgres_mirror::tests::{connect, fresh_schema};
use crate::project_references::Party;
use crate::relationships::Endpoint;
use crate::statutory_deadlines::{self, NewStatutoryDeadline};
use crate::surreal::SurrealDb;
use crate::test_support::{mem_surreal, seed_entity, seed_project_surreal};
use chrono::NaiveDate;

#[test]
fn the_switch_accepts_only_the_strict_values() {
    assert!(!enabled_from(None).unwrap());
    assert!(!enabled_from(Some("0")).unwrap());
    assert!(!enabled_from(Some("false")).unwrap());
    assert!(enabled_from(Some("1")).unwrap());
    assert!(enabled_from(Some("true")).unwrap());
    assert!(matches!(
        enabled_from(Some("yes")),
        Err(MatterAuthorityError::Configuration)
    ));
}

#[test]
fn every_authoritative_table_is_a_registered_record_table() {
    for table in TABLES {
        assert!(
            crate::postgres_mirror::records::columns(table).is_some(),
            "{table}"
        );
    }
}

#[test]
fn a_tagged_link_round_trips() {
    let id = Uuid::now_v7();
    assert_eq!(untagged(&tagged("entity", id)), Some(("entity", id)));
    assert_eq!(untagged(&serde_json::json!({"tb": "entity"})), None);
}

/// A Project held in both stores under one id: its Surreal row answers the
/// owning modules' reference checks, its PostgreSQL row the foreign keys.
async fn project(surreal: &SurrealDb, pg: &PostgresDb, code: &str) -> Uuid {
    let id = seed_project_surreal(surreal, code).await;
    let (entity_type, jurisdiction, entity) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    sqlx::query("INSERT INTO entity_type (id, name) VALUES ($1, 'Synthetic Entity Type')")
        .bind(entity_type)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO jurisdiction (id, name) VALUES ($1, 'Synthetic Jurisdiction')")
        .bind(jurisdiction)
        .execute(pg.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO entity (id, name, entity_type_id, jurisdiction_id) \
         VALUES ($1, 'Synthetic Entity', $2, $3)",
    )
    .bind(entity)
    .bind(entity_type)
    .bind(jurisdiction)
    .execute(pg.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO project (id, code, name, status, entity_id) VALUES ($1, $2, $2, 'open', $3)",
    )
    .bind(id)
    .bind(code)
    .bind(entity)
    .execute(pg.pool())
    .await
    .unwrap();
    id
}

async fn count(pg: &PostgresDb, table: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM \"{table}\""
    )))
    .fetch_one(pg.pool())
    .await
    .unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one synthetic matter exercises every module the switch routes"
)]
async fn matter_records_are_read_and_written_in_postgres_under_authority() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    let surreal = mem_surreal().await;
    let matter = project(&surreal, &pg, "matter-authority").await;
    let holder = Party {
        kind: Endpoint::Entity,
        id: seed_entity(&surreal).await,
    };

    with_test_db(
        pg.clone(),
        Box::pin(async {
            // Statutory deadlines replay onto one row.
            let deadline = NewStatutoryDeadline {
                project_id: matter,
                kind: "fcra",
                trigger_on: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                due_on: NaiveDate::from_ymd_opt(2026, 1, 31).unwrap(),
                statute: "15 USC 1681i",
                source: "notice:1",
            };
            let first = statutory_deadlines::record(&surreal, &deadline)
                .await
                .unwrap();
            let later = NaiveDate::from_ymd_opt(2026, 2, 1).unwrap();
            let second = statutory_deadlines::record(
                &surreal,
                &NewStatutoryDeadline {
                    due_on: later,
                    ..deadline.clone()
                },
            )
            .await
            .unwrap();
            assert_eq!((first.id, second.due_on), (second.id, later));
            assert_eq!(first.inserted_at, second.inserted_at);
            assert_eq!(
                statutory_deadlines::by_project(&surreal, matter)
                    .await
                    .unwrap(),
                vec![second]
            );

            // A notation draft is found while live.
            let draft = notation_drafts::create(
                &surreal,
                &NewNotationDraft {
                    project_id: matter,
                    slug: "engagement",
                    title: "Engagement",
                    source: "# Engagement",
                    notation_id: None,
                },
            )
            .await
            .unwrap();
            let found = notation_drafts::find_live(&surreal, draft.id)
                .await
                .unwrap();
            assert_eq!(found.map(|found| found.slug), Some("engagement".to_owned()));
            assert!(notation_drafts::find_live(&surreal, Uuid::now_v7())
                .await
                .unwrap()
                .is_none());

            // Estate assets keep one row per line.
            let line = EstateAssetFields {
                category: " Real property ".into(),
                description: "Residence".into(),
                passes_by: "trust".into(),
                funding_state: "funded".into(),
                note: None,
            };
            let asset = estate_assets::insert(&surreal, matter, &line)
                .await
                .unwrap();
            assert_eq!(asset.category, "Real property");
            assert!(matches!(
                estate_assets::insert(&surreal, matter, &line).await,
                Err(EstateAssetError::Invalid(_))
            ));
            let funded = estate_assets::update(
                &surreal,
                matter,
                asset.id,
                &EstateAssetFields {
                    funding_state: "in_progress".into(),
                    ..line.clone()
                },
            )
            .await
            .unwrap();
            assert_eq!(
                (funded.id, funded.inserted_at.as_str()),
                (asset.id, asset.inserted_at.as_str())
            );
            assert_eq!(
                estate_assets::find_by_line(&surreal, matter, "Real property", " Residence ")
                    .await
                    .unwrap(),
                Some(funded.clone())
            );
            assert_eq!(
                estate_assets::list(&surreal, matter).await.unwrap(),
                vec![funded]
            );
            estate_assets::delete(&surreal, matter, asset.id)
                .await
                .unwrap();
            assert!(matches!(
                estate_assets::delete(&surreal, matter, asset.id).await,
                Err(EstateAssetError::NotFound)
            ));
            estate_assets::insert(&surreal, matter, &line)
                .await
                .unwrap();
            estate_assets::delete_for_project(&surreal, matter)
                .await
                .unwrap();
            assert!(estate_assets::list(&surreal, matter)
                .await
                .unwrap()
                .is_empty());

            // Cap table holdings keep the tagged holder and one row per line.
            let holding = CapTableHoldingFields {
                holder,
                share_class: "Common".into(),
                shares: 100,
                vesting_note: None,
            };
            let held = cap_table_holdings::insert(&surreal, matter, &holding)
                .await
                .unwrap();
            assert_eq!(held.holder, holder);
            assert!(matches!(
                cap_table_holdings::insert(&surreal, matter, &holding).await,
                Err(CapTableHoldingError::Invalid(_))
            ));
            let preferred = cap_table_holdings::insert(
                &surreal,
                matter,
                &CapTableHoldingFields {
                    share_class: "Preferred".into(),
                    ..holding.clone()
                },
            )
            .await
            .unwrap();
            let listed: Vec<Uuid> = cap_table_holdings::list(&surreal, matter)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.id)
                .collect();
            assert_eq!(listed, vec![held.id, preferred.id]);
            assert_eq!(
                cap_table_holdings::find_by_line(&surreal, matter, holder, "Common")
                    .await
                    .unwrap()
                    .map(|row| row.id),
                Some(held.id)
            );
            let grown = cap_table_holdings::update(
                &surreal,
                matter,
                held.id,
                &CapTableHoldingFields {
                    shares: 150,
                    ..holding.clone()
                },
            )
            .await
            .unwrap();
            assert_eq!(grown.shares, 150);
            cap_table_holdings::delete(&surreal, matter, held.id)
                .await
                .unwrap();
            assert!(cap_table_holdings::find(&surreal, matter, held.id)
                .await
                .unwrap()
                .is_none());
            cap_table_holdings::delete_for_project(&surreal, matter)
                .await
                .unwrap();

            // Fiduciary roles keep one row per slot and list in roster order.
            let executor = FiduciaryRoleFields {
                role: "executor".into(),
                fiduciary: holder,
                successor_order: 1,
                notation_id: None,
            };
            let named = fiduciary_roles::insert(&surreal, matter, &executor)
                .await
                .unwrap();
            let trustee = fiduciary_roles::insert(
                &surreal,
                matter,
                &FiduciaryRoleFields {
                    role: "trustee".into(),
                    ..executor.clone()
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                fiduciary_roles::insert(&surreal, matter, &executor).await,
                Err(FiduciaryRoleError::Invalid(_))
            ));
            let roster: Vec<Uuid> = fiduciary_roles::list(&surreal, matter)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.id)
                .collect();
            assert_eq!(roster, vec![trustee.id, named.id]);
            assert_eq!(
                fiduciary_roles::find_by_slot(&surreal, matter, "executor", 1)
                    .await
                    .unwrap()
                    .map(|row| row.fiduciary),
                Some(holder)
            );
            let second = fiduciary_roles::update(
                &surreal,
                matter,
                named.id,
                &FiduciaryRoleFields {
                    successor_order: 2,
                    ..executor.clone()
                },
            )
            .await
            .unwrap();
            assert_eq!(second.successor_order, 2);
            fiduciary_roles::delete(&surreal, matter, named.id)
                .await
                .unwrap();
            assert!(fiduciary_roles::find(&surreal, matter, named.id)
                .await
                .unwrap()
                .is_none());
            fiduciary_roles::delete_for_project(&surreal, matter)
                .await
                .unwrap();

            // Git repositories are found or created once per remote.
            let created = git_repositories::find_or_create(&surreal, "remote-hash", "abc")
                .await
                .unwrap();
            let again = git_repositories::find_or_create(&surreal, "remote-hash", "def")
                .await
                .unwrap();
            assert_eq!(created, again);
            assert!(matches!(
                git_repositories::create(&surreal, "remote-hash", "def").await,
                Err(GitRepositoryError::RemoteTaken)
            ));
            assert_eq!(
                git_repositories::list_all(&surreal).await.unwrap(),
                vec![created]
            );

            // The records mirror refuses to repair an authoritative table from
            // SurrealDB, where a missing row is no deletion evidence.
            assert!(matches!(
                crate::postgres_mirror::records::mirror(
                    &surreal,
                    &pg,
                    "statutory_deadline",
                    first.id
                )
                .await,
                Err(crate::postgres_mirror::MirrorError::MatterAuthority)
            ));
        }),
    )
    .await;

    assert_eq!(count(&pg, "statutory_deadline").await, 1);
    assert_eq!(count(&pg, "notation_draft").await, 1);
    assert_eq!(count(&pg, "git_repository").await, 1);
    // Nothing was written to SurrealDB.
    assert!(statutory_deadlines::by_project(&surreal, matter)
        .await
        .unwrap()
        .is_empty());
    assert!(git_repositories::list_all(&surreal)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn the_accessor_refuses_unregistered_tables_and_columns() {
    let Some((options, database)) = fresh_schema().await else {
        return;
    };
    let pg = connect(&options, &database).await;
    assert!(matches!(
        select::<Value>(&pg, "person", serde_json::json!({}), &[]).await,
        Err(MatterAuthorityError::Unregistered { .. })
    ));
    assert!(matches!(
        select::<Value>(&pg, "git_repository", serde_json::json!({"nope": 1}), &[]).await,
        Err(MatterAuthorityError::Unregistered { .. })
    ));
    assert!(matches!(
        select::<Value>(&pg, "git_repository", serde_json::json!({}), &["nope DESC"]).await,
        Err(MatterAuthorityError::Unregistered { .. })
    ));
    assert!(matches!(
        delete(&pg, "git_repository", serde_json::json!({"id": null})).await,
        Ok(0)
    ));
}
