//! A Brand move in `SurrealDB` must converge both PostgreSQL ownership rows.
use std::process::Command;

use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::Row;
use store::persons::{NewPerson, Role};
use store::postgres::PostgresDb;
use store::postgres_mirror::{mirror, MirrorKey, MIRROR_ENV};
use store::surreal::SurrealDb;
use uuid::Uuid;

/// The write hook caches a process-wide pool and reads environment flags.
/// Isolate the enabled-mirror scenario in a child rather than changing another test's store.
#[tokio::test]
async fn source_brand_move_converges_owner_and_seat_when_target_was_never_mirrored() {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(std::env::var("NAV_REQUIRE_POSTGRES").as_deref(), Ok("1"));
        return;
    };
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .ssl_mode(PgSslMode::Disable);
    let admin_database =
        std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".to_string());
    let admin = sqlx::PgPool::connect_with(options.clone().database(&admin_database))
        .await
        .unwrap();
    let database = format!("brand_move_{}", Uuid::now_v7().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = PostgresDb::connect(options.database(&database))
        .await
        .unwrap();
    pg.migrate_embedded(None).await.unwrap();
    pg.pool().close().await;

    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "brand_move_mirror_probe", "--nocapture"])
        .env("NAVIGATOR_BRAND_MOVE_MIRROR_PROBE", "1")
        .env_remove(MIRROR_ENV)
        .env("NAVIGATOR_POSTGRES_TENANCY_AUTHORITY", "false")
        .env("NAVIGATOR_POSTGRES_PEOPLE_AUTHORITY", "false")
        .env("NAVIGATOR_POSTGRES_HOST", "127.0.0.1")
        .env("NAVIGATOR_POSTGRES_PORT", port)
        .env("NAVIGATOR_POSTGRES_USER", "postgres")
        .env("NAVIGATOR_POSTGRES_PASSWORD", "synthetic")
        .env("NAVIGATOR_POSTGRES_DATABASE", database)
        .env("NAVIGATOR_POSTGRES_SSLMODE", "disable")
        .env_remove("NAVIGATOR_POSTGRES_ROOT_CERT")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated Brand move probe failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn practice(db: &SurrealDb, label: &str) -> store::firms::Firm {
    let suffix = Uuid::now_v7().simple().to_string();
    let jurisdiction = store::jurisdictions::create(
        db,
        &store::jurisdictions::NewJurisdiction::new(
            format!("Synthetic {label} {suffix}"),
            format!("S{}", &suffix[suffix.len() - 8..]),
            "state",
        ),
    )
    .await
    .unwrap();
    let kind = store::entity_types::create(db, &format!("Synthetic Type {suffix}"))
        .await
        .unwrap();
    let entity = store::entities::create(
        db,
        &store::entities::NewEntity {
            name: format!("Synthetic Entity {suffix}"),
            entity_type_id: kind.id,
            jurisdiction_id: jurisdiction.id,
            phone: None,
            url: None,
            xero_id: None,
            firm_anchor_key: None,
        },
    )
    .await
    .unwrap();
    let admin = store::persons::create(
        db,
        &NewPerson::with_role(
            "Synthetic Admin",
            format!("brand-admin-{suffix}@example.com"),
            Role::Admin,
        ),
    )
    .await
    .unwrap();
    store::firms::create(
        db,
        &store::firms::NewFirm {
            name: format!("Synthetic {label} {suffix}"),
            status: "active".to_string(),
            entity_id: entity.id,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap()
}

/// Both ownership rows, including their identities and owner timestamp, scoped
/// to this exact fixture Brand. A missing or duplicate seat fails the probe.
#[derive(Debug, PartialEq, Eq)]
struct OwnershipSnapshot {
    owner: Option<Uuid>,
    seat: Uuid,
    seat_id: Uuid,
    owner_updated_at: chrono::DateTime<chrono::Utc>,
}

async fn ownership_snapshot(pg: &PostgresDb, brand_id: Uuid) -> OwnershipSnapshot {
    let mut rows = sqlx::query("SELECT b.firm_id AS owner,fb.firm_id AS seat,fb.id AS seat_id,b.updated_at FROM brand b JOIN firm_brand fb ON fb.brand_key=b.brand_key WHERE b.id=$1")
        .bind(brand_id).fetch_all(pg.pool()).await.unwrap();
    assert_eq!(rows.len(), 1, "one seat must join the fixture Brand");
    let row = rows.pop().unwrap();
    OwnershipSnapshot {
        owner: row.try_get("owner").unwrap(),
        seat: row.try_get("seat").unwrap(),
        seat_id: row.try_get("seat_id").unwrap(),
        owner_updated_at: row.try_get("updated_at").unwrap(),
    }
}

async fn assert_ownership(pg: &PostgresDb, brand_id: Uuid, expected: Uuid) {
    let snapshot = ownership_snapshot(pg, brand_id).await;
    assert_eq!(snapshot.owner, Some(expected));
    assert_eq!(snapshot.seat, expected);
}

#[tokio::test]
async fn brand_move_mirror_probe() {
    if std::env::var("NAVIGATOR_BRAND_MOVE_MIRROR_PROBE").as_deref() != Ok("1") {
        return;
    }
    let source = store::test_support::mem_surreal().await;
    let from = practice(&source, "Source").await;
    let target = practice(&source, "Target").await;
    store::firms::attach_brand(&source, Role::Owner, None, from.id, "neon")
        .await
        .unwrap();
    let brand = store::brands::find_by_key(&source, "neon")
        .await
        .unwrap()
        .unwrap();
    // The compiled fixture Brand starts without this synthetic Firm owner.
    // Establish the pre-move state before mirroring its FK-dependent row.
    source
        .query("UPDATE $brand SET firm_id = $firm")
        .bind(("brand", store::surreal::record_id("brand", brand.id)))
        .bind(("firm", store::surreal::record_id("firm", from.id)))
        .await
        .unwrap()
        .check()
        .unwrap();
    assert_eq!(
        store::brands::find_by_key(&source, "neon")
            .await
            .unwrap()
            .unwrap()
            .firm_id,
        Some(from.id)
    );
    let pg = PostgresDb::from_env().await.unwrap();
    for key in [
        MirrorKey::Firm(from.id),
        MirrorKey::Brand(brand.id),
        MirrorKey::FirmBrand {
            brand_key: "neon".to_string(),
        },
    ] {
        mirror(&source, &pg, &key).await.unwrap();
    }
    let target_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM firm WHERE id = $1)")
            .bind(target.id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
    assert!(
        !target_exists,
        "the later target was created while mirroring was off"
    );

    std::env::set_var(MIRROR_ENV, "1");
    store::firms::move_brand(&source, Role::Owner, None, "neon", target.id)
        .await
        .unwrap();
    for _ in 0..2 {
        let owner: Option<Uuid> = sqlx::query_scalar("SELECT firm_id FROM brand WHERE id = $1")
            .bind(brand.id)
            .fetch_one(pg.pool())
            .await
            .unwrap();
        let seat: Uuid =
            sqlx::query_scalar("SELECT firm_id FROM firm_brand WHERE brand_key = 'neon'")
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(owner, Some(target.id));
        assert_eq!(seat, target.id);
        assert_eq!(
            store::firms::firm_id_for_brand_key(&source, "neon")
                .await
                .unwrap(),
            Some(target.id)
        );
        store::firms::move_brand(&source, Role::Owner, None, "neon", target.id)
            .await
            .unwrap();
    }
    let seats: i64 = sqlx::query_scalar("SELECT count(*) FROM firm_brand WHERE brand_key = 'neon'")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(seats, 1, "repeated moves retain one ownership seat");

    // Simulate a committed source move whose optional projections never
    // converged. Repeating the same source command must repair both copies
    // without writing the source owner timestamp or replacing its seat.
    let source_brand_before = store::brands::find_by_key(&source, "neon")
        .await
        .unwrap()
        .unwrap();
    let source_seat_before = store::firms::firm_brand_for_key(&source, "neon")
        .await
        .unwrap()
        .unwrap();
    let mut drift = pg.pool().begin().await.unwrap();
    sqlx::query("UPDATE brand SET firm_id=$2 WHERE id=$1")
        .bind(brand.id)
        .bind(from.id)
        .execute(&mut *drift)
        .await
        .unwrap();
    sqlx::query("UPDATE firm_brand SET firm_id=$2 WHERE brand_key=$1")
        .bind("neon")
        .bind(from.id)
        .execute(&mut *drift)
        .await
        .unwrap();
    drift.commit().await.unwrap();
    assert_ownership(&pg, brand.id, from.id).await;
    store::firms::move_brand(&source, Role::Owner, None, "neon", target.id)
        .await
        .unwrap();
    assert_ownership(&pg, brand.id, target.id).await;
    assert_eq!(
        store::brands::find_by_key(&source, "neon")
            .await
            .unwrap()
            .unwrap(),
        source_brand_before,
        "projection repair must preserve the source Brand and its timestamps"
    );
    assert_eq!(
        store::firms::firm_brand_for_key(&source, "neon")
            .await
            .unwrap()
            .unwrap(),
        source_seat_before,
        "projection repair must preserve the source seat identity and timestamps"
    );

    // Exercise the public PostgreSQL command itself without enabling the
    // incomplete tenancy cutover. It uses this probe's same cached pool.
    store::postgres::tenancy::move_brand("neon", from.id)
        .await
        .unwrap();
    assert_ownership(&pg, brand.id, from.id).await;
    let before = ownership_snapshot(&pg, brand.id).await;
    store::postgres::tenancy::move_brand("neon", from.id)
        .await
        .unwrap();
    assert_eq!(
        ownership_snapshot(&pg, brand.id).await,
        before,
        "an identical native move preserves seat identity and owner timestamp"
    );

    let unknown_brand = format!("unknown-brand-{}", Uuid::now_v7());
    assert!(
        matches!(store::postgres::tenancy::move_brand(&unknown_brand,target.id).await,
        Err(store::firms::FirmError::UnknownBrand(key)) if key==unknown_brand)
    );
    assert_eq!(ownership_snapshot(&pg, brand.id).await, before);
    let unknown_target = Uuid::now_v7();
    assert!(
        matches!(store::postgres::tenancy::move_brand("neon",unknown_target).await,
        Err(store::firms::FirmError::NoSuchFirm(id)) if id==unknown_target)
    );
    assert_eq!(ownership_snapshot(&pg, brand.id).await, before);

    // Fail the LAST write, after deletion of the old seat and UPDATE of the
    // Brand owner. Both earlier writes must roll back with the rejected INSERT.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE firm_brand ADD CONSTRAINT brand_move_rollback_probe CHECK (brand_key <> 'neon' OR firm_id <> '{}'::uuid)",target.id
    ))).execute(pg.pool()).await.unwrap();
    let failure = store::postgres::tenancy::move_brand("neon", target.id)
        .await
        .unwrap_err();
    assert!(
        matches!(failure,
        store::firms::FirmError::Postgres(store::postgres::tenancy::TenancyError::Query(sqlx::Error::Database(error)))
            if error.constraint()==Some("brand_move_rollback_probe")),
        "the final ownership-seat INSERT must fail its fixture constraint"
    );
    assert_ownership(&pg, brand.id, from.id).await;
    assert_eq!(
        ownership_snapshot(&pg, brand.id).await,
        before,
        "failed final INSERT restores owner, timestamp and original seat identity"
    );
    sqlx::query("ALTER TABLE firm_brand DROP CONSTRAINT brand_move_rollback_probe")
        .execute(pg.pool())
        .await
        .unwrap();
}
