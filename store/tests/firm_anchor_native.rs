//! Native Entity anchor contention; every loser must have the typed refusal.
use std::sync::Arc;
use store::{
    application_store::ApplicationStore,
    entities::{self, EntityError, NewEntity},
};
use uuid::Uuid;
const RACERS: usize = 8;
fn anchor_input(key: &str, entity_type_id: Uuid, jurisdiction_id: Uuid) -> NewEntity {
    NewEntity {
        name: "Example Law PLLC".into(),
        entity_type_id,
        jurisdiction_id,
        phone: None,
        url: None,
        xero_id: None,
        firm_anchor_key: Some(key.into()),
    }
}
async fn anchor_rows(db: &ApplicationStore) -> usize {
    entities::all(db)
        .await
        .unwrap()
        .into_iter()
        .filter(entities::Entity::is_firm_anchor)
        .count()
}
async fn prerequisites(db: &ApplicationStore) -> (Uuid, Uuid) {
    let suffix = Uuid::now_v7();
    let kind = store::entity_types::create(db, &format!("Synthetic kind {suffix}"))
        .await
        .unwrap();
    let jurisdiction = store::jurisdictions::create(
        db,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic jurisdiction {suffix}"),
            suffix.to_string(),
            "state",
        ),
    )
    .await
    .unwrap();
    (kind.id, jurisdiction.id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_creates_land_exactly_one_anchor() {
    let key = "example law pllc";
    let round = 0;
    {
        store::test_support::with_postgres(async {
            let pg = store::postgres::PostgresDb::application().await.unwrap();
            let db = Arc::new(ApplicationStore::uninitialized());
            let (kind, jurisdiction) = prerequisites(&db).await;
            let claim_ids: Vec<Uuid> =
                sqlx::query_scalar("SELECT id FROM entity WHERE firm_anchor_key IS NOT NULL")
                    .fetch_all(pg.pool())
                    .await
                    .unwrap();
            assert!(
                claim_ids.is_empty(),
                "the fresh native database must have no claimed Entity before the race"
            );
            let mut tasks = tokio::task::JoinSet::new();
            for _ in 0..RACERS {
                let db = Arc::clone(&db);
                let pg = pg.clone();
                tasks.spawn(async move {
                    store::postgres::PostgresDb::with_application_db(
                        pg,
                        entities::create(&db, &anchor_input(key, kind, jurisdiction)),
                    )
                    .await
                    .expect("each racer requires native admission")
                });
            }

            let (mut created, mut refused) = (0, 0);
            let mut minted = Vec::new();
            while let Some(outcome) = tasks.join_next().await {
                match outcome.expect("a racer must not panic") {
                    Ok(entity) => {
                        created += 1;
                        minted.push(entity.id);
                    }
                    Err(EntityError::FirmAnchorTaken) => refused += 1,
                    Err(other) => panic!("round {round}: a racer failed unrecognisably: {other}"),
                }
            }

            if created != 1 {
                eprintln!("ANCHOR FORK round={round} minted={minted:?}");
                eprintln!(
                    "ANCHOR FORK round={round} claim_holder={:?}",
                    entities::all(&db).await.map(|rows| rows
                        .into_iter()
                        .filter(|row| row.firm_anchor_key.as_deref() == Some(key))
                        .map(|row| row.id)
                        .collect::<Vec<_>>()),
                );
            }
            assert_eq!(created, 1, "round {round}: exactly one racer may mint");
            assert_eq!(refused, RACERS - 1, "round {round}: the rest are refused");
            assert_eq!(
                anchor_rows(&db).await,
                1,
                "round {round}: the anchor must not fork",
            );
        })
        .await;
    }
}

#[tokio::test]
async fn a_second_entity_is_refused_the_held_anchor() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let (kind, jurisdiction) = prerequisites(&db).await;
        let key = "example law pllc";
        let first = entities::create(&db, &anchor_input(key, kind, jurisdiction))
            .await
            .expect("mint");

        let second = entities::create(&db, &anchor_input(key, kind, jurisdiction)).await;
        assert!(
            matches!(second, Err(EntityError::FirmAnchorTaken)),
            "a second anchor is refused by name, got {second:?}",
        );
        assert_eq!(
            entities::firm_anchor_holder(&db, key).await.unwrap(),
            Some(first.id),
            "the original holder keeps the claim",
        );
        assert_eq!(anchor_rows(&db).await, 1);
    })
    .await;
}

#[tokio::test]
async fn the_holder_may_rewrite_its_own_anchor() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let (kind, jurisdiction) = prerequisites(&db).await;
        let key = "example law pllc";
        let anchor = entities::create(&db, &anchor_input(key, kind, jurisdiction))
            .await
            .expect("mint");

        entities::upsert_with_id(&db, anchor.id, &anchor_input(key, kind, jurisdiction))
            .await
            .expect("a re-run seed reconciles its own row");
        let updated = entities::update(&db, anchor.id, &anchor_input(key, kind, jurisdiction))
            .await
            .expect("an in-place edit of the anchor is allowed")
            .expect("the row is written");

        assert_eq!(updated.id, anchor.id);
        assert_eq!(
            entities::firm_anchor_holder(&db, key).await.unwrap(),
            Some(anchor.id),
        );
        assert_eq!(anchor_rows(&db).await, 1);
    })
    .await;
}

#[tokio::test]
async fn releasing_the_key_frees_the_claim_for_the_next_holder() {
    store::test_support::with_postgres(async {
        let db = ApplicationStore::uninitialized();
        let (kind, jurisdiction) = prerequisites(&db).await;
        let key = "example law pllc";
        let outgoing = entities::create(&db, &anchor_input(key, kind, jurisdiction))
            .await
            .expect("mint");

        entities::set_firm_anchor_key(&db, outgoing.id, None)
            .await
            .expect("release");
        assert_eq!(
            entities::firm_anchor_holder(&db, key).await.unwrap(),
            None,
            "the claim goes with the column",
        );

        let incoming = entities::create(&db, &anchor_input(key, kind, jurisdiction))
            .await
            .expect("the freed anchor may be minted again");
        assert_ne!(incoming.id, outgoing.id);
        assert_eq!(
            entities::firm_anchor_holder(&db, key).await.unwrap(),
            Some(incoming.id),
        );
        assert_eq!(anchor_rows(&db).await, 1);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_creates_through_the_command_door_land_exactly_one_anchor() {
    let round = 0;
    store::test_support::with_postgres(async {
        let pg = store::postgres::PostgresDb::application().await.unwrap();
        let db = Arc::new(ApplicationStore::uninitialized());
        let (entity_type_id, jurisdiction_id) = prerequisites(&db).await;
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..RACERS {
            let db = Arc::clone(&db);
            let pg = pg.clone();
            tasks.spawn(async move {
                store::postgres::PostgresDb::with_application_db(
                    pg,
                    store::entity_commands::create_entity(
                        &db,
                        "Example Law PLLC",
                        &store::entity_commands::CreateEntityCommand {
                            name: "Example Law PLLC".to_string(),
                            entity_type_id,
                            jurisdiction_id,
                        },
                    ),
                )
                .await
                .expect("each command racer requires native admission")
                .map(|_| ())
            });
        }

        let (mut created, mut refused) = (0, 0);
        while let Some(outcome) = tasks.join_next().await {
            match outcome.expect("a racer must not panic") {
                Ok(()) => created += 1,
                Err(store::entity_commands::EntityCommandError::FirmAnchorExists) => refused += 1,
                Err(other) => panic!("round {round}: a racer failed unrecognisably: {other:?}"),
            }
        }

        assert_eq!(created, 1, "round {round}: exactly one racer may mint");
        assert_eq!(refused, RACERS - 1, "round {round}: the rest are refused");
        assert_eq!(
            anchor_rows(&db).await,
            1,
            "round {round}: the anchor must not fork",
        );
    })
    .await;
}
