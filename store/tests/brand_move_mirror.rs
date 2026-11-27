//! Native Brand moves preserve both ownership rows, retries, and atomic rollback.
use sqlx::Row;
use store::application_store::ApplicationStore;
use store::persons::{NewPerson, Role};
use store::postgres::PostgresDb;
use uuid::Uuid;

#[tokio::test]
async fn native_brand_move_preserves_owner_seat_and_atomic_retries() {
    store::test_support::with_postgres(native_brand_move_probe()).await;
}

async fn practice(db: &ApplicationStore, label: &str) -> (store::firms::Firm, Uuid) {
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
    let firm = store::firms::create(
        db,
        &store::firms::NewFirm {
            name: format!("Synthetic {label} {suffix}"),
            status: "active".to_string(),
            entity_id: entity.id,
            admin_dri_person_id: admin.id,
        },
    )
    .await
    .unwrap();
    (firm, admin.id)
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

#[expect(
    clippy::too_many_lines,
    reason = "one ownership lifecycle proves retries, ownership repair and final-write rollback"
)]
async fn native_brand_move_probe() {
    let db = ApplicationStore::uninitialized();
    let (from, admin_id) = practice(&db, "Origin").await;
    let (target, _) = practice(&db, "Target").await;
    let brand = store::brands::create(
        &db,
        Role::Admin,
        Some(admin_id),
        &store::brands::NewBrand {
            name: "Synthetic Native Brand".into(),
            key: "neon".into(),
            firm_id: Some(from.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    store::firms::attach_brand(&db, Role::Owner, None, from.id, "neon")
        .await
        .unwrap();
    let pg = PostgresDb::application().await.unwrap();
    assert_ownership(&pg, brand.id, from.id).await;
    assert!(matches!(
        store::firms::move_brand(&db, Role::Client, None, "neon", target.id).await,
        Err(store::firms::FirmError::NotAuthorized)
    ));
    assert_ownership(&pg, brand.id, from.id).await;
    store::firms::move_brand(&db, Role::Owner, None, "neon", target.id)
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
            store::firms::firm_id_for_brand_key(&db, "neon")
                .await
                .unwrap(),
            Some(target.id)
        );
        store::firms::move_brand(&db, Role::Owner, None, "neon", target.id)
            .await
            .unwrap();
    }
    let seats: i64 = sqlx::query_scalar("SELECT count(*) FROM firm_brand WHERE brand_key = 'neon'")
        .fetch_one(pg.pool())
        .await
        .unwrap();
    assert_eq!(seats, 1, "repeated moves retain one ownership seat");

    // An ownership command repairs both canonical rows when they drift together.
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
    store::firms::move_brand(&db, Role::Owner, None, "neon", target.id)
        .await
        .unwrap();
    assert_ownership(&pg, brand.id, target.id).await;
    let repaired = ownership_snapshot(&pg, brand.id).await;
    store::firms::move_brand(&db, Role::Owner, None, "neon", target.id)
        .await
        .unwrap();
    assert_eq!(
        ownership_snapshot(&pg, brand.id).await,
        repaired,
        "an identical repaired move preserves its seat identity and owner timestamp"
    );

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
    // A compiled reseed seats the brand on the firm the seed names and keeps
    // the website an Owner already stored.
    store::brands::set_website_url(
        &db,
        Role::Owner,
        brand.id,
        Some("https://owner.example.test"),
    )
    .await
    .unwrap();
    let before_reseed = store::postgres::tenancy::brand_by_key("neon")
        .await
        .unwrap()
        .unwrap();
    let input = store::brands::NewBrand {
        name: before_reseed.name.clone(),
        key: "neon".into(),
        firm_id: Some(target.id),
        primary_color: Some("#007c91".into()),
        typeface: Some("gorp-serif".into()),
        ..Default::default()
    };
    for _ in 0..2 {
        let refreshed =
            store::postgres::tenancy::seed_brand(&input, Some("https://example.test/compiled"))
                .await
                .unwrap();
        assert_eq!(refreshed.firm_id, Some(target.id));
        assert_eq!(refreshed.website_url, before_reseed.website_url);
        assert_eq!(
            ownership_snapshot(&pg, brand.id).await.owner,
            Some(target.id)
        );
        let seated: Uuid =
            sqlx::query_scalar("SELECT firm_id FROM firm_brand WHERE brand_key='neon'")
                .fetch_one(pg.pool())
                .await
                .unwrap();
        assert_eq!(seated, target.id);
    }
}
