use super::{
    delete, delete_for_project, find, find_by_line, insert, list, update, EstateAssetError,
    EstateAssetFields,
};
use crate::postgres::PostgresDb;
use crate::test_support::{uninitialized_store, with_required_postgres};
use uuid::Uuid;

async fn project(pg: &PostgresDb, code: &str) -> Uuid {
    let jurisdiction: Uuid = sqlx::query_scalar(
        "INSERT INTO jurisdiction (name, code, jurisdiction_type) \
         VALUES ($1, $2, 'state') RETURNING id",
    )
    .bind(format!("Synthetic Estate Jurisdiction {code}"))
    .bind(format!("estate-{code}"))
    .fetch_one(pg.pool())
    .await
    .unwrap();
    let entity_type: Uuid =
        sqlx::query_scalar("INSERT INTO entity_type (name) VALUES ($1) RETURNING id")
            .bind(format!("Synthetic Estate Entity Type {code}"))
            .fetch_one(pg.pool())
            .await
            .unwrap();
    let entity: Uuid = sqlx::query_scalar(
        "INSERT INTO entity (name, entity_type_id, jurisdiction_id) \
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(format!("Synthetic Estate Entity {code}"))
    .bind(entity_type)
    .bind(jurisdiction)
    .fetch_one(pg.pool())
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO project (code, name, status, brand, entity_id) \
         VALUES ($1, 'Synthetic Estate Plan', 'open', 'neon', $2) RETURNING id",
    )
    .bind(code)
    .bind(entity)
    .fetch_one(pg.pool())
    .await
    .unwrap()
}

fn line(category: &str, description: &str, funding_state: &str) -> EstateAssetFields {
    EstateAssetFields {
        category: category.into(),
        description: description.into(),
        passes_by: "trust".into(),
        funding_state: funding_state.into(),
        note: None,
    }
}

#[tokio::test]
async fn estate_asset_lifecycle_uses_postgres() {
    with_required_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let store = uninitialized_store();
        let project_id = project(&pg, "estate-lifecycle").await;
        let home = insert(
            &store,
            project_id,
            &line("  real_property ", " Primary residence ", "in_progress"),
        )
        .await
        .unwrap();
        assert_eq!(home.category, "real_property");
        assert_eq!(home.description, "Primary residence");

        let account = insert(
            &store,
            project_id,
            &EstateAssetFields {
                passes_by: "beneficiary_designation".into(),
                note: Some("  ".into()),
                ..line("account", "Retirement account", "funded")
            },
        )
        .await
        .unwrap();
        assert_eq!(account.note, None);
        assert_eq!(
            list(&store, project_id)
                .await
                .unwrap()
                .into_iter()
                .map(|asset| asset.id)
                .collect::<Vec<_>>(),
            vec![account.id, home.id]
        );

        let funded = update(
            &store,
            project_id,
            home.id,
            &EstateAssetFields {
                note: Some("Deed recorded".into()),
                ..line("real_property", "Primary residence", "funded")
            },
        )
        .await
        .unwrap();
        assert_eq!(funded.funding_state, "funded");
        assert_eq!(funded.note.as_deref(), Some("Deed recorded"));
        assert_eq!(funded.inserted_at, home.inserted_at);
        assert_eq!(
            find_by_line(&store, project_id, " real_property", "Primary residence ")
                .await
                .unwrap(),
            Some(funded)
        );

        delete(&store, project_id, home.id).await.unwrap();
        assert_eq!(list(&store, project_id).await.unwrap().len(), 1);
        assert!(matches!(
            delete(&store, project_id, home.id).await,
            Err(EstateAssetError::NotFound)
        ));
        delete_for_project(&store, project_id).await.unwrap();
        assert!(list(&store, project_id).await.unwrap().is_empty());
    })
    .await;
}

#[tokio::test]
async fn estate_asset_validation_and_line_uniqueness_use_postgres() {
    with_required_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let store = uninitialized_store();
        let project_id = project(&pg, "estate-validation").await;
        insert(&store, project_id, &line("vehicle", "Sedan", "not_funded"))
            .await
            .unwrap();

        let duplicate = insert(&store, project_id, &line("vehicle", "Sedan", "funded"))
            .await
            .unwrap_err();
        assert!(matches!(duplicate, EstateAssetError::Invalid(_)));
        assert!(find_by_line(&store, project_id, " vehicle", "Sedan ")
            .await
            .unwrap()
            .is_some());

        for invalid in [
            EstateAssetFields {
                passes_by: "intestacy".into(),
                ..line("account", "Checking", "funded")
            },
            line("account", "Checking", "half_funded"),
            line(" ", "Checking", "funded"),
            line("account", "", "funded"),
        ] {
            assert!(matches!(
                insert(&store, project_id, &invalid).await,
                Err(EstateAssetError::Invalid(_))
            ));
        }
        assert_eq!(list(&store, project_id).await.unwrap().len(), 1);
    })
    .await;
}

#[tokio::test]
async fn estate_assets_are_project_scoped_and_require_a_project() {
    with_required_postgres(async {
        let pg = PostgresDb::application().await.unwrap();
        let store = uninitialized_store();
        let mine = project(&pg, "estate-scope-mine").await;
        let other = project(&pg, "estate-scope-other").await;
        let stored = insert(
            &store,
            mine,
            &line("account", "Brokerage", "consent_needed"),
        )
        .await
        .unwrap();
        assert!(list(&store, other).await.unwrap().is_empty());
        assert!(find(&store, other, stored.id).await.unwrap().is_none());

        assert!(matches!(
            insert(
                &store,
                Uuid::now_v7(),
                &line("account", "Savings", "funded")
            )
            .await,
            Err(EstateAssetError::Invalid(_))
        ));
        delete_for_project(&store, other).await.unwrap();
        assert!(find(&store, mine, stored.id).await.unwrap().is_some());
        delete_for_project(&store, mine).await.unwrap();
        assert!(list(&store, mine).await.unwrap().is_empty());
    })
    .await;
}
