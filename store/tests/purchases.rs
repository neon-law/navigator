//! Purchases against real PostgreSQL: one open invoice per Client and
//! Offering, durable before Xero is called, active only once paid.
use chrono::{TimeDelta, TimeZone, Utc};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use sqlx::PgPool;
use store::postgres::PostgresDb;
use store::purchases::{self, Cadence, NewPurchase, PurchaseError, PurchaseState};
use uuid::Uuid;

async fn database() -> Option<PostgresDb> {
    let Ok(port) = std::env::var("NAVIGATOR_TEST_POSTGRES_PORT") else {
        assert_ne!(
            std::env::var("NAV_REQUIRE_POSTGRES").as_deref(),
            Ok("1"),
            "PostgreSQL integration lane requires NAVIGATOR_TEST_POSTGRES_PORT"
        );
        return None;
    };
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port.parse().unwrap())
        .username("postgres")
        .password("synthetic")
        .database(
            &std::env::var("NAVIGATOR_TEST_POSTGRES_DATABASE").unwrap_or_else(|_| "smoke".into()),
        )
        .ssl_mode(PgSslMode::Disable);
    let admin = PgPool::connect_with(options.clone()).await.unwrap();
    let name = format!("purchase_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    admin.close().await;
    let pg = PostgresDb::connect(options.database(&name)).await.unwrap();
    pg.migrate_embedded(None).await.unwrap();
    Some(pg)
}

fn plan(person_id: Uuid) -> NewPurchase {
    NewPurchase {
        person_id,
        email: "client@example.com".into(),
        offering: "delete-your-data-monthly".into(),
        offering_name: "Delete Your Data, monthly".into(),
        cadence: Cadence::Month,
        amount_cents: 2_900,
        currency: "USD".into(),
        account_code: "200".into(),
    }
}

#[tokio::test]
async fn a_purchase_is_invoiced_once_and_active_only_after_payment() {
    let Some(pg) = database().await else { return };
    let now = Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap();
    let person = Uuid::new_v4();

    let first = purchases::open(&pg, &plan(person), now).await.unwrap();
    assert_eq!(first.state, PurchaseState::Invoicing);
    let again = purchases::open(&pg, &plan(person), now).await.unwrap();
    assert_eq!(again.id, first.id, "a retry finds the open Purchase");

    let invoiced = purchases::record_invoice(
        &pg,
        first.id,
        "contact",
        "inv-1",
        Some("https://in.xero.test/1"),
        now,
    )
    .await
    .unwrap();
    assert_eq!(invoiced.state, PurchaseState::Invoiced);
    assert_eq!(
        invoiced.invoice_url.as_deref(),
        Some("https://in.xero.test/1")
    );
    let replay = purchases::record_invoice(&pg, first.id, "contact", "inv-2", None, now)
        .await
        .unwrap();
    assert_eq!(
        replay.xero_invoice_id.as_deref(),
        Some("inv-1"),
        "the first invoice stands"
    );
    assert_eq!(
        purchases::open(&pg, &plan(person), now).await.unwrap().id,
        first.id
    );

    assert_eq!(purchases::invoiced(&pg, 10).await.unwrap().len(), 1);
    let later = now + TimeDelta::minutes(5);
    let active = purchases::activate(&pg, first.id, Some("rep-1"), later)
        .await
        .unwrap();
    assert_eq!(active.state, PurchaseState::Active);
    assert_eq!(active.activated_at, Some(later));
    assert_eq!(active.xero_repeating_invoice_id.as_deref(), Some("rep-1"));
    let twice = purchases::activate(&pg, first.id, None, later + TimeDelta::minutes(5))
        .await
        .unwrap();
    assert_eq!(twice.activated_at, Some(later), "activation is idempotent");
    assert!(purchases::invoiced(&pg, 10).await.unwrap().is_empty());

    let renewal = purchases::open(&pg, &plan(person), later).await.unwrap();
    assert_ne!(
        renewal.id, first.id,
        "a closed Purchase no longer blocks a new one"
    );
    assert_eq!(purchases::for_person(&pg, person).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_complimentary_consultation_is_never_invoiced_and_prices_are_positive() {
    let Some(pg) = database().await else { return };
    let now = Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap();
    let person = Uuid::new_v4();
    let consultation = NewPurchase {
        offering: "consultation".into(),
        offering_name: "Consultation".into(),
        cadence: Cadence::Once,
        amount_cents: 0,
        ..plan(person)
    };
    assert!(matches!(
        purchases::open(&pg, &consultation, now).await,
        Err(PurchaseError::InvalidAmount)
    ));
    let free = purchases::record_complimentary(&pg, &consultation, now)
        .await
        .unwrap();
    assert_eq!(free.state, PurchaseState::Complimentary);
    assert_eq!(free.amount_cents, 0);
    assert!(free.xero_invoice_id.is_none());
    assert!(matches!(
        purchases::activate(&pg, free.id, None, now).await,
        Err(PurchaseError::NotFound)
    ));
    assert!(purchases::invoiced(&pg, 10).await.unwrap().is_empty());
}
